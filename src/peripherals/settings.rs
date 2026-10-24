// Persistent watch settings — dual-slot flash storage with CRC32 integrity check.
//
// Flash layout (each region is one 4 KB sector):
//
//   0xB000  Slot A  (active)
//   0xC000  Slot B  (backup / staging)
//
// Sector layout (first 64 bytes used):
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic:   b"WCH1"
//     4       4   Version: u32 le
//     8       4   CRC32 of bytes 12..56
//    12      44   WatchSettings blob (packed, fixed size)
//    56      8    Reserved / padding to 64 bytes
//
// Write sequence (atomic swap):
//   1. Serialize new settings into staging sector (slot B).
//   2. Compute and write CRC32.
//   3. Erase slot B, write full sector.
//   4. Verify read-back CRC matches.
//   5. Erase slot A, write same sector image.
//   6. Slot A is now active.
//
// Read sequence:
//   Try slot A → if valid CRC, return it.
//   Try slot B → if valid CRC, restore slot A from B and return it.
//   Otherwise return defaults.

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

const MAGIC: [u8; 4] = *b"WCH1";
const VERSION: u32 = 1;

const SLOT_A: u32 = 0xB000;
const SLOT_B: u32 = 0xC000;
const SECTOR_SIZE: u32 = 4096;
const BLOB_OFFSET: usize = 12;
const BLOB_LEN: usize = 44;
const HEADER_LEN: usize = 12; // magic(4) + version(4) + crc(4)

// ── CRC32 (no-alloc, no external crate) ────────────────────────────────────

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

// ── Settings blob ───────────────────────────────────────────────────────────

/// All user-configurable watch settings.
/// Fixed size 44 bytes to match BLOB_LEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchSettings {
    /// Billable hourly rate in cents (e.g. 5000 = $50.00)
    pub hourly_rate_cents: u32, // 4

    /// Display timeout in seconds before dimming (0 = never)
    pub display_timeout_secs: u16, // 2

    /// Default map zoom level (1-18)
    pub map_zoom: u8, // 1

    /// Haptic feedback enabled
    pub haptics_enabled: bool, // 1

    /// Audio alerts enabled
    pub audio_enabled: bool, // 1

    /// Wi-Fi enabled on boot
    pub wifi_enabled: bool, // 1

    /// BLE enabled on boot
    pub ble_enabled: bool, // 1

    /// 24-hour clock display
    pub clock_24h: bool, // 1

    // padding to 44 bytes
    _pad: [u8; 32],
}

impl WatchSettings {
    pub fn default() -> Self {
        Self {
            hourly_rate_cents: 5000,
            display_timeout_secs: 30,
            map_zoom: 14,
            haptics_enabled: true,
            audio_enabled: true,
            wifi_enabled: false,
            ble_enabled: true,
            clock_24h: true,
            _pad: [0u8; 32],
        }
    }

    fn to_bytes(self) -> [u8; BLOB_LEN] {
        let mut out = [0u8; BLOB_LEN];
        out[0..4].copy_from_slice(&self.hourly_rate_cents.to_le_bytes());
        out[4..6].copy_from_slice(&self.display_timeout_secs.to_le_bytes());
        out[6] = self.map_zoom;
        out[7] = self.haptics_enabled as u8;
        out[8] = self.audio_enabled as u8;
        out[9] = self.wifi_enabled as u8;
        out[10] = self.ble_enabled as u8;
        out[11] = self.clock_24h as u8;
        // bytes 12..44 stay 0 (padding / future fields)
        out
    }

    fn from_bytes(b: &[u8; BLOB_LEN]) -> Self {
        Self {
            hourly_rate_cents: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            display_timeout_secs: u16::from_le_bytes([b[4], b[5]]),
            map_zoom: b[6],
            haptics_enabled: b[7] != 0,
            audio_enabled: b[8] != 0,
            wifi_enabled: b[9] != 0,
            ble_enabled: b[10] != 0,
            clock_24h: b[11] != 0,
            _pad: [0u8; 32],
        }
    }
}

// ── Sector encode/decode ────────────────────────────────────────────────────

fn encode_sector(settings: WatchSettings) -> alloc::boxed::Box<[u8; SECTOR_SIZE as usize]> {
    let mut sector = alloc::boxed::Box::new([0xFFu8; SECTOR_SIZE as usize]);
    sector[0..4].copy_from_slice(&MAGIC);
    sector[4..8].copy_from_slice(&VERSION.to_le_bytes());

    let blob = settings.to_bytes();
    sector[BLOB_OFFSET..BLOB_OFFSET + BLOB_LEN].copy_from_slice(&blob);

    let checksum = crc32(&blob);
    sector[8..12].copy_from_slice(&checksum.to_le_bytes());
    sector
}

