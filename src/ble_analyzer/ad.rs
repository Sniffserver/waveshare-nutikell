//! BLE Advertising Data (AD) parser for TrouBLE & Embassy BLE stack.

use heapless::{String, Vec};

pub const MAX_AD_STRUCTURES: usize = 4;
pub const MAX_AD_PAYLOAD: usize = 31;
pub const MAX_NAME_LEN: usize = 32;
pub const MAX_UUIDS: usize = 6;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvertisementInfo {
    pub name: Option<String<MAX_NAME_LEN>>,
    pub company_id: Option<u16>,
    pub service_uuids_16: Vec<u16, MAX_UUIDS>,
}

pub fn parse_advertising_data(
    bytes: &[u8],
) -> Result<Vec<AdStructure, MAX_AD_STRUCTURES>, AdParseError> {
    let mut output = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        let length = bytes[offset] as usize;

        if length == 0 {
            break;
        }

        let end = offset
            .checked_add(1 + length)
            .ok_or(AdParseError::InvalidLength)?;

        if end > bytes.len() || length < 1 {
            return Err(AdParseError::InvalidLength);
        }

        let ad_type = bytes[offset + 1];
        let payload = &bytes[offset + 2..end];

        if payload.len() > MAX_AD_PAYLOAD {
            return Err(AdParseError::PayloadTooLarge);
        }

        let mut data = Vec::new();
        data.extend_from_slice(payload)
            .map_err(|_| AdParseError::PayloadTooLarge)?;

        output
            .push(AdStructure { ad_type, data })
            .map_err(|_| AdParseError::TooManyStructures)?;

        offset = end;
    }

    Ok(output)
}

pub fn extract_ad_info(structures: &[AdStructure]) -> AdvertisementInfo {
    let mut result = AdvertisementInfo::default();

    for structure in structures {
        match structure.ad_type {
            // Shortened local name
            0x08 => {
                if result.name.is_none() {
                    if let Ok(s) = core::str::from_utf8(&structure.data) {
                        let mut name = String::<MAX_NAME_LEN>::new();
                        let _ = name.push_str(s);
                        result.name = Some(name);
                    }
                }
            }
            // Complete local name (takes precedence)
            0x09 => {
                if let Ok(s) = core::str::from_utf8(&structure.data) {
                    let mut name = String::<MAX_NAME_LEN>::new();
                    let _ = name.push_str(s);
                    result.name = Some(name);
                }
            }
            // 16-bit Service UUIDs (incomplete or complete)
            0x02 | 0x03 => {
                for chunk in structure.data.chunks_exact(2) {
                    let uuid = u16::from_le_bytes([chunk[0], chunk[1]]);
                    if !result.service_uuids_16.contains(&uuid) {
                        let _ = result.service_uuids_16.push(uuid);
                    }
                }
            }
            // Manufacturer Specific Data
            0xFF => {
                if structure.data.len() >= 2 {
                    result.company_id =
                        Some(u16::from_le_bytes([structure.data[0], structure.data[1]]));
                }
            }
            _ => {}
        }
    }

    result
}

pub fn explain_ad_type(ad_type: u8) -> &'static str {
    match ad_type {
        0x01 => "Flags",
        0x02 => "Incomplete 16-bit UUIDs",
        0x03 => "Complete 16-bit UUIDs",
        0x04 => "Incomplete 32-bit UUIDs",
        0x05 => "Complete 32-bit UUIDs",
        0x06 => "Incomplete 128-bit UUIDs",
        0x07 => "Complete 128-bit UUIDs",
        0x08 => "Shortened Local Name",
        0x09 => "Complete Local Name",
        0x0A => "TX Power",
        0x16 => "Service Data 16-bit UUID",
        0x20 => "Service Data 32-bit UUID",
        0x21 => "Service Data 128-bit UUID",
        0xFF => "Manufacturer Specific Data",
        _ => "Unknown AD Type",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_company() {
        let bytes = [
            2, 0x01, 0x06, 5, 0x09, b'T', b'e', b's', b't', 4, 0xFF, 0x4C, 0x00, 0x01,
        ];
        let ad = parse_advertising_data(&bytes).unwrap();
        let info = extract_ad_info(&ad);
        assert_eq!(info.name.unwrap().as_str(), "Test");
        assert_eq!(info.company_id, Some(0x004C));
    }

    #[test]
    fn rejects_truncated_structure() {
        let bytes = [5, 0x09, b'A'];
        assert_eq!(
            parse_advertising_data(&bytes),
            Err(AdParseError::InvalidLength)
        );
    }
}
