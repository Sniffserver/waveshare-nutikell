//! Discovered device state tracker with 30s sliding-window RSSI history and heuristic classifier.

use super::{
    ad::{AdStructure, AdvertisementInfo},
    companies::{company_name, is_space_sector},
};
use alloc::boxed::Box;
use heapless::{String, Vec};

pub const MAX_DEVICES: usize = 6;
pub const MAX_RSSI_SAMPLES: usize = 10;
pub const MAX_DEVICE_NAME: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Phone,
    Earbuds,
    Tracker,
    Thermometer,
    HeartRate,
    Mouse,
    Keyboard,
    Smartwatch,
    CyclingSensor,
    Generic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BdAddr(pub [u8; 6]);

impl BdAddr {
    pub fn format<'a>(&self, buf: &'a mut [u8; 18]) -> &'a str {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut p = 0;
        for i in 0..6 {
            let b = self.0[i];
            buf[p] = HEX[(b >> 4) as usize]; p += 1;
            buf[p] = HEX[(b & 0xF) as usize]; p += 1;
            if i < 5 {
                buf[p] = b':'; p += 1;
            }
        }
        core::str::from_utf8(&buf[..p]).unwrap_or("00:00:00:00:00:00")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RssiSample {
    pub timestamp_ms: u64,
    pub rssi: i8,
}

#[derive(Clone, Debug, Default)]
pub struct RssiHistory {
    pub samples: Vec<RssiSample, MAX_RSSI_SAMPLES>,
}

impl RssiHistory {
    pub fn push(&mut self, timestamp_ms: u64, rssi: i8) {
        if self.samples.len() == MAX_RSSI_SAMPLES {
            self.samples.remove(0);
        }
        let _ = self.samples.push(RssiSample { timestamp_ms, rssi });
        self.prune(timestamp_ms);
    }

    pub fn prune(&mut self, now_ms: u64) {
        while let Some(first) = self.samples.first() {
            if now_ms.saturating_sub(first.timestamp_ms) <= 30_000 {
                break;
            }
            self.samples.remove(0);
        }
    }

    pub fn min_avg_max(&self) -> (i8, i8, i8) {
        if self.samples.is_empty() {
            return (-100, -100, -100);
        }
        let mut min = i8::MAX;
        let mut max = i8::MIN;
        let mut sum: i32 = 0;
        for s in &self.samples {
            if s.rssi < min { min = s.rssi; }
            if s.rssi > max { max = s.rssi; }
            sum += s.rssi as i32;
        }
        let avg = (sum / self.samples.len() as i32) as i8;
        (min, avg, max)
    }
}

#[derive(Clone, Debug)]
pub struct TrackedDevice {
    pub address: BdAddr,
    pub name: Option<String<MAX_DEVICE_NAME>>,
    pub company_id: Option<u16>,
    pub company: Option<&'static str>,
    pub is_space: bool,
    pub kind: DeviceKind,
    pub rssi: i8,
    pub last_seen_ms: u64,
    pub rssi_history: RssiHistory,
    pub ad_structures: Vec<AdStructure, { super::ad::MAX_AD_STRUCTURES }>,
}

pub fn classify_device(info: &AdvertisementInfo) -> DeviceKind {
    for uuid in info.service_uuids_16.iter() {
        match *uuid {
            0x1809 => return DeviceKind::Thermometer,
            0x180D => return DeviceKind::HeartRate,
            0x1816 => return DeviceKind::CyclingSensor,
            _ => {}
        }
    }

    if let Some(name) = &info.name {
        let mut lower = String::<MAX_DEVICE_NAME>::new();
        for byte in name.as_bytes() {
            let _ = lower.push((*byte as char).to_ascii_lowercase());
        }
        let text = lower.as_str();

        if text.contains("airpods") || text.contains("buds") || text.contains("headphone") {
            return DeviceKind::Earbuds;
        }
        if text.contains("mouse") {
            return DeviceKind::Mouse;
        }
        if text.contains("keyboard") {
            return DeviceKind::Keyboard;
        }
        if text.contains("tile") || text.contains("track") || text.contains("airtag") {
            return DeviceKind::Tracker;
        }
        if text.contains("watch") || text.contains("garmin") {
            return DeviceKind::Smartwatch;
        }
        if text.contains("iphone") || text.contains("pixel") || text.contains("galaxy") {
            return DeviceKind::Phone;
        }
    }

    if let Some(company_id) = info.company_id {
        match company_id {
            0x004C | 0x00E0 => DeviceKind::Phone,
            0x00E4 | 0x0171 => DeviceKind::Smartwatch,
            _ => DeviceKind::Generic,
        }
    } else {
        DeviceKind::Generic
    }
}

