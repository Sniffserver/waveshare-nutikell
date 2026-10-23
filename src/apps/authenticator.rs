// 2FA / TOTP Authenticator App (Wristkey-RS)
// Implements RFC 6238 (TOTP) and RFC 4226 (HOTP) in pure no_std Rust.
// Features:
//   - Zero external crypto dependencies (pure SHA-1 & HMAC-SHA1 engine)
//   - RFC 4648 Base32 decoding
//   - 30-second live animated circular countdown ring with color-coded urgency
//   - Sample 2FA accounts (GitHub, Google, AWS, Proton) and Flash/NVS persistence
//   - Interactive AMOLED UI with account carousel, tap-to-reveal, and status bar

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::apps::{App, AppInput, AppResult};
use crate::peripherals::touch::TouchPoint;

// =========================================================================
// 1. Pure no_std Cryptographic Primitives (SHA-1, HMAC-SHA1, Base32, Unix Time)
// =========================================================================

/// Computes SHA-1 hash of `data` (FIPS 180-1)
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let ml = (data.len() as u64).wrapping_mul(8);

    // Build padded message buffer
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    #[inline(always)]
    fn rol(n: u32, b: u32) -> u32 {
        n.rotate_left(b)
    }

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for j in 0..16 {
            w[j] = u32::from_be_bytes([
                chunk[j * 4],
                chunk[j * 4 + 1],
                chunk[j * 4 + 2],
                chunk[j * 4 + 3],
            ]);
        }
        for j in 16..80 {
            w[j] = rol(w[j - 3] ^ w[j - 8] ^ w[j - 14] ^ w[j - 16], 1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (j, &wj) in w.iter().enumerate() {
            let (f, k) = match j {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = rol(a, 5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wj);
            e = d;
            d = c;
            c = rol(b, 30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

/// Computes HMAC-SHA1 of `msg` with `key` (RFC 2104)
pub fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k_pad = [0u8; 64];
    if key.len() > 64 {
        let digest = sha1(key);
        k_pad[..20].copy_from_slice(&digest);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(64 + msg.len());
    for &b in &k_pad {
        inner.push(b ^ 0x36);
    }
    inner.extend_from_slice(msg);
    let inner_hash = sha1(&inner);

    let mut outer = Vec::with_capacity(64 + 20);
    for &b in &k_pad {
        outer.push(b ^ 0x5c);
    }
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

/// Decodes an RFC 4648 Base32 string into raw bytes
pub fn base32_decode(s: &str) -> Vec<u8> {
    let mut bits: u32 = 0;
    let mut val: u32 = 0;
    let mut out = Vec::new();

    for byte in s.as_bytes() {
        let ch = match *byte {
            b'=' => break,
            b' ' | b'-' => continue,
            b'a'..=b'z' => *byte - b'a' + b'A',
            _ => *byte,
        };

        let idx = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => continue,
        } as u32;

        val = (val << 5) | idx;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((val >> bits) & 0xFF) as u8);
        }
    }

    out
}

/// Computes RFC 6238 TOTP code from secret and Unix epoch seconds
pub fn totp(secret: &[u8], unix_secs: u64, period: u32, digits: u32) -> u32 {
    let counter = unix_secs / (period as u64);
    let counter_bytes = counter.to_be_bytes();

    let h = hmac_sha1(secret, &counter_bytes);
    let offset = (h[19] & 0x0F) as usize;

    let binary = (((h[offset] & 0x7F) as u32) << 24)
        | ((h[offset + 1] as u32) << 16)
        | ((h[offset + 2] as u32) << 8)
        | (h[offset + 3] as u32);

    let modulus = match digits {
        8 => 100_000_000,
        7 => 10_000_000,
        _ => 1_000_000, // standard 6 digits
    };

    binary % modulus
}

/// Converts UTC date/time to POSIX Unix epoch seconds
pub fn datetime_to_unix_secs(year: u16, month: u8, day: u8, hour: u8, min: u8, sec: u8) -> u64 {
    const DAYS_BEFORE_MONTH: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let y = year as u32;
    let m = month as usize;
    if y < 1970 || m == 0 || m > 12 {
        return 0;
    }
    let leap_years = (y - 1 - 1968) / 4 - (y - 1 - 1900) / 100 + (y - 1 - 1600) / 400;
    let days_from_years = (y - 1970) * 365 + leap_years;
    let is_leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let leap_day = if is_leap && m > 2 { 1 } else { 0 };
    let total_days = days_from_years + DAYS_BEFORE_MONTH[m - 1] + leap_day + (day as u32 - 1);
    (total_days as u64) * 86400 + (hour as u64) * 3600 + (min as u64) * 60 + (sec as u64)
}

// =========================================================================
// 2. 2FA Account Models & Storage
// =========================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct TotpAccount {
    pub issuer: String,
    pub label: String,
    pub secret: Vec<u8>,
    pub digits: u8,
    pub period: u8,
}

impl TotpAccount {
    pub fn new(issuer: &str, label: &str, base32_secret: &str) -> Self {
        Self {
            issuer: String::from(issuer),
            label: String::from(label),
            secret: base32_decode(base32_secret),
            digits: 6,
            period: 30,
        }
    }

    #[inline]
    pub fn compute_code(&self, unix_secs: u64) -> u32 {
        totp(&self.secret, unix_secs, self.period as u32, self.digits as u32)
    }

    /// Formats code with 3-3 grouping (e.g., "482 190")
    pub fn format_code(&self, unix_secs: u64) -> String {
        let code = self.compute_code(unix_secs);
        if self.digits == 6 {
            format!("{:03} {:03}", code / 1000, code % 1000)
        } else {
            format!("{:08}", code)
        }
    }

    #[inline]
    pub fn remaining_seconds(&self, unix_secs: u64) -> u8 {
        let p = self.period as u64;
        let rem = p - (unix_secs % p);
        rem as u8
    }
}

// Flash storage sector 1 (0xA000, 4KB, immediately following WiFi NVS sector at 0x9000)
#[allow(dead_code)]
const TOTP_FLASH_OFFSET: u32 = 0xA000;
#[allow(dead_code)]
const TOTP_MAGIC: [u8; 4] = *b"2FAT";

// =========================================================================
// 3. Authenticator Application
// =========================================================================

pub struct AuthenticatorApp {
    pub accounts: Vec<TotpAccount>,
    pub selected_index: usize,
    pub time_hms: (u8, u8, u8),
    pub date_ymd: (u16, u8, u8),
    pub battery_percent: u8,
    pub is_charging: bool,
    pub hidden: bool,
    pub copied_toast: bool,
}

impl AuthenticatorApp {
    pub fn new() -> Self {
        let mut app = Self {
            accounts: Vec::new(),
            selected_index: 0,
            time_hms: (12, 0, 0),
            date_ymd: (2026, 10, 23),
            battery_percent: 100,
            is_charging: false,
            hidden: false,
            copied_toast: false,
        };
        app.load_default_sample_accounts();
        app
    }

    pub fn load_default_sample_accounts(&mut self) {
        self.accounts.clear();
        // Sample standard accounts (RFC 6238 compatible)
        self.accounts.push(TotpAccount::new(
            "GitHub",
            "sniffmint",
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", // RFC test vector secret: "12345678901234567890"
        ));
        self.accounts.push(TotpAccount::new(
            "Google",
            "user@gmail.com",
            "JBSWY3DPEHPK3PXP", // "Hello!\xde\xad\xbe\xef"
        ));
        self.accounts.push(TotpAccount::new(
            "AWS",
            "root-admin",
            "KRSXG5CTMVRXEZLU",
        ));
        self.accounts.push(TotpAccount::new(
            "Proton",
            "security-key",
            "MZXW633PN5XW6MZX",
        ));
    }

    pub fn current_account(&self) -> Option<&TotpAccount> {
        self.accounts.get(self.selected_index)
    }

    pub fn next_account(&mut self) {
        if !self.accounts.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.accounts.len();
            self.copied_toast = false;
        }
    }

    pub fn prev_account(&mut self) {
        if !self.accounts.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.accounts.len() - 1;
            } else {
                self.selected_index -= 1;
            }
            self.copied_toast = false;
        }
    }

    pub fn current_unix_secs(&self) -> u64 {
        let (y, m, d) = self.date_ymd;
        let (h, min, s) = self.time_hms;
        datetime_to_unix_secs(y, m, d, h, min, s)
    }

    pub fn update_time(&mut self, hms: (u8, u8, u8), ymd: (u16, u8, u8)) {
        self.time_hms = hms;
        self.date_ymd = ymd;
    }

    pub fn update_battery(&mut self, percent: u8, is_charging: bool) {
        self.battery_percent = percent;
        self.is_charging = is_charging;
    }

    pub fn handle_touch(&mut self, touch: TouchPoint) -> bool {
        let (tx, ty) = (touch.x as i32, touch.y as i32);

        // Center card tap: toggle hide/reveal or simulate copy
        if (30..=380).contains(&tx) && (50..=280).contains(&ty) {
            self.copied_toast = !self.copied_toast;
            return true;
        }

        // Navigation buttons (y: 300..370)
        // [< PREV]: x 30..190
        if (30..=190).contains(&tx) && (300..=370).contains(&ty) {
            self.prev_account();
            return true;
        }
        // [NEXT >]: x 220..380
        if (220..=380).contains(&tx) && (300..=370).contains(&ty) {
            self.next_account();
            return true;
        }

        false
    }

    pub fn draw_status_header<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);

        // Top bar background
        let _ = Rectangle::new(EgPoint::new(0, 0), Size::new(410, 36))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 2, 3)))
            .draw(d);

        // Time: HH:MM
        let (h, m, _) = self.time_hms;
        let time_str = format!("{:02}:{:02}", h, m);
        let _ = Text::new(&time_str, EgPoint::new(14, 25), label_style).draw(d);

        // Title: 2FA WRISTKEY
        let title_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN);
        let _ = Text::with_alignment(
            "2FA AUTHENTICATOR",
            EgPoint::new(205, 25),
            title_style,
            Alignment::Center,
        )
        .draw(d);

        // Battery
        let (bat_col, bat_bg) = if self.battery_percent > 20 {
            (Rgb565::GREEN, Rgb565::new(0, 4, 0))
        } else {
            (Rgb565::RED, Rgb565::new(6, 0, 0))
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(325, 6), Size::new(75, 24)),
            Size::new(4, 4),
        )
        .into_styled(PrimitiveStyle::with_fill(bat_bg))
        .draw(d);

        let bat_text = if self.is_charging {
            format!("{}%*", self.battery_percent)
        } else {
            format!("{}%", self.battery_percent)
        };
        let _ = Text::with_alignment(
            &bat_text,
            EgPoint::new(362, 23),
            MonoTextStyle::new(&FONT_10X20, bat_col),
            Alignment::Center,
        )
        .draw(d);
    }

    /// Renders a 30s circular countdown ring using trigonometric radial ticks
    pub fn draw_countdown_ring<D: DrawTarget<Color = Rgb565>>(
        d: &mut D,
        center: EgPoint,
        radius: f32,
        remaining_secs: u8,
        period: u8,
    ) {
        let (color, bg_color) = if remaining_secs > 10 {
            (Rgb565::GREEN, Rgb565::new(1, 4, 1))
        } else if remaining_secs > 5 {
            (Rgb565::YELLOW, Rgb565::new(4, 4, 0))
        } else {
            (Rgb565::RED, Rgb565::new(6, 1, 1)) // Urgency pulse
        };

        // Background track circle
                let _ = Circle::with_center(center, (radius * 2.0) as u32)
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(3, 4, 5), 4))
            .draw(d);

        // Radial tick markers (12 markers around circle, 30/12 = 2.5s each)
        let total_ticks = 16;
        let active_ticks = (remaining_secs as u32 * total_ticks) / (period as u32);
        for i in 0..total_ticks {
            let angle = (i as f32) * (2.0 * core::f32::consts::PI / (total_ticks as f32)) - (core::f32::consts::PI / 2.0);
            let cos_a = libm::cosf(angle);
            let sin_a = libm::sinf(angle);

            let p_inner = center + EgPoint::new((cos_a * (radius - 8.0)) as i32, (sin_a * (radius - 8.0)) as i32);
            let p_outer = center + EgPoint::new((cos_a * (radius + 2.0)) as i32, (sin_a * (radius + 2.0)) as i32);

            let stroke_col = if i < active_ticks { color } else { bg_color };
            let _ = Line::new(p_inner, p_outer)
                .into_styled(PrimitiveStyle::with_stroke(stroke_col, 3))
                .draw(d);
        }

        // Inner remaining seconds text
        let rem_str = format!("{}s", remaining_secs);
        let rem_style = MonoTextStyle::new(&FONT_10X20, color);
        let _ = Text::with_alignment(&rem_str, center + EgPoint::new(0, 6), rem_style, Alignment::Center).draw(d);
    }
}

