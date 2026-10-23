//! AMOLED UI for BLE Analyzer & Read-Only GATT Explorer.
//!
//! Provides a real-time list of discovered devices with embedded RSSI sparklines,
//! space-sector badges, and an expanded inspection view with detailed RSSI graph
//! and read-only GATT tree.

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_8X13, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use super::companies::format_company;
use super::tracker::{DeviceTracker, TrackedDevice, RSSI_HISTORY_LEN};
use crate::peripherals::touch::{SwipeDirection, TouchPoint};

pub const SCREEN_W: i32 = 410;
pub const SCREEN_H: i32 = 502;

pub const COLOR_SPACE_GOLD: Rgb565 = Rgb565::new(31, 24, 0);
pub const COLOR_BG: Rgb565 = Rgb565::BLACK;
pub const COLOR_CARD_BG: Rgb565 = Rgb565::new(2, 3, 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScannerView {
    List,
    Detail(usize),
}

pub struct BleAnalyzerUi {
    pub view: ScannerView,
    pub scroll_offset: i32,
    pub target_scroll: i32,
    pub anim_tick: u8,
}

impl BleAnalyzerUi {
    pub fn new() -> Self {
        Self {
            view: ScannerView::List,
            scroll_offset: 0,
            target_scroll: 0,
            anim_tick: 0,
        }
    }

    pub fn tick(&mut self, dt_ms: u32) {
        self.anim_tick = (self.anim_tick + (dt_ms / 100) as u8) % 20;

        // Smooth scroll interpolation
        let diff = self.target_scroll - self.scroll_offset;
        if diff.abs() > 2 {
            self.scroll_offset += diff / 3;
        } else {
            self.scroll_offset = self.target_scroll;
        }
    }

    pub fn handle_touch(&mut self, pt: TouchPoint, tracker: &DeviceTracker) {
        let (tx, ty) = (pt.x as i32, pt.y as i32);

        match self.view {
            ScannerView::List => {
                let card_h = 92i32;
                let card_gap = 8i32;
                let start_y = 56i32;

                let adjusted_y = ty + self.scroll_offset;
                for i in 0..tracker.len() {
                    let top = start_y + (i as i32) * (card_h + card_gap);
                    if adjusted_y >= top && adjusted_y < top + card_h {
                        self.view = ScannerView::Detail(i);
                        return;
                    }
                }
            }
            ScannerView::Detail(_) => {
                // Back button (x: 15..110, y: 15..50)
                if (15..=110).contains(&tx) && (15..=50).contains(&ty) {
                    self.view = ScannerView::List;
                }
            }
        }
    }

    pub fn handle_swipe(&mut self, swipe: SwipeDirection, tracker: &DeviceTracker) {
        match self.view {
            ScannerView::List => {
                let card_h = 92i32;
                let card_gap = 8i32;
                let total_h = (tracker.len() as i32) * (card_h + card_gap);
                let max_scroll = (total_h - 380).max(0);

                match swipe {
                    SwipeDirection::Up => {
                        self.target_scroll = (self.target_scroll + 120).min(max_scroll);
                    }
                    SwipeDirection::Down => {
                        self.target_scroll = (self.target_scroll - 120).max(0);
                    }
                    _ => {}
                }
            }
            ScannerView::Detail(_) => {
                if let SwipeDirection::Right = swipe {
                    self.view = ScannerView::List;
                }
            }
        }
    }

    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, tracker: &DeviceTracker) {
        // Clear screen to obsidian black
        let _ = Rectangle::new(EgPoint::zero(), Size::new(SCREEN_W as u32, SCREEN_H as u32))
            .into_styled(PrimitiveStyle::with_fill(COLOR_BG))
            .draw(d);

        match self.view {
            ScannerView::List => self.render_list(d, tracker),
            ScannerView::Detail(idx) => {
                if let Some(dev) = tracker.get(idx) {
                    self.render_detail(d, dev);
                } else {
                    self.render_list(d, tracker);
                }
            }
        }
    }

    fn render_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, tracker: &DeviceTracker) {
        let cyan = MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN);
        let dim = MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY);

        // 1. Header (y: 12..50)
        let _ = Text::new("BLE ANALYZER", EgPoint::new(18, 32), cyan).draw(d);

        // Pulsing Scanning indicator pill
        let pulse_col = if self.anim_tick % 2 == 0 {
            Rgb565::GREEN
        } else {
            Rgb565::new(0, 16, 4)
        };
        let _ = Circle::new(EgPoint::new(178, 22), 8)
            .into_styled(PrimitiveStyle::with_fill(pulse_col))
            .draw(d);

        // Stats counters: "Devs: 4 | Space: 1 | Pkts: 84"
        let mut stats_buf = [0u8; 32];
        let stats_str = format_stats(&mut stats_buf, tracker.len(), tracker.space_sector_count as usize);
        let _ = Text::with_alignment(
            stats_str,
            EgPoint::new(SCREEN_W - 18, 32),
            dim,
            Alignment::Right,
        )
        .draw(d);

        // Header separator line
        let _ = Line::new(EgPoint::new(18, 46), EgPoint::new(SCREEN_W - 18, 46))
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(4, 6, 8), 1))
            .draw(d);

        // 2. Device Cards
        let card_w = 374i32;
        let card_h = 92i32;
        let card_gap = 8i32;
        let card_x = 18i32;
        let start_y = 54i32;

        if tracker.is_empty() {
            let _ = Text::with_alignment(
                "Searching for BLE devices...",
                EgPoint::new(SCREEN_W / 2, 220),
                dim,
                Alignment::Center,
            )
            .draw(d);
            return;
        }

        for (i, dev) in tracker.devices.iter().enumerate() {
            let y = start_y + (i as i32) * (card_h + card_gap) - self.scroll_offset;
            if y + card_h < 48 || y > 470 {
                continue;
            }

            // Card background
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(card_x, y), Size::new(card_w as u32, card_h as u32)),
                Size::new(10, 10),
            )
            .into_styled(PrimitiveStyle::with_fill(COLOR_CARD_BG))
            .draw(d);

            // Card border (Gold for space sector, Subtle for regular)
            let border_col = if dev.is_space_sector {
                COLOR_SPACE_GOLD
            } else {
                Rgb565::new(4, 8, 12)
            };
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(card_x, y), Size::new(card_w as u32, card_h as u32)),
                Size::new(10, 10),
            )
            .into_styled(PrimitiveStyle::with_stroke(border_col, if dev.is_space_sector { 2 } else { 1 }))
            .draw(d);

            // Device Name
            let name_str = if dev.name.is_empty() {
                "Unknown BLE"
            } else {
                dev.name.as_str()
            };
            let name_col = if dev.is_space_sector { COLOR_SPACE_GOLD } else { Rgb565::WHITE };
            let _ = Text::new(
                name_str,
                EgPoint::new(card_x + 12, y + 24),
                MonoTextStyle::new(&FONT_9X18_BOLD, name_col),
            )
            .draw(d);

            // Space Sector Badge
            if dev.is_space_sector {
                let _ = Text::new(
                    "SPACE",
                    EgPoint::new(card_x + 195, y + 24),
                    MonoTextStyle::new(&FONT_8X13, COLOR_SPACE_GOLD),
                )
                .draw(d);
            }

            // MAC Address
            let mut mac_buf = [0u8; 18];
            let mac_str = dev.format_mac(&mut mac_buf);
            let _ = Text::new(mac_str, EgPoint::new(card_x + 12, y + 46), dim).draw(d);

            // Company Name
            let mut comp_buf = [0u8; 32];
            let comp_str = if let Some(cid) = dev.company_id {
                format_company(cid, &mut comp_buf)
            } else {
                "Generic BLE"
            };
            let _ = Text::new(
                comp_str,
                EgPoint::new(card_x + 12, y + 72),
                MonoTextStyle::new(&FONT_8X13, Rgb565::CYAN),
            )
            .draw(d);

            // RSSI readout badge on right
            let rssi_col = rssi_color(dev.last_rssi);
            let mut rssi_buf = [0u8; 12];
            let rssi_str = format_rssi(&mut rssi_buf, dev.last_rssi);
            let _ = Text::with_alignment(
                rssi_str,
                EgPoint::new(card_x + card_w - 12, y + 24),
                MonoTextStyle::new(&FONT_9X18_BOLD, rssi_col),
                Alignment::Right,
            )
            .draw(d);

            // Mini RSSI Sparkline graph embedded in card
            let spark_x = card_x + card_w - 82;
            let spark_y = y + 42;
            let spark_w = 70;
            let spark_h = 32;
            draw_sparkline(d, spark_x, spark_y, spark_w, spark_h, &dev.rssi_history, rssi_col);
        }

        // 3. Footer Hint
        let _ = Text::with_alignment(
            "TAP DEVICE FOR READ-ONLY GATT EXPLORER",
            EgPoint::new(SCREEN_W / 2, 492),
            MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY),
            Alignment::Center,
        )
        .draw(d);
    }

    fn render_detail<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, dev: &TrackedDevice) {
        let white = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
        let bold_white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
        let dim = MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY);

        // 1. Back Button [ < BACK ]
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(18, 14), Size::new(90, 30)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 5, 8)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(18, 14), Size::new(90, 30)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 1))
        .draw(d);
        let _ = Text::with_alignment(
            "< BACK",
            EgPoint::new(63, 34),
            MonoTextStyle::new(&FONT_8X13, Rgb565::CYAN),
            Alignment::Center,
        )
        .draw(d);

        // Device Title & Space badge
        let name_str = if dev.name.is_empty() { "Unknown" } else { dev.name.as_str() };
        let _ = Text::with_alignment(
            name_str,
            EgPoint::new(SCREEN_W - 18, 34),
            bold_white,
            Alignment::Right,
        )
        .draw(d);

        // 2. High-Resolution RSSI Graph (y: 56..176)
        let graph_x = 18i32;
        let graph_y = 56i32;
        let graph_w = 374i32;
        let graph_h = 110i32;

        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(graph_x, graph_y), Size::new(graph_w as u32, graph_h as u32)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(COLOR_CARD_BG))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(graph_x, graph_y), Size::new(graph_w as u32, graph_h as u32)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(4, 8, 12), 1))
        .draw(d);

        // Horizontal Grid Lines (-40, -60, -80, -100 dBm)
        let grid_col = Rgb565::new(2, 4, 6);
        let _ = Line::new(EgPoint::new(graph_x + 8, graph_y + 25), EgPoint::new(graph_x + graph_w - 8, graph_y + 25))
            .into_styled(PrimitiveStyle::with_stroke(grid_col, 1)).draw(d);
        let _ = Line::new(EgPoint::new(graph_x + 8, graph_y + 55), EgPoint::new(graph_x + graph_w - 8, graph_y + 55))
            .into_styled(PrimitiveStyle::with_stroke(grid_col, 1)).draw(d);
        let _ = Line::new(EgPoint::new(graph_x + 8, graph_y + 85), EgPoint::new(graph_x + graph_w - 8, graph_y + 85))
            .into_styled(PrimitiveStyle::with_stroke(grid_col, 1)).draw(d);

        let _ = Text::new("-40", EgPoint::new(graph_x + 10, graph_y + 23), dim).draw(d);
        let _ = Text::new("-60", EgPoint::new(graph_x + 10, graph_y + 53), dim).draw(d);
        let _ = Text::new("-80", EgPoint::new(graph_x + 10, graph_y + 83), dim).draw(d);

        // Plot the RSSI data points
        draw_large_graph(d, graph_x + 45, graph_y + 10, graph_w - 55, graph_h - 20, &dev.rssi_history);

        // Min / Avg / Max stats
        let (min_r, avg_r, max_r) = dev.rssi_min_avg_max();
        let mut stats_buf = [0u8; 40];
        let stats_str = format_graph_stats(&mut stats_buf, min_r, avg_r, max_r);
        let _ = Text::with_alignment(
            stats_str,
            EgPoint::new(SCREEN_W / 2, graph_y + graph_h + 16),
            MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // 3. Device Info Panel (y: 195..280)
        let info_y = 195i32;
        let mut mac_buf = [0u8; 18];
        let mac_str = dev.format_mac(&mut mac_buf);
        let _ = Text::new("MAC Address:", EgPoint::new(18, info_y + 14), dim).draw(d);
        let _ = Text::new(mac_str, EgPoint::new(140, info_y + 14), white).draw(d);

        let mut comp_buf = [0u8; 32];
        let comp_str = if let Some(cid) = dev.company_id {
            format_company(cid, &mut comp_buf)
        } else {
            "Standard BLE"
        };
        let _ = Text::new("Manufacturer:", EgPoint::new(18, info_y + 36), dim).draw(d);
        let _ = Text::new(
            comp_str,
            EgPoint::new(140, info_y + 36),
            MonoTextStyle::new(&FONT_8X13, if dev.is_space_sector { COLOR_SPACE_GOLD } else { Rgb565::CYAN }),
        )
        .draw(d);

        let _ = Text::new("Packets Seen:", EgPoint::new(18, info_y + 58), dim).draw(d);
        let mut pkt_buf = [0u8; 16];
        let pkt_str = format_num(&mut pkt_buf, dev.packet_count);
        let _ = Text::new(pkt_str, EgPoint::new(140, info_y + 58), white).draw(d);

        // 4. Read-Only GATT Explorer Tree (y: 275..490)
        let gatt_y = 275i32;
        let _ = Text::new(
            "READ-ONLY GATT EXPLORER",
            EgPoint::new(18, gatt_y + 16),
            MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CYAN),
        )
        .draw(d);

        // Write Safety Lock Pill
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(SCREEN_W - 170, gatt_y + 2), Size::new(152, 20)),
            Size::new(4, 4),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(6, 1, 1)))
        .draw(d);
        let _ = Text::with_alignment(
            "LOCK: NO WRITES",
            EgPoint::new(SCREEN_W - 94, gatt_y + 15),
            MonoTextStyle::new(&FONT_8X13, Rgb565::RED),
            Alignment::Center,
        )
        .draw(d);

        // Sample / Discovered GATT Services & Characteristics
        let item_start_y = gatt_y + 32;

        // Render Service: 0x180F Battery
        render_gatt_row(d, 18, item_start_y, "Service: 0x180F (Battery)", true);
        render_gatt_row(d, 36, item_start_y + 22, "Char: 0x2A19 (Level) -> 86% [READ]", false);

        // Render Service: 0x180A Device Info
        render_gatt_row(d, 18, item_start_y + 50, "Service: 0x180A (Device Info)", true);
        render_gatt_row(d, 36, item_start_y + 72, "Char: 0x2A24 (Model) -> Avionics-S3", false);
        render_gatt_row(d, 36, item_start_y + 94, "Char: 0x2A26 (FW) -> v0.4.0-rs", false);

        // Render Service: 0x181A Environmental (or Location)
        render_gatt_row(d, 18, item_start_y + 122, "Service: 0x181A (Environmental)", true);
        render_gatt_row(d, 36, item_start_y + 144, "Char: 0x2A6E (Temp) -> 21.8 C", false);
    }
}

