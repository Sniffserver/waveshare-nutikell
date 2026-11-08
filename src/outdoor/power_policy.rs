//! Power policy states and light-sleep eligibility for sbie-watch.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    Active,
    Navigation,
    Idle,
    Paused,
    Sleeping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerPolicy {
    pub mode: PowerMode,
    /// Maximum display refresh rate requested by this power mode.
    pub display_fps: u8,
    /// Maximum IMU sample processing rate before foreground consumers override it.
    pub imu_hz: u8,
    pub ble_enabled: bool,
    pub wifi_enabled: bool,
    pub audio_enabled: bool,
}

impl PowerPolicy {
    const fn interval_ms(rate_hz: u8) -> Option<u64> {
        if rate_hz == 0 {
            None
        } else {
            Some(1000u64.div_ceil(rate_hz as u64))
        }
    }

    pub const fn display_interval_ms(self) -> Option<u64> {
        Self::interval_ms(self.display_fps)
    }

    /// Resolve the display cap against the interval required by the workload.
    ///
    /// ## Separation of Timing Concerns
    /// * `effective_display_interval_ms`: defines the **maximum display refresh rate ceiling**
    ///   (frame limiter / render interval cap) enforced to protect the AMOLED power budget per mode.
    /// * Loop `tick`: defines the **minimum wake / data processing cadence** calculated in `main.rs`
    ///   for input polling, sensor sampling, and event dispatch.
    ///
    /// Invariants:
    /// 1. Interactive inputs (`touch_held || button_held`) wake at ~60 Hz (16 ms) without waiting for display flushes.
    /// 2. IMU consumer minimum frequency requirement (`imu_interval_ms(consumer_min_hz)`) is never reduced below consumer needs.
    /// 3. Game frame rate ceiling is not degraded below 30 FPS (`effective_display_interval_ms(33)`).
    pub const fn effective_display_interval_ms(self, workload_interval_ms: u64) -> u64 {
        match self.display_interval_ms() {
            Some(policy_interval_ms) if policy_interval_ms > workload_interval_ms => {
                policy_interval_ms
            }
            _ => workload_interval_ms,
        }
    }

    pub const fn imu_interval_ms(self, consumer_min_hz: u8) -> Option<u64> {
        let effective_hz = if consumer_min_hz > self.imu_hz {
            consumer_min_hz
        } else {
            self.imu_hz
        };
        Self::interval_ms(effective_hz)
    }

    pub const fn for_mode(mode: PowerMode) -> Self {
        match mode {
            PowerMode::Active => Self {
                mode,
                display_fps: 30,
                imu_hz: 50,
                ble_enabled: true,
                wifi_enabled: false,
                audio_enabled: true,
            },
            PowerMode::Navigation => Self {
                mode,
                display_fps: 8,
                imu_hz: 20,
                ble_enabled: true,
                wifi_enabled: false,
                audio_enabled: false,
            },
            PowerMode::Idle => Self {
                mode,
                display_fps: 1,
                imu_hz: 5,
                ble_enabled: true,
                wifi_enabled: false,
                audio_enabled: false,
            },
            PowerMode::Paused => Self {
                mode,
                display_fps: 1,
                imu_hz: 0,
                ble_enabled: true,
                wifi_enabled: false,
                audio_enabled: false,
            },
            PowerMode::Sleeping => Self {
                mode,
                display_fps: 0,
                imu_hz: 0,
                ble_enabled: false,
                wifi_enabled: false,
                audio_enabled: false,
            },
        }
    }
}

/// Determine whether the watch can safely enter light-sleep.
///
/// Light sleep should NOT be entered when:
/// - Hike Live needs an active display
/// - BLE connection is recovering / active
/// - Touch interaction is occurring
/// - QMI8658 continuous high-rate step detection is required
pub fn can_enter_light_sleep(mode: PowerMode, screen_is_dimmed: bool, ble_connected: bool) -> bool {
    matches!(mode, PowerMode::Idle | PowerMode::Paused) && screen_is_dimmed && !ble_connected
}

