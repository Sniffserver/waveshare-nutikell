//! BLE Analyzer & Read-Only GATT Explorer module.
//!
//! Provides:
//! - Raw Advertising Data (AD) parser (`ad.rs`)
//! - Aerospace & Space sector Bluetooth SIG Company ID lookup (`companies.rs`)
//! - Read-only GATT service & characteristic model (`gatt.rs`)
//! - Discovered device tracker with multi-sample RSSI history (`tracker.rs`)
//! - AMOLED UI with live RSSI sparklines and expanded GATT inspector (`ui.rs`)

pub mod ad;
pub mod companies;
pub mod gatt;
pub mod tracker;
pub mod ui;

pub use ad::{parse_advertising_data, AdStructure};
pub use companies::{format_company, is_space_sector, lookup_company};
pub use gatt::{DiscoveredCharacteristic, DiscoveredService, GattProfile, ReadOnlyGuard};
pub use tracker::{DeviceTracker, TrackedDevice};
pub use ui::{BleAnalyzerUi, ScannerView};

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::DrawTarget;

use crate::apps::{App, AppInput, AppResult};

pub struct BleAnalyzerApp {
    pub tracker: DeviceTracker,
    pub ui: BleAnalyzerUi,
    sim_timer_ms: u32,
    sim_step: u32,
}

impl BleAnalyzerApp {
    pub fn new() -> Self {
        let mut app = Self {
            tracker: DeviceTracker::new(),
            ui: BleAnalyzerUi::new(),
            sim_timer_ms: 0,
            sim_step: 0,
        };
        app.seed_initial_beacons();
        app
    }

    /// Seeds realistic initial beacons (including space-sector avionics)
    fn seed_initial_beacons(&mut self) {
        // 1. Honeywell Aerospace Avionics Beacon (Space Sector)
        let hw_mac = [0x00, 0x18, 0x82, 0x33, 0x44, 0x55];
        let hw_ad = [
            0x02, 0x01, 0x06, // Flags
            0x0A, 0x09, b'H', b'o', b'n', b'e', b'y', b'w', b'e', b'l', b'l', // Complete Name
            0x04, 0xFF, 0x82, 0x01, 0x42, // Company ID 0x0182 (Honeywell Aerospace)
            0x03, 0x03, 0x0A, 0x18, // Service 0x180A (Device Info)
        ];
        self.tracker.ingest_advertisement(hw_mac, -58, &hw_ad, 10);
        if let Some(dev) = self.tracker.get_mut(0) {
            dev.push_rssi(-60);
            dev.push_rssi(-57);
            dev.push_rssi(-55);
            dev.push_rssi(-58);
        }

        // 2. SpaceX / Starlink Terminal Sensor (Space Sector)
        let sx_mac = [0x50, 0x41, 0x43, 0x45, 0x58, 0x01];
        let sx_ad = [
            0x02, 0x01, 0x06,
            0x09, 0x09, b'S', b't', b'a', b'r', b'l', b'i', b'n', b'k',
            0x04, 0xFF, 0x8E, 0x0A, 0x01, // Company ID 0x0A8E (SpaceX / Starlink)
            0x03, 0x03, 0x1A, 0x18, // Service 0x181A (Environmental)
        ];
        self.tracker.ingest_advertisement(sx_mac, -72, &sx_ad, 12);
        if let Some(dev) = self.tracker.get_mut(1) {
            dev.push_rssi(-74);
            dev.push_rssi(-70);
            dev.push_rssi(-69);
            dev.push_rssi(-72);
        }

        // 3. Apple iPhone Companion
        let ap_mac = [0xF4, 0xD4, 0x88, 0xAA, 0xBB, 0xCC];
        let ap_ad = [
            0x02, 0x01, 0x1A,
            0x07, 0x09, b'i', b'P', b'h', b'o', b'n', b'e',
            0x04, 0xFF, 0x4C, 0x00, 0x10, // Company ID 0x004C (Apple Inc.)
            0x03, 0x03, 0x0F, 0x18, // Service 0x180F (Battery)
        ];
        self.tracker.ingest_advertisement(ap_mac, -64, &ap_ad, 14);
        if let Some(dev) = self.tracker.get_mut(2) {
            dev.push_rssi(-66);
            dev.push_rssi(-63);
            dev.push_rssi(-65);
            dev.push_rssi(-64);
        }

        // 4. Garmin GPS Flight Deck (Space / Aviation)
        let gm_mac = [0x00, 0xE4, 0x12, 0x34, 0x56, 0x78];
        let gm_ad = [
            0x02, 0x01, 0x06,
            0x0B, 0x09, b'G', b'a', b'r', b'm', b'i', b'n', b' ', b'G', b'P', b'S',
            0x04, 0xFF, 0xE4, 0x00, 0x01, // Company ID 0x00E4 (Garmin)
            0x03, 0x03, 0x19, 0x18, // Service 0x1819 (Location & Navigation)
        ];
        self.tracker.ingest_advertisement(gm_mac, -81, &gm_ad, 15);
        if let Some(dev) = self.tracker.get_mut(3) {
            dev.push_rssi(-83);
            dev.push_rssi(-80);
            dev.push_rssi(-82);
            dev.push_rssi(-81);
        }
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
        self.ui.view = ScannerView::List;
    }

    fn update(&mut self, input: &AppInput) -> AppResult {
        self.ui.tick(input.dt_ms);

        // Advance simulation RSSI jitter to keep graph dynamic
        self.sim_timer_ms = self.sim_timer_ms.saturating_add(input.dt_ms);
        if self.sim_timer_ms >= 800 {
            self.sim_timer_ms = 0;
            self.sim_step = (self.sim_step + 1) % 10;

            // Subtle live signal fluctuation for dynamic AMOLED graph
            let offsets: [i8; 10] = [1, -2, 2, -1, 3, -3, 0, 1, -1, 2];
            let delta = offsets[self.sim_step as usize];

            for d in &mut self.tracker.devices {
                let new_rssi = (d.last_rssi + delta).clamp(-95, -45);
                d.push_rssi(new_rssi);
                d.packet_count = d.packet_count.saturating_add(1);
            }
        }

        // Handle Touch
        if let Some(pt) = input.touch {
            self.ui.handle_touch(pt, &self.tracker);
        }

        // Handle Swipe
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
    fn app_initializes_with_space_sector_beacons() {
        let app = BleAnalyzerApp::new();
        assert_eq!(app.tracker.len(), 4);
        assert_eq!(app.tracker.space_sector_count, 3); // Honeywell, SpaceX, Garmin
    }
}
