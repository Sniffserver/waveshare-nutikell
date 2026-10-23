//! Read-Only GATT Explorer & Attribute Model.
//!
//! Enforces a strict read-only security boundary on the ESP32-S3 watch:
//! all write operations (Write Request, Write Command, Signed Write) are
//! permanently forbidden and rejected by type safety and policy.

use heapless::Vec;

pub const MAX_SERVICES_PER_DEVICE: usize = 4;
pub const MAX_CHARS_PER_SERVICE: usize = 4;
pub const MAX_VALUE_LEN: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GattError {
    WriteForbidden,
    BufferFull,
    InvalidHandle,
    ReadFailed,
}

/// Strict Read-Only Security Guard
pub struct ReadOnlyGuard;

impl ReadOnlyGuard {
    /// Explicitly rejects any write operation attempt.
    pub fn enforce_no_write() -> Result<(), GattError> {
        Err(GattError::WriteForbidden)
    }
}

/// Standard 16-bit Bluetooth SIG Assigned Services
pub fn lookup_service_name(uuid: u16) -> &'static str {
    match uuid {
        0x1800 => "Generic Access",
        0x1801 => "Generic Attribute",
        0x180A => "Device Information",
        0x180F => "Battery Service",
        0x180D => "Heart Rate Service",
        0x181A => "Environmental Sensing",
        0x1819 => "Location & Navigation",
        0x1815 => "Automation IO",
        0x1802 => "Immediate Alert",
        0x1803 => "Link Loss",
        0x1804 => "Tx Power",
        0x1805 => "Current Time Service",
        0x1812 => "Human Interface Device",
        0xFEAA => "Google Eddystone",
        0xFD6F => "Exposure Notification",
        0xFFE0 => "Nordic UART Service",
        _ => "Custom Service",
    }
}

/// Standard 16-bit Bluetooth SIG Assigned Characteristics
pub fn lookup_characteristic_name(uuid: u16) -> &'static str {
    match uuid {
        0x2A00 => "Device Name",
        0x2A01 => "Appearance",
        0x2A04 => "Peripheral Pref Params",
        0x2A05 => "Service Changed",
        0x2A19 => "Battery Level",
        0x2A24 => "Model Number",
        0x2A25 => "Serial Number",
        0x2A26 => "Firmware Revision",
        0x2A27 => "Hardware Revision",
        0x2A28 => "Software Revision",
        0x2A29 => "Manufacturer Name",
        0x2A37 => "Heart Rate Measurement",
        0x2A67 => "Location and Speed",
        0x2A68 => "Navigation",
        0x2A6E => "Temperature",
        0x2A6F => "Humidity",
        0x2A76 => "UV Index",
        _ => "Custom Char",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredCharacteristic {
    pub uuid: u16,
    pub handle: u16,
    pub properties: u8, // Read, Notify, Indicate
    pub value: Vec<u8, MAX_VALUE_LEN>,
}

impl DiscoveredCharacteristic {
    pub fn new(uuid: u16, handle: u16, properties: u8) -> Self {
        Self {
            uuid,
            handle,
            properties,
            value: Vec::new(),
        }
    }

    /// Read-only update of the cached characteristic value.
    pub fn update_value(&mut self, raw: &[u8]) {
        self.value.clear();
        let len = raw.len().min(MAX_VALUE_LEN);
        let _ = self.value.extend_from_slice(&raw[..len]);
    }

    /// Attempting to write will always return an error.
    pub fn write_value(&mut self, _data: &[u8]) -> Result<(), GattError> {
        ReadOnlyGuard::enforce_no_write()
    }

    /// Formats the characteristic value for human display.
    pub fn format_value<'a>(&self, buf: &'a mut [u8; 48]) -> &'a str {
        if self.value.is_empty() {
            return "(empty)";
        }

        match self.uuid {
            // Battery Level (0-100%)
            0x2A19 => {
                let pct = self.value[0];
                let mut p = 0;
                if pct >= 100 {
                    buf[p] = b'1'; p += 1;
                    buf[p] = b'0'; p += 1;
                    buf[p] = b'0'; p += 1;
                } else {
                    if pct >= 10 {
                        buf[p] = b'0' + (pct / 10); p += 1;
                    }
                    buf[p] = b'0' + (pct % 10); p += 1;
                }
                buf[p] = b'%'; p += 1;
                core::str::from_utf8(&buf[..p]).unwrap_or("?%")
            }
            // Strings: Device Name, Model, Serial, FW, Manufacturer
            0x2A00 | 0x2A24 | 0x2A25 | 0x2A26 | 0x2A27 | 0x2A28 | 0x2A29 => {
                if let Ok(s) = core::str::from_utf8(&self.value) {
                    let len = s.len().min(47);
                    buf[..len].copy_from_slice(&s.as_bytes()[..len]);
                    core::str::from_utf8(&buf[..len]).unwrap_or("???")
                } else {
                    format_hex(&self.value, buf)
                }
            }
            // Fallback: Hex bytes
            _ => format_hex(&self.value, buf),
        }
    }
}

