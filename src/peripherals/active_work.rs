//! CRC-protected A/B recovery records for an in-progress work session.
//!
//! A replacement is written to an erased peer record and read back before it
//! supersedes the old record. The allocated sector is only erased after the
//! completed work-log record is durable.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

use crate::peripherals::flash_layout::{ACTIVE_WORK_A, ACTIVE_WORK_B};

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

const V4_MAGIC_OFFSET: usize = 0;
const V4_MAGIC_LEN: usize = 4;
const V4_VERSION_OFFSET: usize = V4_MAGIC_OFFSET + V4_MAGIC_LEN;
const V4_STATE_OFFSET: usize = V4_VERSION_OFFSET + 1;
const V4_PREFIX_RESERVED_OFFSET: usize = V4_STATE_OFFSET + 1;
const V4_PREFIX_RESERVED_LEN: usize = 2;
const V4_SEQUENCE_OFFSET: usize = V4_PREFIX_RESERVED_OFFSET + V4_PREFIX_RESERVED_LEN;
const V4_SEQUENCE_LEN: usize = 8;
const V4_SESSION_ID_OFFSET: usize = V4_SEQUENCE_OFFSET + V4_SEQUENCE_LEN;
const V4_SESSION_ID_LEN: usize = 8;
const V4_START_UNIX_OFFSET: usize = V4_SESSION_ID_OFFSET + V4_SESSION_ID_LEN;
const V4_START_UNIX_LEN: usize = 8;
const V4_START_DATE_YEAR_OFFSET: usize = V4_START_UNIX_OFFSET + V4_START_UNIX_LEN;
const V4_START_DATE_YEAR_LEN: usize = 2;
const V4_START_DATE_MONTH_OFFSET: usize = V4_START_DATE_YEAR_OFFSET + V4_START_DATE_YEAR_LEN;
const V4_START_DATE_DAY_OFFSET: usize = V4_START_DATE_MONTH_OFFSET + 1;
const V4_START_TIME_HOUR_OFFSET: usize = V4_START_DATE_DAY_OFFSET + 1;
const V4_START_TIME_MINUTE_OFFSET: usize = V4_START_TIME_HOUR_OFFSET + 1;
const V4_START_TIME_SECOND_OFFSET: usize = V4_START_TIME_MINUTE_OFFSET + 1;
const V4_PAYLOAD_RESERVED_OFFSET: usize = V4_START_TIME_SECOND_OFFSET + 1;
const V4_HOURLY_RATE_OFFSET: usize = V4_PAYLOAD_RESERVED_OFFSET + 1;
const V4_HOURLY_RATE_LEN: usize = 4;
const V4_CRC_OFFSET: usize = V4_HOURLY_RATE_OFFSET + V4_HOURLY_RATE_LEN;
const V4_CRC_LEN: usize = 4;
const V4_COMMIT_MARKER_OFFSET: usize = V4_CRC_OFFSET + V4_CRC_LEN;
const V4_COMMIT_MARKER_LEN: usize = 4;
const V4_TRAILING_RESERVED_OFFSET: usize = V4_COMMIT_MARKER_OFFSET + V4_COMMIT_MARKER_LEN;
const V4_TRAILING_RESERVED_LEN: usize = V4_RECORD_LEN - V4_TRAILING_RESERVED_OFFSET;
const V4_IMMUTABLE_PAYLOAD_LEN: usize =
    V4_VERSION_OFFSET + 1 + V4_CRC_OFFSET - V4_PREFIX_RESERVED_OFFSET;

fn v4_payload_crc(raw: &[u8; V4_RECORD_LEN]) -> u32 {
    let mut payload = [0u8; V4_IMMUTABLE_PAYLOAD_LEN];
    let header_len = V4_VERSION_OFFSET + 1;
    payload[..header_len].copy_from_slice(&raw[V4_MAGIC_OFFSET..header_len]);
    payload[header_len..].copy_from_slice(&raw[V4_PREFIX_RESERVED_OFFSET..V4_CRC_OFFSET]);
    crc32(&payload)
}

