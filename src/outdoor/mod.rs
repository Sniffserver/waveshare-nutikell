pub mod attitude;
pub mod battery;
pub mod ble;
pub mod packet;
pub mod power_policy;
pub mod solar;
pub mod state;
pub mod steps;
pub mod ui;

pub use attitude::estimate_attitude;
pub use ble::{BleState, BleStatus};
pub use power_policy::PowerPolicy;
pub use state::{HikeStatus, OutdoorState};
pub use steps::{AccelSample, StepDetector};
