// Comprehensive host-side test suite for sbie-watch 5-layer architecture.
// Uses embedded-hal-mock to test real I2C driver logic on host x86_64 without hardware.

use embedded_hal_mock::eh1::i2c::{Mock as I2cMock, Transaction as I2cTransaction};

// ── 1. QMI8658 Driver & Logic ──────────────────────────────────────────────

const QMI8658_ADDR: u8 = 0x6B;
const REG_WHO_AM_I: u8 = 0x00;
const REG_CTRL1: u8 = 0x02;
const REG_CTRL2: u8 = 0x03;
const REG_CTRL3: u8 = 0x04;
const REG_CTRL5: u8 = 0x06;
const REG_CTRL7: u8 = 0x08;
const REG_AX_L: u8 = 0x35;

pub struct Qmi8658Imu<I> {
    i2c: I,
    accel_scale: f32,
}

impl<I: embedded_hal::i2c::I2c> Qmi8658Imu<I> {
    pub fn new(i2c: I) -> Self {
        Self {
            i2c,
            accel_scale: 8.0 / 32768.0, // ±8g default
        }
    }

    pub fn release(self) -> I {
        self.i2c
    }

    pub fn read_chip_id(&mut self) -> Result<u8, I::Error> {
        let mut buf = [0u8; 1];
        self.i2c.write_read(QMI8658_ADDR, &[REG_WHO_AM_I], &mut buf)?;
        Ok(buf[0])
    }

    pub fn init_accel_only(&mut self) -> Result<bool, I::Error> {
        let mut buf = [0u8; 1];
        self.i2c.write_read(QMI8658_ADDR, &[REG_WHO_AM_I], &mut buf)?;
        if buf[0] != 0x05 {
            return Ok(false);
        }
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL7, 0x00])?;
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL1, 0x60])?;
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL2, 0x25])?;
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL3, 0x00])?;
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL5, 0x11])?;
        self.i2c.write(QMI8658_ADDR, &[REG_CTRL7, 0x01])?;
        Ok(true)
    }

    pub fn read_accel(&mut self) -> Result<(f32, f32, f32), I::Error> {
        let mut buf = [0u8; 6];
        self.i2c.write_read(QMI8658_ADDR, &[REG_AX_L], &mut buf)?;
        let x = i16::from_le_bytes([buf[0], buf[1]]) as f32 * self.accel_scale;
        let y = i16::from_le_bytes([buf[2], buf[3]]) as f32 * self.accel_scale;
        let z = i16::from_le_bytes([buf[4], buf[5]]) as f32 * self.accel_scale;
        Ok((x, y, z))
    }
}

// ── 2. AXP2101 Driver & Logic ──────────────────────────────────────────────

const AXP2101_ADDR: u8 = 0x34;
const REG_AXP_STATUS1: u8 = 0x00;
const REG_AXP_VBAT_H: u8 = 0x34;
const REG_AXP_VBAT_L: u8 = 0x35;
const REG_AXP_BAT_PERCENT: u8 = 0xA4;
const REG_AXP_CHG_STATUS: u8 = 0x01;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    pub connected: bool,
    pub voltage_mv: u16,
    pub percent: u8,
    pub charging: bool,
    pub vbus_present: bool,
}

pub struct Axp2101Power<I> {
    i2c: I,
}

impl<I: embedded_hal::i2c::I2c> Axp2101Power<I> {
    pub fn new(i2c: I) -> Self {
        Self { i2c }
    }

    pub fn release(self) -> I {
        self.i2c
    }

    pub fn get_battery_voltage(&mut self) -> Result<u16, I::Error> {
        let mut h = [0u8; 1];
        let mut l = [0u8; 1];
        self.i2c.write_read(AXP2101_ADDR, &[REG_AXP_VBAT_H], &mut h)?;
        self.i2c.write_read(AXP2101_ADDR, &[REG_AXP_VBAT_L], &mut l)?;
        let raw = ((h[0] as u16) << 8) | (l[0] as u16);
        Ok(raw & 0x3FFF)
    }

    pub fn get_battery_percent(&mut self) -> Result<u8, I::Error> {
        let mut p = [0u8; 1];
        self.i2c.write_read(AXP2101_ADDR, &[REG_AXP_BAT_PERCENT], &mut p)?;
        Ok(p[0])
    }

    pub fn is_charging(&mut self) -> Result<bool, I::Error> {
        let mut st = [0u8; 1];
        self.i2c.write_read(AXP2101_ADDR, &[REG_AXP_CHG_STATUS], &mut st)?;
        let chg = (st[0] >> 5) & 0x07;
        Ok(chg >= 1 && chg <= 3)
    }

