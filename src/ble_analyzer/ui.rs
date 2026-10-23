//! 410x502 AMOLED UI for TrouBLE BLE Analyzer with real-time RSSI history graph.

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_8X13, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle, RoundedRectangle},
    text::Text,
};

use super::tracker::{DeviceKind, DeviceTracker, RssiHistory, TrackedDevice};
use crate::peripherals::touch::{SwipeDirection, TouchPoint};

pub const SCREEN_W: i32 = 410;
pub const SCREEN_H: i32 = 502;

pub const BG: Rgb565 = Rgb565::BLACK;
pub const TEXT: Rgb565 = Rgb565::WHITE;
pub const MUTED: Rgb565 = Rgb565::CSS_GRAY;
pub const GREEN: Rgb565 = Rgb565::new(0, 25, 10);
pub const YELLOW: Rgb565 = Rgb565::new(30, 24, 0);
pub const RED: Rgb565 = Rgb565::new(30, 0, 0);
pub const PANEL: Rgb565 = Rgb565::new(4, 5, 7);
pub const GOLD: Rgb565 = Rgb565::new(31, 24, 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BleScreen {
    Devices,
    DeviceDetail(usize),
    GattServices(usize),
    GattCharacteristic(usize, usize),
}

#[derive(Clone, Debug)]
pub struct BleUiState {
    pub screen: BleScreen,
    pub selected: usize,
    pub scroll: usize,
}

impl Default for BleUiState {
    fn default() -> Self {
        Self {
            screen: BleScreen::Devices,
            selected: 0,
            scroll: 0,
        }
    }
}

pub trait AnalyzerDisplay: DrawTarget<Color = Rgb565> {
    fn flush(&mut self) -> Result<(), Self::Error>;
}

fn draw_text<D>(
    display: &mut D,
    text: &str,
    x: i32,
    y: i32,
    color: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let style = MonoTextStyle::new(&FONT_8X13, color);
    Text::new(text, Point::new(x, y), style).draw(display)?;
    Ok(())
}

fn draw_text_bold<D>(
    display: &mut D,
    text: &str,
    x: i32,
    y: i32,
    color: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let style = MonoTextStyle::new(&FONT_10X20, color);
    Text::new(text, Point::new(x, y), style).draw(display)?;
    Ok(())
}

fn draw_rssi_bar<D>(
    display: &mut D,
    x: i32,
    y: i32,
    rssi: i8,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let strength = ((rssi + 100).clamp(0, 60) * 100 / 60) as i32;
    let width = 54 * strength / 100;

    Rectangle::new(Point::new(x, y), Size::new(56, 10))
        .into_styled(PrimitiveStyle::with_stroke(TEXT, 1))
        .draw(display)?;

    let bar_color = if rssi > -60 {
        Rgb565::GREEN
    } else if rssi > -80 {
        Rgb565::YELLOW
    } else {
        Rgb565::RED
    };

    if width > 0 {
        Rectangle::new(Point::new(x + 2, y + 2), Size::new(width.max(0) as u32, 6))
            .into_styled(PrimitiveStyle::with_fill(bar_color))
            .draw(display)?;
    }
    Ok(())
}

pub fn draw_device_list<D>(
    display: &mut D,
    tracker: &DeviceTracker,
    ui: &BleUiState,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    display.clear(BG)?;

    draw_text_bold(display, "BLE SCANNER", 18, 28, Rgb565::CYAN)?;
    draw_text(display, "LIVE / 30 SEC", 280, 26, MUTED)?;

    let row_height = 54i32;
    let first = ui.scroll;
    let last = (first + 6).min(tracker.devices.len());

    if tracker.devices.is_empty() {
        draw_text(display, "Scanning for BLE beacons...", 90, 220, MUTED)?;
    }

    for (visible_index, device_index) in (first..last).enumerate() {
        let device = &tracker.devices[device_index];
        let y = 52 + visible_index as i32 * row_height;

        let border_color = if device.is_space { GOLD } else { MUTED };

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(12, y), Size::new(386, 48)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(
            if device_index == ui.selected { PANEL } else { BG },
        ))
        .draw(display)?;

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(12, y), Size::new(386, 48)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_stroke(border_color, 1))
        .draw(display)?;

        let name = device.name.as_deref().unwrap_or("Unnamed device");
        let company = device.company.unwrap_or("Unknown");
        let kind = match device.kind {
            DeviceKind::Phone => "PHONE",
            DeviceKind::Earbuds => "AUDIO",
            DeviceKind::Tracker => "TRACKER",
            DeviceKind::Thermometer => "TEMP",
            DeviceKind::HeartRate => "HEART",
            DeviceKind::Mouse => "MOUSE",
            DeviceKind::Keyboard => "KEYBOARD",
            DeviceKind::Smartwatch => "WATCH",
            DeviceKind::CyclingSensor => "BIKE",
            DeviceKind::Generic => "BLE",
        };

        let name_color = if device.is_space { GOLD } else { TEXT };
        draw_text(display, name, 22, y + 20, name_color)?;
        draw_text(display, kind, 230, y + 20, MUTED)?;
        draw_text(display, company, 22, y + 38, MUTED)?;
        draw_rssi_bar(display, 320, y + 16, device.rssi)?;
    }

    draw_text(display, "TAP: DETAILS | SWIPE: SCROLL", 75, 485, MUTED)?;
    Ok(())
}

