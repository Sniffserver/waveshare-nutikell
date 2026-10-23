//! PositionPacket binary codec (34 bytes) for iPhone BLE GPS telemetry.

pub const POSITION_PACKET_LEN: usize = 34;
pub const FLAG_COORDS_VALID: u8 = 1 << 0;
pub const FLAG_ALTITUDE_VALID: u8 = 1 << 1;
pub const FLAG_SPEED_VALID: u8 = 1 << 2;
pub const FLAG_COURSE_VALID: u8 = 1 << 3;
pub const FLAG_ACCURACY_VALID: u8 = 1 << 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionError {
    InvalidLength,
    UnsupportedVersion,
    OutOfOrder,
    InvalidLatitude,
    InvalidLongitude,
    InvalidCourse,
    InvalidAccuracy,
}

#[derive(Clone, Copy, Debug)]
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
    /// Decode a 34-byte packet from BLE GATT write.
    pub fn decode(data: &[u8]) -> Result<Self, PositionError> {
        if data.len() != POSITION_PACKET_LEN {
            return Err(PositionError::InvalidLength);
        }
        if data[0] != 1 {
            return Err(PositionError::UnsupportedVersion);
        }

        let packet = Self {
            version: data[0],
            flags: data[1],
            sequence: u32::from_le_bytes([data[2], data[3], data[4], data[5]]),
            timestamp_unix: i64::from_le_bytes([
                data[6], data[7], data[8], data[9], data[10], data[11], data[12], data[13],
            ]),
            latitude_e7: i32::from_le_bytes([data[14], data[15], data[16], data[17]]),
            longitude_e7: i32::from_le_bytes([data[18], data[19], data[20], data[21]]),
            altitude_cm: i32::from_le_bytes([data[22], data[23], data[24], data[25]]),
            speed_cms: u16::from_le_bytes([data[26], data[27]]),
            course_cdeg: u16::from_le_bytes([data[28], data[29]]),
            accuracy_cm: u32::from_le_bytes([data[30], data[31], data[32], data[33]]),
        };

        let lat = packet.latitude_deg();
        let lon = packet.longitude_deg();
        if !(-90.0..=90.0).contains(&lat) {
            return Err(PositionError::InvalidLatitude);
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(PositionError::InvalidLongitude);
        }
        if packet.flags & FLAG_COURSE_VALID != 0 && packet.course_cdeg >= 36_000 {
            return Err(PositionError::InvalidCourse);
        }
        if packet.flags & FLAG_ACCURACY_VALID != 0 && packet.accuracy_cm == 0 {
            return Err(PositionError::InvalidAccuracy);
        }

        Ok(packet)
    }

    /// Encode packet to 34-byte array (useful for testing, companion app verification, or loopback).
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

    pub fn coords_valid(&self) -> bool {
        self.flags & FLAG_COORDS_VALID != 0
    }
    pub fn altitude_valid(&self) -> bool {
        self.flags & FLAG_ALTITUDE_VALID != 0
    }
    pub fn speed_valid(&self) -> bool {
        self.flags & FLAG_SPEED_VALID != 0
    }
    pub fn course_valid(&self) -> bool {
        self.flags & FLAG_COURSE_VALID != 0
    }
    pub fn accuracy_valid(&self) -> bool {
        self.flags & FLAG_ACCURACY_VALID != 0
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
