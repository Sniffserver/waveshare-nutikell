//! CRC-protected A/B recovery records for an in-progress work session.
//!
//! A replacement is written to an erased peer record and read back before it
//! supersedes the old record. The allocated sector is only erased after the
//! completed work-log record is durable.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

use crate::peripherals::flash_layout::ACTIVE_WORK_A;

// ============================================================================
// V3 (current) single-sector two-slot format
// ============================================================================

const OFFSET: u32 = ACTIVE_WORK_A.start;
const SECTOR_SIZE: u32 = ACTIVE_WORK_A.len;
const V3_MAGIC: [u8; 4] = *b"WACT";
const V3_VERSION: u8 = 3;
const V3_RECORD_LEN: usize = 48;
const V3_SLOT_COUNT: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveWorkSession {
    /// Persisted monotonic work-session sequence, never derived from time.
    pub session_id: u64,
    pub start_unix: i64,
    pub start_date_ymd: (u16, u8, u8),
    pub start_time_hms: (u8, u8, u8),
    pub hourly_rate_cents: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveWorkError {
    Io,
    Corrupt,
    NoErasedSlot,
    VerifyFailed,
}

/// Host-testable minimal flash interface. Callers must constrain ranges to the
/// checked-in flash layout; this module only uses `ACTIVE_WORK`.
pub trait ActiveWorkFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()>;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()>;
    fn erase(&mut self, from: u32, to: u32) -> Result<(), ()>;
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
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

fn v3_slot_offset(slot: usize) -> u32 {
    OFFSET + (slot * V3_RECORD_LEN) as u32
}

fn v3_encode(session: ActiveWorkSession) -> [u8; V3_RECORD_LEN] {
    let mut raw = [0xFF; V3_RECORD_LEN];
    raw[0..4].copy_from_slice(&V3_MAGIC);
    raw[4] = V3_VERSION;
    raw[5..13].copy_from_slice(&session.session_id.to_le_bytes());
    raw[13..21].copy_from_slice(&session.start_unix.to_le_bytes());
    raw[21..23].copy_from_slice(&session.start_date_ymd.0.to_le_bytes());
    raw[23] = session.start_date_ymd.1;
    raw[24] = session.start_date_ymd.2;
    raw[25] = session.start_time_hms.0;
    raw[26] = session.start_time_hms.1;
    raw[27] = session.start_time_hms.2;
    raw[28..32].copy_from_slice(&session.hourly_rate_cents.to_le_bytes());
    let checksum = crc32(&raw[..40]);
    raw[40..44].copy_from_slice(&checksum.to_le_bytes()); // commit written last
    raw
}

fn v3_decode(raw: &[u8; V3_RECORD_LEN]) -> Option<ActiveWorkSession> {
    if raw[..4] != V3_MAGIC || raw[4] != V3_VERSION {
        return None;
    }
    if u32::from_le_bytes(raw[40..44].try_into().ok()?) != crc32(&raw[..40]) {
        return None;
    }
    Some(ActiveWorkSession {
        session_id: u64::from_le_bytes(raw[5..13].try_into().ok()?),
        start_unix: i64::from_le_bytes(raw[13..21].try_into().ok()?),
        start_date_ymd: (
            u16::from_le_bytes(raw[21..23].try_into().ok()?),
            raw[23],
            raw[24],
        ),
        start_time_hms: (raw[25], raw[26], raw[27]),
        hourly_rate_cents: u32::from_le_bytes(raw[28..32].try_into().ok()?),
    })
}

fn v3_read_slot<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: usize,
) -> Result<[u8; V3_RECORD_LEN], ActiveWorkError> {
    let mut raw = [0u8; V3_RECORD_LEN];
    flash
        .read(v3_slot_offset(slot), &mut raw)
        .map_err(|_| ActiveWorkError::Io)?;
    Ok(raw)
}

/// Select the highest valid sequence. An interrupted newer write is ignored.
pub fn load_active_session_from<F: ActiveWorkFlash>(
    flash: &mut F,
) -> Result<Option<ActiveWorkSession>, ActiveWorkError> {
    let mut selected = None;
    for slot in 0..V3_SLOT_COUNT {
        if let Some(candidate) = v3_decode(&v3_read_slot(flash, slot)?) {
            if selected.map_or(true, |current: ActiveWorkSession| {
                candidate.session_id > current.session_id
            }) {
                selected = Some(candidate);
            }
        }
    }
    Ok(selected)
}

