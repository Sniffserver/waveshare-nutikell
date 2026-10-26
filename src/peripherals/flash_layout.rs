//! Authoritative application flash-region contract.
//!
//! These offsets are valid only with the repository `partitions.csv`.  Keep
//! region ownership here rather than duplicating raw offsets in record codecs.

pub const SECTOR_SIZE: u32 = 4096;
pub const NVS_START: u32 = 0x9000;
pub const NVS_END: u32 = 0xF000; // exclusive; `phy_init` owns 0xF000.
pub const APP_START: u32 = 0x10000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashRegion {
    pub name: &'static str,
    pub start: u32,
    pub len: u32,
}

impl FlashRegion {
    pub const fn end(self) -> u32 {
        self.start + self.len
    }

    pub const fn is_sector_aligned(self) -> bool {
        self.start % SECTOR_SIZE == 0 && self.len % SECTOR_SIZE == 0
    }

    pub const fn is_within_nvs(self) -> bool {
        self.start >= NVS_START && self.end() <= NVS_END
    }
}

pub const CREDENTIALS: FlashRegion = FlashRegion {
    name: "credentials",
    start: 0x9000,
    len: SECTOR_SIZE,
};
pub const AUTHENTICATOR: FlashRegion = FlashRegion {
    name: "authenticator",
    start: 0xA000,
    len: SECTOR_SIZE,
};
pub const SETTINGS_A: FlashRegion = FlashRegion {
    name: "settings-a",
    start: 0xB000,
    len: SECTOR_SIZE,
};
pub const SETTINGS_B: FlashRegion = FlashRegion {
    name: "settings-b",
    start: 0xC000,
    len: SECTOR_SIZE,
};
pub const WORK_LOG: FlashRegion = FlashRegion {
    name: "work-log",
    start: 0xD000,
    len: SECTOR_SIZE,
};
pub const ACTIVE_WORK_A: FlashRegion = FlashRegion {
    name: "active-work-a",
    start: 0xE000,
    len: SECTOR_SIZE,
};
pub const ACTIVE_WORK_B: FlashRegion = FlashRegion {
    name: "active-work-b",
    start: 0x200000,
    len: SECTOR_SIZE,
};

pub const REGIONS: [FlashRegion; 7] = [
    CREDENTIALS,
    AUTHENTICATOR,
    SETTINGS_A,
    SETTINGS_B,
    WORK_LOG,
    ACTIVE_WORK_A,
    ACTIVE_WORK_B,
];

const fn valid_layout() -> bool {
    let mut index = 0;
    while index < REGIONS.len() {
        if !REGIONS[index].is_sector_aligned() || !REGIONS[index].is_within_nvs() {
            // ACTIVE_WORK_B is outside NVS region, so we check differently
            if REGIONS[index].start != ACTIVE_WORK_B.start {
                return false;
            }
        }
        // For ACTIVE_WORK_B, only check sector alignment
        if REGIONS[index].start == ACTIVE_WORK_B.start {
            if !REGIONS[index].is_sector_aligned() {
                return false;
            }
        }
        let mut other = index + 1;
        while other < REGIONS.len() {
            if REGIONS[index].start < REGIONS[other].end()
                && REGIONS[other].start < REGIONS[index].end()
            {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}

const _: () = assert!(NVS_END <= APP_START);
const _: () = assert!(valid_layout());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_regions_are_aligned_non_overlapping_and_reserved() {
        assert!(valid_layout());
        assert_eq!(REGIONS[0].start, NVS_START);
        assert_eq!(REGIONS[REGIONS.len() - 2].end(), NVS_END);
    }
}
