//! Step detector & motion classifier using QMI8658 accelerometer data.
//!
//! No `std` dependency — all float math uses `libm`. The `#[cfg(test)]`
//! block links against std so tests run on the host with `cargo test`.

// ── Types ──────────────────────────────────────────────────────────────────

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

impl Default for PeakState {
    fn default() -> Self {
        PeakState::Waiting
    }
}

// ── Configuration ──────────────────────────────────────────────────────────

/// All tunable parameters in one place — pass to `StepDetector::new()`.
#[derive(Clone, Copy, Debug)]
pub struct StepConfig {
    /// Low threshold: minimum dynamic signal to start a peak (g).
    pub threshold_low: f32,
    /// High threshold: signal must reach this to confirm a step (g).
    pub threshold_high: f32,
    /// Minimum time between two consecutive steps (ms).
    pub min_step_interval_ms: u64,
    /// IIR coefficient for the gravity tracking filter (0 < α ≤ 1).
    /// Smaller → slower adaptation. Typical: 0.04.
    pub gravity_alpha: f32,
    /// IIR coefficient for the dynamic-signal smoothing filter (0 < α ≤ 1).
    /// Smaller → smoother but more lag. Typical: 0.20.
    pub signal_alpha: f32,
}

impl Default for StepConfig {
    /// Tuned for slow–normal walking cadence (≈ 80–170 steps/min).
    ///
    /// If the watch counts wrist flicks as steps: raise thresholds to
    /// `(0.10, 0.20)` and `min_step_interval_ms` to `320`.
    ///
    /// If steps are missed on light walkers: lower to `(0.05, 0.11)` and
    /// `min_step_interval_ms` to `250`.
    fn default() -> Self {
        Self {
            threshold_low: 0.07,
            threshold_high: 0.14,
            min_step_interval_ms: 280,
            gravity_alpha: 0.04,
            signal_alpha: 0.20,
        }
    }
}

// ── Input sample ───────────────────────────────────────────────────────────

/// One accelerometer reading in g-units.
#[derive(Clone, Copy, Debug, Default)]
pub struct AccelSample {
    pub x_g: f32,
    pub y_g: f32,
    pub z_g: f32,
}

impl AccelSample {
    #[inline]
    pub fn magnitude(self) -> f32 {
        libm::sqrtf(self.x_g * self.x_g + self.y_g * self.y_g + self.z_g * self.z_g)
    }
}

// ── Detector ───────────────────────────────────────────────────────────────

pub struct StepDetector {
    steps: u32,
    /// Low-pass tracked gravity magnitude (g).
    gravity: f32,
    /// Low-pass filtered dynamic acceleration (g).
    dynamic: f32,
    previous_dynamic: f32,
    state: PeakState,
    last_step_ms: u64,
    config: StepConfig,
}

impl StepDetector {
    pub fn new(config: StepConfig) -> Self {
        Self {
            steps: 0,
            gravity: 1.0,
            dynamic: 0.0,
            previous_dynamic: 0.0,
            state: PeakState::Waiting,
            last_step_ms: 0,
            config,
        }
    }

    // ── Accessors ────────────────────────────────────────────────────────

    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// Current smoothed dynamic-acceleration signal (g).
    /// Useful for real-time signal visualisation or adaptive thresholding.
    pub fn dynamic_signal(&self) -> f32 {
        self.dynamic
    }

    pub fn config(&self) -> &StepConfig {
        &self.config
    }

    pub fn motion_state(&self) -> MotionState {
        if self.dynamic < self.config.threshold_low {
            MotionState::Still
        } else if self.dynamic < 0.35 {
            MotionState::Walking
        } else {
            MotionState::Active
        }
    }

    // ── Mutation ─────────────────────────────────────────────────────────

    pub fn reset(&mut self) {
        self.steps = 0;
        self.gravity = 1.0;
        self.dynamic = 0.0;
        self.previous_dynamic = 0.0;
        self.state = PeakState::Waiting;
        self.last_step_ms = 0;
    }

    pub fn set_config(&mut self, config: StepConfig) {
        self.config = config;
    }

    // ── Core update ──────────────────────────────────────────────────────