#[derive(Default, Clone)]
pub struct DeviceTracker {
    pub devices: Vec<Box<TrackedDevice>, MAX_DEVICES>,
}

impl DeviceTracker {
    pub fn update(
        &mut self,
        address: BdAddr,
        rssi: i8,
        now_ms: u64,
        ad_structures: Vec<AdStructure, { super::ad::MAX_AD_STRUCTURES }>,
    ) {
        let info = super::ad::extract_ad_info(&ad_structures);
        let kind = classify_device(&info);
        let space = info.company_id.map(is_space_sector).unwrap_or(false);

        if let Some(device) = self.devices.iter_mut().find(|device| device.address == address) {
            device.rssi = rssi;
            device.last_seen_ms = now_ms;
            device.company_id = info.company_id;
            device.company = info.company_id.and_then(company_name);
            device.is_space = space;
            device.kind = kind;
            device.ad_structures = ad_structures;
            if let Some(name) = info.name {
                device.name = Some(name);
            }
            device.rssi_history.push(now_ms, rssi);
            return;
        }

        if self.devices.len() >= MAX_DEVICES {
            // In-place recycling: find oldest entry and reuse the existing Box
            // to eliminate heap fragmentation during continuous scanning.
            let mut oldest_idx = 0;
            let mut oldest_time = u64::MAX;
            for (idx, dev) in self.devices.iter().enumerate() {
                if dev.last_seen_ms < oldest_time {
                    oldest_time = dev.last_seen_ms;
                    oldest_idx = idx;
                }
            }
            let dev = &mut self.devices[oldest_idx];
            dev.address = address;
            dev.name = info.name;
            dev.company_id = info.company_id;
            dev.company = info.company_id.and_then(company_name);
            dev.is_space = space;
            dev.kind = kind;
            dev.rssi = rssi;
            dev.last_seen_ms = now_ms;
            dev.rssi_history.samples.clear();
            dev.rssi_history.push(now_ms, rssi);
            dev.ad_structures = ad_structures;
            return;
        }

        let _ = self.devices.push(Box::new(TrackedDevice {
            address,
            name: info.name,
            company_id: info.company_id,
            company: info.company_id.and_then(company_name),
            is_space: space,
            kind,
            rssi,
            last_seen_ms: now_ms,
            rssi_history: {
                let mut history = RssiHistory::default();
                history.push(now_ms, rssi);
                history
            },
            ad_structures,
        }));
    }

    pub fn prune(&mut self, now_ms: u64) {
        self.devices
            .retain(|device| now_ms.saturating_sub(device.last_seen_ms) <= 30_000);
    }

    pub fn sort_by_rssi(&mut self) {
        self.devices.sort_unstable_by(|a, b| b.rssi.cmp(&a.rssi));
    }

    pub fn space_sector_count(&self) -> usize {
        self.devices.iter().filter(|d| d.is_space).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rssi_history_keeps_last_thirty_seconds() {
        let mut history = RssiHistory::default();
        history.push(0, -60);
        history.push(10_000, -65);
        history.push(31_000, -80);
        assert_eq!(history.samples.len(), 2);
        assert_eq!(history.samples[0].timestamp_ms, 10_000);
    }

    #[test]
    fn classifies_smartwatch_and_space() {
        let mut info = AdvertisementInfo::default();
        let mut name = String::<MAX_DEVICE_NAME>::new();
        let _ = name.push_str("Garmin Forerunner");
        info.name = Some(name);
        info.company_id = Some(0x00E4);

        let kind = classify_device(&info);
        assert_eq!(kind, DeviceKind::Smartwatch);
        assert!(is_space_sector(0x00E4));
    }
}