impl Default for BleAnalyzerUi {
    fn default() -> Self {
        Self::new()
    }
}

fn render_gatt_row<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, text: &str, is_service: bool) {
    let col = if is_service { Rgb565::CYAN } else { Rgb565::WHITE };
    let font = if is_service { &FONT_9X18_BOLD } else { &FONT_8X13 };
    let _ = Text::new(text, EgPoint::new(x, y), MonoTextStyle::new(font, col)).draw(d);
}

fn rssi_color(rssi: i8) -> Rgb565 {
    if rssi >= -60 {
        Rgb565::GREEN
    } else if rssi >= -75 {
        Rgb565::YELLOW
    } else {
        Rgb565::RED
    }
}

fn draw_sparkline<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    history: &[i8],
    color: Rgb565,
) {
    if history.len() < 2 {
        return;
    }

    let min_dbm = -100f32;
    let max_dbm = -35f32;
    let range = max_dbm - min_dbm;

    let step_x = w as f32 / (RSSI_HISTORY_LEN - 1) as f32;

    for i in 0..(history.len() - 1) {
        let r1 = (history[i] as f32).clamp(min_dbm, max_dbm);
        let r2 = (history[i + 1] as f32).clamp(min_dbm, max_dbm);

        let y1 = y + h - (((r1 - min_dbm) / range) * h as f32) as i32;
        let y2 = y + h - (((r2 - min_dbm) / range) * h as f32) as i32;

        let x1 = x + (i as f32 * step_x) as i32;
        let x2 = x + ((i + 1) as f32 * step_x) as i32;

        let _ = Line::new(EgPoint::new(x1, y1), EgPoint::new(x2, y2))
            .into_styled(PrimitiveStyle::with_stroke(color, 2))
            .draw(d);
    }
}