    /// Feed one accelerometer sample and the current monotonic timestamp (ms).
    /// Returns `true` on the falling edge of a confirmed peak (= one step).
    pub fn update(&mut self, sample: AccelSample, now_ms: u64) -> bool {
        let magnitude = sample.magnitude();

        // Gravity IIR tracker — very slow to track orientation changes.
        let ga = self.config.gravity_alpha;
        self.gravity = self.gravity * (1.0 - ga) + magnitude * ga;

        // Raw dynamic component: deviation from gravity.
        let raw_dynamic = libm::fabsf(magnitude - self.gravity);

        // Smooth dynamic signal.
        let sa = self.config.signal_alpha;
        self.dynamic = self.dynamic * (1.0 - sa) + raw_dynamic * sa;

        let rising = self.dynamic > self.previous_dynamic;
        let falling = self.dynamic < self.previous_dynamic;
        let elapsed = now_ms.saturating_sub(self.last_step_ms);
        let enough_time = elapsed >= self.config.min_step_interval_ms;

        let mut detected = false;

        match self.state {
            PeakState::Waiting => {
                if rising && self.dynamic >= self.config.threshold_low {
                    self.state = PeakState::Rising;
                }
            }
            PeakState::Rising => {
                if self.dynamic >= self.config.threshold_high {
                    self.state = PeakState::AboveThreshold;
                } else if falling {
                    // Peak too small — ignore and wait for the next one.
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
}

impl Default for StepDetector {
    fn default() -> Self {
        Self::new(StepConfig::default())
    }
}

// ── Unit tests (host, std allowed) ────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const WALKING_ENVELOPE: &[f32] = &[
        1.00, 1.02, 1.05, 1.10, 1.18, 1.28, 1.38, 1.38, 1.38, 1.30, 1.20, 1.10, 1.03, 1.00, 1.00,
    ];

    fn sample(magnitude: f32) -> AccelSample {
        AccelSample {
            x_g: 0.0,
            y_g: 0.0,
            z_g: magnitude,
        }
    }

    fn inject_step(d: &mut StepDetector, now: &mut u64) -> bool {
        let mut detected = false;
        for &m in WALKING_ENVELOPE {
            if d.update(sample(m), *now) {
                detected = true;
            }
            *now += 32;
        }
        detected
    }

    #[test]
    fn starts_with_zero_steps() {
        let d = StepDetector::default();
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
    }

    #[test]
    fn remains_still_for_constant_gravity() {
        let mut d = StepDetector::default();
        for i in 0..300 {
            let detected = d.update(sample(1.0), i * 32);
            assert!(!detected, "false step at i={i}");
        }
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
    }

    #[test]
    fn detects_single_peak() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        for _ in 0..50 {
            d.update(sample(1.0), now);
            now += 32;
        }
        let det = inject_step(&mut d, &mut now);
        assert!(det, "falling edge of confirmed peak should trigger step");
        assert_eq!(d.steps(), 1);
    }

    #[test]
    fn rejects_peaks_too_close_together() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        for _ in 0..50 {
            d.update(sample(1.0), now);
            now += 32;
        }
        // First step
        assert!(inject_step(&mut d, &mut now));
        assert_eq!(d.steps(), 1);

        // Immediate rapid chatter/vibration (< min_step_interval_ms = 280ms)
        let fast_chatter = [1.00, 1.38, 1.38, 1.00];
        let mut rapid_detected = false;
        for &m in &fast_chatter {
            if d.update(sample(m), now) {
                rapid_detected = true;
            }
            now += 15;
        }
        assert!(
            !rapid_detected,
            "chatter within refractory interval should be rejected"
        );
        assert_eq!(d.steps(), 1);
    }

    #[test]
    fn accepts_peaks_far_enough_apart() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        for _ in 0..50 {
            d.update(sample(1.0), now);
            now += 32;
        }
        assert!(inject_step(&mut d, &mut now));
        assert_eq!(d.steps(), 1);

        // Wait refractory period (> 280 ms)
        now += 400;
        assert!(inject_step(&mut d, &mut now));
        assert_eq!(d.steps(), 2);
    }

    #[test]
    fn reset_clears_steps_and_state() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        for _ in 0..50 {
            d.update(sample(1.0), now);
            now += 32;
        }
        assert!(inject_step(&mut d, &mut now));
        assert!(d.steps() > 0);

        d.reset();
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
        assert_eq!(d.dynamic_signal(), 0.0);
    }

    #[test]
    fn motion_state_active_above_threshold() {
        let mut d = StepDetector::default();
        // Warm up gravity at 1 g.
        for i in 0..50 {
            d.update(sample(1.0), i * 32);
        }
        // Inject a large jolt — dynamic signal should exceed 0.35 g.
        for i in 50..60 {
            d.update(sample(1.6), i * 32);
        }
        assert_eq!(d.motion_state(), MotionState::Active);
    }

    #[test]
    fn custom_config_raises_thresholds() {
        let config = StepConfig {
            threshold_low: 0.10,
            threshold_high: 0.20,
            min_step_interval_ms: 320,
            gravity_alpha: 0.04,
            signal_alpha: 0.20,
        };
        let mut d = StepDetector::new(config);
        let mut now = 0u64;
        for _ in 0..50 {
            d.update(sample(1.0), now);
            now += 32;
        }
        // A peak that would trigger default thresholds (0.07/0.14) but not
        // the custom ones (0.10/0.20).
        for &mag in &[1.00_f32, 1.05, 1.12, 1.10, 1.05, 1.00] {
            d.update(sample(mag), now);
            now += 32;
        }
        assert_eq!(
            d.steps(),
            0,
            "sub-threshold peak should not count with raised config"
        );
    }
}
