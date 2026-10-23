// Standalone test binary — no ESP deps.

//! Step detector & motion classifier using QMI8658 accelerometer data.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionState { Still, Walking, Active }
impl Default for MotionState { fn default() -> Self { MotionState::Still } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeakState { Waiting, Rising, AboveThreshold }
impl Default for PeakState { fn default() -> Self { PeakState::Waiting } }

#[derive(Clone, Copy, Debug)]
pub struct StepConfig {
    pub threshold_low: f32,
    pub threshold_high: f32,
    pub min_step_interval_ms: u64,
    pub gravity_alpha: f32,
    pub signal_alpha: f32,
}
impl Default for StepConfig {
    fn default() -> Self {
        Self { threshold_low: 0.07, threshold_high: 0.14,
               min_step_interval_ms: 280, gravity_alpha: 0.04, signal_alpha: 0.20 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AccelSample { pub x_g: f32, pub y_g: f32, pub z_g: f32 }
impl AccelSample {
    pub fn magnitude(self) -> f32 {
        libm::sqrtf(self.x_g*self.x_g + self.y_g*self.y_g + self.z_g*self.z_g)
    }
}

pub struct StepDetector {
    steps: u32, gravity: f32, dynamic: f32, previous_dynamic: f32,
    state: PeakState, last_step_ms: u64, config: StepConfig,
}
impl StepDetector {
    pub fn new(config: StepConfig) -> Self {
        Self { steps: 0, gravity: 1.0, dynamic: 0.0, previous_dynamic: 0.0,
               state: PeakState::Waiting, last_step_ms: 0, config }
    }
    pub fn steps(&self) -> u32 { self.steps }
    pub fn dynamic_signal(&self) -> f32 { self.dynamic }
    pub fn motion_state(&self) -> MotionState {
        if self.dynamic < self.config.threshold_low { MotionState::Still }
        else if self.dynamic < 0.35 { MotionState::Walking }
        else { MotionState::Active }
    }
    pub fn reset(&mut self) {
        self.steps = 0; self.gravity = 1.0; self.dynamic = 0.0;
        self.previous_dynamic = 0.0; self.state = PeakState::Waiting; self.last_step_ms = 0;
    }
    pub fn set_config(&mut self, c: StepConfig) { self.config = c; }
    pub fn update(&mut self, s: AccelSample, now_ms: u64) -> bool {
        let mag = s.magnitude();
        let ga = self.config.gravity_alpha;
        self.gravity = self.gravity * (1.0 - ga) + mag * ga;
        let raw = libm::fabsf(mag - self.gravity);
        let sa = self.config.signal_alpha;
        self.dynamic = self.dynamic * (1.0 - sa) + raw * sa;
        let rising  = self.dynamic > self.previous_dynamic;
        let falling = self.dynamic < self.previous_dynamic;
        let enough  = now_ms.saturating_sub(self.last_step_ms) >= self.config.min_step_interval_ms;
        let mut det = false;
        match self.state {
            PeakState::Waiting => { if rising && self.dynamic >= self.config.threshold_low { self.state = PeakState::Rising; } }
            PeakState::Rising  => {
                if self.dynamic >= self.config.threshold_high { self.state = PeakState::AboveThreshold; }
                else if falling { self.state = PeakState::Waiting; }
            }
            PeakState::AboveThreshold => {
                if falling && enough {
                    self.steps = self.steps.saturating_add(1);
                    self.last_step_ms = now_ms;
                    self.state = PeakState::Waiting;
                    det = true;
                }
            }
        }
        self.previous_dynamic = self.dynamic;
        det
    }
}
impl Default for StepDetector { fn default() -> Self { Self::new(StepConfig::default()) } }

fn main() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(m: f32) -> AccelSample { AccelSample { x_g: 0.0, y_g: 0.0, z_g: m } }

    /// 200 samples @ 32 ms → 99.98% gravity convergence (α=0.04).
    fn warmup(d: &mut StepDetector, mag: f32, now: &mut u64) {
        for _ in 0..200 { d.update(z(mag), *now); *now += 32; }
    }

    /// Slow, clear walking peak; 15 samples × 32 ms = 480 ms total.
    /// Peak amplitude ~0.38 g above gravity → well above threshold_high=0.14.
    fn inject_peak(d: &mut StepDetector, now: &mut u64) -> bool {
        let env: &[f32] = &[
            1.00, 1.02, 1.05, 1.10, 1.18, 1.28, 1.38,
            1.38, 1.38,
            1.30, 1.20, 1.10, 1.03, 1.00, 1.00,
        ];
        let mut det = false;
        for &m in env { if d.update(z(m), *now) { det = true; } *now += 32; }
        det
    }

    /// Rapid peak: 6 samples × 16 ms = 96 ms — used to test debounce rejection.
    fn inject_fast_peak(d: &mut StepDetector, now: &mut u64) -> bool {
        let env: &[f32] = &[1.00, 1.20, 1.38, 1.38, 1.20, 1.00];
        let mut det = false;
        for &m in env { if d.update(z(m), *now) { det = true; } *now += 16; }
        det
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
            assert!(!d.update(z(1.0), i * 32), "false step at {i}");
        }
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
    }

    #[test]
    fn detects_single_peak() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);
        let det = inject_peak(&mut d, &mut now);
        assert!(det, "peak not detected; steps={}", d.steps());
        assert_eq!(d.steps(), 1);
    }

