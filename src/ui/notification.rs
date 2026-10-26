// Pop-up Notification Overlay Engine
// Displays animated modal notification cards over any screen (Watchface, Outdoor, Apps)
// Features:
//   - Urgency levels (Silent, Normal, Critical)
//   - Category-specific color accents (Messages = Green, Calendar = Yellow, Security = Red, Slack = Cyan)
//   - Smooth slide-in animation from top of screen
//   - Auto-dismiss countdown with pause on touch
//   - Dismissible via tap on [✕] or swipe up
//   - I2S chime alert trigger

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::peripherals::touch::TouchPoint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    Silent,   // Visual card only, no sound
    Normal,   // Chime alert, auto-dismisses after 5s
    Critical, // Chime alert, stays on screen until dismissed by user
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub timestamp_hms: (u8, u8, u8),
    pub urgency: Urgency,
    pub accent_color: Rgb565,
}

impl Notification {
    pub fn new(
        id: u32,
        app_name: &str,
        title: &str,
        body: &str,
        timestamp_hms: (u8, u8, u8),
        urgency: Urgency,
    ) -> Self {
        let accent_color = match app_name.to_ascii_lowercase().as_str() {
            "messages" | "whatsapp" | "sms" | "imessage" => Rgb565::GREEN,
            "calendar" | "reminder" | "alarm" => Rgb565::YELLOW,
            "security" | "urgent" | "call" | "emergency" => Rgb565::RED,
            "slack" | "github" | "telegram" | "email" => Rgb565::CYAN,
            "soundcloud" | "music" | "spotify" => Rgb565::new(31, 21, 0), // Vibrant Orange
            _ => Rgb565::new(10, 20, 31),                                 // Ice blue
        };

        Self {
            id,
            app_name: String::from(app_name),
            title: String::from(title),
            body: String::from(body),
            timestamp_hms,
            urgency,
            accent_color,
        }
    }
}