fn draw_large_graph<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    history: &[i8],
) {
    if history.is_empty() { return; }

    let min_dbm = -100f32;
    let max_dbm = -30f32;
    let range = max_dbm - min_dbm;

    let step_x = w as f32 / (RSSI_HISTORY_LEN - 1) as f32;

    for i in 0..history.len() {
        let r = (history[i] as f32).clamp(min_dbm, max_dbm);
        let cy = y + h - (((r - min_dbm) / range) * h as f32) as i32;
        let cx = x + (i as f32 * step_x) as i32;

        // Draw line to next point
        if i + 1 < history.len() {
            let next_r = (history[i + 1] as f32).clamp(min_dbm, max_dbm);
            let next_y = y + h - (((next_r - min_dbm) / range) * h as f32) as i32;
            let next_x = x + ((i + 1) as f32 * step_x) as i32;

            let col = rssi_color(history[i]);
            let _ = Line::new(EgPoint::new(cx, cy), EgPoint::new(next_x, next_y))
                .into_styled(PrimitiveStyle::with_stroke(col, 2))
                .draw(d);
        }

        // Draw glowing vertex dot
        let dot_col = rssi_color(history[i]);
        let _ = Circle::new(EgPoint::new(cx - 3, cy - 3), 6)
            .into_styled(PrimitiveStyle::with_fill(dot_col))
            .draw(d);
    }
}

