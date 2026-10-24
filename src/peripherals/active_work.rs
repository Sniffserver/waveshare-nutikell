//! One-sector, CRC-protected recovery record for an in-progress work session.
//!
//! This is intentionally separate from the completed-session log: it is written
//! once at clock-in and erased only after the completed record is durable.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

const OFFSET: u32 = 0xE000;
const SECTOR_SIZE: u32 = 4096;
const MAGIC: [u8; 4] = *b"WACT";
const VERSION: u8 = 2;
const RECORD_LEN: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveWorkSession {
    pub session_id: u64,
    pub start_unix: i64,
    pub start_date_ymd: (u16, u8, u8),
    pub start_time_hms: (u8, u8, u8),
    pub hourly_rate_cents: u32,
}

pub const fn session_id(start_unix: i64, hourly_rate_cents: u32) -> u64 {
    (start_unix as u64).rotate_left(17) ^ ((hourly_rate_cents as u64) << 32) ^ 0x574F_524B_0000_0002
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
    let checksum = crc32(&raw[0..32]);
    raw[32..36].copy_from_slice(&checksum.to_le_bytes());
    raw
}

fn decode(raw: &[u8; RECORD_LEN]) -> Option<ActiveWorkSession> {
    if raw[0..4] != MAGIC || raw[4] != VERSION {
        return None;
    }
    // v1 had no session ID. Reject it instead of risking a duplicate after an
    // interrupted stop; a user can manually recover it from the old record.
    let stored = u32::from_le_bytes(raw[32..36].try_into().ok()?);
    if stored != crc32(&raw[0..32]) {
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

#[cfg(target_arch = "xtensa")]
pub fn load_active_session() -> Option<ActiveWorkSession> {
    let mut storage = FlashStorage::new();
    let mut raw = [0u8; RECORD_LEN];
    storage.read(OFFSET, &mut raw).ok()?;
    decode(&raw)
}

/// Save before considering a clock-in durable. Payload is written before CRC.
#[cfg(target_arch = "xtensa")]
pub fn save_active_session(session: ActiveWorkSession) -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    let raw = encode(session);
    storage
        .erase(OFFSET, OFFSET + SECTOR_SIZE)
        .map_err(|_| ())?;
    storage.write(OFFSET, &raw[..32]).map_err(|_| ())?;
    storage.write(OFFSET + 32, &raw[32..36]).map_err(|_| ())?;
    if load_active_session() == Some(session) {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(target_arch = "xtensa")]
pub fn clear_active_session() -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    storage.erase(OFFSET, OFFSET + SECTOR_SIZE).map_err(|_| ())
}

/// Convert a local RTC calendar value to a stable seconds counter. Both values
/// use the same local time basis, so elapsed duration is correct across reboot.
pub fn calendar_to_unix(ymd: (u16, u8, u8), hms: (u8, u8, u8)) -> i64 {
    let (year, month, day) = ymd;
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
    for days_in_month in month_days.iter().take(month.saturating_sub(1) as usize) {
        days += *days_in_month as i64;
    }
    days += day.saturating_sub(1) as i64;
    days * 86_400 + hms.0 as i64 * 3_600 + hms.1 as i64 * 60 + hms.2 as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_record_rejects_partial_write() {
        let session = ActiveWorkSession {
            session_id: 42,
            start_unix: 1_729_758_400,
            start_date_ymd: (2024, 10, 24),
            start_time_hms: (9, 0, 0),
            hourly_rate_cents: 5000,
        };
        let mut raw = encode(session);
        raw[24..28].copy_from_slice(&[0xFF; 4]);
        assert_eq!(decode(&raw), None);
    }
}
