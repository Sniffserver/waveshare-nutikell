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

use crate::peripherals::flash_layout::{SETTINGS_A, SETTINGS_B};
const SLOT_A: u32 = SETTINGS_A.start;
const SLOT_B: u32 = SETTINGS_B.start;
const SECTOR_SIZE: u32 = SETTINGS_A.len;
const BLOB_OFFSET: usize = 12;
const BLOB_LEN: usize = 44;
#[allow(dead_code)]
const HEADER_LEN: usize = 12; // magic(4) + version(4) + crc(4)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsStorageError {
    Read,
    Erase,
    Write,
    VerifyFailed,
}

pub trait SettingsFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SettingsStorageError>;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SettingsStorageError>;
    fn erase(&mut self, from: u32, to: u32) -> Result<(), SettingsStorageError>;
}

#[cfg(target_arch = "xtensa")]
impl SettingsFlash for FlashStorage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SettingsStorageError> {
        ReadNorFlash::read(self, offset, bytes).map_err(|_| SettingsStorageError::Read)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SettingsStorageError> {
        NorFlash::write(self, offset, bytes).map_err(|_| SettingsStorageError::Write)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), SettingsStorageError> {
        NorFlash::erase(self, from, to).map_err(|_| SettingsStorageError::Erase)
    }
}

use crate::peripherals::crc::crc32;

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

    /// Timezone offset from UTC in minutes (e.g. +120 for UTC+2, -300 for UTC-5)
    pub timezone_offset_mins: i16, // 2

    // padding to 44 bytes
    _pad: [u8; 30],
}

impl Default for WatchSettings {
    fn default() -> Self {
        Self {
            hourly_rate_cents: 5000,
            display_timeout_secs: 30,
            map_zoom: 14,
            haptics_enabled: true,
            audio_enabled: true,
            wifi_enabled: false,
            ble_enabled: true,
            clock_24h: true,
            timezone_offset_mins: 0,
            _pad: [0u8; 30],
        }
    }
}