fn decode_sector(buf: &[u8; SECTOR_SIZE as usize]) -> Option<WatchSettings> {
    if &buf[0..4] != &MAGIC {
        return None;
    }
    // This fixed-size decoder only understands v1. Rejecting newer layouts is
    // safer than silently interpreting their bytes as v1 settings.
    let version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if version != VERSION {
        return None;
    }

    let stored_crc = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let blob: [u8; BLOB_LEN] = buf[BLOB_OFFSET..BLOB_OFFSET + BLOB_LEN].try_into().ok()?;
    let computed_crc = crc32(&blob);

    if stored_crc != computed_crc {
        return None;
    }

    Some(WatchSettings::from_bytes(&blob))
}

// ── Flash I/O (xtensa only) ──────────────────────────────────────────────────

#[cfg(target_arch = "xtensa")]
fn try_read_slot(storage: &mut FlashStorage, offset: u32) -> Option<WatchSettings> {
    let mut buf = alloc::boxed::Box::new([0u8; SECTOR_SIZE as usize]);
    storage.read(offset, buf.as_mut()).ok()?;
    decode_sector(&buf)
}

/// Load settings from flash. Returns defaults if both slots are invalid.
#[cfg(target_arch = "xtensa")]
pub fn load_settings() -> WatchSettings {
    let mut storage = FlashStorage::new();

    // Try slot A first (primary)
    if let Some(s) = try_read_slot(&mut storage, SLOT_A) {
        return s;
    }

    // Try slot B (backup / partially-written recovery)
    if let Some(s) = try_read_slot(&mut storage, SLOT_B) {
        // Restore slot A from valid slot B
        let _ = restore_from_backup(&mut storage, s);
        return s;
    }

    WatchSettings::default()
}

#[cfg(target_arch = "xtensa")]
fn restore_from_backup(storage: &mut FlashStorage, s: WatchSettings) -> Result<(), ()> {
    let sector = encode_sector(s);
    storage
        .erase(SLOT_A, SLOT_A + SECTOR_SIZE)
        .map_err(|_| ())?;
    storage.write(SLOT_A, sector.as_ref()).map_err(|_| ())?;
    Ok(())
}

/// Persist settings atomically using the two-slot scheme.
///
/// Returns `Ok(())` on success. On error the previous slot A remains intact.
#[cfg(target_arch = "xtensa")]
pub fn save_settings(settings: WatchSettings) -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    let sector = encode_sector(settings);

    // Step 1: Write to slot B (staging)
    storage
        .erase(SLOT_B, SLOT_B + SECTOR_SIZE)
        .map_err(|_| ())?;
    storage.write(SLOT_B, sector.as_ref()).map_err(|_| ())?;

    // Step 2: Verify slot B read-back
    if try_read_slot(&mut storage, SLOT_B) != Some(settings) {
        return Err(());
    }

    // Step 3: Promote to slot A
    storage
        .erase(SLOT_A, SLOT_A + SECTOR_SIZE)
        .map_err(|_| ())?;
    storage.write(SLOT_A, sector.as_ref()).map_err(|_| ())?;

    Ok(())
}

/// Erase both slots (factory reset of settings).
#[cfg(target_arch = "xtensa")]
pub fn erase_settings() -> Result<(), ()> {
    let mut storage = FlashStorage::new();
    storage
        .erase(SLOT_A, SLOT_A + SECTOR_SIZE)
        .map_err(|_| ())?;
    storage
        .erase(SLOT_B, SLOT_B + SECTOR_SIZE)
        .map_err(|_| ())?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let s = WatchSettings {
            hourly_rate_cents: 7500,
            display_timeout_secs: 60,
            map_zoom: 12,
            haptics_enabled: false,
            audio_enabled: true,
            wifi_enabled: true,
            ble_enabled: false,
            clock_24h: false,
            _pad: [0; 32],
        };
        let sector = encode_sector(s);
        let recovered = decode_sector(&sector).expect("decode_sector should succeed");
        assert_eq!(recovered, s);
    }

    #[test]
    fn corrupt_magic_rejected() {
        let s = WatchSettings::default();
        let mut sector = encode_sector(s);
        sector[0] = 0xDE; // corrupt magic
        assert!(decode_sector(&sector).is_none());
    }

    #[test]
    fn corrupt_crc_rejected() {
        let s = WatchSettings::default();
        let mut sector = encode_sector(s);
        sector[8] ^= 0xFF; // flip CRC bytes
        assert!(decode_sector(&sector).is_none());
    }

    #[test]
    fn corrupt_payload_rejected() {
        let s = WatchSettings::default();
        let mut sector = encode_sector(s);
        sector[BLOB_OFFSET] ^= 0x01; // flip one payload bit
        assert!(decode_sector(&sector).is_none());
    }

    #[test]
    fn default_settings_are_sane() {
        let d = WatchSettings::default();
        assert_eq!(d.hourly_rate_cents, 5000);
        assert!(d.haptics_enabled);
        assert!(d.audio_enabled);
        assert!(!d.wifi_enabled);
        assert!(d.ble_enabled);
        assert!(d.clock_24h);
    }

    #[test]
    fn blob_size_matches_constant() {
        let s = WatchSettings::default();
        assert_eq!(s.to_bytes().len(), BLOB_LEN);
    }
}
