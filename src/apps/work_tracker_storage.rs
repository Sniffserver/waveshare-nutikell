use crate::apps::work_tracker::{WorkSession, WorkTracker};
use crate::peripherals::active_work::{
    calendar_to_unix, clear_active_session_v4_from, load_active_session_compatible_from,
    save_active_checkpoint_to, save_active_session_compatible_to, ActiveWorkError, ActiveWorkFlash,
    ActiveWorkSession,
};
use crate::peripherals::session_log::{
    append_session_to, next_session_id_from, session_log_health_from, summarize_day_from,
    LogSeverity, SessionLogError, SessionLogFlash, SessionRecord,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkSessionStoreError {
    InvalidDateTime,
    ActiveWork(ActiveWorkError),
    SessionLog(SessionLogError),
}

/// Persist a new active session before the tracker transitions to Working.
pub fn start_work_session_to<F: ActiveWorkFlash + SessionLogFlash>(
    flash: &mut F,
    start_date_ymd: (u16, u8, u8),
    start_time_hms: (u8, u8, u8),
    hourly_rate_cents: u32,
) -> Result<ActiveWorkSession, WorkSessionStoreError> {
    let start_unix = calendar_to_unix(start_date_ymd, start_time_hms)
        .ok_or(WorkSessionStoreError::InvalidDateTime)?;
    let session_id = next_session_id_from(flash).map_err(WorkSessionStoreError::SessionLog)?;
    let active = ActiveWorkSession {
        session_id,
        start_unix,
        start_date_ymd,
        start_time_hms,
        hourly_rate_cents,
        break_secs: 0,
        paused: false,
        pause_started_unix: 0,
    };
    save_active_session_compatible_to(flash, active).map_err(WorkSessionStoreError::ActiveWork)?;
    Ok(active)
}

/// Persist a pause/resume transition before changing the in-memory tracker.
pub fn checkpoint_work_session_to<F: ActiveWorkFlash + SessionLogFlash>(
    flash: &mut F,
    session_id: u64,
    session: WorkSession,
    paused: bool,
    now_unix: i64,
) -> Result<u32, WorkSessionStoreError> {
    let mut active = load_active_session_compatible_from(flash)
        .map_err(WorkSessionStoreError::ActiveWork)?
        .filter(|active| active.session_id == session_id)
        .ok_or(WorkSessionStoreError::ActiveWork(ActiveWorkError::Corrupt))?;

    if now_unix < active.start_unix {
        return Err(WorkSessionStoreError::InvalidDateTime);
    }
    if paused {
        if active.paused {
            return Ok(active.break_secs);
        }
        active.break_secs = session.break_secs;
        active.paused = true;
        active.pause_started_unix = now_unix;
    } else {
        if !active.paused {
            return Ok(active.break_secs);
        }
        let paused_secs = now_unix
            .checked_sub(active.pause_started_unix)
            .and_then(|secs| u32::try_from(secs).ok())
            .ok_or(WorkSessionStoreError::InvalidDateTime)?;
        active.break_secs = session
            .break_secs
            .max(active.break_secs.saturating_add(paused_secs));
        active.paused = false;
        active.pause_started_unix = 0;
    }
    save_active_checkpoint_to(flash, active).map_err(WorkSessionStoreError::ActiveWork)?;
    Ok(active.break_secs)
}

/// Commit a completed log entry before erasing its active recovery record.
/// Repeating this operation is safe if append succeeded but erase did not.
pub fn finish_work_session_to<F: ActiveWorkFlash + SessionLogFlash>(
    flash: &mut F,
    session_id: u64,
    session: WorkSession,
) -> Result<(), WorkSessionStoreError> {
    let record = SessionRecord {
        session_id,
        year: session.start_date_ymd.0,
        month: session.start_date_ymd.1,
        day: session.start_date_ymd.2,
        hour: session.start_time_hms.0,
        minute: session.start_time_hms.1,
        second: session.start_time_hms.2,
        elapsed_secs: session.elapsed_secs,
        break_secs: session.break_secs,
        hourly_rate_cents: session.hourly_rate_cents,
        earned_cents: session.earned_cents(),
    };
    append_session_to(flash, &record).map_err(WorkSessionStoreError::SessionLog)?;
    clear_active_session_v4_from(flash).map_err(WorkSessionStoreError::ActiveWork)
}

#[cfg(target_arch = "xtensa")]
pub fn start_work_session(
    start_date_ymd: (u16, u8, u8),
    start_time_hms: (u8, u8, u8),
    hourly_rate_cents: u32,
) -> Result<ActiveWorkSession, WorkSessionStoreError> {
    use esp_storage::FlashStorage;

    start_work_session_to(
        &mut FlashStorage::new(),
        start_date_ymd,
        start_time_hms,
        hourly_rate_cents,
    )
}

#[cfg(target_arch = "xtensa")]
pub fn finish_work_session(
    session_id: u64,
    session: WorkSession,
) -> Result<(), WorkSessionStoreError> {
    use esp_storage::FlashStorage;

    finish_work_session_to(&mut FlashStorage::new(), session_id, session)
}

#[cfg(target_arch = "xtensa")]
pub fn checkpoint_work_session(
    session_id: u64,
    session: WorkSession,
    paused: bool,
    now_unix: i64,
) -> Result<u32, WorkSessionStoreError> {
    use esp_storage::FlashStorage;

    checkpoint_work_session_to(
        &mut FlashStorage::new(),
        session_id,
        session,
        paused,
        now_unix,
    )
}

/// Refresh the WorkTracker's daily summary and health status from flash.
/// Updates start_allowed and clears storage_error if summarize_day succeeds.
pub fn refresh_work_tracker_daily_summary_from<F: SessionLogFlash>(
    tracker: &mut WorkTracker,
    flash: &mut F,
) {
    let health = session_log_health_from(flash);
    tracker.set_log_health(health);
    tracker.set_start_allowed(health.free_records > 0 && health.severity() != LogSeverity::Error);
    match summarize_day_from(flash, tracker.date_ymd) {
        Ok(summary) => {
            tracker.set_daily_summary(summary);
            tracker.set_storage_error(false);
        }
        Err(_) => {
            tracker.set_storage_error(true);
        }
    }
}

#[cfg(target_arch = "xtensa")]
pub fn refresh_work_tracker_daily_summary(tracker: &mut WorkTracker) {
    use esp_storage::FlashStorage;
    refresh_work_tracker_daily_summary_from(tracker, &mut FlashStorage::new());
}
