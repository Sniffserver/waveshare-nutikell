// NVS-style persistent storage for WiFi credentials and device key.
// Dual-slot transactional flash storage with HMAC-SHA256 authentication.
//
// Flash layout:
//   Sector: 0x9000..0xA000 (4 KB, application-owned NVS partition)
//   Slot 0: Offset 0x9000 (256 bytes)
//   Slot 1: Offset 0x9100 (256 bytes)
//
// Slot Binary Layout (Version 3):
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic: b"SBIE"
//     4       1   Record version (3)
//     5       4   Sequence number (u32 le)
//     9       1   SSID length (0..=32)
//    10      32   SSID bytes
//    42       1   Password length (0..=63)
//    43      64   Password bytes
//   107      32   HMAC-SHA-256 tag (covers bytes 0..107)
//   139      32   Per-device random HMAC key
//   171      85   Padding / reserved (0xFF) to 256 bytes
//
// Also supports reading legacy Version 2 single-slot records:
//   Offset 0..4: Magic, 4: Version (2), 5: SSID len, 6..38: SSID,
//   38: Pass len, 39..103: Pass, 104..136: HMAC, 136..168: Key.

use crate::peripherals::flash_layout::CREDENTIALS;
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[cfg(target_arch = "xtensa")]
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
#[cfg(target_arch = "xtensa")]
use esp_storage::FlashStorage;

const CRED_FLASH_OFFSET: u32 = CREDENTIALS.start;
const CRED_SECTOR_SIZE: u32 = CREDENTIALS.len;

pub const SLOT_SIZE: u32 = 256;
pub const SLOT_0_OFFSET: u32 = CRED_FLASH_OFFSET;
pub const SLOT_1_OFFSET: u32 = CRED_FLASH_OFFSET + SLOT_SIZE;

const MAGIC: [u8; 4] = *b"SBIE";
const LEGACY_VERSION: u8 = 2;
const CURRENT_VERSION: u8 = 3;

const V2_AUTH_END: usize = 104;
const V2_TAG_START: usize = 104;
const V2_TAG_END: usize = 136;
const V2_KEY_START: usize = 136;
const V2_KEY_END: usize = 168;

const V3_AUTH_END: usize = 107;
const V3_TAG_START: usize = 107;
const V3_TAG_END: usize = 139;
const V3_KEY_START: usize = 139;
const V3_KEY_END: usize = 171;

const DEVICE_KEY_LEN: usize = 32;

pub trait NvsFlash {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), CredentialStorageError>;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), CredentialStorageError>;
    fn erase(&mut self, from: u32, to: u32) -> Result<(), CredentialStorageError>;
}

#[cfg(target_arch = "xtensa")]
impl NvsFlash for FlashStorage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), CredentialStorageError> {
        ReadNorFlash::read(self, offset, bytes).map_err(|_| CredentialStorageError::Read)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), CredentialStorageError> {
        NorFlash::write(self, offset, bytes).map_err(|_| CredentialStorageError::Write)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), CredentialStorageError> {
        NorFlash::erase(self, from, to).map_err(|_| CredentialStorageError::Erase)
    }
}

fn flash_encryption_enabled() -> bool {
    #[cfg(target_arch = "xtensa")]
    {
        esp_hal::efuse::flash_encryption()
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        true
    }
}

/// Stored WiFi credentials. Zeroable / copyable so callers can own it.
#[derive(Clone)]
pub struct StoredCredentials {
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub password: [u8; 64],
    pub pass_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStorageError {
    Read,
    InvalidLength,
    Erase,
    Write,
    VerifyFailed,
    KeyUnavailable,
    AuthenticationFailed,
    FlashEncryptionRequired,
}

impl StoredCredentials {
    pub fn empty() -> Self {
        Self {
            ssid: [0; 32],
            ssid_len: 0,
            password: [0; 64],
            pass_len: 0,
        }
    }

    pub fn ssid_str(&self) -> &str {
        core::str::from_utf8(&self.ssid[..self.ssid_len]).unwrap_or("")
    }

    pub fn pass_str(&self) -> &str {
        core::str::from_utf8(&self.password[..self.pass_len]).unwrap_or("")
    }

    pub fn is_configured(&self) -> bool {
        self.ssid_len > 0
    }
}

impl Drop for StoredCredentials {
    fn drop(&mut self) {
        self.password.fill(0);
        self.ssid.fill(0);
        self.pass_len = 0;
        self.ssid_len = 0;
    }
}

pub struct DeviceAuthKey(pub [u8; DEVICE_KEY_LEN]);

impl DeviceAuthKey {
    pub fn as_bytes(&self) -> &[u8; DEVICE_KEY_LEN] {
        &self.0
    }
}

impl Drop for DeviceAuthKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Clone)]
pub struct DecodedRecord {
    pub creds: StoredCredentials,
    pub device_key: [u8; DEVICE_KEY_LEN],
    pub sequence: u32,
}

