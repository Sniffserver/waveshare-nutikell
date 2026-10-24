pub mod attitude;
pub mod battery;
pub mod ble;
pub mod packet;
pub mod power_policy;
pub mod solar;
pub mod state;
pub mod steps;
pub mod ui;

pub use attitude::{estimate_attitude, valid_gravity_sample, Attitude};
pub use battery::{battery_level, draw_battery_icon, BatteryState};
pub use ble::{handle_position_packet, reconnect_delay_secs, BleState, BleStatus};
pub use packet::{
    PositionError, PositionPacket, FLAG_ACCURACY_VALID, FLAG_ALTITUDE_VALID, FLAG_COORDS_VALID,
    FLAG_COURSE_VALID, FLAG_SPEED_VALID, POSITION_PACKET_LEN,
};
pub use power_policy::{can_enter_light_sleep, PowerMode, PowerPolicy};
pub use state::{
    haversine_distance_m, telemetry_state, HikeSession, HikeStatus, OutdoorState, TelemetryState,
};
pub use steps::{AccelSample, MotionState, StepConfig, StepDetector};
pub use ui::{
    AppState, DirtyRegions, OutdoorDisplay, Screen, TouchPoint, UiEvent, BLACK, GREEN, MUTED,
    ORANGE, PANEL, RED, WHITE,
};

pub use solar::{calculate_solar, SolarPosition};