/// Encode a V4 session into a 64-byte record.
/// All reserved and state bytes are initialized to 0xFF.
/// CRC is computed over immutable payload bytes only (excludes state, CRC, commit marker, trailing reserved).
pub fn v4_encode(session: V4ActiveWorkSession) -> [u8; V4_RECORD_LEN] {
    let mut raw = [0xFF; V4_RECORD_LEN];

    raw[V4_MAGIC_OFFSET..V4_MAGIC_OFFSET + V4_MAGIC_LEN].copy_from_slice(&V4_MAGIC);
    raw[V4_VERSION_OFFSET] = V4_VERSION;
    raw[V4_STATE_OFFSET] = STATE_EMPTY;
    raw[V4_SEQUENCE_OFFSET..V4_SEQUENCE_OFFSET + V4_SEQUENCE_LEN]
        .copy_from_slice(&session.sequence.to_le_bytes());
    raw[V4_SESSION_ID_OFFSET..V4_SESSION_ID_OFFSET + V4_SESSION_ID_LEN]
        .copy_from_slice(&session.session_id.to_le_bytes());
    raw[V4_START_UNIX_OFFSET..V4_START_UNIX_OFFSET + V4_START_UNIX_LEN]
        .copy_from_slice(&session.start_unix.to_le_bytes());
    raw[V4_START_DATE_YEAR_OFFSET..V4_START_DATE_YEAR_OFFSET + V4_START_DATE_YEAR_LEN]
        .copy_from_slice(&session.start_date_ymd.0.to_le_bytes());
    raw[V4_START_DATE_MONTH_OFFSET] = session.start_date_ymd.1;
    raw[V4_START_DATE_DAY_OFFSET] = session.start_date_ymd.2;
    raw[V4_START_TIME_HOUR_OFFSET] = session.start_time_hms.0;
    raw[V4_START_TIME_MINUTE_OFFSET] = session.start_time_hms.1;
    raw[V4_START_TIME_SECOND_OFFSET] = session.start_time_hms.2;
    raw[V4_HOURLY_RATE_OFFSET..V4_HOURLY_RATE_OFFSET + V4_HOURLY_RATE_LEN]
        .copy_from_slice(&session.hourly_rate_cents.to_le_bytes());
    let checksum = v4_payload_crc(&raw).to_le_bytes();
    raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN].copy_from_slice(&checksum);

    raw
}

/// Decode a V4 session from a 64-byte record.
/// Returns None if the record is not a valid committed V4 record.
pub fn v4_decode(raw: &[u8; V4_RECORD_LEN]) -> Option<V4ActiveWorkSession> {
    if raw[V4_MAGIC_OFFSET..V4_MAGIC_OFFSET + V4_MAGIC_LEN] != V4_MAGIC
        || raw[V4_VERSION_OFFSET] != V4_VERSION
    {
        return None;
    }
    if raw[V4_STATE_OFFSET] != STATE_COMMITTED {
        return None;
    }
    if raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + V4_COMMIT_MARKER_LEN]
        != V4_COMMIT_MARKER
    {
        return None;
    }
    if u32::from_le_bytes(
        raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN]
            .try_into()
            .ok()?,
    ) != v4_payload_crc(raw)
    {
        return None;
    }

    Some(V4ActiveWorkSession {
        sequence: u64::from_le_bytes(
            raw[V4_SEQUENCE_OFFSET..V4_SEQUENCE_OFFSET + V4_SEQUENCE_LEN]
                .try_into()
                .ok()?,
        ),
        session_id: u64::from_le_bytes(
            raw[V4_SESSION_ID_OFFSET..V4_SESSION_ID_OFFSET + V4_SESSION_ID_LEN]
                .try_into()
                .ok()?,
        ),
        start_unix: i64::from_le_bytes(
            raw[V4_START_UNIX_OFFSET..V4_START_UNIX_OFFSET + V4_START_UNIX_LEN]
                .try_into()
                .ok()?,
        ),
        start_date_ymd: (
            u16::from_le_bytes(
                raw[V4_START_DATE_YEAR_OFFSET..V4_START_DATE_YEAR_OFFSET + V4_START_DATE_YEAR_LEN]
                    .try_into()
                    .ok()?,
            ),
            raw[V4_START_DATE_MONTH_OFFSET],
            raw[V4_START_DATE_DAY_OFFSET],
        ),
        start_time_hms: (
            raw[V4_START_TIME_HOUR_OFFSET],
            raw[V4_START_TIME_MINUTE_OFFSET],
            raw[V4_START_TIME_SECOND_OFFSET],
        ),
        hourly_rate_cents: u32::from_le_bytes(
            raw[V4_HOURLY_RATE_OFFSET..V4_HOURLY_RATE_OFFSET + V4_HOURLY_RATE_LEN]
                .try_into()
                .ok()?,
        ),
    })
}

