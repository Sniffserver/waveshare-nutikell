use std::vec;
use std::vec::Vec;
use waveshare_watch_rs::apps::work_tracker::{TrackerState, WorkSession, WorkTracker};
use waveshare_watch_rs::apps::work_tracker_storage::{
    checkpoint_work_session_to, finish_work_session_to, refresh_work_tracker_daily_summary_from,
    start_work_session_to, WorkSessionStoreError,
};
use waveshare_watch_rs::apps::{App, AppInput, AppResult};
use waveshare_watch_rs::peripherals::active_work::{
    load_active_session_compatible_from, save_active_session_to, v4_decode, ActiveWorkError,
    ActiveWorkFlash, ActiveWorkSession, V4_RECORD_LEN,
};
use waveshare_watch_rs::peripherals::flash_layout::{ACTIVE_WORK_B, WORK_LOG};
use waveshare_watch_rs::peripherals::session_log::{
    append_session_to, contains_session_id_from, summarize_day_from, SessionLogError,
    SessionLogFlash, SessionRecord, MAX_SESSION_RECORDS,
};
use waveshare_watch_rs::peripherals::touch::TouchPoint;

const LOG_HEADER_SIZE: usize = 12;
const LOG_RECORD_SIZE: usize = 48;

struct FakeFlash {
    bytes: Vec<u8>,
    writes: usize,
    fail_write_at: Option<usize>,
    fail_erase: bool,
}

impl FakeFlash {
    fn new() -> Self {
        Self {
            bytes: vec![0xFF; ACTIVE_WORK_B.end() as usize],
            writes: 0,
            fail_write_at: None,
            fail_erase: false,
        }
    }

    fn read_bytes(&self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
        let start = offset as usize;
        let end = start.checked_add(bytes.len()).ok_or(())?;
        let source = self.bytes.get(start..end).ok_or(())?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn write_bytes(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
        self.writes += 1;
        if self.fail_write_at == Some(self.writes) {
            return Err(());
        }
        let start = offset as usize;
        let end = start.checked_add(bytes.len()).ok_or(())?;
        let target = self.bytes.get_mut(start..end).ok_or(())?;
        for (stored, &next) in target.iter_mut().zip(bytes) {
            if (*stored & next) != next {
                return Err(());
            }
            *stored = next;
        }
        Ok(())
    }

    fn count_log_records(&self) -> usize {
        (0..MAX_SESSION_RECORDS)
            .filter(|&slot| {
                let start = WORK_LOG.start as usize + LOG_HEADER_SIZE + slot * LOG_RECORD_SIZE;
                self.bytes[start..start + LOG_RECORD_SIZE]
                    .iter()
                    .any(|&byte| byte != 0xFF)
            })
            .count()
    }
}

impl ActiveWorkFlash for FakeFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
        self.read_bytes(offset, bytes)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
        self.write_bytes(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
        if self.fail_erase {
            return Err(());
        }
        self.bytes
            .get_mut(from as usize..to as usize)
            .ok_or(())?
            .fill(0xFF);
        Ok(())
    }
}

impl SessionLogFlash for FakeFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
        self.read_bytes(offset, bytes)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
        self.write_bytes(offset, bytes)
    }
}

fn sample_session() -> WorkSession {
    WorkSession {
        start_date_ymd: (2026, 11, 8),
        start_time_hms: (10, 30, 0),
        elapsed_secs: 3_600,
        break_secs: 120,
        hourly_rate_cents: 5_000,
    }
}

#[test]
fn start_persists_active_record_before_returning() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), 5_000)
        .expect("clock-in should persist");

    assert_eq!(active.session_id, 1);
    assert_eq!(
        load_active_session_compatible_from(&mut flash),
        Ok(Some(active))
    );
    assert_eq!(flash.count_log_records(), 0);
}

#[test]
fn pause_and_resume_checkpoints_survive_recovery() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), 5_000)
        .expect("clock-in should persist");
    let session = WorkSession {
        elapsed_secs: 3_600,
        break_secs: 120,
        ..WorkSession::new(
            active.start_date_ymd,
            active.start_time_hms,
            active.hourly_rate_cents,
        )
    };
    let pause_at = active.start_unix + 3_720;

    assert_eq!(
        checkpoint_work_session_to(&mut flash, active.session_id, session, true, pause_at),
        Ok(120)
    );
    let paused = load_active_session_compatible_from(&mut flash)
        .unwrap()
        .unwrap();
    assert!(paused.paused);
    assert_eq!(paused.pause_started_unix, pause_at);
    assert_eq!(
        paused.recover_elapsed_and_break_secs(pause_at + 180),
        Some((3_600, 300))
    );

    let resumed_session = WorkSession {
        break_secs: 300,
        ..session
    };
    assert_eq!(
        checkpoint_work_session_to(
            &mut flash,
            active.session_id,
            resumed_session,
            false,
            pause_at + 180,
        ),
        Ok(300)
    );
    let resumed = load_active_session_compatible_from(&mut flash)
        .unwrap()
        .unwrap();
    assert!(!resumed.paused);
    assert_eq!(resumed.break_secs, 300);
    assert_eq!(resumed.session_id, active.session_id);
    assert_eq!(
        resumed.recover_elapsed_and_break_secs(pause_at + 480),
        Some((3_900, 300))
    );
}

