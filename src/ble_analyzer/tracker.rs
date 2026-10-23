//! Discovered device state tracker with RSSI history and space-sector detection.

use alloc::boxed::Box;
use heapless::{String, Vec};

use super::ad::{parse_16bit_services, parse_advertising_data, parse_device_name, parse_manufacturer_data, parse_tx_power};
use super::companies::is_space_sector;
use super::gatt::GattProfile;

pub const MAX_TRACKED_DEVICES: usize = 8;
pub const RSSI_HISTORY_LEN: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct TrackedDevice {
    pub mac: [u8; 6],
    pub name: String<24>,
    pub company_id: Option<u16>,
    pub is_space_sector: bool,
    pub last_rssi: i8,
    pub rssi_history: Vec<i8, RSSI_HISTORY_LEN>,
    pub packet_count: u32,
    pub last_seen_secs: u32,
    pub tx_power: Option<i8>,
    pub advertised_services: Vec<u16, 6>,
    pub gatt_profile: Option<Box<GattProfile>>,
}

impl TrackedDevice {
    pub fn new(mac: [u8; 6], rssi: i8, timestamp_secs: u32) -> Self {
        let mut history = Vec::new();
        let _ = history.push(rssi);

        Self {
            mac,
            name: String::new(),
            company_id: None,
            is_space_sector: false,
            last_rssi: rssi,
            rssi_history: history,
            packet_count: 0,
            last_seen_secs: timestamp_secs,
            tx_power: None,
            advertised_services: Vec::new(),
            gatt_profile: None,
        }
    }

    /// Appends a new RSSI sample into the sliding history window.
    pub fn push_rssi(&mut self, rssi: i8) {
        self.last_rssi = rssi;
        if self.rssi_history.len() == RSSI_HISTORY_LEN {
            self.rssi_history.remove(0);
        }
        let _ = self.rssi_history.push(rssi);
    }

    /// Formats MAC address as "AA:BB:CC:DD:EE:FF".
    pub fn format_mac<'a>(&self, buf: &'a mut [u8; 18]) -> &'a str {
        const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";
        let mut p = 0;
        for i in 0..6 {
            let b = self.mac[i];
            buf[p] = HEX_CHARS[(b >> 4) as usize]; p += 1;
            buf[p] = HEX_CHARS[(b & 0xF) as usize]; p += 1;
            if i < 5 {
                buf[p] = b':'; p += 1;
            }
        }
        core::str::from_utf8(&buf[..p]).unwrap_or("00:00:00:00:00:00")
    }

    /// Calculates (min, avg, max) RSSI from the history buffer.
    pub fn rssi_min_avg_max(&self) -> (i8, i8, i8) {
        if self.rssi_history.is_empty() {
            return (self.last_rssi, self.last_rssi, self.last_rssi);
        }

        let mut min = i8::MAX;
        let mut max = i8::MIN;
        let mut sum: i32 = 0;

        for &r in &self.rssi_history {
            if r < min { min = r; }
            if r > max { max = r; }
            sum += r as i32;
        }

        let avg = (sum / self.rssi_history.len() as i32) as i8;
        (min, avg, max)
    }

    /// Updates metadata from parsed advertising structures.
    pub fn update_from_payload(&mut self, payload: &[u8], timestamp_secs: u32) {
        self.last_seen_secs = timestamp_secs;
        self.packet_count = self.packet_count.saturating_add(1);

        if let Ok(structures) = parse_advertising_data(payload) {
            // Name
            if let Some(name) = parse_device_name(&structures) {
                self.name.clear();
                let len = name.len().min(24);
                let _ = self.name.push_str(&name[..len]);
            }

            // Manufacturer Specific Data & Space Sector
            if let Some((cid, _)) = parse_manufacturer_data(&structures) {
                self.company_id = Some(cid);
                self.is_space_sector = is_space_sector(cid);
            }

            // Tx Power
            if let Some(tx) = parse_tx_power(&structures) {
                self.tx_power = Some(tx);
            }

            // 16-bit Service UUIDs
            let svcs = parse_16bit_services(&structures);
            for s in svcs {
                if !self.advertised_services.contains(&s) && self.advertised_services.len() < 6 {
                    let _ = self.advertised_services.push(s);
                }
            }
        }
    }
}