fn decode_slot(slot: &[u8]) -> Option<DecodedRecord> {
    if slot.len() < SLOT_SIZE as usize || &slot[0..4] != &MAGIC {
        return None;
    }

    let version = slot[4];
    match version {
        CURRENT_VERSION => {
            let sequence = u32::from_le_bytes([slot[5], slot[6], slot[7], slot[8]]);
            let ssid_len = slot[9] as usize;
            if ssid_len > 32 {
                return None;
            }
            let pass_len = slot[42] as usize;
            if pass_len > 63 {
                return None;
            }

            let key_slice = &slot[V3_KEY_START..V3_KEY_END];
            if key_slice.iter().all(|b| *b == 0xFF) || key_slice.iter().all(|b| *b == 0) {
                return None;
            }

            let mut mac = Hmac::<Sha256>::new_from_slice(key_slice).ok()?;
            mac.update(&slot[..V3_AUTH_END]);
            mac.verify_slice(&slot[V3_TAG_START..V3_TAG_END]).ok()?;

            let mut creds = StoredCredentials::empty();
            creds.ssid_len = ssid_len;
            creds.ssid[..ssid_len].copy_from_slice(&slot[10..10 + ssid_len]);
            creds.pass_len = pass_len;
            creds.password[..pass_len].copy_from_slice(&slot[43..43 + pass_len]);

            let mut device_key = [0u8; DEVICE_KEY_LEN];
            device_key.copy_from_slice(key_slice);

            Some(DecodedRecord {
                creds,
                device_key,
                sequence,
            })
        }
        LEGACY_VERSION => {
            let ssid_len = slot[5] as usize;
            if ssid_len > 32 {
                return None;
            }
            let pass_len = slot[38] as usize;
            if pass_len > 63 {
                return None;
            }

            let key_slice = &slot[V2_KEY_START..V2_KEY_END];
            if key_slice.iter().all(|b| *b == 0xFF) || key_slice.iter().all(|b| *b == 0) {
                return None;
            }

            let mut mac = Hmac::<Sha256>::new_from_slice(key_slice).ok()?;
            mac.update(&slot[..V2_AUTH_END]);
            mac.verify_slice(&slot[V2_TAG_START..V2_TAG_END]).ok()?;

            let mut creds = StoredCredentials::empty();
            creds.ssid_len = ssid_len;
            creds.ssid[..ssid_len].copy_from_slice(&slot[6..6 + ssid_len]);
            creds.pass_len = pass_len;
            creds.password[..pass_len].copy_from_slice(&slot[39..39 + pass_len]);

            let mut device_key = [0u8; DEVICE_KEY_LEN];
            device_key.copy_from_slice(key_slice);

            Some(DecodedRecord {
                creds,
                device_key,
                sequence: 1,
            })
        }
        _ => None,
    }
}

pub fn load_active_record_from<F: NvsFlash>(flash: &mut F) -> Option<(DecodedRecord, usize)> {
    let mut slot0 = [0u8; SLOT_SIZE as usize];
    let mut slot1 = [0u8; SLOT_SIZE as usize];

    let rec0 = flash
        .read(SLOT_0_OFFSET, &mut slot0)
        .ok()
        .and_then(|_| decode_slot(&slot0));
    let rec1 = flash
        .read(SLOT_1_OFFSET, &mut slot1)
        .ok()
        .and_then(|_| decode_slot(&slot1));

    match (rec0, rec1) {
        (Some(r0), Some(r1)) => {
            if r1.sequence > r0.sequence {
                Some((r1, 1))
            } else {
                Some((r0, 0))
            }
        }
        (Some(r0), None) => Some((r0, 0)),
        (None, Some(r1)) => Some((r1, 1)),
        (None, None) => None,
    }
}

