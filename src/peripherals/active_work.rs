//! CRC-protected A/B recovery records for an in-progress work session.
//!
//! A replacement is written to an erased peer record and read back before it
//! supersedes the old record. The allocated sector is only erased after the
//! completed work-log record is durable.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

use crate::peripherals::flash_layout::ACTIVE_WORK;

const OFFSET: u32 = ACTIVE_WORK.start;
const SECTOR_SIZE: u32 = ACTIVE_WORK.len;
const MAGIC: [u8; 4] = *b"WACT";
const VERSION: u8 = 3;
const RECORD_LEN: usize = 48;
const SLOT_COUNT: usize = 2;

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

fn slot_offset(slot: usize) -> u32 {
    OFFSET + (slot * RECORD_LEN) as u32
}

fn encode(session: ActiveWorkSession) -> [u8; RECORD_LEN] {
    let mut raw = [0xFF; RECORD_LEN];
    raw[0..4].copy_from_slice(&MAGIC);
    raw[4] = VERSION;
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

fn decode(raw: &[u8; RECORD_LEN]) -> Option<ActiveWorkSession> {
    if raw[..4] != MAGIC || raw[4] != VERSION {
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

fn read_slot<F: ActiveWorkFlash>(
    flash: &mut F,
    slot: usize,
) -> Result<[u8; RECORD_LEN], ActiveWorkError> {
    let mut raw = [0u8; RECORD_LEN];
    flash
        .read(slot_offset(slot), &mut raw)
        .map_err(|_| ActiveWorkError::Io)?;
    Ok(raw)
}

/// Select the highest valid sequence. An interrupted newer write is ignored.
pub fn load_active_session_from<F: ActiveWorkFlash>(
    flash: &mut F,
) -> Result<Option<ActiveWorkSession>, ActiveWorkError> {
    let mut selected = None;
    for slot in 0..SLOT_COUNT {
        if let Some(candidate) = decode(&read_slot(flash, slot)?) {
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
    for slot in 0..SLOT_COUNT {
        let raw = read_slot(flash, slot)?;
        if let Some(found) = decode(&raw) {
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
    let raw = encode(session);
    flash
        .write(slot_offset(slot), &raw[..40])
        .map_err(|_| ActiveWorkError::Io)?;
    flash
        .write(slot_offset(slot) + 40, &raw[40..44])
        .map_err(|_| ActiveWorkError::Io)?;
    if decode(&read_slot(flash, slot)?) == Some(session) {
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
