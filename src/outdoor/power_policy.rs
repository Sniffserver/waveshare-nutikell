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