pub fn save_credentials_to<F: NvsFlash>(
    flash: &mut F,
    creds: &StoredCredentials,
) -> Result<(), CredentialStorageError> {
    if !flash_encryption_enabled() {
        return Err(CredentialStorageError::FlashEncryptionRequired);
    }
    if creds.ssid_len == 0
        || creds.ssid_len > creds.ssid.len()
        || creds.pass_len > 63
        || core::str::from_utf8(&creds.ssid[..creds.ssid_len]).is_err()
        || core::str::from_utf8(&creds.password[..creds.pass_len]).is_err()
    {
        return Err(CredentialStorageError::InvalidLength);
    }

    let active = load_active_record_from(flash);
    let (mut key, sequence, active_slot) = match active {
        Some((rec, slot)) => (rec.device_key, rec.sequence.saturating_add(1), Some(slot)),
        None => {
            let mut new_key = [0u8; DEVICE_KEY_LEN];
            #[cfg(target_arch = "xtensa")]
            esp_hal::rng::Rng::new().read(&mut new_key);
            #[cfg(not(target_arch = "xtensa"))]
            new_key.copy_from_slice(b"host-only-test-device-key-32byte");
            (new_key, 1, None)
        }
    };

    let mut buf = [0xFFu8; SLOT_SIZE as usize];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = CURRENT_VERSION;
    buf[5..9].copy_from_slice(&sequence.to_le_bytes());
    buf[9] = creds.ssid_len as u8;
    buf[10..10 + creds.ssid_len].copy_from_slice(&creds.ssid[..creds.ssid_len]);
    buf[42] = creds.pass_len as u8;
    buf[43..43 + creds.pass_len].copy_from_slice(&creds.password[..creds.pass_len]);

    let mut mac =
        Hmac::<Sha256>::new_from_slice(&key).map_err(|_| CredentialStorageError::KeyUnavailable)?;
    mac.update(&buf[..V3_AUTH_END]);
    buf[V3_TAG_START..V3_TAG_END].copy_from_slice(&mac.finalize().into_bytes());
    buf[V3_KEY_START..V3_KEY_END].copy_from_slice(&key);

    // Determine target slot
    match active_slot {
        Some(0) => {
            // Check if slot 1 is erased so we can write without erasing the sector
            let mut check = [0u8; SLOT_SIZE as usize];
            let is_erased = flash
                .read(SLOT_1_OFFSET, &mut check)
                .map(|_| check.iter().all(|b| *b == 0xFF))
                .unwrap_or(false);

            if is_erased {
                flash.write(SLOT_1_OFFSET, &buf)?;
            } else {
                // Erase entire sector and write to slot 0
                flash.erase(CRED_FLASH_OFFSET, CRED_FLASH_OFFSET + CRED_SECTOR_SIZE)?;
                flash.write(SLOT_0_OFFSET, &buf)?;
            }
        }
        _ => {
            // Slot 1 is active (or initial write / unrecognized slot): erase sector and write to slot 0
            flash.erase(CRED_FLASH_OFFSET, CRED_FLASH_OFFSET + CRED_SECTOR_SIZE)?;
            flash.write(SLOT_0_OFFSET, &buf)?;
        }
    }

    // Verify written active record
    match load_active_record_from(flash) {
        Some((saved, _))
            if saved.sequence == sequence
                && saved.creds.ssid_len == creds.ssid_len
                && saved.creds.pass_len == creds.pass_len
                && saved.creds.ssid[..saved.creds.ssid_len] == creds.ssid[..creds.ssid_len]
                && saved.creds.password[..saved.creds.pass_len]
                    == creds.password[..creds.pass_len] =>
        {
            key.fill(0);
            buf.fill(0);
            Ok(())
        }
        _ => Err(CredentialStorageError::VerifyFailed),
    }
}

pub fn erase_credentials_from<F: NvsFlash>(flash: &mut F) -> Result<(), CredentialStorageError> {
    flash.erase(CRED_FLASH_OFFSET, CRED_FLASH_OFFSET + CRED_SECTOR_SIZE)
}

/// Read credentials from flash. Returns `None` if no valid record found.
pub fn read_credentials() -> Option<StoredCredentials> {
    if !flash_encryption_enabled() {
        return None;
    }
    #[cfg(target_arch = "xtensa")]
    {
        let mut storage = FlashStorage::new();
        load_active_record_from(&mut storage).map(|(r, _)| r.creds)
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        None
    }
}

pub fn read_device_auth_key() -> Option<DeviceAuthKey> {
    if !flash_encryption_enabled() {
        return None;
    }
    #[cfg(target_arch = "xtensa")]
    {
        let mut storage = FlashStorage::new();
        load_active_record_from(&mut storage).map(|(r, _)| DeviceAuthKey(r.device_key))
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        None
    }
}

pub fn write_credentials(creds: &StoredCredentials) -> Result<(), CredentialStorageError> {
    #[cfg(target_arch = "xtensa")]
    {
        let mut storage = FlashStorage::new();
        save_credentials_to(&mut storage, creds)
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        let _ = creds;
        Ok(())
    }
}

