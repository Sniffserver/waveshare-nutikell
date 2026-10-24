// Work Tracker App: Track billable hours, active sessions, breaks, and earnings.

use alloc::format;
use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::apps::{App, AppInput, AppResult};
use crate::peripherals::touch::TouchPoint;

pub const PRESET_RATES: [u32; 6] = [2500, 3500, 5000, 7500, 10000, 15000]; // in cents ($25, $35, $50, $75, $100, $150)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerState {
    Idle,
    Working,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkSession {
    /// Local calendar date when the session was started.
    pub start_date_ymd: (u16, u8, u8),
    pub start_time_hms: (u8, u8, u8),
    pub elapsed_secs: u32,
    pub break_secs: u32,
    pub hourly_rate_cents: u32,
}

impl WorkSession {
    pub fn new(
        start_date_ymd: (u16, u8, u8),
        start_time_hms: (u8, u8, u8),
        hourly_rate_cents: u32,
    ) -> Self {
        Self {
            start_date_ymd,
            start_time_hms,
            elapsed_secs: 0,
            break_secs: 0,
            hourly_rate_cents,
        }
    }

    #[inline]
    pub fn earned_cents(&self) -> u32 {
        ((self.elapsed_secs as u64 * self.hourly_rate_cents as u64) / 3600) as u32
    }
}

pub struct WorkTracker {
    pub state: TrackerState,
    pub active_session: Option<WorkSession>,
    pub hourly_rate_cents: u32,
    pub today_total_secs: u32,
    pub today_earnings_cents: u32,
    pub completed_sessions_today: u16,
    pub time_hms: (u8, u8, u8),
    pub date_ymd: (u16, u8, u8),
    pub battery_percent: u8,
    pub is_charging: bool,
    /// Set by the storage layer before clock-in to preserve existing history.
    pub start_allowed: bool,
    /// Last completed session, retained until flash storage confirms it.
    pub pending_session: Option<WorkSession>,
}

impl WorkTracker {
    pub fn new() -> Self {
        Self {
            state: TrackerState::Idle,
            active_session: None,
            hourly_rate_cents: 5000, // $50.00 / hr
            today_total_secs: 0,
            today_earnings_cents: 0,
            completed_sessions_today: 0,
            time_hms: (12, 0, 0),
            date_ymd: (2000, 1, 1),
            battery_percent: 100,
            is_charging: false,
            start_allowed: true,
            pending_session: None,
        }
    }

    pub fn with_rate(hourly_rate_cents: u32) -> Self {
        let mut tracker = Self::new();
        tracker.hourly_rate_cents = hourly_rate_cents;
        tracker
    }

    pub fn clock_in(&mut self) {
        if self.state == TrackerState::Idle && self.pending_session.is_none() && self.start_allowed
        {
            self.active_session = Some(WorkSession::new(
                self.date_ymd,
                self.time_hms,
                self.hourly_rate_cents,
            ));
            self.state = TrackerState::Working;
        }
    }

    /// Restore a session recovered from the dedicated active-session record.
    /// It always resumes as working; no completed log entry is created here.
    pub fn restore_active_session(&mut self, session: WorkSession, elapsed_secs: u32) {
        self.hourly_rate_cents = session.hourly_rate_cents;
        self.active_session = Some(WorkSession {
            elapsed_secs,
            ..session
        });
        self.state = TrackerState::Working;
    }