pub struct NotificationManager {
    pub active: Option<Notification>,
    pub display_timeout_ms: u32,
    pub elapsed_ms: u32,
    pub anim_y: i32,
    pub target_y: i32,
    pub pending_chime: bool,
    pub history: Vec<Notification>,
    pub unread_count: usize,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            active: None,
            display_timeout_ms: 5500, // 5.5 seconds visible
            elapsed_ms: 0,
            anim_y: -140, // start hidden above top of screen
            target_y: 12, // rest position: 12px from top
            pending_chime: false,
            history: Vec::new(),
            unread_count: 0,
        }
    }

    pub fn history_capacity(&self) -> usize {
        self.history.capacity()
    }

    /// Pushes a new incoming notification, triggers slide-in and chime
    pub fn push(&mut self, notif: Notification) {
        if notif.urgency != Urgency::Silent {
            self.pending_chime = true;
        }
        self.elapsed_ms = 0;
        self.anim_y = -140; // reset slide-in animation
        self.target_y = 12;

        if self.history.len() >= 10 {
            self.history.remove(0);
        }
        self.history.push(notif.clone());
        self.unread_count = self.unread_count.saturating_add(1);
        self.active = Some(notif);
    }

    /// Advances animation and auto-dismiss timer
    pub fn tick(&mut self, dt_ms: u32) {
        if self.active.is_none() {
            return;
        }

        // Animate slide-in: smoothly move from -140 to 12
        if self.anim_y < self.target_y {
            let step = (dt_ms as i32) * 2; // ~200px per 100ms
            self.anim_y = (self.anim_y + step).min(self.target_y);
        }

        self.elapsed_ms = self.elapsed_ms.saturating_add(dt_ms);

        // Auto-dismiss if not Critical
        if let Some(ref notif) = self.active {
            if notif.urgency != Urgency::Critical && self.elapsed_ms >= self.display_timeout_ms {
                self.dismiss();
            }
        }
    }

    /// Checks and clears pending chime flag
    pub fn take_pending_chime(&mut self) -> bool {
        let flag = self.pending_chime;
        self.pending_chime = false;
        flag
    }

    /// Manually dismiss the current pop-up modal
    pub fn dismiss(&mut self) {
        self.active = None;
        self.anim_y = -140;
        self.elapsed_ms = 0;
    }

    #[inline]
    pub fn is_visible(&self) -> bool {
        self.active.is_some() && self.anim_y > -130
    }

    /// Handles touch interaction with the modal card
    pub fn handle_touch(&mut self, touch: TouchPoint) -> bool {
        if !self.is_visible() {
            return false;
        }

        let (tx, ty) = (touch.x as i32, touch.y as i32);
        let card_top = self.anim_y;
        let card_bottom = card_top + 120;

        // Check if touch is inside the notification card (x: 15..395, y: card_top..card_bottom)
        if (15..=395).contains(&tx) && (card_top..=card_bottom).contains(&ty) {
            // Dismiss button on top right (x: 340..395, y: card_top..card_top + 40)
            // or tapping anywhere on the card to acknowledge
            self.dismiss();
            return true;
        }

        false
    }

    /// Renders the floating pop-up card overlay onto the framebuffer
    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let notif = match self.active {
            Some(ref n) => n,
            None => return,
        };

        let card_x = 18;
        let card_y = self.anim_y;
        let card_w = 374;
        let card_h = 122;

        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let title_style = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
        let body_style = MonoTextStyle::new(&FONT_10X20, Rgb565::new(26, 28, 30));

        // 1. Semi-translucent dark shadow / background panel
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(card_x, card_y), Size::new(card_w, card_h)),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 2, 4))) // Obsidian / Deep Navy
        .draw(d);

        // 2. Colored Accent Border
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(card_x, card_y), Size::new(card_w, card_h)),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_stroke(notif.accent_color, 2))
        .draw(d);

        // 3. Header: App Badge Pill
        let badge_bg = match notif.accent_color {
            Rgb565::GREEN => Rgb565::new(0, 6, 2),
            Rgb565::RED => Rgb565::new(6, 1, 1),
            Rgb565::YELLOW => Rgb565::new(5, 5, 0),
            _ => Rgb565::new(1, 6, 12),
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(card_x + 12, card_y + 10), Size::new(125, 24)),
            Size::new(5, 5),
        )
        .into_styled(PrimitiveStyle::with_fill(badge_bg))
        .draw(d);

        let app_label = notif.app_name.to_uppercase();
        let _ = Text::with_alignment(
            &app_label,
            EgPoint::new(card_x + 74, card_y + 27),
            MonoTextStyle::new(&FONT_10X20, notif.accent_color),
            Alignment::Center,
        )
        .draw(d);

        // 4. Timestamp: "14:28"
        let (h, m, _) = notif.timestamp_hms;
        let time_str = format!("{:02}:{:02}", h, m);
        let _ = Text::new(
            &time_str,
            EgPoint::new(card_x + 270, card_y + 27),
            label_style,
        )
        .draw(d);

        // 5. Dismiss [✕] button indicator
        let _ = Text::new(
            "✕",
            EgPoint::new(card_x + 342, card_y + 27),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY),
        )
        .draw(d);

        // 6. Title
        let _ = Text::new(
            &notif.title,
            EgPoint::new(card_x + 14, card_y + 60),
            title_style,
        )
        .draw(d);

        // 7. Body snippet (truncated if too long to fit cleanly)
        let body_preview = if notif.body.len() > 34 {
            let mut s = String::from(&notif.body[..31]);
            s.push_str("...");
            s
        } else {
            notif.body.clone()
        };
        let _ = Text::new(
            &body_preview,
            EgPoint::new(card_x + 14, card_y + 88),
            body_style,
        )
        .draw(d);

        // 8. Progress timeout bar at bottom of card
        if notif.urgency != Urgency::Critical && self.display_timeout_ms > 0 {
            let remaining_ms = self.display_timeout_ms.saturating_sub(self.elapsed_ms);
            let bar_w = ((remaining_ms as u32) * (card_w - 24)) / self.display_timeout_ms;
            let _ = Line::new(
                EgPoint::new(card_x + 12, card_y + card_h as i32 - 4),
                EgPoint::new(card_x + 12 + bar_w as i32, card_y + card_h as i32 - 4),
            )
            .into_styled(PrimitiveStyle::with_stroke(notif.accent_color, 2))
            .draw(d);
        }
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_notification_sets_active_and_chime() {
        let mut mgr = NotificationManager::new();
        assert!(mgr.active.is_none());
        assert!(!mgr.is_visible());

        let notif = Notification::new(
            1,
            "WhatsApp",
            "Alice Smith",
            "Are we meeting at 3pm?",
            (14, 30, 0),
            Urgency::Normal,
        );
        mgr.push(notif);

        assert!(mgr.active.is_some());
        assert_eq!(mgr.unread_count, 1);
        assert_eq!(mgr.history.len(), 1);
        assert!(
            mgr.take_pending_chime(),
            "normal notification must request chime"
        );
        assert!(!mgr.take_pending_chime(), "chime flag should be consumed");
    }

    #[test]
    fn silent_notification_does_not_chime() {
        let mut mgr = NotificationManager::new();
        let notif = Notification::new(
            2,
            "Calendar",
            "Team Sync in 15m",
            "Conference Room A",
            (9, 45, 0),
            Urgency::Silent,
        );
        mgr.push(notif);
        assert!(!mgr.take_pending_chime());
    }

    #[test]
    fn auto_dismiss_after_timeout() {
        let mut mgr = NotificationManager::new();
        let notif = Notification::new(
            3,
            "Messages",
            "Bob",
            "On my way!",
            (12, 0, 0),
            Urgency::Normal,
        );
        mgr.push(notif);

        // Advance 2000 ms (2s)
        mgr.tick(2000);
        assert!(mgr.active.is_some());
        assert!(mgr.is_visible());

        // Advance another 4000 ms (total 6000ms > 5500ms timeout)
        mgr.tick(4000);
        assert!(mgr.active.is_none(), "should auto-dismiss after timeout");
        assert!(!mgr.is_visible());
    }

    #[test]
    fn critical_notification_does_not_auto_dismiss() {
        let mut mgr = NotificationManager::new();
        let notif = Notification::new(
            4,
            "Security",
            "Server Down",
            "Production outage detected",
            (18, 0, 0),
            Urgency::Critical,
        );
        mgr.push(notif);

        mgr.tick(10000); // 10s elapsed
        assert!(
            mgr.active.is_some(),
            "critical notification must not auto-dismiss"
        );

        // Dismiss via touch
        let handled = mgr.handle_touch(TouchPoint { x: 50, y: 30 });
        assert!(handled);
        assert!(mgr.active.is_none());
    }
}