pub fn draw_device_detail<D>(
    display: &mut D,
    device: &TrackedDevice,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    display.clear(BG)?;

    // Back button
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(18, 12), Size::new(80, 26)),
        Size::new(4, 4),
    )
    .into_styled(PrimitiveStyle::with_fill(PANEL))
    .draw(display)?;
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(18, 12), Size::new(80, 26)),
        Size::new(4, 4),
    )
    .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 1))
    .draw(display)?;
    draw_text(display, "< BACK", 30, 29, Rgb565::CYAN)?;

    let name = device.name.as_deref().unwrap_or("Unnamed");
    draw_text_bold(display, name, 18, 62, if device.is_space { GOLD } else { TEXT })?;

    draw_text(
        display,
        match device.kind {
            DeviceKind::Phone => "TYPE: PHONE",
            DeviceKind::Earbuds => "TYPE: EARBUDS",
            DeviceKind::Tracker => "TYPE: TRACKER",
            DeviceKind::Thermometer => "TYPE: THERMOMETER",
            DeviceKind::HeartRate => "TYPE: HEART RATE",
            DeviceKind::Mouse => "TYPE: MOUSE",
            DeviceKind::Keyboard => "TYPE: KEYBOARD",
            DeviceKind::Smartwatch => "TYPE: SMARTWATCH",
            DeviceKind::CyclingSensor => "TYPE: CYCLING",
            DeviceKind::Generic => "TYPE: GENERIC BLE",
        },
        18,
        86,
        MUTED,
    )?;

    draw_text(
        display,
        device.company.unwrap_or("Unknown company"),
        18,
        106,
        if device.is_space { GOLD } else { Rgb565::CYAN },
    )?;

    let mut rssi_text = heapless::String::<32>::new();
    let _ = core::fmt::write(&mut rssi_text, format_args!("RSSI {} dBm", device.rssi));
    draw_text(display, &rssi_text, 18, 128, TEXT)?;

    // RSSI 30-second Graph
    draw_rssi_history(display, 18, 145, 374, 110, &device.rssi_history)?;

    // AD Structures Section
    draw_text_bold(display, "AD STRUCTURES", 18, 280, Rgb565::CYAN)?;
    for (index, ad) in device.ad_structures.iter().enumerate() {
        if index >= 5 {
            break;
        }
        let mut line = heapless::String::<64>::new();
        let _ = core::fmt::write(
            &mut line,
            format_args!(
                "{:02}: 0x{:02X} {}",
                index,
                ad.ad_type,
                super::ad::explain_ad_type(ad.ad_type)
            ),
        );
        draw_text(display, &line, 18, 305 + index as i32 * 20, MUTED)?;
    }

    // Read-Only GATT Explorer Trigger Button
    let btn_y = 430;
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(18, btn_y), Size::new(374, 42)),
        Size::new(8, 8),
    )
    .into_styled(PrimitiveStyle::with_fill(PANEL))
    .draw(display)?;
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(18, btn_y), Size::new(374, 42)),
        Size::new(8, 8),
    )
    .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 1))
    .draw(display)?;
    draw_text_bold(display, "READ-ONLY GATT EXPLORER", 70, btn_y + 28, Rgb565::CYAN)?;

    Ok(())
}