    pub fn pause(&mut self) {
        if self.state == TrackerState::Working {
            self.state = TrackerState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == TrackerState::Paused {
            self.state = TrackerState::Working;
        }
    }

    pub fn clock_out(&mut self) -> Option<WorkSession> {
        if let Some(session) = self.active_session.take() {
            let earned = session.earned_cents();
            self.today_total_secs = self.today_total_secs.saturating_add(session.elapsed_secs);
            self.today_earnings_cents = self.today_earnings_cents.saturating_add(earned);
            self.completed_sessions_today = self.completed_sessions_today.saturating_add(1);
            self.state = TrackerState::Idle;
            self.pending_session = Some(session);
            Some(session)
        } else {
            self.state = TrackerState::Idle;
            None
        }
    }

    /// The completed session remains pending until durable storage confirms it.
    pub fn pending_completed_session(&self) -> Option<WorkSession> {
        self.pending_session
    }

    pub fn confirm_completed_session_saved(&mut self) {
        self.pending_session = None;
    }

    pub fn tick_1s(&mut self) {
        if let Some(ref mut session) = self.active_session {
            match self.state {
                TrackerState::Working => {
                    session.elapsed_secs = session.elapsed_secs.saturating_add(1);
                }
                TrackerState::Paused => {
                    session.break_secs = session.break_secs.saturating_add(1);
                }
                TrackerState::Idle => {}
            }
        }
    }

    pub fn cycle_rate(&mut self) {
        let idx = PRESET_RATES
            .iter()
            .position(|&r| r == self.hourly_rate_cents)
            .unwrap_or(0);
        let next_idx = (idx + 1) % PRESET_RATES.len();
        self.hourly_rate_cents = PRESET_RATES[next_idx];
        // A running session keeps the rate it started with. Changing the
        // preference affects the next clock-in and never reprices work that
        // has already elapsed.
    }

    pub fn current_session_earnings(&self) -> u32 {
        self.active_session.as_ref().map_or(0, |s| s.earned_cents())
    }

    pub fn update_time(&mut self, hms: (u8, u8, u8)) {
        self.time_hms = hms;
    }

    pub fn update_date(&mut self, ymd: (u16, u8, u8)) {
        self.date_ymd = ymd;
    }

    pub fn update_battery(&mut self, percent: u8, is_charging: bool) {
        self.battery_percent = percent;
        self.is_charging = is_charging;
    }

    pub fn set_start_allowed(&mut self, allowed: bool) {
        self.start_allowed = allowed;
    }

    pub fn handle_touch(&mut self, touch: TouchPoint) -> bool {
        let (tx, ty) = (touch.x as i32, touch.y as i32);

        // Header rate tap (tap on rate pill: x 230..390, y 100..150)
        if (230..=390).contains(&tx) && (100..=150).contains(&ty) {
            self.cycle_rate();
            return true;
        }

        // Action buttons area (y: 330..410)
        match self.state {
            TrackerState::Idle => {
                // [ CLOCK IN ] Button: x 50..360, y 330..400
                if (50..=360).contains(&tx) && (330..=400).contains(&ty) {
                    self.clock_in();
                    return true;
                }
            }
            TrackerState::Working => {
                // [ PAUSE ] Button: x 30..195, y 330..400
                if (30..=195).contains(&tx) && (330..=400).contains(&ty) {
                    self.pause();
                    return true;
                }
                // [ STOP / CLOCK OUT ]: x 215..380, y 330..400
                if (215..=380).contains(&tx) && (330..=400).contains(&ty) {
                    self.clock_out();
                    return true;
                }
            }
            TrackerState::Paused => {
                // [ RESUME ] Button: x 30..195, y 330..400
                if (30..=195).contains(&tx) && (330..=400).contains(&ty) {
                    self.resume();
                    return true;
                }
                // [ STOP / CLOCK OUT ]: x 215..380, y 330..400
                if (215..=380).contains(&tx) && (330..=400).contains(&ty) {
                    self.clock_out();
                    return true;
                }
            }
        }

        false
    }

    pub fn draw_status_header<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);

        // Background top bar
        let _ = Rectangle::new(EgPoint::new(0, 0), Size::new(410, 36))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 2, 3)))
            .draw(d);

        // Time: HH:MM
        let (h, m, _) = self.time_hms;
        let time_str = format!("{:02}:{:02}", h, m);
        let _ = Text::new(&time_str, EgPoint::new(14, 25), label_style).draw(d);

        // Title: WORK TRACKER
        let title_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN);
        let _ = Text::with_alignment(
            "WORK TRACKER",
            EgPoint::new(205, 25),
            title_style,
            Alignment::Center,
        )
        .draw(d);

        // Battery: [85% ⚡]
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
}

impl Default for WorkTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl App for WorkTracker {
    fn name(&self) -> &str {
        "Work Tracker"
    }

    fn setup(&mut self) {}

