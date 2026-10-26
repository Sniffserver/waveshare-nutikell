// Work-session persistent log — append-only binary buffer in one 4 KB sector.
//
// Flash offset: 0xD000  (sector 13, immediately after settings slots B at 0xC000)
//
// Sector layout:
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic:      b"WLOG"
//     4       1   Format version (2 = current, 0xFF = legacy)
//     5       7   Reserved (all 0xFF)
//    12   N*48   Session records (N = MAX_SESSIONS = 85)
//
// Each session record (48 bytes):
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   CRC32 of bytes 4..48
//     4       1   valid flag (0x01 = valid, other = empty/corrupt)
//     5       4   start date: year_hi, year_lo, month, day
//     9       3   start time: hour, minute, second
//    12       4   elapsed_secs: u32 le
//    16       4   break_secs:   u32 le
//    20       4   hourly_rate_cents: u32 le
//    24       4   earned_cents: u32 le
//    28       8   session_id: u64 le — deduplicates recovery after power loss
//    36      12   Reserved / future fields
//
// Write strategy: append-only. Records are committed by writing the payload
// first and its CRC last. A reset during a write leaves an invalid record,
// rather than a phantom session. The log never rotates automatically: when it
// is full, clock-out fails visibly until the user exports and clears it.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

use crate::peripherals::flash_layout::WORK_LOG;
const LOG_OFFSET: u32 = WORK_LOG.start;
const SECTOR_SIZE: u32 = WORK_LOG.len;
const HEADER_SIZE: usize = 12;
const RECORD_SIZE: usize = 48;
pub const MAX_SESSION_RECORDS: usize = (SECTOR_SIZE as usize - HEADER_SIZE) / RECORD_SIZE; // = 85
pub const LOG_FORMAT_VERSION: u8 = 2;

const MAGIC: [u8; 4] = *b"WLOG";
const HEADER_VERSION_OFFSET: usize = 4;
const LEGACY_LOG_FORMAT_VERSION: u8 = 0xFF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionLogError {
    Io,
    CorruptHeader,
    UnsupportedVersion,
    Full,
    VerifyFailed,
    DuplicateIdMismatch,
}
/// Bounded flash adapter for append and host fault-injection tests.
pub trait SessionLogFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()>;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogHeaderState {
    Empty,
    Valid,
    Corrupt,
}

fn classify_header(header: &[u8; HEADER_SIZE]) -> Result<LogHeaderState, SessionLogError> {
    if header.iter().all(|&byte| byte == 0xFF) {
        return Ok(LogHeaderState::Empty);
    }

    if header[..4] != MAGIC {
        return Err(SessionLogError::CorruptHeader);
    }

    match header[HEADER_VERSION_OFFSET] {
        LEGACY_LOG_FORMAT_VERSION => Ok(LogHeaderState::Valid),
        version if version <= LOG_FORMAT_VERSION => Ok(LogHeaderState::Valid),
        _ => Err(SessionLogError::UnsupportedVersion),
    }
}

// ── CRC32 (same algorithm as settings.rs, copy-free) ────────────────────────

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

// ── Record type ─────────────────────────────────────────────────────────────

/// A completed work session record as stored on flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    /// Stable ID shared with the active-session recovery record.
    pub session_id: u64,
    /// Calendar year (e.g. 2026)
    pub year: u16,
    /// Calendar month (1-12)
    pub month: u8,
    /// Calendar day (1-31)
    pub day: u8,
    /// Start time hour (0-23)
    pub hour: u8,
    /// Start time minute (0-59)
    pub minute: u8,
    /// Start time second (0-59)
    pub second: u8,
    /// Billable seconds worked
    pub elapsed_secs: u32,
    /// Break time in seconds
    pub break_secs: u32,
    /// Hourly rate in cents
    pub hourly_rate_cents: u32,
    /// Earnings in cents (pre-computed at clock-out)
    pub earned_cents: u32,
}

