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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeakState {
    Waiting,
    Rising,
    AboveThreshold,
}

pub struct StepDetector {
    steps: u32,
    gravity: f32,
    dynamic: f32,
    previous_dynamic: f32,
    state: PeakState,
    last_step_ms: u64,
    threshold_low: f32,
    threshold_high: f32,
    min_step_interval_ms: u64,
}

impl StepDetector {
    pub const fn new() -> Self {
        Self {
            steps: 0,
            gravity: 1.0,
            dynamic: 0.0,
            previous_dynamic: 0.0,
            state: PeakState::Waiting,
            last_step_ms: 0,
            threshold_low: 0.07,
            threshold_high: 0.14,
            min_step_interval_ms: 280,
        }
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn reset(&mut self) {
        self.steps = 0;
        self.gravity = 1.0;
        self.dynamic = 0.0;
        self.previous_dynamic = 0.0;
        self.state = PeakState::Waiting;
        self.last_step_ms = 0;
    }

    pub fn configure(
        &mut self,
        threshold_low: f32,
        threshold_high: f32,
        min_step_interval_ms: u64,
    ) {
        self.threshold_low = threshold_low;
        self.threshold_high = threshold_high;
        self.min_step_interval_ms = min_step_interval_ms;
    }

    /// Update with raw accelerometer samples (in g) and current timestamp in ms.
    pub fn update(&mut self, ax_g: f32, ay_g: f32, az_g: f32, now_ms: u64) -> bool {
        let magnitude = libm::sqrtf(ax_g * ax_g + ay_g * ay_g + az_g * az_g);

        // Slow gravity vector magnitude tracking
        self.gravity = self.gravity * 0.96 + magnitude * 0.04;

        // Dynamic body movement acceleration
        let dynamic = libm::fabsf(magnitude - self.gravity);

        // Low-pass filter (IIR)
        self.dynamic = self.dynamic * 0.80 + dynamic * 0.20;

        let rising = self.dynamic > self.previous_dynamic;
        let falling = self.dynamic < self.previous_dynamic;
        let enough_time = now_ms.saturating_sub(self.last_step_ms) >= self.min_step_interval_ms;
        let mut detected = false;

        match self.state {
            PeakState::Waiting => {
                if rising && self.dynamic >= self.threshold_low {
                    self.state = PeakState::Rising;
                }
            }
            PeakState::Rising => {
                if self.dynamic >= self.threshold_high {
                    self.state = PeakState::AboveThreshold;
                } else if falling {
                    self.state = PeakState::Waiting;
                }
            }
            PeakState::AboveThreshold => {
                if falling && enough_time {
                    self.steps = self.steps.saturating_add(1);
                    self.last_step_ms = now_ms;
                    self.state = PeakState::Waiting;
                    detected = true;
                }
            }
        }

        self.previous_dynamic = self.dynamic;
        detected
    }

    pub fn motion_state(&self) -> MotionState {
        if self.dynamic < self.threshold_low {
            MotionState::Still
        } else if self.dynamic < 0.35 {
            MotionState::Walking
        } else {
            MotionState::Active
        }
    }
}
