//! UI screen manager, event dispatcher, and dirty region tracking.

use crate::outdoor::state::OutdoorState;
use crate::peripherals::power::BatteryState;

// Standard 24-bit RGB color constants
pub const BLACK: u32 = 0x000000;
pub const WHITE: u32 = 0xFFFFFF;
pub const MUTED: u32 = 0x8A8A8A;
pub const GREEN: u32 = 0x40D889;
pub const RED: u32 = 0xFF5F56;
pub const ORANGE: u32 = 0xFFB454;
pub const PANEL: u32 = 0x161A20;

/// Generic display interface for drawing primitives on the AMOLED screen.
pub trait OutdoorDisplay {
    type Error;
    fn clear(&mut self, color: u32) -> Result<(), Self::Error>;
    fn text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        scale: u8,
    ) -> Result<(), Self::Error>;
    fn rect(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        color: u32,
    ) -> Result<(), Self::Error>;
    fn line(
        &mut self,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: u32,
    ) -> Result<(), Self::Error>;
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    Touch(TouchPoint),
    BleConnected,
    BleDisconnected,
    PositionUpdated,
    BatteryUpdated,
    Tick,
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

#[derive(Clone, Debug)]
pub struct AppState {
    pub screen: Screen,
    pub previous_screen: Option<Screen>,
    pub outdoor: OutdoorState,
    pub battery: BatteryState,
    pub dirty: DirtyRegions,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            screen: Screen::OutdoorHome,
            previous_screen: None,
            outdoor: OutdoorState::new(),
            battery: BatteryState::default(),
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

    pub fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::BleConnected => {
                self.outdoor.ble_connected = true;
                self.dirty.status = true;
            }
            UiEvent::BleDisconnected => {
                self.outdoor.ble_connected = false;
                self.dirty.status = true;
            }
            UiEvent::PositionUpdated => {
                self.dirty.status = true;
                self.dirty.distance = true;
                self.dirty.speed = true;
                self.dirty.course = true;
            }
            UiEvent::BatteryUpdated => {
                self.dirty.battery = true;
            }
            UiEvent::Touch(point) => {
                self.handle_touch(point);
            }
            UiEvent::Tick => {
                self.outdoor.fix_age_secs = self.outdoor.fix_age_secs.saturating_add(1);
                self.dirty.status = true;
            }
        }
    }

    pub fn handle_touch(&mut self, point: TouchPoint) {
        match self.screen {
            Screen::OutdoorHome => {
                if point.y > 400 {
                    if self.outdoor.is_hike_running() {
                        self.outdoor.pause_hike();
                    } else {
                        self.outdoor.start_hike();
                    }
                    self.dirty.controls = true;
                } else if point.y >= 340 && point.x < 205 {
                    self.switch_screen(Screen::Sun);
                } else if point.y >= 340 {
                    self.switch_screen(Screen::LinkDiagnostics);
                }
            }
            Screen::HikeLive => {
                if point.y > 420 {
                    self.outdoor.pause_hike();
                    self.dirty.controls = true;
                }
            }
            Screen::Sun | Screen::LinkDiagnostics | Screen::Level => {
                if point.y < 80 {
                    self.back();
                }
            }
            _ => {}
        }
    }
}