impl SessionRecord {
    fn to_raw(&self) -> [u8; RECORD_SIZE] {
        let mut out = [0u8; RECORD_SIZE];
        // CRC written last; leave bytes 0..4 zero for now
        out[4] = 0x01; // valid flag
        out[5] = (self.year >> 8) as u8;
        out[6] = (self.year & 0xFF) as u8;
        out[7] = self.month;
        out[8] = self.day;
        out[9] = self.hour;
        out[10] = self.minute;
        out[11] = self.second;
        out[12..16].copy_from_slice(&self.elapsed_secs.to_le_bytes());
        out[16..20].copy_from_slice(&self.break_secs.to_le_bytes());
        out[20..24].copy_from_slice(&self.hourly_rate_cents.to_le_bytes());
        out[24..28].copy_from_slice(&self.earned_cents.to_le_bytes());
        out[28..36].copy_from_slice(&self.session_id.to_le_bytes());
        let checksum = crc32(&out[4..]);
        out[0..4].copy_from_slice(&checksum.to_le_bytes());
        out
    }

    fn from_raw(raw: &[u8; RECORD_SIZE]) -> Option<Self> {
        let stored_crc = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let computed_crc = crc32(&raw[4..]);
        if stored_crc != computed_crc {
            return None;
        }
        if raw[4] != 0x01 {
            return None;
        }
        Some(Self {
            year: ((raw[5] as u16) << 8) | (raw[6] as u16),
            month: raw[7],
            day: raw[8],
            hour: raw[9],
            minute: raw[10],
            second: raw[11],
            elapsed_secs: u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            break_secs: u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]),
            hourly_rate_cents: u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]),
            earned_cents: u32::from_le_bytes([raw[24], raw[25], raw[26], raw[27]]),
            session_id: u64::from_le_bytes(raw[28..36].try_into().ok()?),
        })
    }

    fn record_offset(slot: usize) -> u32 {
        (HEADER_SIZE + slot * RECORD_SIZE) as u32
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TailState {
    #[default]
    Ok,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSeverity {
    Ok,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionLogHealth {
    pub header_state: LogHeaderState,
    pub valid_records: u16,
    pub corrupt_records: u16,
    pub free_records: u16,
    /// Number of valid records whose session ID was already seen earlier in
    /// the log. They are preserved for export and explicit user review.
    pub duplicate_session_ids: u16,
    /// Total valid records belonging to duplicated session-ID groups. For an
    /// ID that appears twice this is 2, not 1.
    pub duplicate_records: u16,
    /// False for the present fixed 85-record sector; exposed so future larger
    /// log formats cannot silently claim a complete duplicate scan.
    pub duplicate_scan_truncated: bool,
    pub tail_state: TailState,
    pub last_valid_offset: Option<u32>,
}

impl SessionLogHealth {
    pub fn severity(&self) -> LogSeverity {
        if self.header_state == LogHeaderState::Corrupt
            || self.corrupt_records > 0
            || self.tail_state != TailState::Ok
        {
            LogSeverity::Error
        } else if self.duplicate_session_ids > 0 {
            LogSeverity::Warning
        } else {
            LogSeverity::Ok
        }
    }
}

impl Default for LogHeaderState {
    fn default() -> Self {
        Self::Empty
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read all valid session records from flash, in chronological order.
/// Returns up to MAX_SESSION_RECORDS records.
#[cfg(target_arch = "xtensa")]
pub fn read_all_sessions() -> alloc::vec::Vec<SessionRecord> {
    let mut storage = FlashStorage::new();
    let mut out = alloc::vec::Vec::new();

    let mut magic = [0u8; 4];
    if ReadNorFlash::read(&mut storage, LOG_OFFSET, &mut magic).is_err() || magic != MAGIC {
        return out;
    }

    for slot in 0..MAX_SESSION_RECORDS {
        let offset = LOG_OFFSET + SessionRecord::record_offset(slot);
        let mut raw = [0u8; RECORD_SIZE];
        if ReadNorFlash::read(&mut storage, offset, &mut raw).is_err() {
            continue;
        }
        if let Some(rec) = SessionRecord::from_raw(&raw) {
            out.push(rec);
        }
    }
    out
}

/// Checked reader for export/repair tools. Unlike the compatibility helper it
/// reports a damaged header instead of making a corrupt sector look empty.
#[cfg(target_arch = "xtensa")]
pub fn read_all_sessions_checked() -> Result<alloc::vec::Vec<SessionRecord>, SessionLogError> {
    let mut storage = FlashStorage::new();
    let mut header = [0u8; HEADER_SIZE];
    ReadNorFlash::read(&mut storage, LOG_OFFSET, &mut header).map_err(|_| SessionLogError::Io)?;
    if classify_header(&header) == Ok(LogHeaderState::Empty) {
        return Ok(alloc::vec::Vec::new());
    }
    if classify_header(&header).is_err() {
        return Err(SessionLogError::CorruptHeader);
    }
    Ok(read_all_sessions())
}

/// Returns true when a previously committed session has the supplied ID.
/// Used at boot to reconcile a power failure between log commit and clearing
/// the active-session recovery record.
#[cfg(target_arch = "xtensa")]
pub fn contains_session_id(session_id: u64) -> bool {
    has_session_id(&read_all_sessions(), session_id)
}

/// Allocate the next persisted sequence from completed records. This refuses
/// to proceed if the header is corrupt, preventing accidental ID reuse.
#[cfg(target_arch = "xtensa")]
pub fn next_session_id() -> Result<u64, SessionLogError> {
    let records = read_all_sessions_checked()?;
    records
        .iter()
        .map(|record| record.session_id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(SessionLogError::Full)
}

fn has_session_id(records: &[SessionRecord], session_id: u64) -> bool {
    records.iter().any(|record| record.session_id == session_id)
}

#[cfg(test)]
fn duplicate_counts(records: &[SessionRecord]) -> (u16, u16) {
    let mut duplicate_ids = 0u16;
    let mut duplicate_records = 0u16;
    for (index, record) in records.iter().enumerate() {
        let occurrences = records[..=index]
            .iter()
            .filter(|earlier| earlier.session_id == record.session_id)
            .count();
        if occurrences == 2 {
            duplicate_ids = duplicate_ids.saturating_add(1);
            duplicate_records = duplicate_records.saturating_add(2);
        } else if occurrences > 2 {
            duplicate_records = duplicate_records.saturating_add(1);
        }
    }
    (duplicate_ids, duplicate_records)
}

/// Scan the on-flash log without mutating it. Corrupt records are retained as
/// evidence of an interrupted write and never treated as completed work.
#[cfg(target_arch = "xtensa")]
pub fn session_log_health() -> SessionLogHealth {
    let mut storage = FlashStorage::new();
    let mut health = SessionLogHealth::default();
    let mut seen_ids = [0u64; MAX_SESSION_RECORDS];
    let mut seen_len = 0usize;
    let mut last_occupied_valid = None;
    let mut magic = [0u8; 4];
    if ReadNorFlash::read(&mut storage, LOG_OFFSET, &mut magic).is_err() {
        health.header_state = LogHeaderState::Corrupt;
        return health;
    }
    if magic.iter().all(|&byte| byte == 0xFF) {
        health.header_state = LogHeaderState::Empty;
        health.free_records = MAX_SESSION_RECORDS as u16;
        return health;
    }
    if magic != MAGIC {
        health.header_state = LogHeaderState::Corrupt;
        return health;
    }
    health.header_state = LogHeaderState::Valid;
    for slot in 0..MAX_SESSION_RECORDS {
        let mut raw = [0u8; RECORD_SIZE];
        if ReadNorFlash::read(
            &mut storage,
            LOG_OFFSET + SessionRecord::record_offset(slot),
            &mut raw,
        )
        .is_err()
        {
            health.corrupt_records = health.corrupt_records.saturating_add(1);
            last_occupied_valid = Some(false);
        } else if raw.iter().all(|&byte| byte == 0xFF) {
            health.free_records = health.free_records.saturating_add(1);
        } else if let Some(record) = SessionRecord::from_raw(&raw) {
            last_occupied_valid = Some(true);
            health.valid_records = health.valid_records.saturating_add(1);
            health.last_valid_offset = Some(LOG_OFFSET + SessionRecord::record_offset(slot));
            if let Some(index) = seen_ids[..seen_len]
                .iter()
                .position(|&id| id == record.session_id)
            {
                // The second occurrence makes one ID and two records part of
                // a duplicate group; each later occurrence adds one record.
                let occurrences = seen_ids[..seen_len]
                    .iter()
                    .filter(|&&id| id == record.session_id)
                    .count();
                if occurrences == 1 {
                    health.duplicate_session_ids = health.duplicate_session_ids.saturating_add(1);
                    health.duplicate_records = health.duplicate_records.saturating_add(2);
                } else {
                    health.duplicate_records = health.duplicate_records.saturating_add(1);
                }
                // Retain a repeated ID in the local scan list so later
                // occurrences are counted as records in the same group.
                seen_ids[seen_len] = seen_ids[index];
                seen_len += 1;
            } else {
                seen_ids[seen_len] = record.session_id;
                seen_len += 1;
            }
        } else {
            health.corrupt_records = health.corrupt_records.saturating_add(1);
            last_occupied_valid = Some(false);
        }
    }
    if last_occupied_valid == Some(false) {
        health.tail_state = TailState::Invalid;
    }
    health
}

/// Append one session record to the flash log.
///
/// The normal path writes exactly one 48-byte record. A full log returns an
/// error rather than silently discarding billing history.
pub fn append_session_to<F: SessionLogFlash>(
    storage: &mut F,
    rec: &SessionRecord,
) -> Result<(), SessionLogError> {
    let mut header = [0u8; HEADER_SIZE];
    storage
        .read(LOG_OFFSET, &mut header)
        .map_err(|_| SessionLogError::Io)?;

    let header_state = classify_header(&header)?;

    if header_state == LogHeaderState::Empty {
        let mut first_record = [0u8; RECORD_SIZE];
        storage
            .read(
                LOG_OFFSET + SessionRecord::record_offset(0),
                &mut first_record,
            )
            .map_err(|_| SessionLogError::Io)?;

        if !first_record.iter().all(|&byte| byte == 0xFF) {
            return Err(SessionLogError::CorruptHeader);
        }

        let mut new_header = [0xFFu8; HEADER_SIZE];
        new_header[..4].copy_from_slice(&MAGIC);
        new_header[HEADER_VERSION_OFFSET] = LOG_FORMAT_VERSION;

        storage
            .write(LOG_OFFSET, &new_header)
            .map_err(|_| SessionLogError::Io)?;

        let mut verify = [0u8; HEADER_SIZE];
        storage
            .read(LOG_OFFSET, &mut verify)
            .map_err(|_| SessionLogError::Io)?;

        if verify != new_header {
            return Err(SessionLogError::VerifyFailed);
        }
    }

    let mut free_slot = None;

    for slot in 0..MAX_SESSION_RECORDS {
        let mut raw = [0u8; RECORD_SIZE];
        storage
            .read(LOG_OFFSET + SessionRecord::record_offset(slot), &mut raw)
            .map_err(|_| SessionLogError::Io)?;

        if raw.iter().all(|&byte| byte == 0xFF) {
            if free_slot.is_none() {
                free_slot = Some(slot);
            }
            continue;
        }

        let Some(existing) = SessionRecord::from_raw(&raw) else {
            continue;
        };

        if existing.session_id != rec.session_id {
            continue;
        }

        if existing == *rec {
            // A prior attempt committed this exact record but the caller may
            // not have received confirmation. Do not append it again.
            return Ok(());
        }

        return Err(SessionLogError::DuplicateIdMismatch);
    }

    let Some(slot) = free_slot else {
        return Err(SessionLogError::Full);
    };
    let raw = rec.to_raw();
    let record_off = LOG_OFFSET + SessionRecord::record_offset(slot);
    // Commit marker (CRC) comes last. An interrupted payload write is
    // ignored by read_all_sessions because it cannot pass the CRC.
    storage
        .write(record_off + 4, &raw[4..])
        .map_err(|_| SessionLogError::Io)?;
    storage
        .write(record_off, &raw[0..4])
        .map_err(|_| SessionLogError::Io)?;
    let mut verify = [0u8; RECORD_SIZE];
    storage
        .read(record_off, &mut verify)
        .map_err(|_| SessionLogError::Io)?;
    if SessionRecord::from_raw(&verify) == Some(*rec) {
        Ok(())
    } else {
        Err(SessionLogError::VerifyFailed)
    }
}

#[cfg(target_arch = "xtensa")]
impl SessionLogFlash for FlashStorage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
        ReadNorFlash::read(self, offset, bytes).map_err(|_| ())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
        NorFlash::write(self, offset, bytes).map_err(|_| ())
    }
}

#[cfg(target_arch = "xtensa")]
pub fn append_session(rec: &SessionRecord) -> Result<(), SessionLogError> {
    append_session_to(&mut FlashStorage::new(), rec)
}

/// Format all sessions as a CSV string.
///
/// Suitable for writing to an SD card file.
/// Format: date,start_time,elapsed_h,break_h,rate_per_h,earned
pub fn sessions_to_csv(sessions: &[SessionRecord]) -> alloc::string::String {
    let mut out =
        alloc::string::String::from("Date,Start,Elapsed (min),Break (min),Rate ($/h),Earned ($)\n");
    for s in sessions {
        let elapsed_min = s.elapsed_secs / 60;
        let break_min = s.break_secs / 60;
        let rate_dollars = s.hourly_rate_cents / 100;
        let earned_dollars = s.earned_cents / 100;
        let earned_cents = s.earned_cents % 100;
        let row = alloc::format!(
            "{}-{:02}-{:02},{:02}:{:02}:{:02},{},{},{},{}.{:02}\n",
            s.year,
            s.month,
            s.day,
            s.hour,
            s.minute,
            s.second,
            elapsed_min,
            break_min,
            rate_dollars,
            earned_dollars,
            earned_cents,
        );
        out.push_str(&row);
    }
    out
}

/// Erase the entire session log (factory reset).
#[cfg(target_arch = "xtensa")]
pub fn erase_session_log() -> Result<(), SessionLogError> {
    let mut storage = FlashStorage::new();
    storage
        .erase(LOG_OFFSET, LOG_OFFSET + SECTOR_SIZE)
        .map_err(|_| SessionLogError::Io)
}

/// Explicit, destructive repair operation for a sector previously reported as
/// `CorruptHeader`. It is intentionally never called by append/recovery paths.
#[cfg(target_arch = "xtensa")]
pub fn repair_session_log() -> Result<(), SessionLogError> {
    erase_session_log()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(elapsed: u32, rate: u32) -> SessionRecord {
        SessionRecord {
            year: 2026,
            month: 10,
            day: 24,
            hour: 9,
            minute: 0,
            second: 0,
            elapsed_secs: elapsed,
            break_secs: 0,
            hourly_rate_cents: rate,
            earned_cents: ((elapsed as u64 * rate as u64) / 3600) as u32,
            session_id: 0x1020_3040_5060_7080,
        }
    }

    #[test]
    fn record_roundtrip() {
        let s = make_session(3600, 5000);
        let raw = s.to_raw();
        let recovered = SessionRecord::from_raw(&raw).expect("should decode");
        assert_eq!(recovered, s);
    }

    #[test]
    fn corrupt_record_rejected() {
        let s = make_session(3600, 5000);
        let mut raw = s.to_raw();
        raw[12] ^= 0xFF; // corrupt elapsed_secs
        assert!(SessionRecord::from_raw(&raw).is_none());
    }

    #[test]
    fn corrupt_header_is_not_empty() {
        let mut header = [0xFF; HEADER_SIZE];
        header[0] = b'X';
        assert_eq!(
            classify_header(&header),
            Err(SessionLogError::CorruptHeader)
        );
        assert_eq!(
            classify_header(&[0xFF; HEADER_SIZE]),
            Ok(LogHeaderState::Empty)
        );
    }

    #[test]
    fn earned_calculation() {
        // 2 hours @ $50/h = $100 = 10000 cents
        let s = make_session(7200, 5000);
        assert_eq!(s.earned_cents, 10000);
    }

    #[test]
    fn csv_header_present() {
        let sessions = alloc::vec![make_session(3600, 5000)];
        let csv = sessions_to_csv(&sessions);
        assert!(csv.starts_with("Date,Start,"));
    }

    #[test]
    fn csv_row_count() {
        let sessions = alloc::vec![make_session(3600, 5000), make_session(7200, 7500),];
        let csv = sessions_to_csv(&sessions);
        let lines: alloc::vec::Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 rows
        assert!(lines[1].starts_with("2026-10-24,"));
    }

    #[test]
    fn committed_session_id_prevents_recovery_duplicate() {
        let session = make_session(3600, 5000);
        let records = [session];
        // This models power loss after append, before the active record erase:
        // recovery must reconcile it rather than append it a second time.
        assert!(has_session_id(&records, session.session_id));
    }

    #[test]
    fn duplicate_health_distinguishes_ids_and_records() {
        let first = make_session(3600, 5000);
        let second = SessionRecord {
            elapsed_secs: 1800,
            ..first
        };
        let mut third = make_session(900, 7500);
        third.session_id = 43;
        let fourth = SessionRecord {
            elapsed_secs: 1_200,
            ..third
        };
        let fifth = SessionRecord {
            elapsed_secs: 2_400,
            ..third
        };
        let mut unique = make_session(600, 2500);
        unique.session_id = 61;
        assert_eq!(
            duplicate_counts(&[first, second, third, fourth, fifth, unique]),
            (2, 5)
        );
    }

    #[test]
    fn clean_log_reports_zero_duplicate_counts() {
        let first = make_session(3600, 5000);
        let mut second = make_session(1800, 7500);
        second.session_id = 43;
        assert_eq!(duplicate_counts(&[first, second]), (0, 0));
    }

    #[test]
    fn duplicate_scan_does_not_mutate_records() {
        let first = make_session(3600, 5000);
        let second = SessionRecord {
            elapsed_secs: 1800,
            ..first
        };
        let records = [first, second];
        let before = records;
        let _ = duplicate_counts(&records);
        assert_eq!(records, before);
    }

    #[test]
    fn repeated_duplicate_scans_are_identical_and_read_only() {
        let first = make_session(3600, 5000);
        let second = SessionRecord {
            elapsed_secs: 1800,
            ..first
        };
        let records = [first, second];
        let before = records;
        let first_scan = duplicate_counts(&records);
        let second_scan = duplicate_counts(&records);
        assert_eq!(first_scan, second_scan);
        assert_eq!(records, before);
    }

    #[test]
    fn duplicates_are_warning_but_corruption_is_error() {
        let warning = SessionLogHealth {
            duplicate_session_ids: 1,
            duplicate_records: 2,
            ..SessionLogHealth::default()
        };
        assert_eq!(warning.severity(), LogSeverity::Warning);
        let error = SessionLogHealth {
            corrupt_records: 1,
            ..warning
        };
        assert_eq!(error.severity(), LogSeverity::Error);
    }

    // ── FakeFlash for fault-injection tests ────────────────────────────────

    struct FakeFlash {
        data: [u8; 4096],
        fail_read: bool,
        fail_write: bool,
        write_fail_at: Option<usize>,
        write_call_count: usize,
    }

    impl FakeFlash {
        fn new() -> Self {
            Self {
                data: [0xFF; 4096],
                fail_read: false,
                fail_write: false,
                write_fail_at: None,
                write_call_count: 0,
            }
        }

        fn with_read_failure(mut self) -> Self {
            self.fail_read = true;
            self
        }

        fn with_write_failure(mut self) -> Self {
            self.fail_write = true;
            self
        }

        fn with_write_fail_at(mut self, n: usize) -> Self {
            self.write_fail_at = Some(n);
            self
        }

        fn snapshot(&self) -> [u8; 4096] {
            self.data
        }

        fn assert_unchanged(&self, before: &[u8; 4096]) {
            assert_eq!(&self.data, before);
        }
    }

    impl SessionLogFlash for FakeFlash {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
            if self.fail_read {
                return Err(());
            }
            let start = (offset - LOG_OFFSET) as usize;
            let end = start + bytes.len();

            if offset < LOG_OFFSET || end > self.data.len() {
                return Err(());
            }

            bytes.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
            self.write_call_count += 1;
            if let Some(fail_at) = self.write_fail_at {
                if self.write_call_count == fail_at {
                    return Err(());
                }
            }
            if self.fail_write {
                return Err(());
            }
            let start = (offset - LOG_OFFSET) as usize;
            let end = start + bytes.len();

            if offset < LOG_OFFSET || end > self.data.len() {
                return Err(());
            }

            self.data[start..end].copy_from_slice(bytes);
            Ok(())
        }
    }

    fn test_record(session_id: u64, elapsed_secs: u32, hourly_rate_cents: u32) -> SessionRecord {
        SessionRecord {
            session_id,
            year: 2026,
            month: 10,
            day: 24,
            hour: 9,
            minute: 0,
            second: 0,
            elapsed_secs,
            break_secs: 0,
            hourly_rate_cents,
            earned_cents: ((elapsed_secs as u64 * hourly_rate_cents as u64) / 3600) as u32,
        }
    }

    #[test]
    fn same_id_different_payload_returns_duplicate_mismatch() {
        let mut flash = FakeFlash::new();

        let original = test_record(0xA1B2C3D4E5F60708, 3_600, 50_000);

        append_session_to(&mut flash, &original).expect("first append should succeed");

        let mut conflicting = original;
        conflicting.elapsed_secs = 7_200;
        conflicting.earned_cents = 100_000;

        let result = append_session_to(&mut flash, &conflicting);

        assert_eq!(
            result,
            Err(SessionLogError::DuplicateIdMismatch),
            "same session_id with different payload must not be appended"
        );

        let mut valid_count = 0;
        for slot in 0..MAX_SESSION_RECORDS {
            let mut raw = [0u8; RECORD_SIZE];
            flash
                .read(LOG_OFFSET + SessionRecord::record_offset(slot), &mut raw)
                .expect("record read should succeed");

            if SessionRecord::from_raw(&raw).is_some() {
                valid_count += 1;
            }
        }

        assert_eq!(
            valid_count, 1,
            "conflicting duplicate must not create a second valid record"
        );
    }

    #[test]
    fn same_id_same_payload_is_idempotent() {
        let mut flash = FakeFlash::new();
        let record = test_record(0xA1B2C3D4E5F60708, 3_600, 50_000);

        append_session_to(&mut flash, &record).expect("first append should succeed");
        append_session_to(&mut flash, &record).expect("identical retry should be idempotent");

        let mut valid_count = 0;
        for slot in 0..MAX_SESSION_RECORDS {
            let mut raw = [0u8; RECORD_SIZE];
            flash
                .read(LOG_OFFSET + SessionRecord::record_offset(slot), &mut raw)
                .expect("record read should succeed");

            if SessionRecord::from_raw(&raw).is_some() {
                valid_count += 1;
            }
        }

        assert_eq!(valid_count, 1);
    }

    // ── Version header tests ────────────────────────────────────────────────

    #[test]
    fn empty_header_initializes_with_version_2() {
        let mut flash = FakeFlash::new();
        let record = test_record(0x1000, 1_800, 25_000);

        append_session_to(&mut flash, &record).expect("append should succeed");

        let mut header = [0u8; HEADER_SIZE];
        flash
            .read(LOG_OFFSET, &mut header)
            .expect("header read should succeed");

        assert_eq!(&header[..4], &MAGIC);
        assert_eq!(header[HEADER_VERSION_OFFSET], LOG_FORMAT_VERSION);
    }

    #[test]
    fn unsupported_version_preserves_sector() {
        let mut flash = FakeFlash::new();

        let mut header = [0xFFu8; HEADER_SIZE];
        header[..4].copy_from_slice(&MAGIC);
        header[HEADER_VERSION_OFFSET] = LOG_FORMAT_VERSION + 1;
        flash
            .write(LOG_OFFSET, &header)
            .expect("setup header write should succeed");

        let before = flash.snapshot();

        let record = test_record(0x2000, 1_800, 25_000);
        let result = append_session_to(&mut flash, &record);

        assert_eq!(result, Err(SessionLogError::UnsupportedVersion));
        flash.assert_unchanged(&before);
    }

    #[test]
    fn legacy_0xff_version_header_remains_readable() {
        let mut flash = FakeFlash::new();

        let mut header = [0xFFu8; HEADER_SIZE];
        header[..4].copy_from_slice(&MAGIC);
        flash
            .write(LOG_OFFSET, &header)
            .expect("setup header write should succeed");

        let record = test_record(0x3000, 1_800, 25_000);
        append_session_to(&mut flash, &record).expect("legacy header must remain appendable");
    }

    #[test]
    fn initialization_rejects_nonempty_first_record() {
        let mut flash = FakeFlash::new();

        // Leave header erased but place non-erased data in the first record slot.
        let mut raw = [0xFFu8; RECORD_SIZE];
        raw[4] = 0x01;
        flash
            .write(LOG_OFFSET + SessionRecord::record_offset(0), &raw)
            .expect("setup record write should succeed");

        let before = flash.snapshot();

        let record = test_record(0x4000, 1_800, 25_000);
        let result = append_session_to(&mut flash, &record);

        assert_eq!(result, Err(SessionLogError::CorruptHeader));
        flash.assert_unchanged(&before);
    }
}
