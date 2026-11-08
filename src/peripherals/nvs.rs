// NVS-style persistent storage for WiFi credentials
// Uses esp-storage FlashStorage with a simple binary layout:
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic header: b"SBIE"
//     4       1   Record version (2)
//     5       1   SSID length (0-32)
//     6      32   SSID bytes
//    38       1   Password length (0-63)
//    39      64   Password bytes (one byte reserved)
//   104      32   HMAC-SHA-256 record tag
//   136      32   Per-device random HMAC key (inside encrypted flash)
//
// Total record: 136 bytes, stored in one flash sector (4096 bytes).
// The sector address is 0x9000, which is the first sector of the application-owned NVS partition.
// The entire 24 KiB NVS partition (0x9000..0xEFFF) is managed by this firmware.
// ESP-IDF's own NVS implementation is not used by this application.
// phy_init is a separate partition at 0xF000.
//
// NOTE: FlashStorage::write() erases + rewrites the entire 4KB sector
// on each call, so writes are safe but moderately expensive (~10ms).

use crate::peripherals::flash_layout::CREDENTIALS;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Flash sector address for our credential storage.
/// 0x9000 = start of application-owned NVS partition (24 KiB total: 0x9000..0xEFFF).
/// The CREDENTIALS sector occupies the first 4 KiB (0x9000..0xA000).
/// Adjust offset if using a custom partition table where NVS does not start at 0x9000.
const CRED_FLASH_OFFSET: u32 = CREDENTIALS.start;
const CRED_SECTOR_SIZE: u32 = CREDENTIALS.len;

const MAGIC: [u8; 4] = *b"SBIE";
const RECORD_VERSION: u8 = 2;
const RECORD_AUTH_END: usize = 104;
const RECORD_TAG_START: usize = RECORD_AUTH_END;
const RECORD_READ_LEN: usize = RECORD_TAG_START + 32;
const DEVICE_KEY_START: usize = 136;
const DEVICE_KEY_LEN: usize = 32;
const DEVICE_RECORD_READ_LEN: usize = DEVICE_KEY_START + DEVICE_KEY_LEN;