/// Commit a session into an erased peer slot. This function never erases a
/// sector containing a recovery record; callers get `NoErasedSlot` instead.
pub fn save_active_session_to<F: ActiveWorkFlash>(
    flash: &mut F,
    session: ActiveWorkSession,
) -> Result<(), ActiveWorkError> {
    let mut highest = None;
    let mut target = None;
    for slot in 0..V3_SLOT_COUNT {
        let raw = v3_read_slot(flash, slot)?;
        if let Some(found) = v3_decode(&raw) {
            if found == session {
                return Ok(());
            }
            if highest.map_or(true, |current: ActiveWorkSession| {
                found.session_id > current.session_id
            }) {
                highest = Some(found);
            }
        } else if raw.iter().all(|&byte| byte == 0xFF) {
            target = Some(slot);
        }
    }
    if highest.map_or(false, |current: ActiveWorkSession| {
        session.session_id <= current.session_id
    }) {
        return Err(ActiveWorkError::Corrupt);
    }
    let slot = target.ok_or(ActiveWorkError::NoErasedSlot)?;
    let raw = v3_encode(session);
    flash
        .write(v3_slot_offset(slot), &raw[..40])
        .map_err(|_| ActiveWorkError::Io)?;
    flash
        .write(v3_slot_offset(slot) + 40, &raw[40..44])
        .map_err(|_| ActiveWorkError::Io)?;
    if v3_decode(&v3_read_slot(flash, slot)?) == Some(session) {
        Ok(())
    } else {
        Err(ActiveWorkError::VerifyFailed)
    }
}

/// Only call after an equivalent completed session record has verified.
pub fn clear_active_session_from<F: ActiveWorkFlash>(flash: &mut F) -> Result<(), ActiveWorkError> {
    flash
        .erase(OFFSET, OFFSET + SECTOR_SIZE)
        .map_err(|_| ActiveWorkError::Io)
}

#[cfg(target_arch = "xtensa")]
impl ActiveWorkFlash for FlashStorage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
        ReadNorFlash::read(self, offset, bytes).map_err(|_| ())
    }
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
        NorFlash::write(self, offset, bytes).map_err(|_| ())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
        NorFlash::erase(self, from, to).map_err(|_| ())
    }
}
#[cfg(target_arch = "xtensa")]
pub fn load_active_session() -> Option<ActiveWorkSession> {
    load_active_session_from(&mut FlashStorage::new())
        .ok()
        .flatten()
}
#[cfg(target_arch = "xtensa")]
pub fn save_active_session(session: ActiveWorkSession) -> Result<(), ActiveWorkError> {
    save_active_session_to(&mut FlashStorage::new(), session)
}
#[cfg(target_arch = "xtensa")]
pub fn clear_active_session() -> Result<(), ActiveWorkError> {
    clear_active_session_from(&mut FlashStorage::new())
}

// ============================================================================
// V4 A/B format — new record layout for independent sectors
// ============================================================================

use crate::peripherals::flash_layout::{ACTIVE_WORK_A, ACTIVE_WORK_B};

/// V4 record length: 64 bytes
pub const V4_RECORD_LEN: usize = 64;

/// V4 magic bytes: "WACT"
pub const V4_MAGIC: [u8; 4] = *b"WACT";

/// V4 format version
pub const V4_VERSION: u8 = 4;

/// V4 commit marker: "COMM"
pub const V4_COMMIT_MARKER: [u8; 4] = *b"COMM";

/// NOR-safe state constants (1-bit transitions only: 1 -> 0)
pub const STATE_EMPTY: u8 = 0xFF;
pub const STATE_WRITING: u8 = 0xFE;
pub const STATE_COMMITTED: u8 = 0xFC;
pub const STATE_RETIRED: u8 = 0xF8;