    fn update(&mut self, input: &AppInput) -> AppResult {
        if let Some(swipe) = input.swipe {
            if swipe == crate::peripherals::touch::SwipeDirection::Right {
                return AppResult::Exit;
            }
        }

        if let Some(touch) = input.touch {
            // Exit back button at bottom (x 120..290, y 420..465)
            if (120..=290).contains(&(touch.x as i32)) && (420..=465).contains(&(touch.y as i32)) {
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

        // Status Card (y: 46..220)
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 46), Size::new(370, 175)),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 6)))
        .draw(d);

        // State Badge (top-left of card)
        let (status_str, status_color, status_bg) = match self.state {
            TrackerState::Idle => ("IDLE", Rgb565::CSS_GRAY, Rgb565::new(4, 4, 4)),
            TrackerState::Working => ("WORKING", Rgb565::GREEN, Rgb565::new(0, 10, 2)),
            TrackerState::Paused => ("PAUSED", Rgb565::YELLOW, Rgb565::new(10, 8, 0)),
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(35, 60), Size::new(110, 30)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(status_bg))
        .draw(d);
        let _ = Text::with_alignment(
            status_str,
            EgPoint::new(90, 81),
            MonoTextStyle::new(&FONT_10X20, status_color),
            Alignment::Center,
        )
        .draw(d);

        // Hourly Rate Pill (top-right of card, tap to cycle)
        let rate_dollars = self.hourly_rate_cents / 100;
        let rate_str = format!("${}/h", rate_dollars);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(245, 60), Size::new(130, 30)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(4, 6, 12)))
        .draw(d);
        let _ = Text::with_alignment(
            &rate_str,
            EgPoint::new(310, 81),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
            Alignment::Center,
        )
        .draw(d);

        // Big Elapsed Timer
        let elapsed = self.active_session.map_or(0, |s| s.elapsed_secs);
        let hrs = elapsed / 3600;
        let mins = (elapsed % 3600) / 60;
        let secs = elapsed % 60;
        let time_str = format!("{:02}:{:02}:{:02}", hrs, mins, secs);

        let _ = Text::new(&time_str, EgPoint::new(40, 140), bold_style).draw(d);

        // Live Earnings
        let earned_cents = self.current_session_earnings();
        let earned_str = format!("${}.{:02}", earned_cents / 100, earned_cents % 100);
        let _ = Text::new(
            &earned_str,
            EgPoint::new(240, 140),
            MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN),
        )
        .draw(d);

        // Secondary Info: Break time / start time
        let break_secs = self.active_session.map_or(0, |s| s.break_secs);
        let info_str = if self.state != TrackerState::Idle {
            format!("Break: {}m {:02}s", break_secs / 60, break_secs % 60)
        } else {
            if self.pending_session.is_some() {
                "SAVE FAILED — retry pending".into()
            } else if !self.start_allowed {
                "LOG FULL — export and clear".into()
            } else {
                "Tap [CLOCK IN] to start".into()
            }
        };
        let _ = Text::new(&info_str, EgPoint::new(40, 195), label_style).draw(d);

        // Today Summary Card (y: 232..320)
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 232), Size::new(370, 88)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 3, 4)))
        .draw(d);

        let total_today_hrs = self.today_total_secs / 3600;
        let total_today_mins = (self.today_total_secs % 3600) / 60;
        let today_time_str = format!("Today: {}h {:02}m", total_today_hrs, total_today_mins);
        let today_earn_str = format!(
            "Total: ${}.{:02}",
            self.today_earnings_cents / 100,
            self.today_earnings_cents % 100
        );
        let today_sess_str = format!("Sessions: {}", self.completed_sessions_today);

        let _ = Text::new(&today_time_str, EgPoint::new(35, 262), value_style).draw(d);
        let _ = Text::new(
            &today_earn_str,
            EgPoint::new(35, 298),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
        )
        .draw(d);
        let _ = Text::new(&today_sess_str, EgPoint::new(240, 262), label_style).draw(d);

        // Action Buttons (y: 332..402)
        match self.state {
            TrackerState::Idle => {
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(40, 332), Size::new(330, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0, 16, 4)))
                .draw(d);
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(40, 332), Size::new(330, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 2))
                .draw(d);
                let _ = Text::with_alignment(
                    "CLOCK IN",
                    EgPoint::new(205, 375),
                    bold_style,
                    Alignment::Center,
                )
                .draw(d);
            }
            TrackerState::Working => {
                // [ PAUSE ]
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(30, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::new(14, 10, 0)))
                .draw(d);
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(30, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
                .draw(d);
                let _ = Text::with_alignment(
                    "PAUSE",
                    EgPoint::new(112, 375),
                    bold_style,
                    Alignment::Center,
                )
                .draw(d);

                // [ CLOCK OUT ]
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(215, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::new(16, 2, 2)))
                .draw(d);
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(215, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::RED, 2))
                .draw(d);
                let _ = Text::with_alignment(
                    "CLOCK OUT",
                    EgPoint::new(297, 375),
                    bold_style,
                    Alignment::Center,
                )
                .draw(d);
            }
            TrackerState::Paused => {
                // [ RESUME ]
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(30, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0, 14, 2)))
                .draw(d);
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(30, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 2))
                .draw(d);
                let _ = Text::with_alignment(
                    "RESUME",
                    EgPoint::new(112, 375),
                    bold_style,
                    Alignment::Center,
                )
                .draw(d);

                // [ CLOCK OUT ]
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(215, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::new(16, 2, 2)))
                .draw(d);
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(EgPoint::new(215, 332), Size::new(165, 70)),
                    Size::new(10, 10),
                )
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::RED, 2))
                .draw(d);
                let _ = Text::with_alignment(
                    "CLOCK OUT",
                    EgPoint::new(297, 375),
                    bold_style,
                    Alignment::Center,
                )
                .draw(d);
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_idle() {
        let tracker = WorkTracker::new();
        assert_eq!(tracker.state, TrackerState::Idle);
        assert!(tracker.active_session.is_none());
        assert_eq!(tracker.hourly_rate_cents, 5000);
        assert_eq!(tracker.today_total_secs, 0);
        assert_eq!(tracker.today_earnings_cents, 0);
        assert_eq!(tracker.completed_sessions_today, 0);
    }

    #[test]
    fn clock_in_and_accumulate_time_and_earnings() {
        let mut tracker = WorkTracker::with_rate(6000); // $60.00 / hr ($1.00 / min, 1.666 cents / s)
        tracker.update_time((9, 0, 0));
        tracker.clock_in();

        assert_eq!(tracker.state, TrackerState::Working);
        assert!(tracker.active_session.is_some());

        // Tick 3600 seconds (1 hour)
        for _ in 0..3600 {
            tracker.tick_1s();
        }

        assert_eq!(tracker.active_session.unwrap().elapsed_secs, 3600);
        assert_eq!(tracker.current_session_earnings(), 6000); // Exactly $60.00

        // Pause for 60 seconds
        tracker.pause();
        assert_eq!(tracker.state, TrackerState::Paused);
        for _ in 0..60 {
            tracker.tick_1s();
        }
        // Elapsed should still be 3600, break 60, earnings still $60.00
        assert_eq!(tracker.active_session.unwrap().elapsed_secs, 3600);
        assert_eq!(tracker.active_session.unwrap().break_secs, 60);
        assert_eq!(tracker.current_session_earnings(), 6000);

        // Resume and tick 1800 seconds (30 mins = $30.00)
        tracker.resume();
        assert_eq!(tracker.state, TrackerState::Working);
        for _ in 0..1800 {
            tracker.tick_1s();
        }
        assert_eq!(tracker.active_session.unwrap().elapsed_secs, 5400);
        assert_eq!(tracker.current_session_earnings(), 9000); // $90.00

        // Clock out
        let session = tracker.clock_out().expect("session should be returned");
        assert_eq!(session.elapsed_secs, 5400);
        assert_eq!(session.break_secs, 60);
        assert_eq!(tracker.state, TrackerState::Idle);
        assert!(tracker.active_session.is_none());
        assert_eq!(tracker.today_total_secs, 5400);
        assert_eq!(tracker.today_earnings_cents, 9000);
        assert_eq!(tracker.completed_sessions_today, 1);
    }

    #[test]
    fn rate_cycling_and_preset_rates() {
        let mut tracker = WorkTracker::with_rate(2500);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 3500);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 5000);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 7500);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 10000);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 15000);
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 2500);
    }

    #[test]
    fn changing_default_rate_does_not_reprice_active_session() {
        let mut tracker = WorkTracker::with_rate(5000);
        tracker.clock_in();
        tracker.cycle_rate();
        assert_eq!(tracker.hourly_rate_cents, 7500);
        assert_eq!(
            tracker
                .active_session
                .expect("active session")
                .hourly_rate_cents,
            5000
        );
    }

    #[test]
    fn recovered_session_resumes_with_saved_rate_and_elapsed_time() {
        let mut tracker = WorkTracker::with_rate(7500);
        let session = WorkSession::new((2026, 10, 24), (9, 0, 0), 5000);
        tracker.restore_active_session(session, 1_800);

        assert_eq!(tracker.state, TrackerState::Working);
        assert_eq!(tracker.current_session_earnings(), 2500);
        assert_eq!(
            tracker
                .active_session
                .expect("restored session")
                .start_date_ymd,
            (2026, 10, 24)
        );
    }

    #[test]
    fn clock_in_is_refused_when_log_is_full() {
        let mut tracker = WorkTracker::new();
        tracker.set_start_allowed(false);
        tracker.clock_in();
        assert_eq!(tracker.state, TrackerState::Idle);
        assert!(tracker.active_session.is_none());
    }

    #[test]
    fn touch_handling_state_transitions() {
        let mut tracker = WorkTracker::new();
        // Tap clock in (x: 200, y: 350)
        let handled = tracker.handle_touch(TouchPoint { x: 200, y: 350 });
        assert!(handled);
        assert_eq!(tracker.state, TrackerState::Working);

        // Tap pause (x: 100, y: 350)
        let handled = tracker.handle_touch(TouchPoint { x: 100, y: 350 });
        assert!(handled);
        assert_eq!(tracker.state, TrackerState::Paused);

        // Tap resume (x: 100, y: 350)
        let handled = tracker.handle_touch(TouchPoint { x: 100, y: 350 });
        assert!(handled);
        assert_eq!(tracker.state, TrackerState::Working);

        // Tap clock out (x: 300, y: 350)
        let handled = tracker.handle_touch(TouchPoint { x: 300, y: 350 });
        assert!(handled);
        assert_eq!(tracker.state, TrackerState::Idle);
        assert_eq!(tracker.completed_sessions_today, 1);
    }
}
