//! Outdoor state and telemetry aggregation.

use crate::outdoor::packet::{PositionError, PositionPacket};
use crate::outdoor::steps::MotionState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HikeStatus {
    Stopped,
    Running,
    Paused,
}

impl Default for HikeStatus {
    fn default() -> Self {
        HikeStatus::Stopped
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HikeSession {
    pub status: HikeStatus,
    pub start_time_secs: u64,
    pub elapsed_secs: u32,
    pub distance_m: f32,
    pub steps_start: u32,
    pub steps_session: u32,
    pub avg_speed_mps: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OutdoorState {
    pub ble_connected: bool,
    pub gps_valid: bool,
    pub fix_age_secs: u32,
    pub latitude_deg: f32,
    pub longitude_deg: f32,
    pub altitude_m: f32,
    pub speed_mps: f32,
    pub course_deg: Option<f32>,
    pub accuracy_m: Option<f32>,
    pub distance_m: f32,
    pub steps: u32,
    pub roll_deg: f32,
    pub pitch_deg: f32,
    pub level_valid: bool,
    pub imu_motion: MotionState,
    pub hike: HikeSession,
    pub last_sequence: u32,
    prev_lat: f32,
    prev_lon: f32,
    has_prev_coord: bool,
}

impl OutdoorState {
    pub const fn new() -> Self {
        Self {
            ble_connected: false,
            gps_valid: false,
            fix_age_secs: 0,
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            altitude_m: 0.0,
            speed_mps: 0.0,
            course_deg: None,
            accuracy_m: None,
            distance_m: 0.0,
            steps: 0,
            roll_deg: 0.0,
            pitch_deg: 0.0,
            level_valid: false,
            imu_motion: MotionState::Still,
            hike: HikeSession {
                status: HikeStatus::Stopped,
                start_time_secs: 0,
                elapsed_secs: 0,
                distance_m: 0.0,
                steps_start: 0,
                steps_session: 0,
                avg_speed_mps: 0.0,
            },
            last_sequence: 0,
            prev_lat: 0.0,
            prev_lon: 0.0,
            has_prev_coord: false,
        }
    }

    /// Process a newly received PositionPacket from BLE GATT write.
    pub fn handle_position_write(
        &mut self,
        data: &[u8],
        now_unix: i64,
    ) -> Result<(), PositionError> {
        let packet = PositionPacket::decode(data)?;
        if self.last_sequence != 0 && packet.sequence <= self.last_sequence {
            return Err(PositionError::OutOfOrder);
        }

        let new_lat = packet.latitude_deg();
        let new_lon = packet.longitude_deg();

        // Distance accumulation during running hike
        if self.hike.status == HikeStatus::Running && packet.coords_valid() {
            let accuracy_ok = packet.accuracy_m().map(|acc| acc <= 50.0).unwrap_or(false);
            if self.has_prev_coord {
                let segment = haversine_distance_m(self.prev_lat, self.prev_lon, new_lat, new_lon);
                // Reject unreasonable jumps (> 100m in one update) or inaccurate GPS fixes
                if accuracy_ok && segment >= 1.5 && segment <= 100.0 {
                    self.distance_m += segment;
                    self.hike.distance_m += segment;
                }
            }
            self.prev_lat = new_lat;
            self.prev_lon = new_lon;
            self.has_prev_coord = true;
        }

        self.ble_connected = true;
        self.gps_valid = packet.coords_valid();
        self.latitude_deg = new_lat;
        self.longitude_deg = new_lon;
        self.altitude_m = packet.altitude_m();
        self.speed_mps = packet.speed_mps();
        self.course_deg = packet.course_deg();
        self.accuracy_m = packet.accuracy_m();
        self.last_sequence = packet.sequence;

        if now_unix > 0 && packet.timestamp_unix > 0 {
            self.fix_age_secs =
                (now_unix.saturating_sub(packet.timestamp_unix)).clamp(0, u32::MAX as i64) as u32;
        } else {
            self.fix_age_secs = 0;
        }

        Ok(())
    }

    pub fn start_hike(&mut self) {
        self.hike.status = HikeStatus::Running;
        self.hike.distance_m = 0.0;
        self.hike.elapsed_secs = 0;
        self.hike.steps_start = self.steps;
        self.hike.steps_session = 0;
        self.hike.avg_speed_mps = 0.0;
        self.has_prev_coord = false;
    }

    pub fn pause_hike(&mut self) {
        if self.hike.status == HikeStatus::Running {
            self.hike.status = HikeStatus::Paused;
            self.has_prev_coord = false;
        } else if self.hike.status == HikeStatus::Paused {
            self.hike.status = HikeStatus::Running;
        }
    }

    pub fn stop_hike(&mut self) {
        self.hike.status = HikeStatus::Stopped;
        self.has_prev_coord = false;
    }

    /// Periodic update (called every 1 second).
    pub fn tick_1s(&mut self) {
        if self.ble_connected {
            self.fix_age_secs = self.fix_age_secs.saturating_add(1);
            if self.fix_age_secs > 10 {
                self.gps_valid = false;
            }
            if self.fix_age_secs > 30 {
                self.ble_connected = false;
            }
        }

        if self.hike.status == HikeStatus::Running {
            self.hike.elapsed_secs = self.hike.elapsed_secs.saturating_add(1);
            self.hike.steps_session = self.steps.saturating_sub(self.hike.steps_start);
            if self.hike.elapsed_secs > 0 {
                self.hike.avg_speed_mps = self.hike.distance_m / (self.hike.elapsed_secs as f32);
            }
        }
    }
}

/// Great-circle distance between two GPS coordinates in meters using haversine formula.
pub fn haversine_distance_m(lat1: f32, lon1: f32, lat2: f32, lon2: f32) -> f32 {
    let r = 6_371_000.0; // Earth radius in meters
    let to_rad = core::f32::consts::PI / 180.0;
    let dlat = (lat2 - lat1) * to_rad;
    let dlon = (lon2 - lon1) * to_rad;
    let lat1_rad = lat1 * to_rad;
    let lat2_rad = lat2 * to_rad;

    let sin_dlat2 = libm::sinf(dlat * 0.5);
    let sin_dlon2 = libm::sinf(dlon * 0.5);

    let a =
        sin_dlat2 * sin_dlat2 + libm::cosf(lat1_rad) * libm::cosf(lat2_rad) * sin_dlon2 * sin_dlon2;
    let a_clamped = if a < 0.0 {
        0.0
    } else if a > 1.0 {
        1.0
    } else {
        a
    };
    let c = 2.0 * libm::atan2f(libm::sqrtf(a_clamped), libm::sqrtf(1.0 - a_clamped));
    r * c
}