/// Classify a V4 record without fully decoding it.
/// Used for boot recovery and slot selection.
pub fn v4_classify(raw: &[u8; V4_RECORD_LEN]) -> V4RecordClass {
    // All 0xFF => Empty
    if raw.iter().all(|&b| b == 0xFF) {
        return V4RecordClass::Empty;
    }

    if raw[V4_MAGIC_OFFSET..V4_MAGIC_OFFSET + V4_MAGIC_LEN] != V4_MAGIC {
        return V4RecordClass::Corrupt;
    }

    let version = raw[V4_VERSION_OFFSET];
    if version != V4_VERSION {
        // Versions 1-3 are legacy
        if version >= 1 && version <= 3 {
            return V4RecordClass::Legacy(version);
        }
        return V4RecordClass::Corrupt;
    }

    match raw[V4_STATE_OFFSET] {
        STATE_EMPTY => V4RecordClass::Empty,
        STATE_WRITING => V4RecordClass::Writing,
        STATE_COMMITTED => {
            if raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + V4_COMMIT_MARKER_LEN]
                != V4_COMMIT_MARKER
            {
                return V4RecordClass::Corrupt;
            }
            let Ok(stored_crc) = raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN].try_into() else {
                return V4RecordClass::Corrupt;
            };
            if u32::from_le_bytes(stored_crc) == v4_payload_crc(raw) {
                V4RecordClass::Committed
            } else {
                V4RecordClass::Corrupt
            }
        }
        STATE_RETIRED => {
            let Ok(stored_crc) = raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN].try_into() else {
                return V4RecordClass::Corrupt;
            };
            if u32::from_le_bytes(stored_crc) == v4_payload_crc(raw) {
                V4RecordClass::Retired
            } else {
                V4RecordClass::Corrupt
            }
        }
        _ => V4RecordClass::Corrupt,
    }
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
    raw[V4_COMMIT_MARKER_OFFSET..V4_COMMIT_MARKER_OFFSET + V4_COMMIT_MARKER_LEN]
        .copy_from_slice(&V4_COMMIT_MARKER);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum V4Slot {
    SlotA,
    SlotB,
}

impl V4Slot {
    const ALL: [Self; 2] = [Self::SlotA, Self::SlotB];

    fn peer(self) -> Self {
        match self {
            Self::SlotA => Self::SlotB,
            Self::SlotB => Self::SlotA,
        }
    }

    fn offset(self) -> u32 {
        match self {
            Self::SlotA => ACTIVE_WORK_A.start,
            Self::SlotB => ACTIVE_WORK_B.start,
        }
    }

    fn len(self) -> u32 {
        match self {
            Self::SlotA => ACTIVE_WORK_A.len,
            Self::SlotB => ACTIVE_WORK_B.len,
        }
    }
}

fn read_v4_slot<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
) -> Result<[u8; V4_RECORD_LEN], ActiveWorkError> {
    let mut raw = [0; V4_RECORD_LEN];
    flash
        .read(slot.offset(), &mut raw)
        .map_err(|_| ActiveWorkError::Io)?;
    Ok(raw)
}

