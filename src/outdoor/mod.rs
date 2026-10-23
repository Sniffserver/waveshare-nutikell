pub mod attitude;
pub mod packet;
pub mod state;
pub mod steps;

pub use attitude::{estimate_attitude, valid_gravity_sample, Attitude};
pub use packet::{
    PositionError, PositionPacket, FLAG_ACCURACY_VALID, FLAG_ALTITUDE_VALID, FLAG_COORDS_VALID,
    FLAG_COURSE_VALID, FLAG_SPEED_VALID, POSITION_PACKET_LEN,
};
pub use state::{haversine_distance_m, HikeSession, HikeStatus, OutdoorState};
pub use steps::{MotionState, StepDetector};
