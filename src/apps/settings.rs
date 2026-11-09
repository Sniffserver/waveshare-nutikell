// Settings app - WiFi config with T9 keyboard input & WiFi reset

use alloc::format;
use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::peripherals::settings::{save_settings, WatchSettings};
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
    pub watch_settings: WatchSettings,
    pub log_clear_confirm: bool,
    pub log_cleared: bool,
    active_field: SettingsField,
    editing: bool,
    connect_requested: bool,
    settings_changed: bool,
    settings_save_failed: bool,
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            wifi_config: WifiConfig::new(),
            wifi_state: WifiState::Disconnected,
            keyboard: T9Keyboard::new(),
            stored_ssid: [0; 32],
            stored_ssid_len: 0,
            watch_settings: WatchSettings::default(),
            log_clear_confirm: false,
            log_cleared: false,
            active_field: SettingsField::Ssid,
            editing: false,
            connect_requested: false,
            settings_changed: false,
            settings_save_failed: false,
        }
    }

    pub fn with_settings(settings: WatchSettings) -> Self {
        let mut app = Self::new();
        app.watch_settings = settings;
        app
    }

    pub fn take_settings_update(&mut self) -> Option<WatchSettings> {
        if self.settings_changed {
            self.settings_changed = false;
            Some(self.watch_settings)
        } else {
            None
        }
    }

    fn persist_settings(&mut self) {
        self.settings_save_failed = save_settings(self.watch_settings).is_err();
        self.settings_changed = true;
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
                if self.wifi_config.validate().is_ok() {
                    self.wifi_state = WifiState::Connecting;
                    self.connect_requested = true;
                } else {
                    self.wifi_state = WifiState::Error;
                }
            }
            return true;
        }

        // Timezone adjustment: y in 312..358
        if !self.keyboard.is_active() && y >= 312 && y <= 358 {
            if x >= 40 && x < 120 {
                // Decrement timezone by 1 hour (down to -12h = -720m)
                self.watch_settings.timezone_offset_mins =
                    (self.watch_settings.timezone_offset_mins - 60).max(-720);
                self.persist_settings();
                return true;
            } else if x >= 290 && x <= 370 {
                // Increment timezone by 1 hour (up to +14h = +840m)
                self.watch_settings.timezone_offset_mins =
                    (self.watch_settings.timezone_offset_mins + 60).min(840);
                self.persist_settings();
                return true;
            }
        }

        // 24H Clock toggle: y in 366..412 && x in 40..370
        if !self.keyboard.is_active() && y >= 366 && y <= 412 && x >= 40 && x <= 370 {
            self.watch_settings.clock_24h = !self.watch_settings.clock_24h;
            self.persist_settings();
            return true;
        }

        // Clear Work Log button: y in 420..465, x in 40..370
        if !self.keyboard.is_active() && y >= 420 && y <= 465 && x >= 40 && x <= 370 {
            if self.log_clear_confirm {
                self.log_clear_confirm = false;
                if crate::peripherals::session_log::erase_session_log().is_ok() {
                    self.log_cleared = true;
                }
            } else {
                self.log_clear_confirm = true;
                self.log_cleared = false;
            }
            return true;
        } else if !self.keyboard.is_active() && self.log_clear_confirm {
            // Tapping anywhere else cancels confirmation
            self.log_clear_confirm = false;
        }

        false
    }

    pub fn take_connect_request(&mut self) -> Option<WifiConfig> {
        if self.connect_requested {
            self.connect_requested = false;
            Some(self.wifi_config)
        } else {
            None
        }
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

        // --- Timezone Card ---
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 312), Size::new(330, 44)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 6)))
        .draw(d);

        // [ < ] button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(48, 316), Size::new(44, 36)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 8, 12)))
        .draw(d);
        let _ = Text::with_alignment(
            "<",
            EgPoint::new(70, 340),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
            Alignment::Center,
        )
        .draw(d);

        // [ > ] button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(318, 316), Size::new(44, 36)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 8, 12)))
        .draw(d);
        let _ = Text::with_alignment(
            ">",
            EgPoint::new(340, 340),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
            Alignment::Center,
        )
        .draw(d);

        let offset_hours = self.watch_settings.timezone_offset_mins / 60;
        let offset_rem = (self.watch_settings.timezone_offset_mins % 60).abs();
        let tz_text = if self.watch_settings.timezone_offset_mins == 0 {
            format!("TZ: UTC")
        } else if offset_rem == 0 {
            format!("TZ: UTC{:+}", offset_hours)
        } else {
            format!("TZ: UTC{:+}:{:02}", offset_hours, offset_rem)
        };
        let _ = Text::with_alignment(
            &tz_text,
            EgPoint::new(205, 340),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // --- 24H Clock Toggle Button ---
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 366), Size::new(330, 44)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(
            if self.watch_settings.clock_24h {
                Rgb565::new(2, 8, 4)
            } else {
                Rgb565::new(4, 4, 4)
            },
        ))
        .draw(d);
        let clk_text = if self.watch_settings.clock_24h {
            "CLOCK: 24-HOUR"
        } else {
            "CLOCK: 12-HOUR"
        };
        let _ = Text::with_alignment(
            clk_text,
            EgPoint::new(205, 394),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // --- Clear Work Log button ---
        let (log_btn_bg, log_btn_stroke, log_btn_text) = if self.log_cleared {
            (Rgb565::new(0, 14, 2), Rgb565::GREEN, "WORK LOG CLEARED ✓")
        } else if self.log_clear_confirm {
            (Rgb565::new(28, 4, 0), Rgb565::RED, "CONFIRM: CLEAR LOG?")
        } else {
            (Rgb565::new(12, 2, 2), Rgb565::RED, "CLEAR WORK LOG")
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 420), Size::new(330, 44)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(log_btn_bg))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(40, 420), Size::new(330, 44)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_stroke(log_btn_stroke, 1))
        .draw(d);
        let _ = Text::with_alignment(
            log_btn_text,
            EgPoint::new(205, 447),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        if self.settings_save_failed {
            let _ = Text::with_alignment(
                "SETTINGS NOT SAVED",
                EgPoint::new(205, 480),
                MonoTextStyle::new(&FONT_10X20, Rgb565::RED),
                Alignment::Center,
            )
            .draw(d);
        }

        // Draw keyboard overlay if active
        self.keyboard.render(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::geometry::Size;
    use embedded_graphics::pixelcolor::Rgb565;
    use embedded_graphics::prelude::*;

    struct MockDisplay {
        red_pixels_drawn: usize,
    }

    impl MockDisplay {
        fn new() -> Self {
            Self {
                red_pixels_drawn: 0,
            }
        }
    }

    impl OriginDimensions for MockDisplay {
        fn size(&self) -> Size {
            Size::new(410, 502)
        }
    }

    impl DrawTarget for MockDisplay {
        type Color = Rgb565;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(_, color) in pixels {
                if color == Rgb565::RED {
                    self.red_pixels_drawn += 1;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn settings_save_failure_displays_red_warning() {
        let mut app = SettingsApp::new();
        let mut display = MockDisplay::new();

        // When save has not failed:
        app.render(&mut display);
        let baseline_red = display.red_pixels_drawn;

        // When save failed:
        app.settings_save_failed = true;
        let mut fail_display = MockDisplay::new();
        app.render(&mut fail_display);
        // "SETTINGS NOT SAVED" adds additional red pixels:
        assert!(fail_display.red_pixels_drawn > baseline_red);
    }

    #[test]
    fn clear_work_log_two_step_confirmation() {
        let mut app = SettingsApp::new();
        assert!(!app.log_clear_confirm);
        assert!(!app.log_cleared);

        // First tap: requests confirmation
        assert!(app.handle_tap(205, 440));
        assert!(app.log_clear_confirm);
        assert!(!app.log_cleared);

        // Tap elsewhere: cancels confirmation
        assert!(!app.handle_tap(205, 50));
        assert!(!app.log_clear_confirm);

        // First tap again:
        assert!(app.handle_tap(205, 440));
        assert!(app.log_clear_confirm);

        // Second tap on the same button: confirms and clears
        assert!(app.handle_tap(205, 440));
        assert!(!app.log_clear_confirm);
        assert!(app.log_cleared);
    }

    #[test]
    fn settings_app_lifecycle_and_updates() {
        let mut app = SettingsApp::with_settings(WatchSettings {
            clock_24h: false,
            ..WatchSettings::default()
        });
        assert!(!app.watch_settings.clock_24h);
        assert_eq!(app.take_settings_update(), None);
    }
}
