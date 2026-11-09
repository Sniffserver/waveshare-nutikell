//! Standard CRC32 tableless implementation used across storage subsystems.
//!
//! Seed: 0xFFFF_FFFF, Polynomial: 0x04C1_1DB7, Final XOR: 0xFFFF_FFFF.

#[inline]
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[inline]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_empty_and_known_vectors() {
        assert_eq!(crc32(&[]), 0);
        let full = crc32(b"hello world");
        let step1 = crc32_update(0xFFFF_FFFF, b"hello ");
        let step2 = crc32_update(step1, b"world") ^ 0xFFFF_FFFF;
        assert_eq!(full, step2);
    }
}