    pub fn is_vbus_in(&mut self) -> Result<bool, I::Error> {
        let mut st = [0u8; 1];
        self.i2c.write_read(AXP2101_ADDR, &[REG_AXP_STATUS1], &mut st)?;
        Ok((st[0] & 0x20) != 0)
    }

    pub fn read_battery(&mut self) -> Result<BatteryState, I::Error> {
        let voltage_mv = self.get_battery_voltage().unwrap_or(0);
        let percent = self.get_battery_percent().unwrap_or(0);
        let charging = self.is_charging().unwrap_or(false);
        let vbus_present = self.is_vbus_in().unwrap_or(false);
        let connected = voltage_mv > 2000;
        Ok(BatteryState {
            connected,
            voltage_mv,
            percent,
            charging,
            vbus_present,
        })
    }
}

pub const fn battery_level(percent: u8) -> u8 {
    match percent {
        0..=5 => 0,
        6..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        71..=90 => 4,
        _ => 5,
    }
}

// ── 3. Power Policy Logic ──────────────────────────────────────────────────

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

pub fn can_enter_light_sleep(
    mode: PowerMode,
    screen_is_dimmed: bool,
    ble_connected: bool,
) -> bool {
    matches!(mode, PowerMode::Idle | PowerMode::Paused) && screen_is_dimmed && !ble_connected
}

// ── 4. BLE Reconnect Supervisor & Packet Logic ─────────────────────────────