pub fn erase_credentials() -> Result<(), CredentialStorageError> {
    #[cfg(target_arch = "xtensa")]
    {
        let mut storage = FlashStorage::new();
        erase_credentials_from(&mut storage)
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockFlash {
        data: [u8; 4096],
        fail_next_write: bool,
    }

    impl MockFlash {
        fn new() -> Self {
            Self {
                data: [0xFF; 4096],
                fail_next_write: false,
            }
        }
    }

    impl NvsFlash for MockFlash {
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), CredentialStorageError> {
            let rel = (offset - CRED_FLASH_OFFSET) as usize;
            bytes.copy_from_slice(&self.data[rel..rel + bytes.len()]);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), CredentialStorageError> {
            if self.fail_next_write {
                return Err(CredentialStorageError::Write);
            }
            let rel = (offset - CRED_FLASH_OFFSET) as usize;
            for (i, &b) in bytes.iter().enumerate() {
                self.data[rel + i] &= b;
            }
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), CredentialStorageError> {
            let start = (from - CRED_FLASH_OFFSET) as usize;
            let end = (to - CRED_FLASH_OFFSET) as usize;
            self.data[start..end].fill(0xFF);
            Ok(())
        }
    }

    fn sample_creds(ssid: &str, pass: &str) -> StoredCredentials {
        let mut c = StoredCredentials::empty();
        let s_b = ssid.as_bytes();
        let p_b = pass.as_bytes();
        c.ssid[..s_b.len()].copy_from_slice(s_b);
        c.ssid_len = s_b.len();
        c.password[..p_b.len()].copy_from_slice(p_b);
        c.pass_len = p_b.len();
        c
    }

    #[test]
    fn initial_write_and_readback() {
        let mut flash = MockFlash::new();
        let creds = sample_creds("HomeNetwork", "SuperSecret123");

        save_credentials_to(&mut flash, &creds).expect("save succeeds");
        let (active, slot) = load_active_record_from(&mut flash).expect("active record exists");
        assert_eq!(slot, 0);
        assert_eq!(active.sequence, 1);
        assert_eq!(active.creds.ssid_str(), "HomeNetwork");
        assert_eq!(active.creds.pass_str(), "SuperSecret123");
    }

    #[test]
    fn transactional_update_preserves_slot0_on_failure() {
        let mut flash = MockFlash::new();
        let creds1 = sample_creds("Net1", "Pass1");
        save_credentials_to(&mut flash, &creds1).unwrap();

        // Simulate failure on updating to slot 1:
        flash.fail_next_write = true;
        let creds2 = sample_creds("Net2", "Pass2");
        assert!(save_credentials_to(&mut flash, &creds2).is_err());

        // Slot 0 credentials and device key remain intact:
        let (active, slot) = load_active_record_from(&mut flash).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(active.creds.ssid_str(), "Net1");
    }

    #[test]
    fn sequential_updates_advance_sequence_and_slots() {
        let mut flash = MockFlash::new();
        let creds1 = sample_creds("Net1", "Pass1");
        let creds2 = sample_creds("Net2", "Pass2");

        save_credentials_to(&mut flash, &creds1).unwrap();
        let (r1, s1) = load_active_record_from(&mut flash).unwrap();
        assert_eq!(s1, 0);
        assert_eq!(r1.sequence, 1);

        // Update writes to slot 1
        save_credentials_to(&mut flash, &creds2).unwrap();
        let (r2, s2) = load_active_record_from(&mut flash).unwrap();
        assert_eq!(s2, 1);
        assert_eq!(r2.sequence, 2);
        assert_eq!(r2.creds.ssid_str(), "Net2");
        // Device key is preserved across update
        assert_eq!(r1.device_key, r2.device_key);
    }

    #[test]
    fn legacy_v2_migration() {
        let mut flash = MockFlash::new();
        let key = [42u8; 32];
        let ssid = b"LegacySSID";
        let pass = b"LegacyPass";

        // Build a legacy V2 record in slot 0
        let mut slot0 = [0xFFu8; 256];
        slot0[0..4].copy_from_slice(&MAGIC);
        slot0[4] = LEGACY_VERSION;
        slot0[5] = ssid.len() as u8;
        slot0[6..6 + ssid.len()].copy_from_slice(ssid);
        slot0[38] = pass.len() as u8;
        slot0[39..39 + pass.len()].copy_from_slice(pass);

        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(&slot0[..V2_AUTH_END]);
        slot0[V2_TAG_START..V2_TAG_END].copy_from_slice(&mac.finalize().into_bytes());
        slot0[V2_KEY_START..V2_KEY_END].copy_from_slice(&key);

        flash.data[..256].copy_from_slice(&slot0);

        // Verify it loads with sequence 1
        let (active, slot) = load_active_record_from(&mut flash).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(active.sequence, 1);
        assert_eq!(active.creds.ssid_str(), "LegacySSID");
        assert_eq!(active.device_key, key);

        // Upgrading to V3
        let creds_v3 = sample_creds("UpgradedSSID", "NewPass");
        save_credentials_to(&mut flash, &creds_v3).unwrap();
        let (active_v3, slot_v3) = load_active_record_from(&mut flash).unwrap();
        assert_eq!(slot_v3, 1);
        assert_eq!(active_v3.sequence, 2);
        assert_eq!(active_v3.creds.ssid_str(), "UpgradedSSID");
        assert_eq!(active_v3.device_key, key);
    }
}
