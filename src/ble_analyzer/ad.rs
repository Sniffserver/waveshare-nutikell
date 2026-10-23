//! BLE Advertising Data (AD) structure parser.
//!
//! Parses raw BLE advertisement and scan response payloads into
//! type-safe AD structures without dynamic memory allocation (`no_std`).

use heapless::Vec;

pub const MAX_AD_STRUCTURES: usize = 16;
pub const MAX_AD_PAYLOAD: usize = 31;

/// Common BLE Advertising Data Types (Bluetooth Core Spec v5.4)
pub const AD_TYPE_FLAGS: u8 = 0x01;
pub const AD_TYPE_16BIT_SERVICE_UUID_MORE: u8 = 0x02;
pub const AD_TYPE_16BIT_SERVICE_UUID_COMPLETE: u8 = 0x03;
pub const AD_TYPE_32BIT_SERVICE_UUID_MORE: u8 = 0x04;
pub const AD_TYPE_32BIT_SERVICE_UUID_COMPLETE: u8 = 0x05;
pub const AD_TYPE_128BIT_SERVICE_UUID_MORE: u8 = 0x06;
pub const AD_TYPE_128BIT_SERVICE_UUID_COMPLETE: u8 = 0x07;
pub const AD_TYPE_SHORTENED_LOCAL_NAME: u8 = 0x08;
pub const AD_TYPE_COMPLETE_LOCAL_NAME: u8 = 0x09;
pub const AD_TYPE_TX_POWER_LEVEL: u8 = 0x0A;
pub const AD_TYPE_SERVICE_DATA_16BIT: u8 = 0x16;
pub const AD_TYPE_MANUFACTURER_SPECIFIC: u8 = 0xFF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdStructure {
    pub ad_type: u8,
    pub data: Vec<u8, MAX_AD_PAYLOAD>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdParseError {
    InvalidLength,
    TooManyStructures,
    PayloadTooLarge,
}

/// Parses raw advertising bytes [len][ad_type][data...] into a heapless list of AdStructures.
pub fn parse_advertising_data(
    bytes: &[u8],
) -> Result<Vec<AdStructure, MAX_AD_STRUCTURES>, AdParseError> {
    let mut result = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        let length = bytes[offset] as usize;

        // Length 0 marks padding or end of advertising packet
        if length == 0 {
            break;
        }

        let structure_end = offset
            .checked_add(1 + length)
            .ok_or(AdParseError::InvalidLength)?;

        if structure_end > bytes.len() {
            return Err(AdParseError::InvalidLength);
        }

        if length < 1 {
            return Err(AdParseError::InvalidLength);
        }

        let ad_type = bytes[offset + 1];
        let payload = &bytes[offset + 2..structure_end];

        if payload.len() > MAX_AD_PAYLOAD {
            return Err(AdParseError::PayloadTooLarge);
        }

        let mut data = Vec::new();
        data.extend_from_slice(payload)
            .map_err(|_| AdParseError::PayloadTooLarge)?;

        result
            .push(AdStructure { ad_type, data })
            .map_err(|_| AdParseError::TooManyStructures)?;

        offset = structure_end;
    }

    Ok(result)
}

/// Extracts device name from advertising structures (prefers Complete, falls back to Shortened).
pub fn parse_device_name<'a>(structures: &'a [AdStructure]) -> Option<&'a str> {
    // 1. Try complete name first
    for s in structures {
        if s.ad_type == AD_TYPE_COMPLETE_LOCAL_NAME {
            if let Ok(name) = core::str::from_utf8(&s.data) {
                return Some(name);
            }
        }
    }
    // 2. Fall back to shortened name
    for s in structures {
        if s.ad_type == AD_TYPE_SHORTENED_LOCAL_NAME {
            if let Ok(name) = core::str::from_utf8(&s.data) {
                return Some(name);
            }
        }
    }
    None
}

/// Extracts Manufacturer Specific Data: returns (Company ID, manufacturer payload).
pub fn parse_manufacturer_data<'a>(structures: &'a [AdStructure]) -> Option<(u16, &'a [u8])> {
    for s in structures {
        if s.ad_type == AD_TYPE_MANUFACTURER_SPECIFIC && s.data.len() >= 2 {
            let company_id = u16::from_le_bytes([s.data[0], s.data[1]]);
            return Some((company_id, &s.data[2..]));
        }
    }
    None
}

/// Extracts TX power level in dBm if present.
pub fn parse_tx_power(structures: &[AdStructure]) -> Option<i8> {
    for s in structures {
        if s.ad_type == AD_TYPE_TX_POWER_LEVEL && !s.data.is_empty() {
            return Some(s.data[0] as i8);
        }
    }
    None
}

/// Extracts flags byte if present.
pub fn parse_flags(structures: &[AdStructure]) -> Option<u8> {
    for s in structures {
        if s.ad_type == AD_TYPE_FLAGS && !s.data.is_empty() {
            return Some(s.data[0]);
        }
    }
    None
}

/// Extracts 16-bit Service UUIDs from AD types 0x02 and 0x03.
pub fn parse_16bit_services(structures: &[AdStructure]) -> Vec<u16, 8> {
    let mut services = Vec::new();
    for s in structures {
        if s.ad_type == AD_TYPE_16BIT_SERVICE_UUID_MORE || s.ad_type == AD_TYPE_16BIT_SERVICE_UUID_COMPLETE {
            let mut i = 0;
            while i + 1 < s.data.len() {
                let uuid = u16::from_le_bytes([s.data[i], s.data[i + 1]]);
                let _ = services.push(uuid);
                i += 2;
            }
        }
    }
    services
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_advertising_packet() {
        // Flags: 0x06, Complete Name: "SatLink", Manufacturer: Apple (0x004C)
        let raw = [
            0x02, 0x01, 0x06, // Flags
            0x08, 0x09, b'S', b'a', b't', b'L', b'i', b'n', b'k', // Name
            0x04, 0xFF, 0x4C, 0x00, 0x10, // Manufacturer
        ];

        let parsed = parse_advertising_data(&raw).expect("valid packet");
        assert_eq!(parsed.len(), 3);

        assert_eq!(parsed[0].ad_type, AD_TYPE_FLAGS);
        assert_eq!(&parsed[0].data[..], &[0x06]);

        assert_eq!(parse_device_name(&parsed), Some("SatLink"));
        assert_eq!(parse_flags(&parsed), Some(0x06));

        let (cid, payload) = parse_manufacturer_data(&parsed).expect("mfg data");
        assert_eq!(cid, 0x004C);
        assert_eq!(payload, &[0x10]);
    }

    #[test]
    fn parse_invalid_length_returns_error() {
        // Declared length 10 but buffer only has 2 bytes
        let raw = [0x0A, 0x01];
        assert_eq!(parse_advertising_data(&raw), Err(AdParseError::InvalidLength));
    }

    #[test]
    fn parse_empty_buffer_or_zero_padding() {
        let raw = [0x00, 0x00, 0x00];
        let parsed = parse_advertising_data(&raw).expect("empty");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_16bit_services_list() {
        // AD type 0x03 with 2 UUIDs: 0x180F (Battery), 0x180A (Device Info)
        let raw = [
            0x05, 0x03, 0x0F, 0x18, 0x0A, 0x18,
        ];
        let parsed = parse_advertising_data(&raw).expect("services");
        let svcs = parse_16bit_services(&parsed);
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[0], 0x180F);
        assert_eq!(svcs[1], 0x180A);
    }
}