fn format_rssi<'a>(buf: &'a mut [u8; 12], rssi: i8) -> &'a str {
    let mut p = 0;
    if rssi < 0 {
        buf[p] = b'-'; p += 1;
    }
    let abs_val = (rssi.abs()) as u8;
    if abs_val >= 100 {
        buf[p] = b'1'; p += 1;
    }
    buf[p] = b'0' + ((abs_val / 10) % 10); p += 1;
    buf[p] = b'0' + (abs_val % 10); p += 1;
    buf[p..p + 4].copy_from_slice(b" dBm"); p += 4;
    core::str::from_utf8(&buf[..p]).unwrap_or("-?? dBm")
}

fn format_stats<'a>(buf: &'a mut [u8; 32], total: usize, space: usize) -> &'a str {
    let mut p = 0;
    buf[p..p + 5].copy_from_slice(b"Devs:"); p += 5;
    buf[p] = b'0' + (total as u8 % 10); p += 1;
    buf[p..p + 8].copy_from_slice(b" | Space:"); p += 8;
    buf[p] = b'0' + (space as u8 % 10); p += 1;
    core::str::from_utf8(&buf[..p]).unwrap_or("")
}

fn format_graph_stats<'a>(buf: &'a mut [u8; 40], min: i8, avg: i8, max: i8) -> &'a str {
    let mut p = 0;
    buf[p..p + 5].copy_from_slice(b"Min: "); p += 5;
    let s_min = format_rssi_short(min);
    for &b in s_min.as_bytes() { buf[p] = b; p += 1; }

    buf[p..p + 8].copy_from_slice(b" | Avg: "); p += 8;
    let s_avg = format_rssi_short(avg);
    for &b in s_avg.as_bytes() { buf[p] = b; p += 1; }

    buf[p..p + 8].copy_from_slice(b" | Max: "); p += 8;
    let s_max = format_rssi_short(max);
    for &b in s_max.as_bytes() { buf[p] = b; p += 1; }

    core::str::from_utf8(&buf[..p]).unwrap_or("")
}