fn classify_v4_slot<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
) -> Result<V4RecordClass, ActiveWorkError> {
    Ok(v4_classify(&read_v4_slot(flash, slot)?))
}

fn erase_v4_slot<F: ActiveWorkFlash>(flash: &mut F, slot: V4Slot) -> Result<(), ActiveWorkError> {
    flash
        .erase(slot.offset(), slot.offset() + slot.len())
        .map_err(|_| ActiveWorkError::Io)
}

fn v4_session(sequence: u64, session: ActiveWorkSession) -> V4ActiveWorkSession {
    V4ActiveWorkSession {
        sequence,
        session_id: session.session_id,
        start_unix: session.start_unix,
        start_date_ymd: session.start_date_ymd,
        start_time_hms: session.start_time_hms,
        hourly_rate_cents: session.hourly_rate_cents,
    }
}

fn write_v4_payload<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
    session: V4ActiveWorkSession,
) -> Result<(), ActiveWorkError> {
    let raw = v4_encode(session);
    flash
        .write(
            slot.offset() + V4_MAGIC_OFFSET as u32,
            &raw[V4_MAGIC_OFFSET..V4_STATE_OFFSET],
        )
        .map_err(|_| ActiveWorkError::Io)?;
    flash
        .write(
            slot.offset() + V4_PREFIX_RESERVED_OFFSET as u32,
            &raw[V4_PREFIX_RESERVED_OFFSET..V4_CRC_OFFSET],
        )
        .map_err(|_| ActiveWorkError::Io)
}

fn write_v4_crc<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
    session: V4ActiveWorkSession,
) -> Result<(), ActiveWorkError> {
    let raw = v4_encode(session);
    flash
        .write(
            slot.offset() + V4_CRC_OFFSET as u32,
            &raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN],
        )
        .map_err(|_| ActiveWorkError::Io)
}

fn write_v4_commit_marker<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
) -> Result<(), ActiveWorkError> {
    flash
        .write(
            slot.offset() + V4_COMMIT_MARKER_OFFSET as u32,
            &V4_COMMIT_MARKER,
        )
        .map_err(|_| ActiveWorkError::Io)
}

fn write_v4_committed_state<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
) -> Result<(), ActiveWorkError> {
    flash
        .write(slot.offset() + V4_STATE_OFFSET as u32, &[STATE_COMMITTED])
        .map_err(|_| ActiveWorkError::Io)
}

fn retire_v4_slot<F: ActiveWorkFlash>(flash: &mut F, slot: V4Slot) -> Result<(), ActiveWorkError> {
    flash
        .write(slot.offset() + V4_STATE_OFFSET as u32, &[STATE_RETIRED])
        .map_err(|_| ActiveWorkError::Io)
}

fn verify_v4_payload<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
    session: V4ActiveWorkSession,
) -> Result<(), ActiveWorkError> {
    let actual = read_v4_slot(flash, slot)?;
    let expected = v4_encode(session);
    if actual[V4_STATE_OFFSET] == STATE_WRITING
        && actual[V4_MAGIC_OFFSET..V4_STATE_OFFSET] == expected[V4_MAGIC_OFFSET..V4_STATE_OFFSET]
        && actual[V4_PREFIX_RESERVED_OFFSET..V4_CRC_OFFSET]
            == expected[V4_PREFIX_RESERVED_OFFSET..V4_CRC_OFFSET]
    {
        Ok(())
    } else {
        Err(ActiveWorkError::VerifyFailed)
    }
}

fn committed_v4_session<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: V4Slot,
) -> Result<Option<V4ActiveWorkSession>, ActiveWorkError> {
    Ok(v4_decode(&read_v4_slot(flash, slot)?))
}