pub struct DeviceTracker {
    pub devices: Vec<TrackedDevice, MAX_TRACKED_DEVICES>,
    pub total_packets_seen: u32,
    pub space_sector_count: u32,
}

impl DeviceTracker {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            total_packets_seen: 0,
            space_sector_count: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&TrackedDevice> {
        self.devices.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut TrackedDevice> {
        self.devices.get_mut(index)
    }

    /// Ingests a raw BLE advertisement or scan response packet.
    pub fn ingest_advertisement(
        &mut self,
        mac: [u8; 6],
        rssi: i8,
        ad_bytes: &[u8],
        timestamp_secs: u32,
    ) {
        self.total_packets_seen = self.total_packets_seen.saturating_add(1);

        // 1. Look for existing device by MAC
        for dev in &mut self.devices {
            if dev.mac == mac {
                dev.push_rssi(rssi);
                dev.update_from_payload(ad_bytes, timestamp_secs);
                self.recalculate_space_count();
                return;
            }
        }

        // 2. New device: if buffer is full, evict the oldest device
        if self.devices.len() == MAX_TRACKED_DEVICES {
            let mut oldest_idx = 0;
            let mut oldest_time = u32::MAX;
            for (i, d) in self.devices.iter().enumerate() {
                if d.last_seen_secs < oldest_time {
                    oldest_time = d.last_seen_secs;
                    oldest_idx = i;
                }
            }
            self.devices.remove(oldest_idx);
        }

        // 3. Create new device record
        let mut new_dev = TrackedDevice::new(mac, rssi, timestamp_secs);
        new_dev.update_from_payload(ad_bytes, timestamp_secs);
        let _ = self.devices.push(new_dev);

        self.recalculate_space_count();
    }

    /// Sorts devices with strongest RSSI first.
    pub fn sort_by_rssi(&mut self) {
        let n = self.devices.len();
        if n < 2 { return; }
        for i in 0..n {
            for j in 0..(n - 1 - i) {
                if self.devices[j].last_rssi < self.devices[j + 1].last_rssi {
                    self.devices.swap(j, j + 1);
                }
            }
        }
    }

    fn recalculate_space_count(&mut self) {
        let mut count = 0;
        for d in &self.devices {
            if d.is_space_sector {
                count += 1;
            }
        }
        self.space_sector_count = count;
    }
}

impl Default for DeviceTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_new_devices_and_update_rssi() {
        let mut tracker = DeviceTracker::new();
        let mac1 = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        // Ingest packet 1
        tracker.ingest_advertisement(mac1, -75, &[], 100);
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.total_packets_seen, 1);
        assert_eq!(tracker.get(0).unwrap().last_rssi, -75);
        assert_eq!(tracker.get(0).unwrap().packet_count, 1);

        // Ingest packet 2 from same device with better signal
        tracker.ingest_advertisement(mac1, -62, &[], 101);
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.total_packets_seen, 2);
        assert_eq!(tracker.get(0).unwrap().last_rssi, -62);
        assert_eq!(tracker.get(0).unwrap().packet_count, 2);
        assert_eq!(tracker.get(0).unwrap().rssi_history.len(), 2);
    }

    #[test]
    fn detect_space_sector_device_from_manufacturer_data() {
        let mut tracker = DeviceTracker::new();
        let mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        // Manufacturer payload with Honeywell Aerospace (0x0182)
        let ad_bytes = [
            0x04, 0xFF, 0x82, 0x01, 0x99,
        ];

        tracker.ingest_advertisement(mac, -55, &ad_bytes, 200);
        assert_eq!(tracker.space_sector_count, 1);
        let dev = tracker.get(0).unwrap();
        assert!(dev.is_space_sector);
        assert_eq!(dev.company_id, Some(0x0182));
    }

    #[test]
    fn calculate_rssi_min_avg_max() {
        let mut dev = TrackedDevice::new([1, 2, 3, 4, 5, 6], -80, 0);
        dev.push_rssi(-60);
        dev.push_rssi(-70);

        let (min, avg, max) = dev.rssi_min_avg_max();
        assert_eq!(min, -80);
        assert_eq!(max, -60);
        assert_eq!(avg, -70);
    }

    #[test]
    fn format_mac_address_string() {
        let dev = TrackedDevice::new([0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE], -70, 0);
        let mut buf = [0u8; 18];
        assert_eq!(dev.format_mac(&mut buf), "DE:AD:BE:EF:CA:FE");
    }
}