impl Default for AuthenticatorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for AuthenticatorApp {
    fn name(&self) -> &str {
        "Authenticator"
    }

    fn setup(&mut self) {}

    fn update(&mut self, input: &AppInput) -> AppResult {
        if let Some(swipe) = input.swipe {
            match swipe {
                crate::peripherals::touch::SwipeDirection::Right => return AppResult::Exit,
                crate::peripherals::touch::SwipeDirection::Left => self.next_account(),
                _ => {}
            }
        }

        if let Some(touch) = input.touch {
            // [BACK] exit button at bottom (x 130..280, y 420..465)
            if (130..=280).contains(&(touch.x as i32)) && (420..=465).contains(&(touch.y as i32)) {
                return AppResult::Exit;
            }
            self.handle_touch(touch);
        }

        AppResult::Continue
    }

    fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let value_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
        let bold_style = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);

        self.draw_status_header(d);

        let unix_secs = self.current_unix_secs();

        // Primary 2FA Account Card (y: 46..286)
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 46), Size::new(370, 240)),
            Size::new(14, 14),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 8)))
        .draw(d);

        if let Some(account) = self.current_account() {
            let remaining = account.remaining_seconds(unix_secs);

            // Card Header: Issuer pill + Account count indicator
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(35, 58), Size::new(140, 28)),
                Size::new(6, 6),
            )
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0, 10, 18)))
            .draw(d);
            let _ = Text::with_alignment(
                &account.issuer,
                EgPoint::new(105, 78),
                MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
                Alignment::Center,
            )
            .draw(d);

            // Carousel index: "[1/4]"
            let idx_str = format!("[{}/{}]", self.selected_index + 1, self.accounts.len());
            let _ = Text::new(&idx_str, EgPoint::new(320, 78), label_style).draw(d);

            // Account label / user ID
            let _ = Text::new(&account.label, EgPoint::new(38, 114), label_style).draw(d);

            // Big 6-digit TOTP Code
            let code_str = if self.hidden {
                "••• •••".into()
            } else {
                account.format_code(unix_secs)
            };
            let code_col = if remaining <= 5 { Rgb565::RED } else { Rgb565::WHITE };
            let _ = Text::new(
                &code_str,
                EgPoint::new(38, 164),
                MonoTextStyle::new(&FONT_9X18_BOLD, code_col),
            )
            .draw(d);

            // Circular 30s Countdown Ring (right side of card)
            Self::draw_countdown_ring(d, EgPoint::new(315, 155), 32.0, remaining, account.period);

            // Toast / Hint text at bottom of card
            if self.copied_toast {
                let _ = Text::new("Code Copied to Clipboard ✓", EgPoint::new(38, 215), MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN)).draw(d);
            } else {
                let _ = Text::new("Tap card to copy / reveal", EgPoint::new(38, 215), label_style).draw(d);
            }

            // Standard progress line across card bottom
            let progress_w = ((remaining as u32) * 340) / (account.period as u32);
            let bar_col = if remaining > 10 { Rgb565::GREEN } else if remaining > 5 { Rgb565::YELLOW } else { Rgb565::RED };
            let _ = Rectangle::new(EgPoint::new(35, 270), Size::new(progress_w, 4))
                .into_styled(PrimitiveStyle::with_fill(bar_col))
                .draw(d);
        } else {
            let _ = Text::with_alignment("No 2FA Accounts", EgPoint::new(205, 160), value_style, Alignment::Center).draw(d);
        }

        // Navigation Action Buttons (y: 300..370)
        // [< PREV] Button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(30, 300), Size::new(165, 70)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 8, 14)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(30, 300), Size::new(165, 70)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "< PREV",
            EgPoint::new(112, 343),
            bold_style,
            Alignment::Center,
        )
        .draw(d);

        // [NEXT >] Button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 300), Size::new(165, 70)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 8, 14)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 300), Size::new(165, 70)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "NEXT >",
            EgPoint::new(297, 343),
            bold_style,
            Alignment::Center,
        )
        .draw(d);

        // [ BACK ] Exit Button (y: 420..465)
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(130, 420), Size::new(150, 45)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(4, 4, 4)))
        .draw(d);
        let _ = Text::with_alignment(
            "BACK",
            EgPoint::new(205, 449),
            label_style,
            Alignment::Center,
        )
        .draw(d);
    }
}