/// V4 active-work session with sequence for generation tracking
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4ActiveWorkSession {
    /// Monotonically increasing generation counter
    pub sequence: u64,
    /// Persisted monotonic work-session sequence, never derived from time.
    pub session_id: u64,
    pub start_unix: i64,
    pub start_date_ymd: (u16, u8, u8),
    pub start_time_hms: (u8, u8, u8),
    pub hourly_rate_cents: u32,
}

/// V4 record classification result
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V4RecordClass {
    Empty,
    Writing,
    Committed,
    Retired,
    Legacy(u8),
    Corrupt,
}

/// Offset of the slot_state byte in the V4 record
const V4_STATE_OFFSET: usize = 5;

/// Offset of the payload CRC in the V4 record
const V4_CRC_OFFSET: usize = 44;

/// Offset of the commit marker in the V4 record
const V4_COMMIT_MARKER_OFFSET: usize = 48;

/// End of the CRC-covered payload (exclusive)
/// Covers: magic(4) + version(1) + state(1) + reserved(2) + payload(36) = 44 bytes
/// Excludes: state(1), crc(4), commit_marker(4), trailing_reserved(12)
const V4_CRC_COVERED_END: usize = 44;

/// Encode a V4 session into a 64-byte record.
/// All reserved and state bytes are initialized to 0xFF.
/// CRC is computed over immutable payload bytes only (excludes state, CRC, commit marker, trailing reserved).
pub fn v4_encode(session: V4ActiveWorkSession) -> [u8; V4_RECORD_LEN] {
    let mut raw = [0xFF; V4_RECORD_LEN];

    // Magic
    raw[0..4].copy_from_slice(&V4_MAGIC);
    // Version
    raw[4] = V4_VERSION;
    // State: initialized to Empty (0xFF)
    raw[V4_STATE_OFFSET] = STATE_EMPTY;
    // Reserved (2 bytes at offset 6-7): already 0xFF

    // Payload fields
    raw[8..16].copy_from_slice(&session.sequence.to_le_bytes());
    raw[16..24].copy_from_slice(&session.session_id.to_le_bytes());
    raw[24..32].copy_from_slice(&session.start_unix.to_le_bytes());
    raw[32..34].copy_from_slice(&session.start_date_ymd.0.to_le_bytes());
    raw[34] = session.start_date_ymd.1;
    raw[35] = session.start_date_ymd.2;
    raw[36] = session.start_time_hms.0;
    raw[37] = session.start_time_hms.1;
    raw[38] = session.start_time_hms.2;
    raw[39] = 0xFF; // reserved
    raw[40..44].copy_from_slice(&session.hourly_rate_cents.to_le_bytes());

    // CRC: computed over immutable payload (bytes 0..44, EXCLUDING state byte at offset 5)
    // CRC covers: magic(4) + version(1) + reserved(1 at 5) + reserved(2 at 6-7) + payload(36) = 44 bytes
    // We need to compute CRC with state byte set to a fixed value (0xFF) so that
    // changing state later doesn't change the CRC.
    let mut crc_data = [0u8; V4_CRC_COVERED_END];
    crc_data[0..4].copy_from_slice(&V4_MAGIC);
    crc_data[4] = V4_VERSION;
    crc_data[5] = 0xFF; // state is always 0xFF for CRC computation
    crc_data[6..8].copy_from_slice(&[0xFF, 0xFF]); // reserved
    crc_data[8..44].copy_from_slice(&raw[8..44]); // payload

    let checksum = crc32(&crc_data);
    raw[V4_CRC_OFFSET..V4_CRC_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

    // Commit marker: initialized to all 0xFF (not yet committed)
    raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + 4].copy_from_slice(&[0xFF; 4]);

    // Trailing reserved (12 bytes at offset 52-63): already 0xFF

    raw
}

