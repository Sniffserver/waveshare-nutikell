//! Step detector & motion classifier using QMI8658 accelerometer data.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionState {
    Still,
    Walking,
    Active,
}

impl Default for MotionState {
    fn default() -> Self {
        MotionState::Still
    }
}

pub struct StepDetector {
    pub steps: u32,
    pub filtered: f32,
    previous: f32,
    last_peak_ms: u64,
    last_step_ms: u64,
    threshold_high: f32,
    threshold_low: f32,
}

impl StepDetector {
    pub const fn new() -> Self {
        Self {
            steps: 0,
            filtered: 0.0,
            previous: 0.0,
            last_peak_ms: 0,
            last_step_ms: 0,
            threshold_high: 0.14,
            threshold_low: 0.07,
        }
    }

    /// Update with a 3-axis accelerometer sample in g and current timestamp in ms.
    /// Returns true if a step was confirmed on this sample.
    pub fn update(&mut self, ax_g: f32, ay_g: f32, az_g: f32, now_ms: u64) -> bool {
        let magnitude = libm::sqrtf(ax_g * ax_g + ay_g * ay_g + az_g * az_g);
        let dynamic = libm::fabsf(magnitude - 1.0);
        // Low-pass filter (IIR)
        self.filtered = self.filtered * 0.82 + dynamic * 0.18;
        let rising = self.filtered > self.previous;
        let falling = self.filtered < self.previous;
        let enough_time = now_ms.saturating_sub(self.last_step_ms) >= 280;
        let is_peak = self.previous > self.threshold_high && falling && enough_time && !rising;
        self.previous = self.filtered;

        if is_peak {
            self.steps = self.steps.saturating_add(1);
            self.last_peak_ms = now_ms;
            self.last_step_ms = now_ms;
            return true;
        }
        false
    }

    pub fn reset(&mut self) {
        self.steps = 0;
        self.filtered = 0.0;
        self.previous = 0.0;
        self.last_peak_ms = 0;
        self.last_step_ms = 0;
    }

    pub fn motion_state(&self) -> MotionState {
        if self.filtered < self.threshold_low {
            MotionState::Still
        } else if self.filtered < 0.35 {
            MotionState::Walking
        } else {
            MotionState::Active
        }
    }
}