#[test]
fn legacy_v3_active_session_migrates_and_recovers_wall_clock_time() {
    let mut flash = FakeFlash::new();
    let legacy = ActiveWorkSession {
        session_id: 1,
        start_unix: 1_798_000_000,
        start_date_ymd: (2026, 11, 8),
        start_time_hms: (10, 0, 0),
        hourly_rate_cents: 5_000,
        break_secs: 0,
        paused: false,
        pause_started_unix: 0,
    };
    save_active_session_to(&mut flash, legacy).expect("legacy V3 record should save");

    let result = load_active_session_compatible_from(&mut flash);
    let mut migrated_raw = [0; V4_RECORD_LEN];
    ActiveWorkFlash::read(&mut flash, ACTIVE_WORK_B.start, &mut migrated_raw).unwrap();
    let migrated = v4_decode(&migrated_raw).expect("migrated V4 record should be valid");
    assert_eq!(migrated.session_id, legacy.session_id);
    assert_eq!(result, Ok(Some(legacy)));
    let recovered = result.unwrap().unwrap();
    assert_eq!(
        recovered.recover_elapsed_and_break_secs(legacy.start_unix + 3_600),
        Some((3_600, 0))
    );
}

#[test]
fn corrupt_active_journal_without_recovery_copy_blocks_load() {
    let mut flash = FakeFlash::new();
    flash.bytes[ACTIVE_WORK_B.start as usize] = 0;

    assert_eq!(
        load_active_session_compatible_from(&mut flash),
        Err(ActiveWorkError::Corrupt)
    );
}

#[test]
fn start_rejects_full_log_before_writing_active_record() {
    let mut flash = FakeFlash::new();
    for session_id in 1..=MAX_SESSION_RECORDS as u64 {
        let record = SessionRecord {
            session_id,
            year: 2026,
            month: 11,
            day: 8,
            hour: 10,
            minute: 30,
            second: 0,
            elapsed_secs: 60,
            break_secs: 0,
            hourly_rate_cents: 5_000,
            earned_cents: 83,
        };
        append_session_to(&mut flash, &record).expect("test log should fill");
    }

    assert_eq!(
        start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), 5_000),
        Err(WorkSessionStoreError::SessionLog(SessionLogError::Full))
    );
    assert_eq!(load_active_session_compatible_from(&mut flash), Ok(None));
}

#[test]
fn failed_append_keeps_active_record_for_retry() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), 5_000)
        .expect("clock-in should persist");
    flash.fail_write_at = Some(flash.writes + 2);

    assert_eq!(
        finish_work_session_to(&mut flash, active.session_id, sample_session()),
        Err(WorkSessionStoreError::SessionLog(SessionLogError::Io))
    );
    assert_eq!(
        load_active_session_compatible_from(&mut flash),
        Ok(Some(active))
    );

    flash.fail_write_at = None;
    finish_work_session_to(&mut flash, active.session_id, sample_session())
        .expect("retry should finish the session");
    assert_eq!(load_active_session_compatible_from(&mut flash), Ok(None));
    assert_eq!(flash.count_log_records(), 1);
}

#[test]
fn erase_failure_retry_does_not_duplicate_completed_record() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), 5_000)
        .expect("clock-in should persist");
    flash.fail_erase = true;

    assert_eq!(
        finish_work_session_to(&mut flash, active.session_id, sample_session()),
        Err(WorkSessionStoreError::ActiveWork(ActiveWorkError::Io))
    );
    assert_eq!(
        load_active_session_compatible_from(&mut flash),
        Ok(Some(active))
    );
    assert_eq!(flash.count_log_records(), 1);

    flash.fail_erase = false;
    finish_work_session_to(&mut flash, active.session_id, sample_session())
        .expect("retry should clear the active record");
    assert_eq!(load_active_session_compatible_from(&mut flash), Ok(None));
    assert_eq!(flash.count_log_records(), 1);
    assert_eq!(
        contains_session_id_from(&mut flash, active.session_id),
        Ok(true)
    );
}

#[test]
fn cross_midnight_session_is_allocated_to_its_start_date() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (23, 30, 0), 5_000)
        .expect("clock-in should persist");
    let session = WorkSession {
        elapsed_secs: 5_400,
        ..WorkSession::new(
            active.start_date_ymd,
            active.start_time_hms,
            active.hourly_rate_cents,
        )
    };
    finish_work_session_to(&mut flash, active.session_id, session)
        .expect("cross-midnight session should finish");

    assert_eq!(
        summarize_day_from(&mut flash, (2026, 11, 8))
            .unwrap()
            .elapsed_secs,
        5_400
    );
    assert_eq!(
        summarize_day_from(&mut flash, (2026, 11, 9))
            .unwrap()
            .elapsed_secs,
        0
    );
}

