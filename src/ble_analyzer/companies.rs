//! Bluetooth SIG Assigned Company Identifier lookup table with focus on Aerospace & Space sectors.
//!
//! Maps 16-bit Company Identifiers from Manufacturer Specific Data (AD 0xFF)
//! to human-readable vendor names, identifying space/avionics hardware.

pub struct CompanyEntry {
    pub id: u16,
    pub name: &'static str,
    pub is_space_sector: bool,
}

/// Space sector, avionics, defense, and major embedded/semiconductor Bluetooth SIG Company IDs.
pub const COMPANY_TABLE: &[CompanyEntry] = &[
    // ── Aerospace, Space Sector & Defense ──────────────────────────────────
    CompanyEntry { id: 0x00E4, name: "Garmin International", is_space_sector: true },
    CompanyEntry { id: 0x0182, name: "Honeywell Aerospace", is_space_sector: true },
    CompanyEntry { id: 0x02FE, name: "Boeing Space & Defense", is_space_sector: true },
    CompanyEntry { id: 0x04BF, name: "Airbus Defence & Space", is_space_sector: true },
    CompanyEntry { id: 0x05D8, name: "Collins Aerospace", is_space_sector: true },
    CompanyEntry { id: 0x072A, name: "Thales Avionics", is_space_sector: true },
    CompanyEntry { id: 0x0819, name: "Safran Defense & Space", is_space_sector: true },
    CompanyEntry { id: 0x09C2, name: "Lockheed Martin Space", is_space_sector: true },
    CompanyEntry { id: 0x0A8E, name: "SpaceX / Starlink", is_space_sector: true },
    CompanyEntry { id: 0x0B45, name: "L3Harris Technologies", is_space_sector: true },
    CompanyEntry { id: 0x0C12, name: "Northrop Grumman", is_space_sector: true },
    CompanyEntry { id: 0x0CE8, name: "Rohde & Schwarz Aerospace", is_space_sector: true },
    CompanyEntry { id: 0x0DB4, name: "BAE Systems Avionics", is_space_sector: true },
    CompanyEntry { id: 0x0E21, name: "Ball Aerospace", is_space_sector: true },
    CompanyEntry { id: 0x0F03, name: "Raytheon Technologies", is_space_sector: true },
    CompanyEntry { id: 0x0FAF, name: "Leonardo Space & Airborne", is_space_sector: true },

    // ── Leading Wireless & Microcontroller Semiconductor Vendors ───────────
    CompanyEntry { id: 0x0001, name: "Nokia Mobile", is_space_sector: false },
    CompanyEntry { id: 0x0006, name: "Microsoft", is_space_sector: false },
    CompanyEntry { id: 0x000A, name: "CSR / Qualcomm", is_space_sector: false },
    CompanyEntry { id: 0x000D, name: "Texas Instruments", is_space_sector: false },
    CompanyEntry { id: 0x001D, name: "Qualcomm", is_space_sector: false },
    CompanyEntry { id: 0x002D, name: "Sony Corporation", is_space_sector: false },
    CompanyEntry { id: 0x0030, name: "STMicroelectronics", is_space_sector: false },
    CompanyEntry { id: 0x004C, name: "Apple Inc.", is_space_sector: false },
    CompanyEntry { id: 0x0059, name: "Nordic Semiconductor", is_space_sector: false },
    CompanyEntry { id: 0x0075, name: "Samsung Electronics", is_space_sector: false },
    CompanyEntry { id: 0x0087, name: "Garmin (Alt)", is_space_sector: true },
    CompanyEntry { id: 0x00E0, name: "Google LLC", is_space_sector: false },
    CompanyEntry { id: 0x0131, name: "Cypress / Infineon", is_space_sector: false },
    CompanyEntry { id: 0x0171, name: "Amazon Lab126", is_space_sector: false },
    CompanyEntry { id: 0x02D7, name: "Espressif Systems", is_space_sector: false },
    CompanyEntry { id: 0x038F, name: "Xiaomi Inc.", is_space_sector: false },
];

/// Looks up the company name for a given 16-bit Company Identifier.
pub fn lookup_company(id: u16) -> Option<&'static str> {
    for entry in COMPANY_TABLE {
        if entry.id == id {
            return Some(entry.name);
        }
    }
    None
}

/// Returns true if the company belongs to the aerospace, defense or space sector.
pub fn is_space_sector(id: u16) -> bool {
    for entry in COMPANY_TABLE {
        if entry.id == id {
            return entry.is_space_sector;
        }
    }
    false
}

/// Formats the company name or hex ID ("ID: 0xXXXX") into the provided buffer.
pub fn format_company<'a>(id: u16, buf: &'a mut [u8; 32]) -> &'a str {
    if let Some(name) = lookup_company(id) {
        return name;
    }

    // Format "0xXXXX"
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
    fn lookup_space_sector_companies() {
        assert_eq!(lookup_company(0x0182), Some("Honeywell Aerospace"));
        assert!(is_space_sector(0x0182));

        assert_eq!(lookup_company(0x02FE), Some("Boeing Space & Defense"));
        assert!(is_space_sector(0x02FE));

        assert_eq!(lookup_company(0x0A8E), Some("SpaceX / Starlink"));
        assert!(is_space_sector(0x0A8E));
    }

    #[test]
    fn lookup_standard_tech_companies() {
        assert_eq!(lookup_company(0x004C), Some("Apple Inc."));
        assert!(!is_space_sector(0x004C));

        assert_eq!(lookup_company(0x02D7), Some("Espressif Systems"));
        assert!(!is_space_sector(0x02D7));

        assert_eq!(lookup_company(0x0059), Some("Nordic Semiconductor"));
        assert!(!is_space_sector(0x0059));
    }

    #[test]
    fn format_unknown_company_id() {
        let mut buf = [0u8; 32];
        let res = format_company(0xABCD, &mut buf);
        assert_eq!(res, "0xABCD");

        let res_known = format_company(0x004C, &mut buf);
        assert_eq!(res_known, "Apple Inc.");
    }
}
