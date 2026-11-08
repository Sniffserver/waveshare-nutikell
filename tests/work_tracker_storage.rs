use std::vec;
use std::vec::Vec;
use waveshare_watch_rs::apps::work_tracker::{TrackerState, WorkSession, WorkTracker};
use waveshare_watch_rs::apps::work_tracker_storage::{
    finish_work_session_to, start_work_session_to, WorkSessionStoreError,
};
use waveshare_watch_rs::apps::{App, AppInput, AppResult};
use waveshare_watch_rs::peripherals::active_work::{
    load_active_session_from, ActiveWorkError, ActiveWorkFlash,
};
use waveshare_watch_rs::peripherals::flash_layout::{ACTIVE_WORK_B, WORK_LOG};
use waveshare_watch_rs::peripherals::session_log::{
    append_session_to, contains_session_id_from, SessionLogError, SessionLogFlash, SessionRecord,
    MAX_SESSION_RECORDS,
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
    assert_eq!(load_active_session_from(&mut flash), Ok(Some(active)));
    assert_eq!(flash.count_log_records(), 0);
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
    assert_eq!(load_active_session_from(&mut flash), Ok(None));
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
    assert_eq!(load_active_session_from(&mut flash), Ok(Some(active)));

    flash.fail_write_at = None;
    finish_work_session_to(&mut flash, active.session_id, sample_session())
        .expect("retry should finish the session");
    assert_eq!(load_active_session_from(&mut flash), Ok(None));
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
    assert_eq!(load_active_session_from(&mut flash), Ok(Some(active)));
    assert_eq!(flash.count_log_records(), 1);

    flash.fail_erase = false;
    finish_work_session_to(&mut flash, active.session_id, sample_session())
        .expect("retry should clear the active record");
    assert_eq!(load_active_session_from(&mut flash), Ok(None));
    assert_eq!(flash.count_log_records(), 1);
    assert_eq!(
        contains_session_id_from(&mut flash, active.session_id),
        Ok(true)
    );
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
