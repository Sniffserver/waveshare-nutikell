// WiFi Setup App — shown when no credentials are stored in NVS.
//
// Flow:
//   1. Watch enters AP mode ("Watch-Setup", open network).
//   2. Screen shows:
//       - A QR code encoding the AP connection string so phone camera
//         can join the AP in one tap.
//       - IP address for the captive portal (192.168.4.1).
//       - "Waiting for credentials..." spinner.
//   3. Once the phone submits the form, screen transitions to "Saved!".
//
// QR payload (Wi-Fi Easy Connect standard):
//   WIFI:S:Watch-Setup;T:nopass;;

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Text};

use embedded_qr::{QrBuilder, Version2};

use crate::board;

const W: i32 = board::LCD_WIDTH as i32;

/// AP SSID shown on screen and encoded in QR code.
pub const AP_SSID: &str = "Watch-Setup";
/// IP address of the watch in AP mode (standard esp-idf default).
pub const AP_IP: &str = "192.168.4.1";

/// WiFi-connect QR payload for an open network.
const QR_PAYLOAD: &[u8] = b"WIFI:S:Watch-Setup;T:nopass;;";

#[derive(Clone, Copy, PartialEq)]
pub enum SetupPhase {
    /// Waiting for phone to connect and submit credentials.
    Waiting,
    /// Credentials received — about to reboot into STA mode.
    Done,
}

pub struct WifiSetupApp {
    pub phase: SetupPhase,
    /// Spinning indicator frame counter (incremented by `update`).
    tick: u32,
}

impl WifiSetupApp {
    pub fn new() -> Self {
        Self {
            phase: SetupPhase::Waiting,
            tick: 0,
        }
    }

    /// Advance animation. Call every ~100 ms.
    pub fn update(&mut self, _dt_ms: u32) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn mark_done(&mut self) {
        self.phase = SetupPhase::Done;
    }

    /// Render the setup screen.
    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // Black background
        let _ = Rectangle::new(
            EgPoint::zero(),
            Size::new(board::LCD_WIDTH as u32, board::LCD_HEIGHT as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
        .draw(d);

        let title_st = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CYAN);
        let label_st = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_GRAY);
        let val_st = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
        let ok_st = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::GREEN);

        let _ = Text::with_alignment("WiFi Setup", EgPoint::new(W / 2, 28), title_st, Alignment::Center).draw(d);

        if self.phase == SetupPhase::Done {
            let _ = Text::with_alignment("Credentials saved!", EgPoint::new(W / 2, 240), ok_st, Alignment::Center).draw(d);
            let _ = Text::with_alignment("Rebooting...", EgPoint::new(W / 2, 268), label_st, Alignment::Center).draw(d);
            return;
        }

        // Instructions
        let _ = Text::with_alignment("Scan QR with phone camera", EgPoint::new(W / 2, 50), label_st, Alignment::Center).draw(d);
        let _ = Text::with_alignment("or open browser:", EgPoint::new(W / 2, 62), label_st, Alignment::Center).draw(d);
        let _ = Text::with_alignment("http://192.168.4.1", EgPoint::new(W / 2, 76), val_st, Alignment::Center).draw(d);

        // QR code centred on the screen — built at compile time into a const-size matrix
        draw_qr(d);

        // AP info
        let _ = Text::with_alignment("Join WiFi first:", EgPoint::new(W / 2, 365), label_st, Alignment::Center).draw(d);
        let _ = Text::with_alignment(AP_SSID, EgPoint::new(W / 2, 378), val_st, Alignment::Center).draw(d);
        let _ = Text::with_alignment("(open network, no password)", EgPoint::new(W / 2, 391), label_st, Alignment::Center).draw(d);

        // Waiting spinner
        let spinner = ["Waiting .  ", "Waiting .. ", "Waiting ..."];
        let spin_idx = (self.tick / 6) as usize % 3;
        let _ = Text::with_alignment(spinner[spin_idx], EgPoint::new(W / 2, 416), val_st, Alignment::Center).draw(d);

        // Reset hint
        let _ = Text::with_alignment("Tap screen to skip", EgPoint::new(W / 2, 444), label_st, Alignment::Center).draw(d);
    }
}

/// Render the QR code centred at x=205, starting at y=92.
/// Uses Version 2 (25×25 modules) @ 7px/module = 175px wide.
/// Centre on 410px wide screen: (410 - 175) / 2 = 117.
fn draw_qr<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    const MODULE_PX: u32 = 7;
    const BORDER: u32 = 2; // quiet-zone modules

    let matrix = match QrBuilder::<Version2>::new().build(QR_PAYLOAD) {
        Ok(m) => m,
        Err(_) => return,
    };

    let width = matrix.width() as u32;
    let total_modules = width + BORDER * 2;
    let px_size = total_modules * MODULE_PX;

    // Origin so the QR is horizontally centred
    let origin_x = (board::LCD_WIDTH as i32 - px_size as i32) / 2;
    let origin_y = 90i32;

    // White quiet-zone background
    let _ = Rectangle::new(
        EgPoint::new(origin_x, origin_y),
        Size::new(px_size, px_size),
    )
    .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
    .draw(d);

    let data_origin_x = origin_x + (BORDER * MODULE_PX) as i32;
    let data_origin_y = origin_y + (BORDER * MODULE_PX) as i32;

    for y in 0..width {
        for x in 0..width {
            if matrix.get(x as usize, y as usize) {
                let px = data_origin_x + (x * MODULE_PX) as i32;
                let py = data_origin_y + (y * MODULE_PX) as i32;
                let _ = Rectangle::new(
                    EgPoint::new(px, py),
                    Size::new(MODULE_PX, MODULE_PX),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
                .draw(d);
            }
        }
    }
}