/// Decode a V4 session from a 64-byte record.
/// Returns None if the record is not a valid committed V4 record.
pub fn v4_decode(raw: &[u8; V4_RECORD_LEN]) -> Option<V4ActiveWorkSession> {
    if raw[..4] != V4_MAGIC || raw[4] != V4_VERSION {
        return None;
    }
    if raw[V4_STATE_OFFSET] != STATE_COMMITTED {
        return None;
    }
    if &raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + 4] != V4_COMMIT_MARKER {
        return None;
    }

    // Compute CRC over immutable payload (state = 0xFF for CRC)
    let mut crc_data = [0u8; V4_CRC_COVERED_END];
    crc_data[0..4].copy_from_slice(&V4_MAGIC);
    crc_data[4] = V4_VERSION;
    crc_data[5] = 0xFF;
    crc_data[6..8].copy_from_slice(&[0xFF, 0xFF]);
    crc_data[8..44].copy_from_slice(&raw[8..44]);

    if u32::from_le_bytes(raw[V4_CRC_OFFSET..V4_CRC_OFFSET + 4].try_into().ok()?)
        != crc32(&crc_data)
    {
        return None;
    }

    Some(V4ActiveWorkSession {
        sequence: u64::from_le_bytes(raw[8..16].try_into().ok()?),
        session_id: u64::from_le_bytes(raw[16..24].try_into().ok()?),
        start_unix: i64::from_le_bytes(raw[24..32].try_into().ok()?),
        start_date_ymd: (
            u16::from_le_bytes(raw[32..34].try_into().ok()?),
            raw[34],
            raw[35],
        ),
        start_time_hms: (raw[36], raw[37], raw[38]),
        hourly_rate_cents: u32::from_le_bytes(raw[40..44].try_into().ok()?),
    })
}

/// Classify a V4 record without fully decoding it.
/// Used for boot recovery and slot selection.
pub fn v4_classify(raw: &[u8; V4_RECORD_LEN]) -> V4RecordClass {
    // All 0xFF => Empty
    if raw.iter().all(|&b| b == 0xFF) {
        return V4RecordClass::Empty;
    }

    // Magic check
    if raw[..4] != V4_MAGIC {
        return V4RecordClass::Corrupt;
    }

    // Version check
    let version = raw[4];
    if version != V4_VERSION {
        // Versions 1-3 are legacy
        if version >= 1 && version <= 3 {
            return V4RecordClass::Legacy(version);
        }
        return V4RecordClass::Corrupt;
    }

    let state = raw[V4_STATE_OFFSET];

    match state {
        STATE_WRITING => V4RecordClass::Writing,
        STATE_COMMITTED => {
            // Verify commit marker
            if &raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + 4] != V4_COMMIT_MARKER {
                return V4RecordClass::Corrupt;
            }
            // Verify CRC
            let mut crc_data = [0u8; V4_CRC_COVERED_END];
            crc_data[0..4].copy_from_slice(&V4_MAGIC);
            crc_data[4] = V4_VERSION;
            crc_data[5] = 0xFF;
            crc_data[6..8].copy_from_slice(&[0xFF, 0xFF]);
            crc_data[8..44].copy_from_slice(&raw[8..44]);

            if u32::from_le_bytes(
                raw[V4_CRC_OFFSET..V4_CRC_OFFSET + 4]
                    .try_into()
                    .unwrap_or([0; 4]),
            ) == crc32(&crc_data)
            {
                V4RecordClass::Committed
            } else {
                V4RecordClass::Corrupt
            }
        }
        STATE_RETIRED => {
            // Verify CRC (commit marker not required for Retired)
            let mut crc_data = [0u8; V4_CRC_COVERED_END];
            crc_data[0..4].copy_from_slice(&V4_MAGIC);
            crc_data[4] = V4_VERSION;
            crc_data[5] = 0xFF;
            crc_data[6..8].copy_from_slice(&[0xFF, 0xFF]);
            crc_data[8..44].copy_from_slice(&raw[8..44]);

            if u32::from_le_bytes(
                raw[V4_CRC_OFFSET..V4_CRC_OFFSET + 4]
                    .try_into()
                    .unwrap_or([0; 4]),
            ) == crc32(&crc_data)
            {
                V4RecordClass::Retired
            } else {
                V4RecordClass::Corrupt
            }
        }
        STATE_EMPTY => V4RecordClass::Empty, // Should be caught by all-0xFF check, but handle anyway
        _ => V4RecordClass::Corrupt,
    }
}

