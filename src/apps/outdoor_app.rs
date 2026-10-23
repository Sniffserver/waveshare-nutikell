// Outdoor App: Pedometer, Artificial Horizon (Level), and Hike Live telemetry.

use alloc::format;
use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::outdoor::state::{HikeStatus, OutdoorState, TelemetryState};
use crate::outdoor::steps::MotionState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutdoorScreen {
    Home,
    HikeLive,
    Level,
    Sun,
}

pub struct OutdoorApp {
    pub screen: OutdoorScreen,
}

impl OutdoorApp {
    pub fn new() -> Self {
        Self {
            screen: OutdoorScreen::Home,
        }
    }

    pub fn setup(&mut self) {
        // Default to Home screen unless a hike is currently running
        if self.screen != OutdoorScreen::HikeLive {
            self.screen = OutdoorScreen::Home;
        }
    }

    /// Handle tap event. Returns true if consumed.
    pub fn handle_tap(&mut self, x: u16, y: u16, state: &mut OutdoorState) -> bool {
        match self.screen {
            OutdoorScreen::Home => {
                // [START HIKE] / [CONTINUE HIKE] button: y in 385..455, x in 20..390
                if y >= 385 && y <= 455 && x >= 20 && x <= 390 {
                    if state.hike.status == HikeStatus::Stopped {
                        state.start_hike();
                    }
                    self.screen = OutdoorScreen::HikeLive;
                    return true;
                }

                // [LEVEL] button: y in 310..370, x in 20..195
                if y >= 310 && y <= 370 && x >= 20 && x <= 195 {
                    self.screen = OutdoorScreen::Level;
                    return true;
                }

                // [SUN] button: y in 310..370, x in 215..390
                if y >= 310 && y <= 370 && x >= 215 && x <= 390 {
                    self.screen = OutdoorScreen::Sun;
                    return true;
                }
            }

            OutdoorScreen::HikeLive => {
                // [PAUSE] / [RESUME] button: y in 385..455, x in 20..195
                if y >= 385 && y <= 455 && x >= 20 && x <= 195 {
                    state.pause_hike();
                    return true;
                }

                // [STOP] button: y in 385..455, x in 215..390
                if y >= 385 && y <= 455 && x >= 215 && x <= 390 {
                    state.stop_hike();
                    self.screen = OutdoorScreen::Home;
                    return true;
                }

                // [LEVEL] button shortcut: y in 320..370, x in 20..195
                if y >= 320 && y <= 370 && x >= 20 && x <= 195 {
                    self.screen = OutdoorScreen::Level;
                    return true;
                }

                // [BACK TO HOME] shortcut: top header tap (y < 45)
                if y < 45 {
                    self.screen = OutdoorScreen::Home;
                    return true;
                }
            }

            OutdoorScreen::Level => {
                // [BACK] button: y in 395..455, x in 100..310
                if y >= 395 && y <= 455 && x >= 100 && x <= 310 {
                    self.screen = if state.hike.status == HikeStatus::Running {
                        OutdoorScreen::HikeLive
                    } else {
                        OutdoorScreen::Home
                    };
                    return true;
                }
            }

            OutdoorScreen::Sun => {
                // [BACK] button: y in 395..455, x in 100..310
                if y >= 395 && y <= 455 && x >= 100 && x <= 310 {
                    self.screen = OutdoorScreen::Home;
                    return true;
                }
            }
        }

        false
    }