    #[test]
    fn rejects_peaks_too_close_together() {
        // Two fast peaks separated by only ~96 ms (well below 280 ms).
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);

        inject_fast_peak(&mut d, &mut now);     // first — should count
        let after_first = d.steps();

        // Immediately inject second fast peak (no gap).
        inject_fast_peak(&mut d, &mut now);     // second — debounce must block it

        assert_eq!(after_first, 1, "first peak must register");
        assert_eq!(d.steps(), 1, "second too-close peak must be rejected by debounce");
    }

    #[test]
    fn accepts_peaks_far_enough_apart() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);

        inject_peak(&mut d, &mut now);
        assert_eq!(d.steps(), 1);

        now += 500; // extra gap → total >> 280 ms
        inject_peak(&mut d, &mut now);
        assert_eq!(d.steps(), 2);
    }

    #[test]
    fn reset_clears_steps_and_state() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);
        inject_peak(&mut d, &mut now);
        assert!(d.steps() > 0, "need at least one step before reset");

        d.reset();
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
        assert_eq!(d.dynamic_signal(), 0.0);
    }

    #[test]
    fn motion_state_active_above_threshold() {
        // Active state occurs during the rising transient of a strong jolt.
        // gravity_alpha=0.04 tracks slowly, so for the first ~12 samples after
        // a 1.6 g step the raw dynamic signal exceeds 0.35 g → Active.
        // After ~25 samples gravity catches up and dynamic falls below threshold.
        // We assert that Active is observed at least once during the transient.
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);
        let mut seen_active = false;
        for _ in 0..20 {
            d.update(z(1.6), now);
            now += 32;
            if d.motion_state() == MotionState::Active {
                seen_active = true;
            }
        }
        assert!(seen_active,
            "Active never reached; final dynamic={:.4} g", d.dynamic_signal());
    }

    #[test]
    fn custom_config_raises_thresholds() {
        let config = StepConfig {
            threshold_low: 0.10, threshold_high: 0.20,
            min_step_interval_ms: 320, gravity_alpha: 0.04, signal_alpha: 0.20,
        };
        let mut d = StepDetector::new(config);
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);

        // Small peak (~0.12 g raw, smoothed → < 0.20 high threshold).
        for &m in &[1.00_f32, 1.03, 1.08, 1.12, 1.08, 1.03, 1.00] {
            d.update(z(m), now); now += 32;
        }
        assert_eq!(d.steps(), 0, "sub-threshold peak must not count with raised config");
    }
}
