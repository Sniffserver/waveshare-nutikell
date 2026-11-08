use crate::apps::work_tracker::WorkSession;
use crate::peripherals::active_work::{
    calendar_to_unix, clear_active_session_from, save_active_session_to, ActiveWorkError,
    ActiveWorkFlash, ActiveWorkSession,
};
use crate::peripherals::session_log::{
    append_session_to, next_session_id_from, SessionLogError, SessionLogFlash, SessionRecord,
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
    };
    save_active_session_to(flash, active).map_err(WorkSessionStoreError::ActiveWork)?;
    Ok(active)
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
    clear_active_session_from(flash).map_err(WorkSessionStoreError::ActiveWork)
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

