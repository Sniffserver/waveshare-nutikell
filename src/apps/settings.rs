// Settings app - WiFi config with T9 keyboard input & WiFi reset

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::peripherals::wifi::{WifiConfig, WifiState};
use crate::ui::t9_keyboard::T9Keyboard;

#[derive(Clone, Copy, PartialEq)]
enum SettingsField {
    Ssid,
    Password,
    Connect,
}

pub struct SettingsApp {
    pub wifi_config: WifiConfig,
    pub wifi_state: WifiState,
    pub keyboard: T9Keyboard,
    pub stored_ssid: [u8; 32],
    pub stored_ssid_len: usize,
    active_field: SettingsField,
    editing: bool,
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            wifi_config: WifiConfig::new(),
            wifi_state: WifiState::Disconnected,
            keyboard: T9Keyboard::new(),
            stored_ssid: [0; 32],
            stored_ssid_len: 0,
            active_field: SettingsField::Ssid,
            editing: false,
        }
    }

    pub fn set_stored_ssid(&mut self, ssid: &str) {
        let b = ssid.as_bytes();
        let len = b.len().min(32);
        self.stored_ssid[..len].copy_from_slice(&b[..len]);
        self.stored_ssid_len = len;
    }

    pub fn stored_ssid_str(&self) -> &str {
        core::str::from_utf8(&self.stored_ssid[..self.stored_ssid_len]).unwrap_or("")
    }

    /// Handle tap at screen position. Returns true if consumed.
    pub fn handle_tap(&mut self, x: u16, y: u16) -> bool {
        // Check if keyboard is active and handles it
        if self.keyboard.is_active() {
            if self.keyboard.handle_tap(x, y) {
                // Sync text to active field
                match self.active_field {
                    SettingsField::Ssid => self.wifi_config.set_ssid(self.keyboard.get_text()),
                    SettingsField::Password => {
                        self.wifi_config.set_password(self.keyboard.get_text())
                    }
                    _ => {}
                }
                return true;
            }
            // Tap outside keyboard = close it
            if y < 200 {
                self.keyboard.hide();
                self.editing = false;
                return true;
            }
        }

        // Reset WiFi button: y in 250..305, x in 30..380
        if !self.keyboard.is_active() && y >= 250 && y < 305 && x >= 30 && x <= 380 {
            use esp_println::println;
            println!("[SETTINGS] Reset WiFi requested - erasing NVS credentials...");
            let _ = crate::peripherals::nvs::erase_credentials();
            println!("[SETTINGS] Credentials erased! Rebooting...");
            esp_hal::system::software_reset();
            return true;
        }

        // Field selection
        if y >= 85 && y < 135 {
            // SSID field tapped
            self.active_field = SettingsField::Ssid;
            self.keyboard.clear_text();
            self.keyboard.show();
            self.editing = true;
            return true;
        }
        if y >= 140 && y < 190 {
            // Password field
            self.active_field = SettingsField::Password;
            self.keyboard.clear_text();
            self.keyboard.show();
            self.editing = true;
            return true;
        }
        if y >= 195 && y < 240 {
            // Connect button
            if self.wifi_state == WifiState::Disconnected || self.wifi_state == WifiState::Error {
                self.wifi_state = WifiState::Connecting;
            }
            return true;
        }
        false
    }

    pub fn update(&mut self, dt_ms: u32) {
        self.keyboard.update(dt_ms);
    }

    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let _ = Rectangle::new(EgPoint::zero(), Size::new(410, 502))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 2, 2)))
            .draw(d);

        let title = MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN);
        let label = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let value = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);

        let _ = Text::with_alignment("SETTINGS", EgPoint::new(205, 25), title, Alignment::Center)
            .draw(d);

        // --- Stored WiFi Status Card ---
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(15, 36), Size::new(380, 44)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 6)))
        .draw(d);

        let ssid_str = self.stored_ssid_str();
        let display_ssid = if !ssid_str.is_empty() {
            ssid_str
        } else {
            "(none)"
        };
        let _ = Text::new("WiFi:", EgPoint::new(25, 54), label).draw(d);
        let ssid_color = if self.wifi_state == WifiState::Connected {
            Rgb565::GREEN
        } else {
            Rgb565::WHITE
        };
        let _ = Text::new(
            display_ssid,
            EgPoint::new(85, 54),
            MonoTextStyle::new(&FONT_10X20, ssid_color),
        )
        .draw(d);

        // Status text
        let (status_text, status_color) = match self.wifi_state {
            WifiState::Connected => ("CONNECTED", Rgb565::GREEN),
            WifiState::Connecting => ("CONNECTING...", Rgb565::YELLOW),
            WifiState::Disconnected => ("OFF/IDLE", Rgb565::CSS_GRAY),
            WifiState::Error => ("ERROR", Rgb565::RED),
        };
        let _ = Text::new("Status:", EgPoint::new(25, 72), label).draw(d);
        let _ = Text::new(
            status_text,
            EgPoint::new(105, 72),
            MonoTextStyle::new(&FONT_10X20, status_color),
        )
        .draw(d);

        // --- Manual SSID field ---
        let ssid_bg = if self.active_field == SettingsField::Ssid && self.editing {
            Rgb565::new(3, 6, 3)
        } else {
            Rgb565::new(2, 4, 2)
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(15, 88), Size::new(380, 44)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(ssid_bg))
        .draw(d);
        let _ = Text::new("Manual SSID:", EgPoint::new(25, 106), label).draw(d);
        let m_ssid = self.wifi_config.ssid_str();
        let m_ssid_display = if m_ssid.is_empty() {
            "(tap to edit)"
        } else {
            m_ssid
        };
        let _ = Text::new(m_ssid_display, EgPoint::new(160, 106), value).draw(d);

        // --- Manual Password field ---
        let pass_bg = if self.active_field == SettingsField::Password && self.editing {
            Rgb565::new(3, 6, 3)
        } else {
            Rgb565::new(2, 4, 2)
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(15, 140), Size::new(380, 44)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(pass_bg))
        .draw(d);
        let _ = Text::new("Password:", EgPoint::new(25, 158), label).draw(d);
        let pass_len = self.wifi_config.pass_len;
        let _ = Text::new(
            if pass_len > 0 {
                "********"
            } else {
                "(tap to edit)"
            },
            EgPoint::new(130, 158),
            value,
        )
        .draw(d);

        // --- Connect button ---
        let btn_color = match self.wifi_state {
            WifiState::Disconnected => Rgb565::BLUE,
            WifiState::Connecting => Rgb565::YELLOW,
            WifiState::Connected => Rgb565::GREEN,
            WifiState::Error => Rgb565::RED,
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(100, 195), Size::new(210, 36)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(btn_color))
        .draw(d);
        let btn_text = match self.wifi_state {
            WifiState::Disconnected => "CONNECT",
            WifiState::Connecting => "CONNECTING...",
            WifiState::Connected => "CONNECTED",
            WifiState::Error => "RETRY",
        };
        let _ = Text::with_alignment(
            btn_text,
            EgPoint::new(205, 219),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // --- Reset WiFi button ---
        // Prominent red button to erase NVS and reboot into AP provisioning
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 250), Size::new(330, 48)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(20, 2, 2)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 250), Size::new(330, 48)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::RED, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "RESET WIFI (PROVISION)",
            EgPoint::new(205, 280),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // Draw keyboard overlay if active
        self.keyboard.render(d);
    }
}