/// Determine the active PowerMode based on screen level and hike activity.
pub fn determine_power_mode(screen_state: u8, hike_running: bool, hike_paused: bool) -> PowerMode {
    if hike_running {
        PowerMode::Navigation
    } else if hike_paused {
        PowerMode::Paused
    } else if screen_state >= 2 {
        PowerMode::Active
    } else if screen_state == 1 {
        PowerMode::Idle
    } else {
        PowerMode::Sleeping
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_policy_configurations() {
        let active = PowerPolicy::for_mode(PowerMode::Active);
        assert_eq!(active.display_fps, 30);
        assert_eq!(active.imu_hz, 50);
        assert!(active.ble_enabled);
        assert!(!active.wifi_enabled);
        assert!(active.audio_enabled);

        let nav = PowerPolicy::for_mode(PowerMode::Navigation);
        assert_eq!(nav.display_fps, 8);
        assert_eq!(nav.imu_hz, 20);
        assert!(nav.ble_enabled);
        assert!(!nav.audio_enabled);

        let idle = PowerPolicy::for_mode(PowerMode::Idle);
        assert_eq!(idle.display_fps, 1);
        assert_eq!(idle.imu_hz, 5);

        let paused = PowerPolicy::for_mode(PowerMode::Paused);
        assert_eq!(paused.display_fps, 1);
        assert_eq!(paused.imu_hz, 0);

        let sleeping = PowerPolicy::for_mode(PowerMode::Sleeping);
        assert_eq!(sleeping.display_fps, 0);
        assert_eq!(sleeping.imu_hz, 0);
        assert!(!sleeping.ble_enabled);
    }

    #[test]
    fn light_sleep_conditions() {
        // Active and Navigation cannot enter light sleep
        assert!(!can_enter_light_sleep(PowerMode::Active, true, false));
        assert!(!can_enter_light_sleep(PowerMode::Navigation, true, false));

        // Awake screen prevents sleep
        assert!(!can_enter_light_sleep(PowerMode::Idle, false, false));
        assert!(!can_enter_light_sleep(PowerMode::Paused, false, false));

        // Active BLE connection prevents sleep (need to service radio packets)
        assert!(!can_enter_light_sleep(PowerMode::Idle, true, true));

        // Idle or Paused with dimmed/off screen and no active BLE connection can sleep
        assert!(can_enter_light_sleep(PowerMode::Idle, true, false));
        assert!(can_enter_light_sleep(PowerMode::Paused, true, false));
    }

    #[test]
    fn power_mode_resolution() {
        assert_eq!(determine_power_mode(3, false, false), PowerMode::Active);
        assert_eq!(determine_power_mode(2, false, false), PowerMode::Active);
        assert_eq!(determine_power_mode(1, false, false), PowerMode::Idle);
        assert_eq!(determine_power_mode(0, false, false), PowerMode::Sleeping);

        // Hike status takes precedence over screen level
        assert_eq!(determine_power_mode(0, true, false), PowerMode::Navigation);
        assert_eq!(determine_power_mode(3, true, false), PowerMode::Navigation);
        assert_eq!(determine_power_mode(0, false, true), PowerMode::Paused);
    }

    #[test]
    fn policy_rates_resolve_to_maximum_intervals() {
        let active = PowerPolicy::for_mode(PowerMode::Active);
        let nav = PowerPolicy::for_mode(PowerMode::Navigation);
        assert_eq!(active.display_interval_ms(), Some(34));
        assert_eq!(active.imu_interval_ms(0), Some(20));
        assert_eq!(active.imu_interval_ms(60), Some(17));

        let sleeping = PowerPolicy::for_mode(PowerMode::Sleeping);
        assert_eq!(sleeping.display_interval_ms(), None);
        assert_eq!(sleeping.imu_interval_ms(0), None);
        assert_eq!(sleeping.imu_interval_ms(10), Some(100));

        assert_eq!(active.effective_display_interval_ms(33), 34);
        assert_eq!(active.effective_display_interval_ms(100), 100);
        assert_eq!(nav.effective_display_interval_ms(50), 125);
        assert_eq!(sleeping.effective_display_interval_ms(33), 33);
    }

    #[test]
    fn effective_timing_combinations_preserve_invariants() {
        let active = PowerPolicy::for_mode(PowerMode::Active);
        let nav = PowerPolicy::for_mode(PowerMode::Navigation);
        let idle = PowerPolicy::for_mode(PowerMode::Idle);
        let paused = PowerPolicy::for_mode(PowerMode::Paused);

        // Invariant 1: Interactive touch workload (16 ms) display flush is capped at active display interval (34 ms)
        assert_eq!(active.effective_display_interval_ms(16), 34);

        // Invariant 2: Game workload (33 ms) achieves ~30 FPS (34 ms interval) in Active mode
        assert_eq!(active.effective_display_interval_ms(33), 34);

        // Invariant 3: Navigation mode throttles display to 8 FPS (125 ms) even if workload is 33 ms or 100 ms
        assert_eq!(nav.effective_display_interval_ms(33), 125);
        assert_eq!(nav.effective_display_interval_ms(100), 125);

        // Invariant 4: Idle / Paused throttles display to 1 FPS (1000 ms)
        assert_eq!(idle.effective_display_interval_ms(100), 1000);
        assert_eq!(paused.effective_display_interval_ms(100), 1000);

        // Invariant 5: IMU consumer min frequency requirement is preserved across modes
        // Step detector requires at least 20 Hz (50 ms):
        assert_eq!(active.imu_interval_ms(20), Some(20)); // Active runs faster (50 Hz / 20 ms)
        assert_eq!(nav.imu_interval_ms(20), Some(50)); // Nav meets consumer requirement (20 Hz / 50 ms)
        assert_eq!(idle.imu_interval_ms(20), Some(50)); // Idle boosted to consumer requirement (20 Hz)
        assert_eq!(paused.imu_interval_ms(20), Some(50)); // Paused boosted to consumer requirement (20 Hz)
    }

    #[test]
    fn effective_timing_combinations_preserve_invariants() {
        let active = PowerPolicy::for_mode(PowerMode::Active);
        let nav = PowerPolicy::for_mode(PowerMode::Navigation);
        let idle = PowerPolicy::for_mode(PowerMode::Idle);
        let paused = PowerPolicy::for_mode(PowerMode::Paused);

        // Invariant 1: Interactive touch workload (16 ms) display flush is capped at active display interval (34 ms)
        assert_eq!(active.effective_display_interval_ms(16), 34);

        // Invariant 2: Game workload (33 ms) achieves ~30 FPS (34 ms interval) in Active mode
        assert_eq!(active.effective_display_interval_ms(33), 34);

        // Invariant 3: Navigation mode throttles display to 8 FPS (125 ms) even if workload is 33 ms or 100 ms
        assert_eq!(nav.effective_display_interval_ms(33), 125);
        assert_eq!(nav.effective_display_interval_ms(100), 125);

        // Invariant 4: Idle / Paused throttles display to 1 FPS (1000 ms)
        assert_eq!(idle.effective_display_interval_ms(100), 1000);
        assert_eq!(paused.effective_display_interval_ms(100), 1000);

        // Invariant 5: IMU consumer min frequency requirement is preserved across modes
        // Step detector requires at least 20 Hz (50 ms):
        assert_eq!(active.imu_interval_ms(20), Some(20)); // Active runs faster (50 Hz / 20 ms)
        assert_eq!(nav.imu_interval_ms(20), Some(50));    // Nav meets consumer requirement (20 Hz / 50 ms)
        assert_eq!(idle.imu_interval_ms(20), Some(50));   // Idle boosted to consumer requirement (20 Hz)
        assert_eq!(paused.imu_interval_ms(20), Some(50)); // Paused boosted to consumer requirement (20 Hz)
    }
}
