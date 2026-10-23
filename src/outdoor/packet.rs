//! PositionPacket binary codec (34 bytes) for iPhone BLE GPS telemetry.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionError {
    InvalidLength,
    UnsupportedVersion,
    OutOfOrder,
    InvalidLatitude,
    InvalidLongitude,
}

#[derive(Debug, Clone, Copy)]
pub struct PositionPacket {
    pub version: u8,
    pub flags: u8,
    pub sequence: u32,
    pub timestamp_unix: i64,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub altitude_cm: i32,
    pub speed_cms: u16,
    pub course_cdeg: u16,
    pub accuracy_cm: u32,
}

impl PositionPacket {
    /// Encode packet to 34-byte array (useful for testing and loopback).
    pub fn encode(&self) -> [u8; 34] {
        let mut out = [0u8; 34];
        out[0] = self.version;
        out[1] = self.flags;
        out[2..6].copy_from_slice(&self.sequence.to_le_bytes());
        out[6..14].copy_from_slice(&self.timestamp_unix.to_le_bytes());
        out[14..18].copy_from_slice(&self.latitude_e7.to_le_bytes());
        out[18..22].copy_from_slice(&self.longitude_e7.to_le_bytes());
        out[22..26].copy_from_slice(&self.altitude_cm.to_le_bytes());
        out[26..28].copy_from_slice(&self.speed_cms.to_le_bytes());
        out[28..30].copy_from_slice(&self.course_cdeg.to_le_bytes());
        out[30..34].copy_from_slice(&self.accuracy_cm.to_le_bytes());
        out
    }
    /// Decode a 34-byte packet from BLE GATT write.
    pub fn decode(bytes: &[u8]) -> Result<Self, PositionError> {
        if bytes.len() != 34 {
            return Err(PositionError::InvalidLength);
        }
        if bytes[0] != 1 {
            return Err(PositionError::UnsupportedVersion);
        }
        let flags = bytes[1];
        let sequence = u32::from_le_bytes(bytes[2..6].try_into().unwrap());
        let timestamp_unix = i64::from_le_bytes(bytes[6..14].try_into().unwrap());
        let latitude_e7 = i32::from_le_bytes(bytes[14..18].try_into().unwrap());
        let longitude_e7 = i32::from_le_bytes(bytes[18..22].try_into().unwrap());
        let altitude_cm = i32::from_le_bytes(bytes[22..26].try_into().unwrap());
        let speed_cms = u16::from_le_bytes(bytes[26..28].try_into().unwrap());
        let course_cdeg = u16::from_le_bytes(bytes[28..30].try_into().unwrap());
        let accuracy_cm = u32::from_le_bytes(bytes[30..34].try_into().unwrap());

        let lat = latitude_e7 as f32 / 10_000_000.0;
        let lon = longitude_e7 as f32 / 10_000_000.0;
        if lat < -90.0 || lat > 90.0 {
            return Err(PositionError::InvalidLatitude);
        }
        if lon < -180.0 || lon > 180.0 {
            return Err(PositionError::InvalidLongitude);
        }

        Ok(Self {
            version: bytes[0],
            flags,
            sequence,
            timestamp_unix,
            latitude_e7,
            longitude_e7,
            altitude_cm,
            speed_cms,
            course_cdeg,
            accuracy_cm,
        })
    }

    pub fn coords_valid(&self) -> bool {
        self.flags & 0x01 != 0
    }
    pub fn altitude_valid(&self) -> bool {
        self.flags & 0x02 != 0
    }
    pub fn speed_valid(&self) -> bool {
        self.flags & 0x04 != 0
    }
    pub fn course_valid(&self) -> bool {
        self.flags & 0x08 != 0
    }
    pub fn accuracy_valid(&self) -> bool {
        self.flags & 0x10 != 0
    }

    pub fn latitude_deg(&self) -> f32 {
        self.latitude_e7 as f32 / 10_000_000.0
    }
    pub fn longitude_deg(&self) -> f32 {
        self.longitude_e7 as f32 / 10_000_000.0
    }
    pub fn altitude_m(&self) -> f32 {
        self.altitude_cm as f32 / 100.0
    }
    pub fn speed_mps(&self) -> f32 {
        self.speed_cms as f32 / 100.0
    }
    pub fn course_deg(&self) -> Option<f32> {
        if self.course_valid() {
            Some(self.course_cdeg as f32 / 100.0)
        } else {
            None
        }
    }
    pub fn accuracy_m(&self) -> Option<f32> {
        if self.accuracy_valid() {
            Some(self.accuracy_cm as f32 / 100.0)
        } else {
            None
        }
    }
}
