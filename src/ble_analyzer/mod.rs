//! BLE Analyzer & Read-Only GATT Explorer module for TrouBLE / Embassy.

pub mod ad;
pub mod companies;
pub mod gatt;
pub mod tracker;
pub mod ui;

use ad::parse_advertising_data;
use tracker::{BdAddr, DeviceTracker};
use ui::{BleAnalyzerUi, BleScreen};

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::DrawTarget;
use heapless::Vec;

use crate::apps::{App, AppInput, AppResult};

pub struct ScanAdvertisement {
    pub address: BdAddr,
    pub rssi: i8,
    pub data: Vec<u8, 64>,
}

pub trait BleScanner {
    type Error;
    async fn next_advertisement(&mut self) -> Result<ScanAdvertisement, Self::Error>;
}

pub struct BleAnalyzerApp {
    pub tracker: DeviceTracker,
    pub ui: BleAnalyzerUi,
    sim_timer_ms: u32,
    sim_step: u32,
    sim_now_ms: u64,
}

impl BleAnalyzerApp {
    pub fn new() -> Self {
        let mut app = Self {
            tracker: DeviceTracker::default(),
            ui: BleAnalyzerUi::new(),
            sim_timer_ms: 0,
            sim_step: 0,
            sim_now_ms: 10_000,
        };
        app.seed_initial_beacons();
        app
    }

    fn seed_initial_beacons(&mut self) {
        // 1. Honeywell Aerospace Avionics Beacon (Space Sector)
        let hw_mac = BdAddr([0x00, 0x18, 0x82, 0x33, 0x44, 0x55]);
        let hw_raw = [
            0x02, 0x01, 0x06, 0x0A, 0x09, b'H', b'o', b'n', b'e', b'y', b'w', b'e', b'l', b'l',
            0x04, 0xFF, 0x82, 0x01, 0x42, // Company ID 0x0182 (Honeywell Aerospace)
            0x03, 0x03, 0x0A, 0x18,
        ];
        let hw_ad = parse_advertising_data(&hw_raw).unwrap_or_default();
        self.tracker.update(hw_mac, -58, 10_000, hw_ad);

        // 2. SpaceX / Starlink Terminal Sensor (Space Sector)
        let sx_mac = BdAddr([0x50, 0x41, 0x43, 0x45, 0x58, 0x01]);
        let sx_raw = [
            0x02, 0x01, 0x06, 0x09, 0x09, b'S', b't', b'a', b'r', b'l', b'i', b'n', b'k', 0x04,
            0xFF, 0x8E, 0x0A, 0x01, // Company ID 0x0A8E (SpaceX / Starlink)
            0x03, 0x03, 0x1A, 0x18,
        ];
        let sx_ad = parse_advertising_data(&sx_raw).unwrap_or_default();
        self.tracker.update(sx_mac, -72, 10_500, sx_ad);

        // 3. Apple iPhone Companion
        let ap_mac = BdAddr([0xF4, 0xD4, 0x88, 0xAA, 0xBB, 0xCC]);
        let ap_raw = [
            0x02, 0x01, 0x1A, 0x07, 0x09, b'i', b'P', b'h', b'o', b'n', b'e', 0x04, 0xFF, 0x4C,
            0x00, 0x10, // Company ID 0x004C (Apple Inc.)
            0x03, 0x03, 0x0F, 0x18,
        ];
        let ap_ad = parse_advertising_data(&ap_raw).unwrap_or_default();
        self.tracker.update(ap_mac, -64, 11_000, ap_ad);

        // 4. Garmin GPS Flight Deck (Space / Aviation)
        let gm_mac = BdAddr([0x00, 0xE4, 0x12, 0x34, 0x56, 0x78]);
        let gm_raw = [
            0x02, 0x01, 0x06, 0x0B, 0x09, b'G', b'a', b'r', b'm', b'i', b'n', b' ', b'G', b'P',
            b'S', 0x04, 0xFF, 0xE4, 0x00, 0x01, // Company ID 0x00E4 (Garmin)
            0x03, 0x03, 0x19, 0x18,
        ];
        let gm_ad = parse_advertising_data(&gm_raw).unwrap_or_default();
        self.tracker.update(gm_mac, -81, 11_500, gm_ad);

        self.tracker.sort_by_rssi();
    }
}

impl Default for BleAnalyzerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for BleAnalyzerApp {
    fn name(&self) -> &str {
        "BLE Analyzer"
    }

    fn setup(&mut self) {
        self.ui.state.screen = BleScreen::Devices;
    }

    fn update(&mut self, input: &AppInput) -> AppResult {
        self.sim_timer_ms = self.sim_timer_ms.saturating_add(input.dt_ms);
        self.sim_now_ms = self.sim_now_ms.saturating_add(input.dt_ms as u64);

        if self.sim_timer_ms >= 800 {
            self.sim_timer_ms = 0;
            self.sim_step = (self.sim_step + 1) % 10;

            let offsets: [i8; 10] = [1, -2, 2, -1, 3, -3, 0, 1, -1, 2];
            let delta = offsets[self.sim_step as usize];

            for d in &mut self.tracker.devices {
                let new_rssi = (d.rssi + delta).clamp(-95, -45);
                d.rssi = new_rssi;
                d.rssi_history.push(self.sim_now_ms, new_rssi);
            }
            self.tracker.prune(self.sim_now_ms);
            self.tracker.sort_by_rssi();
        }

        if let Some(pt) = input.touch {
            self.ui.handle_touch(pt, &self.tracker);
        }

        if let Some(swipe) = input.swipe {
            self.ui.handle_swipe(swipe, &self.tracker);
        }

        AppResult::Continue
    }

    fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        self.ui.render(d, &self.tracker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_is_small_enough_for_task_stack() {
        assert!(core::mem::size_of::<DeviceTracker>() <= 64);
    }

    #[test]
    fn app_state_is_small_enough_for_task_stack() {
        assert!(core::mem::size_of::<BleAnalyzerApp>() <= 128);
    }

    #[test]
    fn app_initializes_with_space_sector_beacons() {
        let app = BleAnalyzerApp::new();
        assert_eq!(app.tracker.devices.len(), 4);
        assert_eq!(app.tracker.space_sector_count(), 3);
    }
}