fn format_hex<'a>(bytes: &[u8], buf: &'a mut [u8; 48]) -> &'a str {
    const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";
    let mut p = 0;
    for &b in bytes.iter().take(15) {
        if p + 3 >= buf.len() { break; }
        buf[p] = HEX_CHARS[(b >> 4) as usize]; p += 1;
        buf[p] = HEX_CHARS[(b & 0xF) as usize]; p += 1;
        buf[p] = b' '; p += 1;
    }
    if p > 0 { p -= 1; } // drop trailing space
    core::str::from_utf8(&buf[..p]).unwrap_or("")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredService {
    pub uuid: u16,
    pub start_handle: u16,
    pub end_handle: u16,
    pub characteristics: Vec<DiscoveredCharacteristic, MAX_CHARS_PER_SERVICE>,
}

impl DiscoveredService {
    pub fn new(uuid: u16, start_handle: u16, end_handle: u16) -> Self {
        Self {
            uuid,
            start_handle,
            end_handle,
            characteristics: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GattProfile {
    pub services: Vec<DiscoveredService, MAX_SERVICES_PER_DEVICE>,
}

impl GattProfile {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
        }
    }

    pub fn total_characteristics(&self) -> usize {
        let mut count = 0;
        for s in &self.services {
            count += s.characteristics.len();
        }
        count
    }
}

impl Default for GattProfile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_guard_rejects_write() {
        assert_eq!(ReadOnlyGuard::enforce_no_write(), Err(GattError::WriteForbidden));

        let mut ch = DiscoveredCharacteristic::new(0x2A19, 14, 0x02);
        let write_res = ch.write_value(&[50]);
        assert_eq!(write_res, Err(GattError::WriteForbidden));
    }

    #[test]
    fn format_battery_characteristic() {
        let mut ch = DiscoveredCharacteristic::new(0x2A19, 14, 0x02);
        ch.update_value(&[87]);

        let mut buf = [0u8; 48];
        assert_eq!(ch.format_value(&mut buf), "87%");
    }

    #[test]
    fn format_string_characteristic() {
        let mut ch = DiscoveredCharacteristic::new(0x2A24, 20, 0x02); // Model Number
        ch.update_value(b"Falcon-9-Avionics");

        let mut buf = [0u8; 48];
        assert_eq!(ch.format_value(&mut buf), "Falcon-9-Avionics");
    }

    #[test]
    fn lookup_service_and_char_names() {
        assert_eq!(lookup_service_name(0x180F), "Battery Service");
        assert_eq!(lookup_service_name(0x180A), "Device Information");
        assert_eq!(lookup_characteristic_name(0x2A00), "Device Name");
        assert_eq!(lookup_characteristic_name(0x2A19), "Battery Level");
    }
}