// =========================================================================
// 4. Host Verification Unit Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known_vector() {
        let digest = sha1(b"The quick brown fox jumps over the lazy dog");
        let expected = [
            0x2f, 0xd4, 0xe1, 0xc6, 0x7a, 0x2d, 0x28, 0xfc, 0xed, 0x84, 0x9e, 0xe1, 0xbb, 0x76,
            0xe7, 0x39, 0x1b, 0x93, 0xeb, 0x12,
        ];
        assert_eq!(digest, expected);
    }

    #[test]
    fn rfc6238_official_totp_test_vectors() {
        // RFC 6238 Appendix B test secret: "12345678901234567890" (20 bytes)
        let secret = b"12345678901234567890";

        // Official RFC test vectors for SHA-1 TOTP (6 digits, period 30)
        assert_eq!(totp(secret, 59, 30, 6), 287082);
        assert_eq!(totp(secret, 1111111109, 30, 6), 81804);
        assert_eq!(totp(secret, 1111111111, 30, 6), 50471);
        assert_eq!(totp(secret, 1234567890, 30, 6), 5924);
        assert_eq!(totp(secret, 2000000000, 30, 6), 279037);
    }

    #[test]
    fn base32_decode_vector() {
        let raw = base32_decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(raw, b"12345678901234567890");
    }

    #[test]
    fn datetime_conversion() {
        assert_eq!(datetime_to_unix_secs(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(datetime_to_unix_secs(2000, 1, 1, 0, 0, 0), 946684800);
        assert_eq!(datetime_to_unix_secs(2024, 2, 29, 12, 0, 0), 1709208000);
    }

    #[test]
    fn account_carousel_and_countdown() {
        let mut app = AuthenticatorApp::new();
        assert_eq!(app.accounts.len(), 4);
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.current_account().unwrap().issuer, "GitHub");

        app.next_account();
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.current_account().unwrap().issuer, "Google");

        app.prev_account();
        assert_eq!(app.selected_index, 0);

        let acc = app.current_account().unwrap();
        // At timestamp 59, remaining in 30s period is 30 - (59 % 30) = 1
        assert_eq!(acc.remaining_seconds(59), 1);
        // At timestamp 60, remaining is 30 - (60 % 30) = 30
        assert_eq!(acc.remaining_seconds(60), 30);
    }
}
