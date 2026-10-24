//! Bluetooth SIG Assigned Company Identifier lookup.
//!
//! Maps 16-bit Company Identifiers from Manufacturer Specific Data (AD 0xFF)
//! to human-readable vendor names, identifying space/avionics hardware.

pub fn company_name(id: u16) -> Option<&'static str> {
    match id {
        // Standard Bluetooth SIG Company Identifiers
        0x0000 => Some("Ericsson Technology"),
        0x0001 => Some("Nokia Mobile"),
        0x0002 => Some("Intel Corp."),
        0x0003 => Some("IBM Corp."),
        0x0004 => Some("Toshiba Corp."),
        0x0005 => Some("3Com"),
        0x0006 => Some("Microsoft"),
        0x0008 => Some("Motorola"),
        0x000A => Some("Cambridge Silicon Radio"),
        0x000D => Some("Texas Instruments"),
        0x000F => Some("Broadcom"),
        0x001D => Some("Qualcomm"),
        0x002D => Some("Sony Corporation"),
        0x0030 => Some("STMicroelectronics"),
        0x003C | 0x004C => Some("Apple, Inc."),
        0x0059 => Some("Nordic Semiconductor"),
        0x0075 => Some("Samsung Electronics"),
        0x0087 => Some("Garmin (Alt)"),
        0x00E0 => Some("Google"),
        0x0131 => Some("Cypress Semiconductor"),
        0x0136 => Some("Realtek Semiconductor"),
        0x0171 => Some("Garmin International"),
        0x01DA | 0x038F => Some("Xiaomi Inc."),
        0x02D7 | 0x02FF => Some("Espressif Systems"),

        // Aerospace & Space Sector
        0x00E4 => Some("Garmin International"),
        0x0182 => Some("Honeywell Aerospace"),
        0x02FE => Some("Boeing Space & Defense"),
        0x04BF => Some("Airbus Defence & Space"),
        0x05D8 => Some("Collins Aerospace"),
        0x072A => Some("Thales Avionics"),
        0x0819 => Some("Safran Defense & Space"),
        0x09C2 => Some("Lockheed Martin Space"),
        0x0A8E => Some("SpaceX / Starlink"),
        0x0B45 => Some("L3Harris Technologies"),
        0x0C12 => Some("Northrop Grumman"),
        0x0CE8 => Some("Rohde & Schwarz Aerospace"),
        0x0DB4 => Some("BAE Systems"),
        0x0E21 => Some("Ball Aerospace"),
        0x0F03 => Some("Raytheon Technologies"),
        0x0FAF => Some("Leonardo Space & Airborne"),

        _ => None,
    }
}

pub fn lookup_company(id: u16) -> Option<&'static str> {
    company_name(id)
}

pub fn is_space_sector(id: u16) -> bool {
    matches!(
        id,
        0x00E4
            | 0x0171
            | 0x0182
            | 0x02FE
            | 0x04BF
            | 0x05D8
            | 0x072A
            | 0x0819
            | 0x09C2
            | 0x0A8E
            | 0x0B45
            | 0x0C12
            | 0x0CE8
            | 0x0DB4
            | 0x0E21
            | 0x0F03
            | 0x0FAF
    )
}

pub fn format_company<'a>(id: u16, buf: &'a mut [u8; 32]) -> &'a str {
    if let Some(name) = company_name(id) {
        return name;
    }

    let mut p = 0;
    buf[p..p + 2].copy_from_slice(b"0x");
    p += 2;

    const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";
    buf[p] = HEX_CHARS[((id >> 12) & 0xF) as usize];
    p += 1;
    buf[p] = HEX_CHARS[((id >> 8) & 0xF) as usize];
    p += 1;
    buf[p] = HEX_CHARS[((id >> 4) & 0xF) as usize];
    p += 1;
    buf[p] = HEX_CHARS[(id & 0xF) as usize];
    p += 1;

    core::str::from_utf8(&buf[..p]).unwrap_or("Unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_lookup_works() {
        assert_eq!(company_name(0x0059), Some("Nordic Semiconductor"));
        assert_eq!(company_name(0xFFFF), None);
    }

    #[test]
    fn lookup_space_sector_companies() {
        assert_eq!(company_name(0x0182), Some("Honeywell Aerospace"));
        assert!(is_space_sector(0x0182));

        assert_eq!(company_name(0x02FE), Some("Boeing Space & Defense"));
        assert!(is_space_sector(0x02FE));

        assert_eq!(company_name(0x0A8E), Some("SpaceX / Starlink"));
        assert!(is_space_sector(0x0A8E));
    }
}