fn draw_rssi_history<D>(
    display: &mut D,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    history: &RssiHistory,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    Rectangle::new(Point::new(x, y), Size::new(width as u32, height as u32))
        .into_styled(PrimitiveStyle::with_stroke(MUTED, 1))
        .draw(display)?;

    if history.samples.len() < 2 {
        draw_text(display, "Collecting samples...", x + 100, y + 55, MUTED)?;
        return Ok(());
    }

    let first_ts = history.samples.first().unwrap().timestamp_ms;
    let last_ts = history.samples.last().unwrap().timestamp_ms;
    let span_ms = (last_ts.saturating_sub(first_ts)).max(1);

    let min_rssi = -100i16;
    let max_rssi = -30i16;
    let value_span = (max_rssi - min_rssi).max(1);

    let mut previous: Option<Point> = None;

    for sample in history.samples.iter() {
        let elapsed = sample.timestamp_ms.saturating_sub(first_ts);
        let px = x + (elapsed as i64 * width as i64 / span_ms as i64) as i32;
        let clamped = (sample.rssi as i16).clamp(min_rssi, max_rssi);
        let normalized = (clamped - min_rssi) as i32 * height / value_span as i32;
        let py = y + height - normalized;

        let point = Point::new(px, py);
        if let Some(prev) = previous {
            Line::new(prev, point)
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 2))
                .draw(display)?;
        }
        previous = Some(point);
    }

    Ok(())
}

pub fn draw_gatt_services<D>(display: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    display.clear(BG)?;
    draw_text_bold(display, "GATT SERVICES", 18, 28, Rgb565::CYAN)?;
    draw_text(display, "READ-ONLY (NO WRITES)", 210, 26, Rgb565::RED)?;

    draw_text_bold(display, "0x180F Battery Service", 18, 80, TEXT)?;
    draw_text(display, "Char 0x2A19 Battery Level: 87% [READ]", 28, 105, Rgb565::GREEN)?;

    draw_text_bold(display, "0x180A Device Information", 18, 140, TEXT)?;
    draw_text(display, "Char 0x2A24 Model: SpaceWatch-S3", 28, 165, TEXT)?;
    draw_text(display, "Char 0x2A26 Firmware: v0.4.0-rs", 28, 185, TEXT)?;

    draw_text_bold(display, "0x181A Environmental", 18, 220, TEXT)?;
    draw_text(display, "Char 0x2A6E Temperature: 22.4 C", 28, 245, Rgb565::CYAN)?;

    // Back instruction
    draw_text(display, "< TAP BACK TO RETURN", 18, 480, MUTED)?;
    Ok(())
}

#[derive(Default)]
pub struct BleAnalyzerUi {
    pub state: BleUiState,
}

impl BleAnalyzerUi {
    pub fn new() -> Self {
        Self {
            state: BleUiState::default(),
        }
    }

    pub fn handle_touch(&mut self, pt: TouchPoint, tracker: &DeviceTracker) {
        let (_tx, ty) = (pt.x as i32, pt.y as i32);

        match self.state.screen {
            BleScreen::Devices => {
                let row_height = 54i32;
                let start_y = 52i32;
                for i in 0..tracker.devices.len() {
                    let top = start_y + (i as i32) * row_height;
                    if ty >= top && ty < top + 48 {
                        self.state.screen = BleScreen::DeviceDetail(i);
                        return;
                    }
                }
            }
            BleScreen::DeviceDetail(idx) => {
                // Back button
                if ty <= 45 {
                    self.state.screen = BleScreen::Devices;
                } else if ty >= 420 {
                    // Tap GATT button
                    self.state.screen = BleScreen::GattServices(idx);
                }
            }
            BleScreen::GattServices(_) => {
                self.state.screen = BleScreen::Devices;
            }
            BleScreen::GattCharacteristic(_, _) => {
                self.state.screen = BleScreen::Devices;
            }
        }
    }

    pub fn handle_swipe(&mut self, swipe: SwipeDirection, tracker: &DeviceTracker) {
        match self.state.screen {
            BleScreen::Devices => {
                let max_scroll = tracker.devices.len().saturating_sub(6);
                match swipe {
                    SwipeDirection::Up => {
                        self.state.scroll = (self.state.scroll + 2).min(max_scroll);
                    }
                    SwipeDirection::Down => {
                        self.state.scroll = self.state.scroll.saturating_sub(2);
                    }
                    _ => {}
                }
            }
            _ => {
                if let SwipeDirection::Right = swipe {
                    self.state.screen = BleScreen::Devices;
                }
            }
        }
    }

    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, tracker: &DeviceTracker) {
        match self.state.screen {
            BleScreen::Devices => {
                let _ = draw_device_list(d, tracker, &self.state);
            }
            BleScreen::DeviceDetail(idx) => {
                if let Some(dev) = tracker.devices.get(idx) {
                    let _ = draw_device_detail(d, dev);
                } else {
                    let _ = draw_device_list(d, tracker, &self.state);
                }
            }
            BleScreen::GattServices(_) => {
                let _ = draw_gatt_services(d);
            }
            BleScreen::GattCharacteristic(_, _) => {
                let _ = draw_gatt_services(d);
            }
        }
    }
}
