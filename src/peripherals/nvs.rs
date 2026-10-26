// NVS-style persistent storage for WiFi credentials
// Uses esp-storage FlashStorage with a simple binary layout:
//
//   Offset  Size  Content
//   ------  ----  -------
//     0       4   Magic header: b"SBIE"
//     4       1   SSID length (0-32)
//     5      32   SSID bytes
//    37       1   Password length (0-64)
//    38      64   Password bytes
//   102     ...   (future use)
//
// Total: 102 bytes, stored in one flash sector (4096 bytes).
// The sector address is the last OTA-safe sector in the "nvs" region.
// On ESP32-S3 with IDF partition table, NVS starts at 0x9000.
// We use a dedicated offset well inside that region.
//
// NOTE: FlashStorage::write() erases + rewrites the entire 4KB sector
// on each call, so writes are safe but moderately expensive (~10ms).

use crate::peripherals::flash_layout::CREDENTIALS;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;

/// Flash sector address for our credential storage.
/// 0x9000 = default IDF NVS partition start; we use the first sector.
/// If you have a custom partition table, adjust accordingly.
const CRED_FLASH_OFFSET: u32 = CREDENTIALS.start;
const CRED_SECTOR_SIZE: u32 = CREDENTIALS.len;

const MAGIC: [u8; 4] = *b"SBIE";

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

/// Read credentials from flash. Returns `None` if no valid record found.
pub fn read_credentials() -> Option<StoredCredentials> {
    let mut storage = FlashStorage::new();
    let mut buf = [0u8; 128];

    // FlashStorage::read requires aligned 4-byte offset and len
    if storage.read(CRED_FLASH_OFFSET, &mut buf[..104]).is_err() {
        return None;
    }

    // Check magic
    if &buf[0..4] != &MAGIC {
        return None;
    }

    let ssid_len = buf[4] as usize;
    if ssid_len > 32 {
        return None;
    }
    let pass_len = buf[37] as usize;
    if pass_len > 64 {
        return None;
    }

    let mut creds = StoredCredentials::empty();
    creds.ssid_len = ssid_len;
    creds.ssid[..ssid_len].copy_from_slice(&buf[5..5 + ssid_len]);
    creds.pass_len = pass_len;
    creds.password[..pass_len].copy_from_slice(&buf[38..38 + pass_len]);

    Some(creds)
}

/// Write credentials to flash. Erases the sector and rewrites.
/// Returns Ok(()) on success.
pub fn write_credentials(creds: &StoredCredentials) -> Result<(), CredentialStorageError> {
    if creds.ssid_len == 0
        || creds.ssid_len > creds.ssid.len()
        || creds.pass_len > creds.password.len()
    {
        return Err(CredentialStorageError::InvalidLength);
    }
    let mut storage = FlashStorage::new();

    // Build the 4096-byte sector buffer (rest stays 0xFF to preserve flash health)
    // We only need 102 bytes but FlashStorage::write erases the whole sector.
    let mut sector = alloc::boxed::Box::new([0xFFu8; CRED_SECTOR_SIZE as usize]);
    sector[0..4].copy_from_slice(&MAGIC);
    sector[4] = creds.ssid_len as u8;
    sector[5..5 + creds.ssid_len].copy_from_slice(&creds.ssid[..creds.ssid_len]);
    sector[37] = creds.pass_len as u8;
    sector[38..38 + creds.pass_len].copy_from_slice(&creds.password[..creds.pass_len]);

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
