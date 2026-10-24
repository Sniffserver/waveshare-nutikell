// Work-session persistent log — circular binary buffer in one 4 KB flash sector.
//
// Flash offset: 0xD000  (sector 13, immediately after settings slots B at 0xC000)
//
// Sector layout:
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic:      b"WLOG"
//     4       8   Reserved (all 0xFF)
//    12   N*48   Session records (N = MAX_SESSIONS = 85)
//
// Each session record (48 bytes):
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   CRC32 of bytes 4..48
//     4       1   valid flag (0x01 = valid, other = empty/corrupt)
//     5       3   start date: year_hi, year_lo, month (YY_HH, YY_LL, MM)
//     8       4   start time: hour, minute, second, _pad
//    12       4   elapsed_secs: u32 le
//    16       4   break_secs:   u32 le
//    20       4   hourly_rate_cents: u32 le
//    24       4   earned_cents: u32 le
//    28      20   Reserved / future fields
//
// Write strategy: append-only. Records are committed by writing the payload
// first and its CRC last. A reset during a write leaves an invalid record,
// rather than a phantom session. The sector is rebuilt only after all slots
// are full, retaining the newest MAX_SESSIONS - 1 entries.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

const LOG_OFFSET: u32 = 0xD000;
const SECTOR_SIZE: u32 = 4096;
const HEADER_SIZE: usize = 12;
const RECORD_SIZE: usize = 48;
const MAX_SESSIONS: usize = (SECTOR_SIZE as usize - HEADER_SIZE) / RECORD_SIZE; // = 85

const MAGIC: [u8; 4] = *b"WLOG";

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
    /// Calendar year (e.g. 2026)
    pub year: u16,
    /// Calendar month (1-12)
    pub month: u8,
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
        out[8] = self.hour;
        out[9] = self.minute;
        out[10] = self.second;
        out[11] = 0; // pad
        out[12..16].copy_from_slice(&self.elapsed_secs.to_le_bytes());
        out[16..20].copy_from_slice(&self.break_secs.to_le_bytes());
        out[20..24].copy_from_slice(&self.hourly_rate_cents.to_le_bytes());
        out[24..28].copy_from_slice(&self.earned_cents.to_le_bytes());
        // bytes 28..48 reserved, stay 0
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
            hour: raw[8],
            minute: raw[9],
            second: raw[10],
            elapsed_secs: u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            break_secs: u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]),
            hourly_rate_cents: u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]),
            earned_cents: u32::from_le_bytes([raw[24], raw[25], raw[26], raw[27]]),
        })
    }

    fn record_offset(slot: usize) -> u32 {
        (HEADER_SIZE + slot * RECORD_SIZE) as u32
    }
}

// ── Header r/w ───────────────────────────────────────────────────────────────

// ── Full sector rebuild ───────────────────────────────────────────────────────