fn save_active_session_v4_to<F: ActiveWorkFlash>(
    flash: &mut F,
    session: ActiveWorkSession,
) -> Result<(), ActiveWorkError> {
    let classes = [
        classify_v4_slot(flash, V4Slot::SlotA)?,
        classify_v4_slot(flash, V4Slot::SlotB)?,
    ];
    let committed = [
        committed_v4_session(flash, V4Slot::SlotA)?,
        committed_v4_session(flash, V4Slot::SlotB)?,
    ];
    let highest = committed
        .iter()
        .flatten()
        .max_by_key(|record| record.sequence)
        .copied();
    if let Some(highest_record) = highest {
        if committed
            .iter()
            .flatten()
            .filter(|record| record.sequence == highest_record.sequence)
            .count()
            > 1
        {
            return Err(ActiveWorkError::Corrupt);
        }
    }
    if highest.is_some_and(|record| session.session_id <= record.session_id) {
        return Err(ActiveWorkError::Corrupt);
    }
    let sequence = highest.map_or(1, |record| record.sequence + 1);
    let target_index = if committed[0].is_some() && committed[1].is_some() {
        if committed[0].unwrap().sequence < committed[1].unwrap().sequence {
            0
        } else {
            1
        }
    } else if matches!(classes[0], V4RecordClass::Empty | V4RecordClass::Retired) {
        0
    } else if matches!(classes[1], V4RecordClass::Empty | V4RecordClass::Retired) {
        1
    } else if committed[0].is_some() {
        1
    } else if committed[1].is_some() {
        0
    } else {
        0
    };
    let target = V4Slot::ALL[target_index];
    let old = target.peer();
    let old_committed = committed[1 - target_index];

    if !matches!(classes[target_index], V4RecordClass::Empty) {
        if committed[target_index].is_some() || old_committed.is_some() {
            let valid = old_committed
                .or(committed[target_index])
                .ok_or(ActiveWorkError::VerifyFailed)?;
            if committed_v4_session(flash, if old_committed.is_some() { old } else { target })?
                != Some(valid)
            {
                return Err(ActiveWorkError::VerifyFailed);
            }
        }
        erase_v4_slot(flash, target)?;
    }

    let record = v4_session(sequence, session);
    flash
        .write(target.offset() + V4_STATE_OFFSET as u32, &[STATE_WRITING])
        .map_err(|_| ActiveWorkError::Io)?;
    write_v4_payload(flash, target, record)?;
    verify_v4_payload(flash, target, record)?;
    write_v4_crc(flash, target, record)?;
    write_v4_commit_marker(flash, target)?;
    write_v4_committed_state(flash, target)?;
    if committed_v4_session(flash, target)? != Some(record) {
        return Err(ActiveWorkError::VerifyFailed);
    }
    if old_committed.is_some() {
        retire_v4_slot(flash, old)?;
    }
    match classify_v4_slot(flash, target)? {
        V4RecordClass::Committed => match classify_v4_slot(flash, old)? {
            V4RecordClass::Retired | V4RecordClass::Empty => Ok(()),
            _ if old_committed.is_none() => Ok(()),
            _ => Err(ActiveWorkError::VerifyFailed),
        },
        _ => Err(ActiveWorkError::VerifyFailed),
    }
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
        28 + if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
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
    use std::vec;
    use std::vec::Vec;

    #[derive(Clone, Copy)]
    enum Fault {
        Read(usize),
        Write(usize),
        Erase(usize),
    }

    struct FakeFlash {
        bytes: Vec<u8>,
        reads: usize,
        writes: usize,
        erases: usize,
        fault: Option<Fault>,
    }

    impl FakeFlash {
        fn new() -> Self {
            Self {
                bytes: vec![0xFF; (ACTIVE_WORK_B.start + ACTIVE_WORK_B.len) as usize],
                reads: 0,
                writes: 0,
                erases: 0,
                fault: None,
            }
        }

        fn fail_on(mut self, fault: Fault) -> Self {
            self.fault = Some(fault);
            self
        }

        fn store_committed(&mut self, slot: V4Slot, sequence: u64, session_id: u64) {
            let raw = {
                let mut raw = v4_encode(V4ActiveWorkSession {
                    sequence,
                    session_id,
                    start_unix: 1_700_000_000,
                    start_date_ymd: (2024, 1, 15),
                    start_time_hms: (10, 30, 0),
                    hourly_rate_cents: 2500,
                });
                v4_set_state(&mut raw, STATE_COMMITTED);
                v4_set_commit_marker(&mut raw);
                raw
            };
            let offset = slot.offset() as usize;
            self.bytes[offset..offset + V4_RECORD_LEN].copy_from_slice(&raw);
        }

        fn recover(&mut self) -> Option<V4ActiveWorkSession> {
            V4Slot::ALL
                .iter()
                .filter_map(|&slot| committed_v4_session(self, slot).ok().flatten())
                .max_by_key(|record| record.sequence)
        }
    }

    impl ActiveWorkFlash for FakeFlash {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ()> {
            self.reads += 1;
            if matches!(self.fault, Some(Fault::Read(n)) if n == self.reads) {
                return Err(());
            }
            bytes.copy_from_slice(&self.bytes[offset as usize..offset as usize + bytes.len()]);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ()> {
            self.writes += 1;
            if matches!(self.fault, Some(Fault::Write(n)) if n == self.writes) {
                return Err(());
            }
            for (stored, &next) in self.bytes[offset as usize..offset as usize + bytes.len()]
                .iter_mut()
                .zip(bytes)
            {
                if (*stored & next) != next {
                    return Err(());
                }
                *stored = next;
            }
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
            self.erases += 1;
            if matches!(self.fault, Some(Fault::Erase(n)) if n == self.erases) {
                return Err(());
            }
            self.bytes[from as usize..to as usize].fill(0xFF);
            Ok(())
        }
    }

    fn save_session(session_id: u64) -> ActiveWorkSession {
        ActiveWorkSession {
            session_id,
            start_unix: 1_700_000_100,
            start_date_ymd: (2024, 1, 16),
            start_time_hms: (11, 0, 0),
            hourly_rate_cents: 3000,
        }
    }

    fn flash_with_old_record(fault: Fault) -> FakeFlash {
        let mut flash = FakeFlash::new().fail_on(fault);
        flash.store_committed(V4Slot::SlotA, 1, 10);
        flash
    }

    fn assert_old_recoverable(flash: &mut FakeFlash) {
        assert_eq!(flash.recover().map(|record| record.session_id), Some(10));
    }

    fn sample_v4_session() -> V4ActiveWorkSession {
        V4ActiveWorkSession {
            sequence: 42,
            session_id: 100,
            start_unix: 1_700_000_000,
            start_date_ymd: (2024, 6, 15),
            start_time_hms: (14, 30, 45),
            hourly_rate_cents: 3000,
        }
    }

    fn valid_v4_record(state: u8) -> [u8; V4_RECORD_LEN] {
        let mut raw = v4_encode(sample_v4_session());
        mutate_state(&mut raw, state);
        v4_set_commit_marker(&mut raw);
        raw
    }

    fn mutate_state(raw: &mut [u8; V4_RECORD_LEN], state: u8) {
        raw[V4_STATE_OFFSET] = state;
    }

    fn mutate_crc(raw: &mut [u8; V4_RECORD_LEN]) {
        raw[V4_CRC_OFFSET] ^= 1;
    }

    fn mutate_magic(raw: &mut [u8; V4_RECORD_LEN]) {
        raw[V4_MAGIC_OFFSET] ^= 1;
    }

    fn mutate_version(raw: &mut [u8; V4_RECORD_LEN]) {
        raw[V4_VERSION_OFFSET] ^= 1;
    }

    fn mutate_commit_marker(raw: &mut [u8; V4_RECORD_LEN]) {
        raw[V4_COMMIT_MARKER_OFFSET] ^= 1;
    }

    #[test]
    fn v4_record_classes_are_explicit() {
        assert_eq!(v4_classify(&[0xFF; V4_RECORD_LEN]), V4RecordClass::Empty);

        let mut writing = v4_encode(sample_v4_session());
        mutate_state(&mut writing, STATE_WRITING);
        assert_eq!(v4_classify(&writing), V4RecordClass::Writing);

        assert_eq!(
            v4_classify(&valid_v4_record(STATE_COMMITTED)),
            V4RecordClass::Committed
        );
        assert_eq!(
            v4_classify(&valid_v4_record(STATE_RETIRED)),
            V4RecordClass::Retired
        );
    }

    #[test]
    fn v4_invalid_fields_classify_corrupt_and_old_versions_are_legacy() {
        let mut raw = valid_v4_record(STATE_COMMITTED);
        mutate_crc(&mut raw);
        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);

        let mut raw = valid_v4_record(STATE_COMMITTED);
        mutate_magic(&mut raw);
        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);

        let mut raw = valid_v4_record(STATE_COMMITTED);
        mutate_version(&mut raw);
        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);

        let mut raw = valid_v4_record(STATE_COMMITTED);
        mutate_commit_marker(&mut raw);
        assert_eq!(v4_classify(&raw), V4RecordClass::Corrupt);

        let mut legacy = [0xFF; V4_RECORD_LEN];
        legacy[V4_MAGIC_OFFSET..V4_MAGIC_OFFSET + V4_MAGIC_LEN].copy_from_slice(&V4_MAGIC);
        legacy[V4_VERSION_OFFSET] = 3;
        assert_eq!(v4_classify(&legacy), V4RecordClass::Legacy(3));
    }

    #[test]
    fn v4_crc_is_unchanged_by_state_or_commit_marker() {
        let mut raw = v4_encode(sample_v4_session());
        let encoded_crc = v4_payload_crc(&raw);
        assert_eq!(
            encoded_crc,
            u32::from_le_bytes(
                raw[V4_CRC_OFFSET..V4_CRC_OFFSET + V4_CRC_LEN]
                    .try_into()
                    .unwrap()
            )
        );

        mutate_state(&mut raw, STATE_WRITING);
        assert_eq!(v4_payload_crc(&raw), encoded_crc);
        mutate_state(&mut raw, STATE_COMMITTED);
        assert_eq!(v4_payload_crc(&raw), encoded_crc);
        mutate_state(&mut raw, STATE_RETIRED);
        assert_eq!(v4_payload_crc(&raw), encoded_crc);

        mutate_commit_marker(&mut raw);
        assert_eq!(v4_payload_crc(&raw), encoded_crc);
    }

    #[test]
    fn v4_corrupting_any_crc_covered_byte_invalidates_record() {
        for index in V4_MAGIC_OFFSET..V4_STATE_OFFSET {
            let mut raw = valid_v4_record(STATE_COMMITTED);
            raw[index] ^= 1;
            assert_eq!(
                v4_classify(&raw),
                V4RecordClass::Corrupt,
                "payload byte {index}"
            );
        }
        for index in V4_PREFIX_RESERVED_OFFSET..V4_CRC_OFFSET {
            let mut raw = valid_v4_record(STATE_COMMITTED);
            raw[index] ^= 1;
            assert_eq!(
                v4_classify(&raw),
                V4RecordClass::Corrupt,
                "payload byte {index}"
            );
        }
    }

    #[test]
    fn v4_encode_decode_roundtrip_preserves_all_payload_fields() {
        let original = sample_v4_session();
        let decoded = v4_decode(&valid_v4_record(STATE_COMMITTED)).expect("should decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn v4_encode_leaves_reserved_bytes_erased() {
        let raw = v4_encode(sample_v4_session());
        assert!(
            raw[V4_PREFIX_RESERVED_OFFSET..V4_PREFIX_RESERVED_OFFSET + V4_PREFIX_RESERVED_LEN]
                .iter()
                .all(|&byte| byte == 0xFF)
        );
        assert_eq!(raw[V4_PAYLOAD_RESERVED_OFFSET], 0xFF);
        assert!(raw
            [V4_TRAILING_RESERVED_OFFSET..V4_TRAILING_RESERVED_OFFSET + V4_TRAILING_RESERVED_LEN]
            .iter()
            .all(|&byte| byte == 0xFF));
    }

    #[test]
    fn power_loss_after_target_erase() {
        let mut flash = FakeFlash::new().fail_on(Fault::Write(1));
        flash.store_committed(V4Slot::SlotA, 1, 10);
        flash.store_committed(V4Slot::SlotB, 0, 1);
        assert!(matches!(
            save_active_session_v4_to(&mut flash, save_session(11)),
            Err(ActiveWorkError::Io | ActiveWorkError::VerifyFailed)
        ));
        assert_old_recoverable(&mut flash);
    }

    #[test]
    fn power_loss_after_writing_state() {
        let mut flash = flash_with_old_record(Fault::Write(2));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_old_recoverable(&mut flash);
    }

    #[test]
    fn power_loss_after_payload_write() {
        let mut flash = flash_with_old_record(Fault::Write(4));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_old_recoverable(&mut flash);
        assert!(!matches!(
            classify_v4_slot(&mut flash, V4Slot::SlotB),
            Ok(V4RecordClass::Committed)
        ));
    }

    #[test]
    fn power_loss_after_payload_verify() {
        let mut flash = flash_with_old_record(Fault::Write(4));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_old_recoverable(&mut flash);
    }

    #[test]
    fn power_loss_after_crc_write() {
        let mut flash = flash_with_old_record(Fault::Write(5));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_old_recoverable(&mut flash);
    }

    #[test]
    fn power_loss_after_commit_marker() {
        let mut flash = flash_with_old_record(Fault::Write(6));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_old_recoverable(&mut flash);
        assert!(!matches!(
            classify_v4_slot(&mut flash, V4Slot::SlotB),
            Ok(V4RecordClass::Committed)
        ));
    }

    #[test]
    fn power_loss_after_committed_state() {
        let mut flash = flash_with_old_record(Fault::Read(6));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_eq!(flash.recover().map(|record| record.session_id), Some(11));
    }

    #[test]
    fn power_loss_after_full_verify() {
        let mut flash = flash_with_old_record(Fault::Write(7));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert!(matches!(
            flash.recover().map(|record| record.session_id),
            Some(10 | 11)
        ));
    }

    #[test]
    fn power_loss_after_retire_old() {
        let mut flash = flash_with_old_record(Fault::Write(7));
        assert!(save_active_session_v4_to(&mut flash, save_session(11)).is_err());
        assert_eq!(flash.recover().map(|record| record.session_id), Some(11));
    }

    #[test]
    fn both_committed_older_sector_replaced() {
        let mut flash = FakeFlash::new();
        flash.store_committed(V4Slot::SlotA, 1, 10);
        flash.store_committed(V4Slot::SlotB, 2, 11);
        save_active_session_v4_to(&mut flash, save_session(12)).unwrap();
        assert_eq!(
            committed_v4_session(&mut flash, V4Slot::SlotA)
                .unwrap()
                .unwrap()
                .sequence,
            3
        );
        assert_eq!(
            classify_v4_slot(&mut flash, V4Slot::SlotB).unwrap(),
            V4RecordClass::Retired
        );
    }

    #[test]
    fn equal_committed_sequences_are_not_silently_tied() {
        let mut flash = FakeFlash::new();
        flash.store_committed(V4Slot::SlotA, 1, 10);
        flash.store_committed(V4Slot::SlotB, 1, 11);
        assert!(matches!(
            save_active_session_v4_to(&mut flash, save_session(12)),
            Err(ActiveWorkError::Corrupt | ActiveWorkError::VerifyFailed)
        ));
    }

    #[test]
    fn stale_session_id_rejected() {
        let mut flash = FakeFlash::new();
        flash.store_committed(V4Slot::SlotA, 1, 10);
        assert_eq!(
            save_active_session_v4_to(&mut flash, save_session(9)),
            Err(ActiveWorkError::Corrupt)
        );
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