fn key_from_record(record: &[u8]) -> Option<&[u8]> {
    let key = record.get(DEVICE_KEY_START..DEVICE_RECORD_READ_LEN)?;
    if key.iter().all(|b| *b == 0xFF) || key.iter().all(|b| *b == 0) {
        None
    } else {
        Some(key)
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

/// Stored WiFi credentials. Zeroable / copyable so main.rs can own it.
#[derive(Clone)]
pub struct StoredCredentials {
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub password: [u8; 64],
    pub pass_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStorageError {
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
        // Zeroize password and SSID material when dropping from RAM
        self.password.fill(0);
        self.ssid.fill(0);
        self.pass_len = 0;
        self.ssid_len = 0;
    }
}

/// Read credentials from flash. Returns `None` if no valid record found.
pub fn read_credentials() -> Option<StoredCredentials> {
    if !flash_encryption_enabled() {
        return None;
    }
    let mut storage = FlashStorage::new();
    let mut buf = [0u8; DEVICE_RECORD_READ_LEN];

    // FlashStorage::read requires aligned 4-byte offset and len
    if storage.read(CRED_FLASH_OFFSET, &mut buf).is_err() {
        return None;
    }

    // Check magic
    if &buf[0..4] != &MAGIC {
        return None;
    }

    if buf[4] != RECORD_VERSION {
        return None;
    }
    let ssid_len = buf[5] as usize;
    if ssid_len > 32 {
        return None;
    }
    let pass_len = buf[38] as usize;
    if pass_len > 63 {
        return None;
    }

    let key = key_from_record(&buf)?;
    let mut creds = StoredCredentials::empty();
    creds.ssid_len = ssid_len;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(&buf[..RECORD_AUTH_END]);
    mac.verify_slice(&buf[RECORD_TAG_START..RECORD_READ_LEN])
        .ok()?;
    creds.ssid[..ssid_len].copy_from_slice(&buf[6..6 + ssid_len]);
    creds.pass_len = pass_len;
    creds.password[..pass_len].copy_from_slice(&buf[39..39 + pass_len]);

    Some(creds)
}

pub struct DeviceAuthKey([u8; DEVICE_KEY_LEN]);

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

pub fn read_device_auth_key() -> Option<DeviceAuthKey> {
    if !flash_encryption_enabled() {
        return None;
    }
    let mut storage = FlashStorage::new();
    let mut record = [0u8; DEVICE_RECORD_READ_LEN];
    if storage.read(CRED_FLASH_OFFSET, &mut record).is_err()
        || &record[0..4] != &MAGIC
        || record[4] != RECORD_VERSION
        || record[5] as usize > 32
        || record[38] as usize > 63
    {
        return None;
    }

    let key = key_from_record(&record)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(&record[..RECORD_AUTH_END]);
    mac.verify_slice(&record[RECORD_TAG_START..RECORD_READ_LEN])
        .ok()?;

    let mut device_key = [0u8; DEVICE_KEY_LEN];
    device_key.copy_from_slice(key);
    record.fill(0);
    Some(DeviceAuthKey(device_key))
}

/// Write credentials to flash. Erases the sector and rewrites.
/// Returns Ok(()) on success.
pub fn write_credentials(creds: &StoredCredentials) -> Result<(), CredentialStorageError> {
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
    let mut storage = FlashStorage::new();
    let mut key = [0u8; DEVICE_KEY_LEN];
    let mut existing = [0u8; DEVICE_RECORD_READ_LEN];
    if storage.read(CRED_FLASH_OFFSET, &mut existing).is_ok() {
        if let Some(saved) = key_from_record(&existing) {
            key.copy_from_slice(saved);
        }
    }
    if key.iter().all(|b| *b == 0) {
        #[cfg(target_arch = "xtensa")]
        esp_hal::rng::Rng::new().read(&mut key);
        #[cfg(not(target_arch = "xtensa"))]
        key.copy_from_slice(b"host-only-test-device-key-32bytes!");
    }

    // Build the 4096-byte sector buffer (rest stays 0xFF to preserve flash health)
    // We only need 136 bytes but FlashStorage::write erases the whole sector.
    let mut sector = alloc::boxed::Box::new([0xFFu8; CRED_SECTOR_SIZE as usize]);
    sector[0..4].copy_from_slice(&MAGIC);
    sector[4] = RECORD_VERSION;
    sector[5] = creds.ssid_len as u8;
    sector[6..6 + creds.ssid_len].copy_from_slice(&creds.ssid[..creds.ssid_len]);
    sector[38] = creds.pass_len as u8;
    sector[39..39 + creds.pass_len].copy_from_slice(&creds.password[..creds.pass_len]);
    sector[DEVICE_KEY_START..DEVICE_RECORD_READ_LEN].copy_from_slice(&key);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&key).map_err(|_| CredentialStorageError::KeyUnavailable)?;
    mac.update(&sector[..RECORD_AUTH_END]);
    sector[RECORD_TAG_START..RECORD_READ_LEN].copy_from_slice(&mac.finalize().into_bytes());

    // Erase the sector first
    storage
        .erase(CRED_FLASH_OFFSET, CRED_FLASH_OFFSET + CRED_SECTOR_SIZE)
        .map_err(|_| CredentialStorageError::Erase)?;

    // Write the whole sector back (FlashStorage requires word-aligned writes)
    storage
        .write(CRED_FLASH_OFFSET, sector.as_ref())
        .map_err(|_| CredentialStorageError::Write)?;

    match read_credentials() {
        Some(saved)
            if saved.ssid_len == creds.ssid_len
                && saved.pass_len == creds.pass_len
                && saved.ssid[..saved.ssid_len] == creds.ssid[..creds.ssid_len]
                && saved.password[..saved.pass_len] == creds.password[..creds.pass_len] =>
        {
            Ok(())
        }
        _ => Err(CredentialStorageError::VerifyFailed),
    }
}

/// Erase credentials (reset to factory state).
pub fn erase_credentials() -> Result<(), CredentialStorageError> {
    let mut storage = FlashStorage::new();
    storage
        .erase(CRED_FLASH_OFFSET, CRED_FLASH_OFFSET + CRED_SECTOR_SIZE)
        .map_err(|_| CredentialStorageError::Erase)
}
