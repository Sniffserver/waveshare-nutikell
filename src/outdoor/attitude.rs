//! Level & attitude estimation (artificial horizon) from accelerometer data.

#[derive(Clone, Copy, Debug, Default)]
pub struct Attitude {
    pub roll_deg: f32,
    pub pitch_deg: f32,
    pub valid: bool,
}

/// Calculate roll and pitch in degrees from 3-axis accelerometer readings in g.
pub fn estimate_attitude(ax: f32, ay: f32, az: f32) -> Attitude {
    let roll = libm::atan2f(ay, az) * (180.0 / core::f32::consts::PI);
    let denom = libm::sqrtf(ay * ay + az * az);
    let pitch = libm::atan2f(-ax, denom) * (180.0 / core::f32::consts::PI);
    Attitude {
        roll_deg: roll,
        pitch_deg: pitch,
        valid: valid_gravity_sample(ax, ay, az),
    }
}

/// Check if total acceleration magnitude is close to 1g (0.85g..=1.15g).
/// When moving vigorously, dynamic acceleration corrupts the gravity vector.
pub fn valid_gravity_sample(ax: f32, ay: f32, az: f32) -> bool {
    let magnitude = libm::sqrtf(ax * ax + ay * ay + az * az);
    (0.85..=1.15).contains(&magnitude)
}