#[test]
fn maximum_session_earnings_roundtrip_and_aggregate_without_wrapping() {
    let mut flash = FakeFlash::new();
    let active = start_work_session_to(&mut flash, (2026, 11, 8), (10, 30, 0), u32::MAX)
        .expect("clock-in should persist");
    let session = WorkSession {
        elapsed_secs: u32::MAX,
        ..WorkSession::new(active.start_date_ymd, active.start_time_hms, u32::MAX)
    };
    let expected = (u32::MAX as u64 * u32::MAX as u64) / 3600;
    assert_eq!(session.earned_cents(), expected);
    finish_work_session_to(&mut flash, active.session_id, session)
        .expect("maximum-value session should persist");

    let summary = summarize_day_from(&mut flash, (2026, 11, 8)).unwrap();
    assert_eq!(summary.earned_cents, expected);
    assert_eq!(summary.completed_sessions, 1);
}

#[test]
fn recovery_rejects_backward_clock_and_unrepresentable_break_time() {
    let active = ActiveWorkSession {
        session_id: 1,
        start_unix: 1_800_000_000,
        start_date_ymd: (2026, 11, 8),
        start_time_hms: (10, 0, 0),
        hourly_rate_cents: 5_000,
        break_secs: 0,
        paused: false,
        pause_started_unix: 0,
    };
    assert_eq!(
        active.recover_elapsed_and_break_secs(active.start_unix - 1),
        None
    );

    let too_much_break = ActiveWorkSession {
        break_secs: u32::MAX,
        paused: true,
        pause_started_unix: active.start_unix,
        ..active
    };
    assert_eq!(
        too_much_break.recover_elapsed_and_break_secs(active.start_unix + 1),
        None
    );
}

#[test]
fn daily_summary_counts_duplicate_log_ids_once() {
    let mut flash = FakeFlash::new();
    let record = SessionRecord {
        session_id: 1,
        year: 2026,
        month: 11,
        day: 8,
        hour: 10,
        minute: 0,
        second: 0,
        elapsed_secs: 3_600,
        break_secs: 0,
        hourly_rate_cents: 5_000,
        earned_cents: 5_000,
    };
    append_session_to(&mut flash, &record).unwrap();
    let first = WORK_LOG.start as usize + LOG_HEADER_SIZE;
    let duplicate = first + LOG_RECORD_SIZE;
    flash
        .bytes
        .copy_within(first..first + LOG_RECORD_SIZE, duplicate);

    let summary = summarize_day_from(&mut flash, (2026, 11, 8)).unwrap();
    assert_eq!(summary.elapsed_secs, 3_600);
    assert_eq!(summary.earned_cents, 5_000);
    assert_eq!(summary.completed_sessions, 1);
}

#[test]
fn tracker_actions_wait_for_a_classified_tap() {
    let mut tracker = WorkTracker::new();
    let held_touch = AppInput {
        touch: Some(TouchPoint { x: 200, y: 350 }),
        swipe: None,
        tap: false,
        accel: (0.0, 0.0, 0.0),
        dt_ms: 100,
    };
    assert!(matches!(tracker.update(&held_touch), AppResult::Continue));
    assert_eq!(tracker.state, TrackerState::Idle);

    let tap = AppInput {
        tap: true,
        ..held_touch
    };
    assert!(matches!(tracker.update(&tap), AppResult::Continue));
    assert_eq!(tracker.state, TrackerState::Working);
}

#[test]
fn refresh_daily_summary_after_log_clear_restores_start_allowed_and_clears_today_earnings() {
    let mut flash = FakeFlash::new();
    let mut tracker = WorkTracker::new();
    tracker.date_ymd = (2026, 11, 8);

    // Append a completed session for today
    let session = sample_session();
    finish_work_session_to(&mut flash, 1, session).unwrap();

    // Refresh: daily summary should reflect the session
    refresh_work_tracker_daily_summary_from(&mut tracker, &mut flash);
    assert_eq!(tracker.today_total_secs, 3_600);
    assert_eq!(tracker.today_earnings_cents, 5_000);
    assert_eq!(tracker.completed_sessions_today, 1);
    assert!(tracker.start_allowed);
    assert!(!tracker.storage_error);

    // Simulate sector erase of WORK_LOG as performed by Settings "CLEAR WORK LOG"
    flash.bytes[WORK_LOG.start as usize..WORK_LOG.end() as usize].fill(0xFF);

    // Re-synchronize tracker with flash
    refresh_work_tracker_daily_summary_from(&mut tracker, &mut flash);

    // Tracker should now be clean and ready for new sessions
    assert_eq!(tracker.today_total_secs, 0);
    assert_eq!(tracker.today_earnings_cents, 0);
    assert_eq!(tracker.completed_sessions_today, 0);
    assert!(tracker.start_allowed);
    assert!(!tracker.storage_error);
    assert_eq!(tracker.corrupt_log_records, 0);
}