    /// Render the current outdoor screen into framebuffer.
    pub fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, state: &OutdoorState) {
        // Clear background with deep dark slate
        let _ = Rectangle::new(EgPoint::zero(), Size::new(410, 502))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 2, 2)))
            .draw(d);

        match self.screen {
            OutdoorScreen::Home => self.render_home(d, state),
            OutdoorScreen::HikeLive => self.render_hike_live(d, state),
            OutdoorScreen::Level => self.render_level(d, state),
            OutdoorScreen::Sun => self.render_sun(d, state),
        }
    }

        fn draw_status_header<D: DrawTarget<Color = Rgb565>>(
        d: &mut D,
        state: &OutdoorState,
        title: &str,
        title_color: Rgb565,
    ) {
        // Digital Clock (Left: x=20)
        let time_str = format!("{:02}:{:02}", state.time_hms.0, state.time_hms.1);
        let _ = Text::new(
            &time_str,
            EgPoint::new(20, 28),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
        )
        .draw(d);

        // Screen Title (Center: x=205)
        let _ = Text::with_alignment(
            title,
            EgPoint::new(205, 28),
            MonoTextStyle::new(&FONT_10X20, title_color),
            Alignment::Center,
        )
        .draw(d);

        // Battery Indicator (Right: x=390, right-aligned)
        let pct = state.battery_percent;
        let (bat_color, bat_str) = if state.is_charging {
            (Rgb565::GREEN, format!("⚡{}%", pct))
        } else if state.vbus_present {
            (Rgb565::YELLOW, format!("⚡{}%", pct))
        } else if pct > 50 {
            (Rgb565::GREEN, format!("{}%", pct))
        } else if pct > 20 {
            (Rgb565::YELLOW, format!("{}%", pct))
        } else {
            (Rgb565::RED, format!("{}%", pct))
        };
        let _ = Text::with_alignment(
            &bat_str,
            EgPoint::new(390, 28),
            MonoTextStyle::new(&FONT_10X20, bat_color),
            Alignment::Right,
        )
        .draw(d);
    }

    fn render_home<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, state: &OutdoorState) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let value_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);

        Self::draw_status_header(d, state, "OUTDOOR", Rgb565::CYAN);

        // Status pills: GPS & BLE
        // GPS Pill
        let (gps_text, gps_color) = match state.telemetry {
            TelemetryState::Fresh => {
                let acc = state.accuracy_m.unwrap_or(5.0);
                (format!("GPS ±{:.0}m", acc), Rgb565::GREEN)
            }
            TelemetryState::Stale => {
                (format!("GPS STALE ({}s)", state.fix_age_secs), Rgb565::YELLOW)
            }
            TelemetryState::Invalid => {
                if state.ble_connected {
                    (format!("GPS NO FIX"), Rgb565::RED)
                } else {
                    (format!("GPS NO BLE"), Rgb565::CSS_GRAY)
                }
            }
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 42), Size::new(175, 32)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 6)))
        .draw(d);
        let _ = Text::with_alignment(
            &gps_text,
            EgPoint::new(107, 64),
            MonoTextStyle::new(&FONT_10X20, gps_color),
            Alignment::Center,
        )
        .draw(d);

        // BLE Pill
        let (ble_text, ble_color) = if state.ble_connected {
            ("BLE LINKED", Rgb565::GREEN)
        } else {
            ("BLE DISCONNECTED", Rgb565::CSS_GRAY)
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 42), Size::new(175, 32)),
            Size::new(6, 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 6)))
        .draw(d);
        let _ = Text::with_alignment(
            ble_text,
            EgPoint::new(302, 64),
            MonoTextStyle::new(&FONT_10X20, ble_color),
            Alignment::Center,
        )
        .draw(d);

        // Telemetry Card 1: Distance & Speed
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 84), Size::new(370, 70)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 5, 8)))
        .draw(d);
        let dist_km = state.distance_m / 1000.0;
        let speed_kmh = state.speed_mps * 3.6;
        let dist_str = format!("{:.2} km", dist_km);
        let speed_str = format!("{:.1} km/h", speed_kmh);
        let _ = Text::new("Distance:", EgPoint::new(32, 108), label_style).draw(d);
        let _ = Text::new(&dist_str, EgPoint::new(32, 136), value_style).draw(d);
        let _ = Text::new("Speed:", EgPoint::new(230, 108), label_style).draw(d);
        let _ = Text::new(
            &speed_str,
            EgPoint::new(230, 136),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
        )
        .draw(d);

        // Telemetry Card 2: Steps & Motion
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 162), Size::new(370, 70)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 5, 4)))
        .draw(d);
        let steps_str = format!("{} steps", state.steps);
        let motion_str = match state.imu_motion {
            MotionState::Still => "STILL",
            MotionState::Walking => "WALKING",
            MotionState::Active => "ACTIVE",
        };
        let motion_color = match state.imu_motion {
            MotionState::Still => Rgb565::CSS_GRAY,
            MotionState::Walking => Rgb565::GREEN,
            MotionState::Active => Rgb565::YELLOW,
        };
        let _ = Text::new("Pedometer:", EgPoint::new(32, 186), label_style).draw(d);
        let _ = Text::new(
            &steps_str,
            EgPoint::new(32, 214),
            MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN),
        )
        .draw(d);
        let _ = Text::new("Motion:", EgPoint::new(230, 186), label_style).draw(d);
        let _ = Text::new(
            motion_str,
            EgPoint::new(230, 214),
            MonoTextStyle::new(&FONT_10X20, motion_color),
        )
        .draw(d);

        // Course & Heading info
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 240), Size::new(370, 56)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 3)))
        .draw(d);
        if let Some(course) = state.course_deg {
            let course_str = format!("COURSE: {:.0}° GPS MOVING", course);
            let _ = Text::new(&course_str, EgPoint::new(32, 274), value_style).draw(d);
        } else {
            let _ = Text::new("COURSE: -- STATIONARY", EgPoint::new(32, 274), label_style).draw(d);
        }
        if state.battery_voltage_mv > 0 {
            let volt_str = format!("{:.2}V", state.battery_voltage_mv as f32 / 1000.0);
            let _ = Text::with_alignment(
                &volt_str,
                EgPoint::new(375, 274),
                MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY),
                Alignment::Right,
            )
            .draw(d);
        }

        // Action Buttons Row 1: LEVEL and SUN
        // LEVEL button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 310), Size::new(175, 56)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 6, 12)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 310), Size::new(175, 56)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "LEVEL",
            EgPoint::new(107, 344),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // SUN button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 310), Size::new(175, 56)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(12, 6, 2)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 310), Size::new(175, 56)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "SUN",
            EgPoint::new(302, 344),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // Action Button: START HIKE (or RESUME HIKE)
        let hike_running = state.hike.status == HikeStatus::Running;
        let btn_bg = if hike_running {
            Rgb565::new(0, 18, 4)
        } else {
            Rgb565::new(2, 22, 6)
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 385), Size::new(370, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(btn_bg))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 385), Size::new(370, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 2))
        .draw(d);
        let btn_label = match state.hike.status {
            HikeStatus::Stopped => "START HIKE",
            HikeStatus::Running => "VIEW HIKE LIVE",
            HikeStatus::Paused => "RESUME HIKE",
        };
        let _ = Text::with_alignment(
            btn_label,
            EgPoint::new(205, 426),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);
    }

    fn render_hike_live<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, state: &OutdoorState) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let value_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);

        // Elapsed time header: "HIKE 00:42:17"
        let hrs = state.hike.elapsed_secs / 3600;
        let mins = (state.hike.elapsed_secs % 3600) / 60;
        let secs = state.hike.elapsed_secs % 60;
        let timer_str = format!("HIKE {:02}:{:02}:{:02}", hrs, mins, secs);
        let status_color = if state.hike.status == HikeStatus::Paused {
            Rgb565::YELLOW
        } else {
            Rgb565::GREEN
        };
        Self::draw_status_header(d, state, &timer_str, status_color);

        // Huge Distance Card
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 48), Size::new(370, 95)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 5, 8)))
        .draw(d);
        let dist_km = state.hike.distance_m / 1000.0;
        let dist_str = format!("{:.2} km", dist_km);
        let _ = Text::new("TOTAL DISTANCE", EgPoint::new(35, 74), label_style).draw(d);
        let _ = Text::new(
            &dist_str,
            EgPoint::new(35, 122),
            MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE),
        )
        .draw(d);

        // Speed & Steps Row
        let speed_kmh = state.speed_mps * 3.6;
        let speed_str = format!("{:.1} km/h", speed_kmh);
        let steps_str = format!("{}", state.hike.steps_session);

        // Left: Speed
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 153), Size::new(175, 72)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 4, 6)))
        .draw(d);
        let _ = Text::new("SPEED", EgPoint::new(32, 178), label_style).draw(d);
        let _ = Text::new(
            &speed_str,
            EgPoint::new(32, 208),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
        )
        .draw(d);

        // Right: Steps
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 153), Size::new(175, 72)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 5, 3)))
        .draw(d);
        let _ = Text::new("HIKE STEPS", EgPoint::new(227, 178), label_style).draw(d);
        let _ = Text::new(
            &steps_str,
            EgPoint::new(227, 208),
            MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN),
        )
        .draw(d);

        // Course Direction & GPS Accuracy Card
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 235), Size::new(370, 72)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 3)))
        .draw(d);
        if let Some(course) = state.course_deg {
            let course_str = format!("DIRECTION: {:.0}° (GPS)", course);
            let _ = Text::new(&course_str, EgPoint::new(32, 262), value_style).draw(d);
        } else {
            let _ = Text::new("DIRECTION: STATIONARY", EgPoint::new(32, 262), label_style).draw(d);
        }
        let acc_str = if let Some(acc) = state.accuracy_m {
            format!("GPS Accuracy: ±{:.0} m", acc)
        } else {
            "GPS Accuracy: --".into()
        };
        let _ = Text::new(&acc_str, EgPoint::new(32, 290), label_style).draw(d);

        // Quick shortcut: [LEVEL] button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 318), Size::new(370, 50)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 6, 12)))
        .draw(d);
        let _ = Text::with_alignment(
            "VIEW ARTIFICIAL HORIZON / LEVEL",
            EgPoint::new(205, 350),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
            Alignment::Center,
        )
        .draw(d);

        // Controls: [PAUSE/RESUME] and [STOP]
        let pause_label = if state.hike.status == HikeStatus::Paused {
            "RESUME"
        } else {
            "PAUSE"
        };
        // Pause button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 385), Size::new(175, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(20, 14, 2)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 385), Size::new(175, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
        .draw(d);
        let _ = Text::with_alignment(
            pause_label,
            EgPoint::new(107, 426),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);

        // Stop button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 385), Size::new(175, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(24, 4, 4)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(215, 385), Size::new(175, 68)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::RED, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "STOP",
            EgPoint::new(302, 426),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);
    }

    fn render_level<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, state: &OutdoorState) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let value_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);

        Self::draw_status_header(d, state, "HORIZON", Rgb565::CYAN);

        // Horizon widget frame
        let box_w = 370;
        let box_h = 230;
        let box_x = 20;
        let box_y = 48;
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(
                EgPoint::new(box_x, box_y),
                Size::new(box_w as u32, box_h as u32),
            ),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(1, 3, 5)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(
                EgPoint::new(box_x, box_y),
                Size::new(box_w as u32, box_h as u32),
            ),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(6, 12, 20), 2))
        .draw(d);

        let center_x = box_x + box_w / 2;
        let center_y = box_y + box_h / 2;

        // Fixed reference crosshair in gray
        let _ = Line::new(
            EgPoint::new(center_x - 60, center_y),
            EgPoint::new(center_x - 15, center_y),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_GRAY, 2))
        .draw(d);
        let _ = Line::new(
            EgPoint::new(center_x + 15, center_y),
            EgPoint::new(center_x + 60, center_y),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_GRAY, 2))
        .draw(d);
        let _ = Line::new(
            EgPoint::new(center_x, center_y - 45),
            EgPoint::new(center_x, center_y - 15),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_GRAY, 2))
        .draw(d);
        let _ = Line::new(
            EgPoint::new(center_x, center_y + 15),
            EgPoint::new(center_x, center_y + 45),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_GRAY, 2))
        .draw(d);

        // Dynamic Bubble / Horizon indicator:
        // pitch tilts up/down: offset y by -pitch * 2.0
        // roll tilts left/right: offset x by roll * 2.0
        let bubble_dx = (state.roll_deg * 2.5).clamp(-80.0, 80.0) as i32;
        let bubble_dy = (-state.pitch_deg * 2.5).clamp(-65.0, 65.0) as i32;
        let bubble_x = center_x + bubble_dx;
        let bubble_y = center_y + bubble_dy;

        let is_level = libm::fabsf(state.roll_deg) <= 2.5 && libm::fabsf(state.pitch_deg) <= 2.5;
        let bubble_color = if is_level {
            Rgb565::GREEN
        } else {
            Rgb565::CYAN
        };

        // Draw bubble circle
        let bubble_r = 14;
        let _ = Circle::new(
            EgPoint::new(bubble_x - bubble_r, bubble_y - bubble_r),
            (bubble_r * 2) as u32,
        )
        .into_styled(PrimitiveStyle::with_fill(bubble_color))
        .draw(d);

        // Pitch & Roll numerical values below widget
        let roll_str = format!("Roll:  {:+.1}°", state.roll_deg);
        let pitch_str = format!("Pitch: {:+.1}°", state.pitch_deg);
        let _ = Text::new(&roll_str, EgPoint::new(35, 315), value_style).draw(d);
        let _ = Text::new(&pitch_str, EgPoint::new(230, 315), value_style).draw(d);

        // Motion status banner
        let (mot_str, mot_col) = if state.level_valid {
            ("GRAVITY STABLE — LEVEL ACTIVE", Rgb565::GREEN)
        } else {
            ("MOTION HIGH — LEVEL PAUSED", Rgb565::YELLOW)
        };
        let _ = Text::with_alignment(
            mot_str,
            EgPoint::new(205, 350),
            MonoTextStyle::new(&FONT_10X20, mot_col),
            Alignment::Center,
        )
        .draw(d);

        // [BACK] button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(100, 395), Size::new(210, 56)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 8, 14)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(100, 395), Size::new(210, 56)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CYAN, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "BACK",
            EgPoint::new(205, 430),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);
    }

    fn render_sun<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, state: &OutdoorState) {
        let label_style = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);

        Self::draw_status_header(d, state, "SUN POSITION", Rgb565::YELLOW);

        // Solar Card
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 50), Size::new(370, 290)),
            Size::new(12, 12),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 3)))
        .draw(d);

        let (lat, lon) = if state.gps_valid {
            (state.latitude_deg, state.longitude_deg)
        } else {
            // Default reference: Tallinn, Estonia (59.437° N, 24.753° E)
            (59.437, 24.753)
        };

        let (year, month, day) = state.date_ymd;
        let (hour, minute, second) = state.time_hms;

        let solar = crate::outdoor::solar::calculate_solar(
            year,
            month,
            day,
            hour,
            minute,
            second,
            lat,
            lon,
        );

        let _ = Text::new(
            "Azimuth (Solar Heading):",
            EgPoint::new(35, 85),
            label_style,
        )
        .draw(d);
        let az_str = format!("{:.1}° ({})", solar.azimuth_deg, solar.cardinal_str());
        let _ = Text::new(
            &az_str,
            EgPoint::new(35, 115),
            MonoTextStyle::new(&FONT_10X20, Rgb565::YELLOW),
        )
        .draw(d);

        let _ = Text::new("Elevation (Angle):", EgPoint::new(35, 150), label_style).draw(d);
        let el_str = if solar.elevation_deg >= 0.0 {
            format!("+{:.1}° above horizon", solar.elevation_deg)
        } else {
            format!("{:.1}° below horizon", solar.elevation_deg)
        };
        let el_color = if solar.elevation_deg >= 0.0 {
            Rgb565::WHITE
        } else {
            Rgb565::CSS_GRAY
        };
        let _ = Text::new(&el_str, EgPoint::new(35, 180), MonoTextStyle::new(&FONT_10X20, el_color)).draw(d);

        let _ = Text::new("Sunrise / Sunset:", EgPoint::new(35, 215), label_style).draw(d);
        // Estimate local solar timezone offset from longitude (15 deg per hour)
        let tz_offset_hours = if lon >= 0.0 {
            ((lon + 7.5) / 15.0) as i32
        } else {
            ((lon - 7.5) / 15.0) as i32
        };
        let sun_str = match (solar.sunrise_utc_min, solar.sunset_utc_min) {
            (Some(r), Some(s)) => {
                let r_local = ((r as i32 + tz_offset_hours * 60).rem_euclid(1440)) as u16;
                let s_local = ((s as i32 + tz_offset_hours * 60).rem_euclid(1440)) as u16;
                format!(
                    "Rise {:02}:{:02}  |  Set {:02}:{:02}",
                    r_local / 60,
                    r_local % 60,
                    s_local / 60,
                    s_local % 60
                )
            }
            _ => "Polar Day / Night".into(),
        };
        let _ = Text::new(
            &sun_str,
            EgPoint::new(35, 245),
            MonoTextStyle::new(&FONT_10X20, Rgb565::CYAN),
        )
        .draw(d);

        let fix_str = if state.gps_valid {
            format!(
                "GPS: Lat {:.3}°, Lon {:.3}° (age {}s)",
                state.latitude_deg, state.longitude_deg, state.fix_age_secs
            )
        } else {
            format!("Ref: Lat {:.3}°, Lon {:.3}° (Default)", lat, lon)
        };
        let _ = Text::new(&fix_str, EgPoint::new(35, 295), label_style).draw(d);

        // [BACK] button
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(100, 395), Size::new(210, 56)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(8, 6, 2)))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(100, 395), Size::new(210, 56)),
            Size::new(10, 10),
        )
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
        .draw(d);
        let _ = Text::with_alignment(
            "BACK",
            EgPoint::new(205, 430),
            MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE),
            Alignment::Center,
        )
        .draw(d);
    }
}