fn rebuild_sector(records: &[Option<SessionRecord>; MAX_SESSIONS]) -> alloc::boxed::Box<[u8; SECTOR_SIZE as usize]> {
    let mut sector = alloc::boxed::Box::new([0xFFu8; SECTOR_SIZE as usize]);
    sector[0..4].copy_from_slice(&MAGIC);
    for (i, slot) in records.iter().enumerate() {
        if let Some(rec) = slot {
            let off = HEADER_SIZE + i * RECORD_SIZE;
            let raw = rec.to_raw();
            sector[off..off + RECORD_SIZE].copy_from_slice(&raw);
        } else {
            // Leave as 0xFF (erased)
        }
    }
    sector
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read all valid session records from flash, in chronological order.
/// Returns up to MAX_SESSIONS records.
#[cfg(target_arch = "xtensa")]
pub fn read_all_sessions() -> alloc::vec::Vec<SessionRecord> {
    let mut storage = FlashStorage::new();
    let mut out = alloc::vec::Vec::new();

    let mut magic = [0u8; 4];
    if storage.read(LOG_OFFSET, &mut magic).is_err() || magic != MAGIC {
        return out;
    }

    for slot in 0..MAX_SESSIONS {
        let offset = LOG_OFFSET + SessionRecord::record_offset(slot);
        let mut raw = [0u8; RECORD_SIZE];
        if storage.read(offset, &mut raw).is_err() {
            continue;
        }
        if let Some(rec) = SessionRecord::from_raw(&raw) {
            out.push(rec);
        }
    }
    out
}

/// Append one session record to the circular flash log.
///
/// The normal path writes exactly one 48-byte record. Only a full log causes
/// an erase/rebuild, so ordinary clock-outs do not erase the flash sector.
#[cfg(target_arch = "xtensa")]
pub fn append_session(rec: &SessionRecord) -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    let mut magic = [0u8; 4];
    let initialized = storage.read(LOG_OFFSET, &mut magic).is_ok() && magic == MAGIC;
    if !initialized {
        storage.erase(LOG_OFFSET, LOG_OFFSET + SECTOR_SIZE).map_err(|_| ())?;
        storage.write(LOG_OFFSET, &MAGIC).map_err(|_| ())?;
    }

    let sessions = read_all_sessions();
    let mut free_slot = None;
    for slot in 0..MAX_SESSIONS {
        let mut raw = [0u8; RECORD_SIZE];
        storage
            .read(LOG_OFFSET + SessionRecord::record_offset(slot), &mut raw)
            .map_err(|_| ())?;
        if raw.iter().all(|&byte| byte == 0xFF) {
            free_slot = Some(slot);
            break;
        }
    }

    if let Some(slot) = free_slot {
        let raw = rec.to_raw();
        let record_off = LOG_OFFSET + SessionRecord::record_offset(slot);
        // Commit marker (CRC) comes last. An interrupted payload write is
        // ignored by read_all_sessions because it cannot pass the CRC.
        storage.write(record_off + 4, &raw[4..]).map_err(|_| ())?;
        storage.write(record_off, &raw[0..4]).map_err(|_| ())?;
    } else {
        // Keep the newest records and make room for the new one. This is the
        // only path that erases the sector. This also safely recovers a
        // partially-written final record after a power loss.
        let mut records: [Option<SessionRecord>; MAX_SESSIONS] = [None; MAX_SESSIONS];
        for (i, session) in sessions.iter().skip(1).enumerate() {
            records[i] = Some(*session);
        }
        records[MAX_SESSIONS - 1] = Some(*rec);
        let sector = rebuild_sector(&records);
        storage
            .erase(LOG_OFFSET, LOG_OFFSET + SECTOR_SIZE)
            .map_err(|_| ())?;
        storage.write(LOG_OFFSET, sector.as_ref()).map_err(|_| ())?;
    }

    Ok(())
}

/// Format all sessions as a CSV string.
///
/// Suitable for writing to an SD card file.
/// Format: date,start_time,elapsed_h,break_h,rate_per_h,earned
pub fn sessions_to_csv(sessions: &[SessionRecord]) -> alloc::string::String {
    use alloc::string::ToString;

    let mut out =
        alloc::string::String::from("Date,Start,Elapsed (min),Break (min),Rate ($/h),Earned ($)\n");
    for s in sessions {
        let elapsed_min = s.elapsed_secs / 60;
        let break_min = s.break_secs / 60;
        let rate_dollars = s.hourly_rate_cents / 100;
        let earned_dollars = s.earned_cents / 100;
        let earned_cents = s.earned_cents % 100;
        let row = alloc::format!(
            "{}-{:02}-??,{:02}:{:02}:{:02},{},{},{},{}.{:02}\n",
            s.year,
            s.month,
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
pub fn erase_session_log() -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    storage
        .erase(LOG_OFFSET, LOG_OFFSET + SECTOR_SIZE)
        .map_err(|_| ())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(elapsed: u32, rate: u32) -> SessionRecord {
        SessionRecord {
            year: 2026,
            month: 10,
            hour: 9,
            minute: 0,
            second: 0,
            elapsed_secs: elapsed,
            break_secs: 0,
            hourly_rate_cents: rate,
            earned_cents: ((elapsed as u64 * rate as u64) / 3600) as u32,
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
    }
}