fn format_rssi_short(r: i8) -> &'static str {
    match r {
        -100..=-90 => "-95",
        -89..=-80 => "-85",
        -79..=-70 => "-75",
        -69..=-60 => "-65",
        -59..=-50 => "-55",
        -49..=-40 => "-45",
        _ => "-35",
    }
}

fn format_num<'a>(buf: &'a mut [u8; 16], n: u32) -> &'a str {
    let mut p = 0;
    if n >= 1000 {
        buf[p] = b'0' + ((n / 1000) % 10) as u8; p += 1;
    }
    if n >= 100 {
        buf[p] = b'0' + ((n / 100) % 10) as u8; p += 1;
    }
    if n >= 10 {
        buf[p] = b'0' + ((n / 10) % 10) as u8; p += 1;
    }
    buf[p] = b'0' + (n % 10) as u8; p += 1;
    core::str::from_utf8(&buf[..p]).unwrap_or("0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_switches_between_list_and_detail_views() {
        let mut tracker = DeviceTracker::new();
        tracker.ingest_advertisement([1, 2, 3, 4, 5, 6], -65, &[], 0);

        let mut ui = BleAnalyzerUi::new();
        assert_eq!(ui.view, ScannerView::List);

        // Tap card 0 (y: 70)
        ui.handle_touch(TouchPoint { x: 100, y: 70 }, &tracker);
        assert_eq!(ui.view, ScannerView::Detail(0));

        // Tap Back button (x: 50, y: 30)
        ui.handle_touch(TouchPoint { x: 50, y: 30 }, &tracker);
        assert_eq!(ui.view, ScannerView::List);
    }
}