/// Compute the payload CRC for a V4 record (for verification during writes).
/// Uses the immutable payload bytes with state = 0xFF.
pub fn v4_compute_payload_crc(raw: &[u8; V4_RECORD_LEN]) -> u32 {
    let mut crc_data = [0u8; V4_CRC_COVERED_END];
    crc_data[0..4].copy_from_slice(&V4_MAGIC);
    crc_data[4] = V4_VERSION;
    crc_data[5] = 0xFF;
    crc_data[6..8].copy_from_slice(&[0xFF, 0xFF]);
    crc_data[8..44].copy_from_slice(&raw[8..44]);
    crc32(&crc_data)
}

/// Update the slot_state byte in a V4 record (NOR-safe: only 1->0 transitions).
pub fn v4_set_state(raw: &mut [u8; V4_RECORD_LEN], state: u8) {
    // Only allow 1->0 transitions
    let current = raw[V4_STATE_OFFSET];
    if (current & state) == state {
        raw[V4_STATE_OFFSET] = state;
    }
}

/// Write the commit marker in a V4 record.
pub fn v4_set_commit_marker(raw: &mut [u8; V4_RECORD_LEN]) {
    raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + 4].copy_from_slice(&V4_COMMIT_MARKER);
}

/// Write the payload CRC in a V4 record.
pub fn v4_set_payload_crc(raw: &mut [u8; V4_RECORD_LEN], crc: u32) {
    raw[V4_CRC_OFFSET..V4_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
}

pub fn calendar_to_unix(ymd: (u16, u8, u8), hms: (u8, u8, u8)) -> Option<i64> {
    let (year, month, day) = ymd;
    let (hour, minute, second) = hms;
    if year < 1970 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut days = 0i64;
    for y in 1970..year {
        days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
    }
    let month_days = [
        31,
        28 + if year % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            1
        } else {
            0
        },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > month_days[(month - 1) as usize] {
        return None;
    }
    for value in month_days.iter().take((month - 1) as usize) {
        days += *value as i64;
    }
    days += (day - 1) as i64;
    Some(days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_empty_record_classifies_empty() {
        let raw = [0xFF; V4_RECORD_LEN];
        assert_eq!(v4_classify(&raw), V4RecordClass::Empty);
    }

    #[test]
    fn v4_partial_payload_writing_state_classifies_writing() {
        let mut raw = [0xFF; V4_RECORD_LEN];
        raw[0..4].copy_from_slice(&V4_MAGIC);
        raw[4] = V4_VERSION;
        raw[V4_STATE_OFFSET] = STATE_WRITING;
        // Payload can be partial/garbage
        raw[8] = 0x01;
        assert_eq!(v4_classify(&raw), V4RecordClass::Writing);
    }

    #[test]
    fn v4_valid_committed_record_classifies_committed() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let mut raw = v4_encode(session);
        // Set state to Committed
        v4_set_state(&mut raw, STATE_COMMITTED);
        // Set commit marker
        v4_set_commit_marker(&mut raw);
        // Set CRC (recompute after state change)
        let crc = v4_compute_payload_crc(&raw);
        v4_set_payload_crc(&mut raw, crc);

        assert_eq!(v4_classify(&raw), V4RecordClass::Committed);
    }

    #[test]
    fn v4_committed_bad_crc_classifies_corrupt() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let mut raw = v4_encode(session);
        v4_set_state(&mut raw, STATE_COMMITTED);
        v4_set_commit_marker(&mut raw);
        let crc = v4_compute_payload_crc(&raw);
        v4_set_payload_crc(&mut raw, crc);

        // Corrupt the CRC
        raw[V4_CRC_OFFSET] ^= 0xFF;

        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);
    }

    #[test]
    fn v4_committed_bad_marker_classifies_corrupt() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let mut raw = v4_encode(session);
        v4_set_state(&mut raw, STATE_COMMITTED);
        // Leave commit marker as 0xFF (invalid)
        let crc = v4_compute_payload_crc(&raw);
        v4_set_payload_crc(&mut raw, crc);

        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);
    }

    #[test]
    fn v4_retired_valid_crc_classifies_retired() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let mut raw = v4_encode(session);
        v4_set_state(&mut raw, STATE_RETIRED);
        let crc = v4_compute_payload_crc(&raw);
        v4_set_payload_crc(&mut raw, crc);

        assert_eq!(v4_classify(&raw), V4RecordClass::Retired);
    }

    #[test]
    fn v4_legacy_v3_classifies_legacy() {
        let mut raw = [0xFF; V4_RECORD_LEN];
        raw[0..4].copy_from_slice(&V4_MAGIC);
        raw[4] = 3; // v3 version
                    // Fill some payload
        raw[8] = 0x01;

        assert_eq!(v4_classify(&raw), V4RecordClass::Legacy(3));
    }

    #[test]
    fn v4_encode_decode_roundtrip_preserves_all_fields() {
        let original = V4ActiveWorkSession {
            sequence: 42,
            session_id: 100,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 6, 15),
            start_time_hms: (14, 30, 45),
            hourly_rate_cents: 3000,
        };

        let mut raw = v4_encode(original);
        v4_set_state(&mut raw, STATE_COMMITTED);
        v4_set_commit_marker(&mut raw);
        let crc = v4_compute_payload_crc(&raw);
        v4_set_payload_crc(&mut raw, crc);

        let decoded = v4_decode(&raw).expect("should decode");

        assert_eq!(decoded.sequence, original.sequence);
        assert_eq!(decoded.session_id, original.session_id);
        assert_eq!(decoded.start_unix, original.start_unix);
        assert_eq!(decoded.start_date_ymd, original.start_date_ymd);
        assert_eq!(decoded.start_time_hms, original.start_time_hms);
        assert_eq!(decoded.hourly_rate_cents, original.hourly_rate_cents);
    }

    #[test]
    fn v4_crc_stable_when_state_changes() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };

        let mut raw = v4_encode(session);
        let crc_initial = v4_compute_payload_crc(&raw);

        // Change state to Writing
        v4_set_state(&mut raw, STATE_WRITING);
        let crc_writing = v4_compute_payload_crc(&raw);

        // Change state to Committed
        v4_set_state(&mut raw, STATE_COMMITTED);
        let crc_committed = v4_compute_payload_crc(&raw);

        // Change state to Retired
        v4_set_state(&mut raw, STATE_RETIRED);
        let crc_retired = v4_compute_payload_crc(&raw);

        // All CRC values must be identical
        assert_eq!(crc_initial, crc_writing);
        assert_eq!(crc_initial, crc_committed);
        assert_eq!(crc_initial, crc_retired);
    }

    #[test]
    fn v4_crc_stable_when_commit_marker_changes() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };

        let mut raw = v4_encode(session);
        let crc_no_marker = v4_compute_payload_crc(&raw);

        // Add commit marker
        v4_set_commit_marker(&mut raw);
        let crc_with_marker = v4_compute_payload_crc(&raw);

        // CRC must not change
        assert_eq!(crc_no_marker, crc_with_marker);
    }

    #[test]
    fn v4_reserved_bytes_remain_0xFF_after_encode() {
        let session = V4ActiveWorkSession {
            sequence: 1,
            session_id: 42,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };

        let raw = v4_encode(session);

        // Reserved at offset 6-7
        assert_eq!(raw[6], 0xFF);
        assert_eq!(raw[7], 0xFF);
        // Reserved at offset 39
        assert_eq!(raw[39], 0xFF);
        // Trailing reserved at offset 52-63
        for i in 52..64 {
            assert_eq!(raw[i], 0xFF, "trailing reserved[{}] != 0xFF", i);
        }
    }

    // Existing V3 tests
    #[test]
    fn v3_encode_decode_roundtrip() {
        let session = ActiveWorkSession {
            session_id: 1,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let raw = v3_encode(session);
        let decoded = v3_decode(&raw).expect("decode should succeed");
        assert_eq!(decoded, session);
    }

    #[test]
    fn v3_corrupt_crc_fails_decode() {
        let session = ActiveWorkSession {
            session_id: 1,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 1, 15),
            start_time_hms: (10, 30, 0),
            hourly_rate_cents: 2500,
        };
        let mut raw = v3_encode(session);
        raw[40] ^= 0xFF; // corrupt CRC
        assert!(v3_decode(&raw).is_none());
    }
}