impl WatchSettings {
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
        out[12..14].copy_from_slice(&self.timezone_offset_mins.to_le_bytes());
        // bytes 14..44 stay 0 (padding / future fields)
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
            timezone_offset_mins: i16::from_le_bytes([b[12], b[13]]),
            _pad: [0u8; 30],
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
    if buf[0..4] != MAGIC {
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

fn try_read_slot<F: SettingsFlash>(storage: &mut F, offset: u32) -> Option<WatchSettings> {
    let mut buf = alloc::boxed::Box::new([0u8; SECTOR_SIZE as usize]);
    storage.read(offset, buf.as_mut()).ok()?;
    decode_sector(&buf)
}

/// Load settings from storage. Returns defaults if both slots are invalid.
pub fn load_settings_from<F: SettingsFlash>(storage: &mut F) -> WatchSettings {
    // Try slot A first (primary)
    if let Some(s) = try_read_slot(storage, SLOT_A) {
        return s;
    }

    // Try slot B (backup / partially-written recovery)
    if let Some(s) = try_read_slot(storage, SLOT_B) {
        // Restore slot A from valid slot B
        let _ = restore_from_backup(storage, s);
        return s;
    }

    WatchSettings::default()
}

/// Load settings from the device flash.
#[cfg(target_arch = "xtensa")]
pub fn load_settings() -> WatchSettings {
    load_settings_from(&mut FlashStorage::new())
}

fn restore_from_backup<F: SettingsFlash>(
    storage: &mut F,
    settings: WatchSettings,
) -> Result<(), SettingsStorageError> {
    let sector = encode_sector(settings);
    storage.erase(SLOT_A, SLOT_A + SECTOR_SIZE)?;
    storage.write(SLOT_A, sector.as_ref())?;
    if try_read_slot(storage, SLOT_A) == Some(settings) {
        Ok(())
    } else {
        Err(SettingsStorageError::VerifyFailed)
    }
}

/// Persist settings atomically using the two-slot scheme.
///
/// Returns `Ok(())` on success. On error the previous slot A remains intact.
pub fn save_settings_to<F: SettingsFlash>(
    storage: &mut F,
    settings: WatchSettings,
) -> Result<(), SettingsStorageError> {
    let sector = encode_sector(settings);

    // Step 1: Write to slot B (staging)
    storage.erase(SLOT_B, SLOT_B + SECTOR_SIZE)?;
    storage.write(SLOT_B, sector.as_ref())?;

    // Step 2: Verify slot B read-back
    if try_read_slot(storage, SLOT_B) != Some(settings) {
        return Err(SettingsStorageError::VerifyFailed);
    }

    // Step 3: Promote to slot A
    storage.erase(SLOT_A, SLOT_A + SECTOR_SIZE)?;
    storage.write(SLOT_A, sector.as_ref())?;
    if try_read_slot(storage, SLOT_A) == Some(settings) {
        Ok(())
    } else {
        Err(SettingsStorageError::VerifyFailed)
    }
}

#[cfg(target_arch = "xtensa")]
pub fn save_settings(settings: WatchSettings) -> Result<(), SettingsStorageError> {
    save_settings_to(&mut FlashStorage::new(), settings)
}

/// Erase both slots (factory reset of settings).
pub fn erase_settings_from<F: SettingsFlash>(storage: &mut F) -> Result<(), SettingsStorageError> {
    storage.erase(SLOT_A, SLOT_A + SECTOR_SIZE)?;
    storage.erase(SLOT_B, SLOT_B + SECTOR_SIZE)?;
    Ok(())
}

#[cfg(target_arch = "xtensa")]
pub fn erase_settings() -> Result<(), SettingsStorageError> {
    erase_settings_from(&mut FlashStorage::new())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    struct FakeFlash {
        bytes: Vec<u8>,
        writes: usize,
        fail_write_at: Option<usize>,
        erases: usize,
        fail_erase_at: Option<usize>,
        corrupt_read_offset: Option<u32>,
    }

    impl FakeFlash {
        fn new() -> Self {
            Self {
                bytes: vec![0xFF; SETTINGS_B.end() as usize],
                writes: 0,
                fail_write_at: None,
                erases: 0,
                fail_erase_at: None,
                corrupt_read_offset: None,
            }
        }
    }

    impl SettingsFlash for FakeFlash {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), SettingsStorageError> {
            let start = offset as usize;
            let end = start
                .checked_add(bytes.len())
                .ok_or(SettingsStorageError::Read)?;
            let source = self
                .bytes
                .get(start..end)
                .ok_or(SettingsStorageError::Read)?;
            bytes.copy_from_slice(source);
            if self.corrupt_read_offset == Some(offset) && !bytes.is_empty() {
                bytes[0] ^= 0xFF;
            }
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), SettingsStorageError> {
            self.writes += 1;
            if self.fail_write_at == Some(self.writes) {
                return Err(SettingsStorageError::Write);
            }
            let start = offset as usize;
            let end = start
                .checked_add(bytes.len())
                .ok_or(SettingsStorageError::Write)?;
            let target = self
                .bytes
                .get_mut(start..end)
                .ok_or(SettingsStorageError::Write)?;
            for (stored, &next) in target.iter_mut().zip(bytes) {
                if (*stored & next) != next {
                    return Err(SettingsStorageError::Write);
                }
                *stored = next;
            }
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), SettingsStorageError> {
            self.erases += 1;
            if self.fail_erase_at == Some(self.erases) {
                return Err(SettingsStorageError::Erase);
            }
            self.bytes
                .get_mut(from as usize..to as usize)
                .ok_or(SettingsStorageError::Erase)?
                .fill(0xFF);
            Ok(())
        }
    }

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
            timezone_offset_mins: 120,
            _pad: [0; 30],
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
    fn unknown_version_rejected() {
        let s = WatchSettings::default();
        let mut sector = encode_sector(s);
        sector[4..8].copy_from_slice(&2u32.to_le_bytes());
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

    #[test]
    fn failed_staging_write_keeps_primary_settings() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        flash.fail_write_at = Some(flash.writes + 1);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::Write)
        );
        flash.fail_write_at = None;
        assert_eq!(load_settings_from(&mut flash), original);
    }

    #[test]
    fn interrupted_primary_promotion_recovers_verified_staging_slot() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        flash.fail_write_at = Some(flash.writes + 2);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::Write)
        );
        flash.fail_write_at = None;
        assert_eq!(load_settings_from(&mut flash), updated);

        let mut primary = [0; SECTOR_SIZE as usize];
        flash.read(SLOT_A, &mut primary).unwrap();
        assert_eq!(decode_sector(&primary), Some(updated));
    }

    #[test]
    fn failed_staging_erase_keeps_primary_settings() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        flash.fail_erase_at = Some(flash.erases + 1);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::Erase)
        );
        flash.fail_erase_at = None;
        assert_eq!(load_settings_from(&mut flash), original);
    }

    #[test]
    fn failed_staging_verify_keeps_primary_settings() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        flash.corrupt_read_offset = Some(SLOT_B);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::VerifyFailed)
        );
        flash.corrupt_read_offset = None;
        assert_eq!(load_settings_from(&mut flash), original);
    }

    #[test]
    fn interrupted_primary_erase_keeps_primary_or_recovers_staging() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        // Erase 1 was during original save. Next save: erase 1 is Slot B, erase 2 is Slot A.
        flash.fail_erase_at = Some(flash.erases + 2);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::Erase)
        );
        flash.fail_erase_at = None;
        // Since erase failed before mutating Slot A, Slot A safely retains original:
        assert_eq!(load_settings_from(&mut flash), original);

        // If power loss corrupted Slot A during the erase operation:
        flash.bytes[SLOT_A as usize] = 0x00;
        assert_eq!(load_settings_from(&mut flash), updated);
    }

    #[test]
    fn corrupted_primary_verify_recovers_verified_staging_slot() {
        let original = WatchSettings::default();
        let updated = WatchSettings {
            timezone_offset_mins: 120,
            ..original
        };
        let mut flash = FakeFlash::new();
        save_settings_to(&mut flash, original).unwrap();
        flash.corrupt_read_offset = Some(SLOT_A);

        assert_eq!(
            save_settings_to(&mut flash, updated),
            Err(SettingsStorageError::VerifyFailed)
        );
        flash.corrupt_read_offset = None;
        assert_eq!(load_settings_from(&mut flash), updated);
    }

    #[test]
    fn both_slots_corrupt_returns_default_settings() {
        let mut flash = FakeFlash::new();
        flash.bytes.fill(0x00);
        assert_eq!(load_settings_from(&mut flash), WatchSettings::default());
    }

    #[test]
    fn recovery_from_backup_restores_primary() {
        let target = WatchSettings {
            timezone_offset_mins: 180,
            ..WatchSettings::default()
        };
        let mut flash = FakeFlash::new();
        // Write valid encoded sector to slot B only
        let sector = encode_sector(target);
        flash.write(SLOT_B, sector.as_ref()).unwrap();

        assert_eq!(load_settings_from(&mut flash), target);

        // Verify slot A was restored
        let mut primary = [0; SECTOR_SIZE as usize];
        flash.read(SLOT_A, &mut primary).unwrap();
        assert_eq!(decode_sector(&primary), Some(target));
    }
}