pub const fn reconnect_delay_secs(attempt: u8) -> u64 {
    match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        _ => 30,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BleState {
    #[default]
    Disabled,
    Advertising,
    Connected,
    Disconnected,
    Backoff,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BleStatus {
    pub state: BleState,
    pub reconnect_attempt: u8,
    pub last_sequence: u32,
    pub packets_received: u32,
    pub packets_dropped: u32,
}

// ── 5. UI Screen Manager & Dirty Regions ───────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    OutdoorHome,
    HikeLive,
    Sun,
    LinkDiagnostics,
    Level,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirtyRegions {
    pub full: bool,
    pub header: bool,
    pub status: bool,
    pub distance: bool,
    pub speed: bool,
    pub steps: bool,
    pub course: bool,
    pub battery: bool,
    pub controls: bool,
}

impl DirtyRegions {
    pub fn all() -> Self {
        Self {
            full: true,
            header: true,
            status: true,
            distance: true,
            speed: true,
            steps: true,
            course: true,
            battery: true,
            controls: true,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn any(&self) -> bool {
        self.full
            || self.header
            || self.status
            || self.distance
            || self.speed
            || self.steps
            || self.course
            || self.battery
            || self.controls
    }
}

pub struct SimpleAppState {
    pub screen: Screen,
    pub previous_screen: Option<Screen>,
    pub dirty: DirtyRegions,
}

impl SimpleAppState {
    pub fn new() -> Self {
        Self {
            screen: Screen::OutdoorHome,
            previous_screen: None,
            dirty: DirtyRegions::all(),
        }
    }

    pub fn switch_screen(&mut self, next: Screen) {
        if self.screen == next {
            return;
        }
        self.previous_screen = Some(self.screen);
        self.screen = next;
        self.dirty = DirtyRegions::all();
    }

    pub fn back(&mut self) {
        if let Some(previous) = self.previous_screen {
            self.screen = previous;
            self.previous_screen = None;
            self.dirty = DirtyRegions::all();
        }
    }
}


// ── 6. Step Detector Logic ────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionState { Still, Walking, Active }
impl Default for MotionState { fn default() -> Self { MotionState::Still } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeakState { Waiting, Rising, AboveThreshold }

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
        Self { threshold_low: 0.07, threshold_high: 0.14, min_step_interval_ms: 280, gravity_alpha: 0.04, signal_alpha: 0.20 }
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
        Self { steps: 0, gravity: 1.0, dynamic: 0.0, previous_dynamic: 0.0, state: PeakState::Waiting, last_step_ms: 0, config }
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

// ── Unit & Mock Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── QMI8658 I2C Mock Tests ─────────────────────────────────────────────


    // ── StepDetector Tests ─────────────────────────────────────────────────

    fn z(m: f32) -> AccelSample { AccelSample { x_g: 0.0, y_g: 0.0, z_g: m } }
    fn warmup(d: &mut StepDetector, mag: f32, now: &mut u64) {
        for _ in 0..200 { d.update(z(mag), *now); *now += 32; }
    }
    fn inject_peak(d: &mut StepDetector, now: &mut u64) -> bool {
        let env: &[f32] = &[1.00, 1.02, 1.05, 1.10, 1.18, 1.28, 1.38, 1.38, 1.38, 1.30, 1.20, 1.10, 1.03, 1.00, 1.00];
        let mut det = false;
        for &m in env { if d.update(z(m), *now) { det = true; } *now += 32; }
        det
    }
    fn inject_fast_peak(d: &mut StepDetector, now: &mut u64) -> bool {
        let env: &[f32] = &[1.00, 1.20, 1.38, 1.38, 1.20, 1.00];
        let mut det = false;
        for &m in env { if d.update(z(m), *now) { det = true; } *now += 16; }
        det
    }

    #[test]
    fn step_detector_starts_zero() {
        let d = StepDetector::default();
        assert_eq!(d.steps(), 0);
        assert_eq!(d.motion_state(), MotionState::Still);
    }

    #[test]
    fn step_detector_detects_walking_peak() {
        let mut d = StepDetector::default();
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);
        let det = inject_peak(&mut d, &mut now);
        assert!(det);
        assert_eq!(d.steps(), 1);
    }

    #[test]
    fn step_detector_rejects_sub_threshold() {
        let mut d = StepDetector::new(StepConfig { threshold_low: 0.10, threshold_high: 0.20, min_step_interval_ms: 320, gravity_alpha: 0.04, signal_alpha: 0.20 });
        let mut now = 0u64;
        warmup(&mut d, 1.0, &mut now);
        for &m in &[1.00_f32, 1.03, 1.08, 1.12, 1.08, 1.03, 1.00] {
            d.update(z(m), now); now += 32;
        }
        assert_eq!(d.steps(), 0);
    }

    #[test]
    fn qmi_who_am_i_is_valid() {
        let expectations = [
            I2cTransaction::write_read(QMI8658_ADDR, vec![REG_WHO_AM_I], vec![0x05]),
        ];
        let i2c = I2cMock::new(&expectations);
        let mut imu = Qmi8658Imu::new(i2c);
        let id = imu.read_chip_id().unwrap();
        assert_eq!(id, 0x05);
        imu.release().done();
    }

    #[test]
    fn qmi_read_accel_reads_six_bytes_and_scales() {
        // In ±8g scale (8/32768 = 1/4096 g/LSB):
        // 0x1000 = 4096 LSB => exactly 1.0 g
        let raw_x: i16 = 4096;  // 1.0 g
        let raw_y: i16 = 0;     // 0.0 g
        let raw_z: i16 = -4096; // -1.0 g

        let mut data = Vec::new();
        data.extend_from_slice(&raw_x.to_le_bytes());
        data.extend_from_slice(&raw_y.to_le_bytes());
        data.extend_from_slice(&raw_z.to_le_bytes());

        let expectations = [
            I2cTransaction::write_read(QMI8658_ADDR, vec![REG_AX_L], data),
        ];
        let i2c = I2cMock::new(&expectations);
        let mut imu = Qmi8658Imu::new(i2c);
        let (x, y, z) = imu.read_accel().unwrap();
        assert!((x - 1.0).abs() < 0.001);
        assert!((y - 0.0).abs() < 0.001);
        assert!((z - (-1.0)).abs() < 0.001);
        imu.release().done();
    }

    #[test]
    fn qmi_init_accel_only_writes_expected_registers() {
        let expectations = [
            I2cTransaction::write_read(QMI8658_ADDR, vec![REG_WHO_AM_I], vec![0x05]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL7, 0x00]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL1, 0x60]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL2, 0x25]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL3, 0x00]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL5, 0x11]),
            I2cTransaction::write(QMI8658_ADDR, vec![REG_CTRL7, 0x01]),
        ];
        let i2c = I2cMock::new(&expectations);
        let mut imu = Qmi8658Imu::new(i2c);
        let ok = imu.init_accel_only().unwrap();
        assert!(ok);
        imu.release().done();
    }

    // ── AXP2101 I2C Mock Tests ─────────────────────────────────────────────

    #[test]
    fn axp2101_reads_battery_voltage_and_percent() {
        // Voltage ADC: 14-bit (0x1034 = 4148 mV)
        let high = (4148 >> 8) as u8;
        let low = (4148 & 0xFF) as u8;

        let expectations = [
            I2cTransaction::write_read(AXP2101_ADDR, vec![REG_AXP_VBAT_H], vec![high]),
            I2cTransaction::write_read(AXP2101_ADDR, vec![REG_AXP_VBAT_L], vec![low]),
            I2cTransaction::write_read(AXP2101_ADDR, vec![REG_AXP_BAT_PERCENT], vec![88]),
            // Charging status: bits [7:5] = 0b010 (charging)
            I2cTransaction::write_read(AXP2101_ADDR, vec![REG_AXP_CHG_STATUS], vec![0b01000000]),
            // VBUS status: bit 5 = 1 (VBUS connected)
            I2cTransaction::write_read(AXP2101_ADDR, vec![REG_AXP_STATUS1], vec![0x20]),
        ];
        let i2c = I2cMock::new(&expectations);
        let mut pmic = Axp2101Power::new(i2c);
        let bat = pmic.read_battery().unwrap();

        assert_eq!(bat.voltage_mv, 4148);
        assert_eq!(bat.percent, 88);
        assert!(bat.charging);
        assert!(bat.vbus_present);
        assert!(bat.connected);
        pmic.release().done();
    }

    #[test]
    fn battery_level_buckets() {
        assert_eq!(battery_level(0), 0);
        assert_eq!(battery_level(5), 0);
        assert_eq!(battery_level(6), 1);
        assert_eq!(battery_level(20), 1);
        assert_eq!(battery_level(25), 2);
        assert_eq!(battery_level(50), 3);
        assert_eq!(battery_level(85), 4);
        assert_eq!(battery_level(95), 5);
        assert_eq!(battery_level(100), 5);
    }

    // ── Power Policy Tests ─────────────────────────────────────────────────

    #[test]
    fn power_policy_configurations() {
        let active = PowerPolicy::for_mode(PowerMode::Active);
        assert_eq!(active.display_fps, 30);
        assert_eq!(active.imu_hz, 50);
        assert!(active.ble_enabled);
        assert!(active.audio_enabled);

        let nav = PowerPolicy::for_mode(PowerMode::Navigation);
        assert_eq!(nav.display_fps, 8);
        assert_eq!(nav.imu_hz, 25);
        assert!(nav.ble_enabled);
        assert!(!nav.audio_enabled);

        let idle = PowerPolicy::for_mode(PowerMode::Idle);
        assert_eq!(idle.display_fps, 1);
        assert_eq!(idle.imu_hz, 5);

        let sleeping = PowerPolicy::for_mode(PowerMode::Sleeping);
        assert_eq!(sleeping.display_fps, 0);
        assert_eq!(sleeping.imu_hz, 0);
        assert!(!sleeping.ble_enabled);
    }

    #[test]
    fn light_sleep_rules() {
        // Active mode can never sleep
        assert!(!can_enter_light_sleep(PowerMode::Active, true, false));
        assert!(!can_enter_light_sleep(PowerMode::Navigation, true, false));

        // Idle / Paused can sleep only when screen is dimmed AND ble is not connected
        assert!(can_enter_light_sleep(PowerMode::Idle, true, false));
        assert!(can_enter_light_sleep(PowerMode::Paused, true, false));

        // Screen awake prevents sleep
        assert!(!can_enter_light_sleep(PowerMode::Idle, false, false));

        // Active BLE connection prevents light sleep
        assert!(!can_enter_light_sleep(PowerMode::Idle, true, true));
    }

    // ── BLE Reconnect Backoff Tests ────────────────────────────────────────

    #[test]
    fn ble_reconnect_backoff_progression() {
        assert_eq!(reconnect_delay_secs(0), 1);
        assert_eq!(reconnect_delay_secs(1), 2);
        assert_eq!(reconnect_delay_secs(2), 4);
        assert_eq!(reconnect_delay_secs(3), 8);
        assert_eq!(reconnect_delay_secs(4), 16);
        assert_eq!(reconnect_delay_secs(5), 30);
        assert_eq!(reconnect_delay_secs(10), 30);
    }

    // ── UI Screen Manager Tests ────────────────────────────────────────────

    #[test]
    fn screen_navigation_and_history() {
        let mut app = SimpleAppState::new();
        assert_eq!(app.screen, Screen::OutdoorHome);

        app.switch_screen(Screen::Sun);
        assert_eq!(app.screen, Screen::Sun);
        assert_eq!(app.previous_screen, Some(Screen::OutdoorHome));
        assert!(app.dirty.any());

        app.back();
        assert_eq!(app.screen, Screen::OutdoorHome);
        assert_eq!(app.previous_screen, None);
    }

    #[test]
    fn dirty_regions_tracking() {
        let mut dirty = DirtyRegions::default();
        assert!(!dirty.any());

        dirty.distance = true;
        assert!(dirty.any());

        dirty.clear();
        assert!(!dirty.any());

        let all = DirtyRegions::all();
        assert!(all.any());
        assert!(all.status && all.battery && all.controls);
    }
}
