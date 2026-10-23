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
    pub display_fps: u8,
    pub imu_hz: u8,
    pub ble_enabled: bool,
    pub wifi_enabled: bool,
    pub audio_enabled: bool,
}

impl PowerPolicy {
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
                imu_hz: 25,
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
pub fn can_enter_light_sleep(
    mode: PowerMode,
    screen_is_dimmed: bool,
    ble_connected: bool,
) -> bool {
    matches!(mode, PowerMode::Idle | PowerMode::Paused) && screen_is_dimmed && !ble_connected
}

/// Determine the active PowerMode based on screen level and hike activity.
pub fn determine_power_mode(
    screen_state: u8,
    hike_running: bool,
    hike_paused: bool,
) -> PowerMode {
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
        assert_eq!(nav.imu_hz, 25);
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
}
