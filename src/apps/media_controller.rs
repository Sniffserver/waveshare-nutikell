//! BLE Media Controller with SoundCloud "Add to Playlist" integration.
//!
//! Provides interactive playback controls (Play, Pause, Prev, Next, Volume),
//! live track metadata display (Title, Artist, Album, Progress, Duration),
//! and a dedicated SoundCloud hero button to save current tracks to a playlist
//! over BLE companion protocol.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::geometry::Point as EgPoint;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_8X13, FONT_9X18_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Text};

use crate::apps::{App, AppInput, AppResult};
use crate::peripherals::touch::{SwipeDirection, TouchPoint};

pub const SCREEN_W: i32 = 410;
pub const SCREEN_H: i32 = 502;

/// Vibrant SoundCloud Brand Orange (#FF5500)
pub const COLOR_SOUNDCLOUD: Rgb565 = Rgb565::new(31, 21, 0);
/// Spotify Neon Green (#1DB954)
pub const COLOR_SPOTIFY: Rgb565 = Rgb565::new(3, 58, 26);
/// Apple Music Magenta
pub const COLOR_APPLE_MUSIC: Rgb565 = Rgb565::new(31, 8, 16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaSource {
    SoundCloud,
    Spotify,
    AppleMusic,
    LocalSd,
}

impl MediaSource {
    pub fn name(&self) -> &str {
        match self {
            MediaSource::SoundCloud => "SoundCloud",
            MediaSource::Spotify => "Spotify",
            MediaSource::AppleMusic => "Apple Music",
            MediaSource::LocalSd => "Local SD",
        }
    }

    pub fn accent_color(&self) -> Rgb565 {
        match self {
            MediaSource::SoundCloud => COLOR_SOUNDCLOUD,
            MediaSource::Spotify => COLOR_SPOTIFY,
            MediaSource::AppleMusic => COLOR_APPLE_MUSIC,
            MediaSource::LocalSd => Rgb565::CYAN,
        }
    }

    pub fn next(&self) -> Self {
        match self {
            MediaSource::SoundCloud => MediaSource::Spotify,
            MediaSource::Spotify => MediaSource::AppleMusic,
            MediaSource::AppleMusic => MediaSource::LocalSd,
            MediaSource::LocalSd => MediaSource::SoundCloud,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// Commands transmitted over BLE to the phone companion app
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    Play = 0x01,
    Pause = 0x02,
    TogglePlayPause = 0x03,
    NextTrack = 0x04,
    PreviousTrack = 0x05,
    VolumeUp = 0x06,
    VolumeDown = 0x07,
    SetVolume = 0x08,
    AddToSoundCloudPlaylist = 0x10,
    LikeTrack = 0x11,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u32,
    pub position_secs: u32,
    pub in_playlist: bool,
}

impl TrackInfo {
    pub fn new(title: &str, artist: &str, album: &str, duration_secs: u32) -> Self {
        Self {
            title: String::from(title),
            artist: String::from(artist),
            album: String::from(album),
            duration_secs,
            position_secs: 0,
            in_playlist: false,
        }
    }
}

/// Events returned to the main event loop
#[derive(Clone, Debug, PartialEq)]
pub enum MediaAction {
    None,
    SendCommand(MediaCommand),
    AddToPlaylist(MediaSource, String), // Source, Track Title
}

pub struct MediaController {
    pub source: MediaSource,
    pub state: PlaybackState,
    pub volume_pct: u8, // 0..100
    pub tracks: Vec<TrackInfo>,
    pub current_track: usize,
    pub toast_timer_ms: u32,
    pub anim_phase: u8,
    pub ble_connected: bool,
    pub pending_action: MediaAction,
}

impl MediaController {
    pub fn new() -> Self {
        let mut tracks = Vec::new();
        tracks.push(TrackInfo::new(
            "Strobe (Club Edit)",
            "deadmau5",
            "mau5trap Chill",
            355,
        ));
        tracks.push(TrackInfo::new(
            "Sunset Lover",
            "Petit Biscuit",
            "SoundCloud Wave",
            237,
        ));
        tracks.push(TrackInfo::new(
            "Resonance",
            "HOME",
            "Synthwave Essentials",
            212,
        ));
        tracks.push(TrackInfo::new(
            "Midnight City",
            "M83",
            "Hurry Up, We Are Dreaming",
            243,
        ));

        Self {
            source: MediaSource::SoundCloud,
            state: PlaybackState::Playing,
            volume_pct: 75,
            tracks,
            current_track: 0,
            toast_timer_ms: 0,
            anim_phase: 0,
            ble_connected: true,
            pending_action: MediaAction::None,
        }
    }

    pub fn set_ble_connected(&mut self, connected: bool) {
        self.ble_connected = connected;
    }

    pub fn active_track(&self) -> Option<&TrackInfo> {
        self.tracks.get(self.current_track)
    }

    pub fn active_track_mut(&mut self) -> Option<&mut TrackInfo> {
        self.tracks.get_mut(self.current_track)
    }

    /// Takes and resets any pending media action
    pub fn take_pending_action(&mut self) -> MediaAction {
        core::mem::replace(&mut self.pending_action, MediaAction::None)
    }

    /// Periodic tick for progress advancement and animations
    pub fn tick(&mut self, dt_ms: u32) {
        self.anim_phase = (self.anim_phase + (dt_ms / 50) as u8) % 36;
        if self.toast_timer_ms > 0 {
            self.toast_timer_ms = self.toast_timer_ms.saturating_sub(dt_ms);
        }

        // Advance playback progress if currently playing
        if self.state == PlaybackState::Playing {
            if let Some(track) = self.active_track_mut() {
                // Approximate 1 second tick
                let new_pos = track.position_secs + (dt_ms / 1000);
                if new_pos >= track.duration_secs {
                    track.position_secs = 0;
                } else {
                    track.position_secs = new_pos;
                }
            }
        }
    }

    /// Handles a touch coordinate event
    pub fn handle_touch_point(&mut self, pt: TouchPoint) -> MediaAction {
        let (tx, ty) = (pt.x as i32, pt.y as i32);

        // 1. Source Selector Pill (x: 20..180, y: 15..55)
        if (20..=180).contains(&tx) && (15..=55).contains(&ty) {
            self.source = self.source.next();
            return MediaAction::None;
        }

        // 2. Transport: Prev Track (x: 40..115, y: 340..400)
        if (40..=115).contains(&tx) && (340..=400).contains(&ty) {
            if !self.tracks.is_empty() {
                self.current_track = if self.current_track == 0 {
                    self.tracks.len() - 1
                } else {
                    self.current_track - 1
                };
                if let Some(t) = self.active_track_mut() {
                    t.position_secs = 0;
                }
            }
            let act = MediaAction::SendCommand(MediaCommand::PreviousTrack);
            self.pending_action = act.clone();
            return act;
        }

        // 3. Transport: Play/Pause Central Button (x: 165..245, y: 330..410)
        if (165..=245).contains(&tx) && (330..=410).contains(&ty) {
            self.state = match self.state {
                PlaybackState::Playing => PlaybackState::Paused,
                PlaybackState::Paused | PlaybackState::Stopped => PlaybackState::Playing,
            };
            let cmd = if self.state == PlaybackState::Playing {
                MediaCommand::Play
            } else {
                MediaCommand::Pause
            };
            let act = MediaAction::SendCommand(cmd);
            self.pending_action = act.clone();
            return act;
        }

        // 4. Transport: Next Track (x: 295..370, y: 340..400)
        if (295..=370).contains(&tx) && (340..=400).contains(&ty) {
            if !self.tracks.is_empty() {
                self.current_track = (self.current_track + 1) % self.tracks.len();
                if let Some(t) = self.active_track_mut() {
                    t.position_secs = 0;
                }
            }
            let act = MediaAction::SendCommand(MediaCommand::NextTrack);
            self.pending_action = act.clone();
            return act;
        }

        // 5. Volume Down (x: 35..90, y: 410..445)
        if (35..=90).contains(&tx) && (410..=445).contains(&ty) {
            self.volume_pct = self.volume_pct.saturating_sub(10);
            let act = MediaAction::SendCommand(MediaCommand::VolumeDown);
            self.pending_action = act.clone();
            return act;
        }

        // 6. Volume Up (x: 320..375, y: 410..445)
        if (320..=375).contains(&tx) && (410..=445).contains(&ty) {
            self.volume_pct = (self.volume_pct + 10).min(100);
            let act = MediaAction::SendCommand(MediaCommand::VolumeUp);
            self.pending_action = act.clone();
            return act;
        }

        // 7. HERO BUTTON: Add to SoundCloud Playlist (x: 25..385, y: 446..496)
        if (25..=385).contains(&tx) && (446..=496).contains(&ty) {
            let track_title = self
                .active_track()
                .map(|t| t.title.clone())
                .unwrap_or_else(|| String::from("Current Track"));

            if let Some(t) = self.active_track_mut() {
                t.in_playlist = true;
            }
            self.toast_timer_ms = 4000;

            let act = MediaAction::AddToPlaylist(self.source, track_title);
            self.pending_action = act.clone();
            return act;
        }

        MediaAction::None
    }
}

impl Default for MediaController {
    fn default() -> Self {
        Self::new()
    }
}

impl App for MediaController {
    fn name(&self) -> &str {
        "Media & SoundCloud"
    }

    fn setup(&mut self) {
        self.state = PlaybackState::Playing;
        self.toast_timer_ms = 0;
    }

    fn update(&mut self, input: &AppInput) -> AppResult {
        self.tick(input.dt_ms);

        if let Some(touch) = input.touch {
            self.handle_touch_point(touch);
        }

        // Swipes: Left/Right to skip tracks
        if let Some(SwipeDirection::Right) = input.swipe {
            if !self.tracks.is_empty() {
                self.current_track = (self.current_track + 1) % self.tracks.len();
                if let Some(t) = self.active_track_mut() {
                    t.position_secs = 0;
                }
            }
            self.pending_action = MediaAction::SendCommand(MediaCommand::NextTrack);
        }
        if let Some(SwipeDirection::Left) = input.swipe {
            if !self.tracks.is_empty() {
                self.current_track = if self.current_track == 0 {
                    self.tracks.len() - 1
                } else {
                    self.current_track - 1
                };
                if let Some(t) = self.active_track_mut() {
                    t.position_secs = 0;
                }
            }
            self.pending_action = MediaAction::SendCommand(MediaCommand::PreviousTrack);
        }

        AppResult::Continue
    }

    fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // Background: Deep Obsidian
        let _ = Rectangle::new(EgPoint::zero(), Size::new(SCREEN_W as u32, SCREEN_H as u32))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
            .draw(d);

        let white = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
        let _dim = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_GRAY);
        let bold_white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
        let accent = self.source.accent_color();

        // 1. Header: Source Badge Pill (x: 20, y: 16)
        let pill_bg = match self.source {
            MediaSource::SoundCloud => Rgb565::new(12, 5, 0),
            MediaSource::Spotify => Rgb565::new(0, 10, 4),
            MediaSource::AppleMusic => Rgb565::new(10, 2, 4),
            MediaSource::LocalSd => Rgb565::new(0, 6, 10),
        };
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 16), Size::new(155, 30)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(pill_bg))
        .draw(d);
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(20, 16), Size::new(155, 30)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_stroke(accent, 1))
        .draw(d);

        let _ = Text::with_alignment(
            self.source.name(),
            EgPoint::new(97, 37),
            MonoTextStyle::new(&FONT_9X18_BOLD, accent),
            Alignment::Center,
        )
        .draw(d);

        // Header: BLE Link Status Indicator
        let (ble_label, ble_col) = if self.ble_connected {
            ("BLE LINK", Rgb565::GREEN)
        } else {
            ("NO BLE", Rgb565::CSS_GRAY)
        };
        let _ = Text::with_alignment(
            ble_label,
            EgPoint::new(340, 36),
            MonoTextStyle::new(&FONT_8X13, ble_col),
            Alignment::Center,
        )
        .draw(d);

        // 2. Vinyl Record / Glowing Album Graphic (y: 65..240)
        let center = EgPoint::new(SCREEN_W / 2, 145);
        let outer_r = 75i32;

        // Outer glow ring
        let _ = Circle::new(
            center - EgPoint::new(outer_r, outer_r),
            (outer_r * 2) as u32,
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(2, 3, 4)))
        .draw(d);
        let _ = Circle::new(
            center - EgPoint::new(outer_r, outer_r),
            (outer_r * 2) as u32,
        )
        .into_styled(PrimitiveStyle::with_stroke(accent, 2))
        .draw(d);

        // Vinyl Grooves
        let _ = Circle::new(center - EgPoint::new(55, 55), 110)
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(6, 8, 10), 1))
            .draw(d);
        let _ = Circle::new(center - EgPoint::new(38, 38), 76)
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::new(8, 10, 12), 1))
            .draw(d);

        // Center label / Cloud icon
        let _ = Circle::new(center - EgPoint::new(22, 22), 44)
            .into_styled(PrimitiveStyle::with_fill(accent))
            .draw(d);
        let _ = Text::with_alignment("☁", center + EgPoint::new(0, 6), white, Alignment::Center)
            .draw(d);

        // 3. Track Details (y: 245..295)
        let (title, artist, album, pos_secs, dur_secs, in_playlist) =
            if let Some(t) = self.active_track() {
                (
                    t.title.as_str(),
                    t.artist.as_str(),
                    t.album.as_str(),
                    t.position_secs,
                    t.duration_secs,
                    t.in_playlist,
                )
            } else {
                ("No Track", "Unknown Artist", "None", 0, 1, false)
            };

        let _ = Text::with_alignment(
            title,
            EgPoint::new(SCREEN_W / 2, 252),
            bold_white,
            Alignment::Center,
        )
        .draw(d);

        let artist_style = MonoTextStyle::new(&FONT_10X20, accent);
        let _ = Text::with_alignment(
            artist,
            EgPoint::new(SCREEN_W / 2, 276),
            artist_style,
            Alignment::Center,
        )
        .draw(d);

        let _ = Text::with_alignment(
            album,
            EgPoint::new(SCREEN_W / 2, 298),
            MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY),
            Alignment::Center,
        )
        .draw(d);

        // 4. Scrubber Bar (y: 312..332)
        let bar_x = 40i32;
        let bar_w = 330i32;
        let bar_y = 314i32;
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(bar_x, bar_y), Size::new(bar_w as u32, 6)),
            Size::new(3, 3),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(4, 5, 6)))
        .draw(d);

        let progress_w = if dur_secs > 0 {
            ((pos_secs as i32 * bar_w) / dur_secs as i32).min(bar_w)
        } else {
            0
        };

        if progress_w > 0 {
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(bar_x, bar_y), Size::new(progress_w as u32, 6)),
                Size::new(3, 3),
            )
            .into_styled(PrimitiveStyle::with_fill(accent))
            .draw(d);
        }

        // Time indicators: 01:42 / 03:55
        let elapsed_str = format!("{:02}:{:02}", pos_secs / 60, pos_secs % 60);
        let duration_str = format!("{:02}:{:02}", dur_secs / 60, dur_secs % 60);
        let time_style = MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY);
        let _ = Text::new(&elapsed_str, EgPoint::new(bar_x, bar_y + 18), time_style).draw(d);
        let _ = Text::with_alignment(
            &duration_str,
            EgPoint::new(bar_x + bar_w, bar_y + 18),
            time_style,
            Alignment::Right,
        )
        .draw(d);

        // 5. Transport Controls (y: 345..405)
        // Prev button [ |<< ]
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(45, 345), Size::new(65, 50)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 6)))
        .draw(d);
        let _ =
            Text::with_alignment("|<<", EgPoint::new(77, 376), white, Alignment::Center).draw(d);

        // Central Play/Pause Hero Button
        let play_center = EgPoint::new(SCREEN_W / 2, 370);
        let play_bg = if self.state == PlaybackState::Playing {
            accent
        } else {
            Rgb565::new(8, 10, 12)
        };
        let _ = Circle::new(play_center - EgPoint::new(32, 32), 64)
            .into_styled(PrimitiveStyle::with_fill(play_bg))
            .draw(d);
        let _ = Circle::new(play_center - EgPoint::new(32, 32), 64)
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::WHITE, 2))
            .draw(d);

        let icon = match self.state {
            PlaybackState::Playing => "||",
            PlaybackState::Paused | PlaybackState::Stopped => ">",
        };
        let _ = Text::with_alignment(
            icon,
            play_center + EgPoint::new(0, 7),
            bold_white,
            Alignment::Center,
        )
        .draw(d);

        // Next button [ >>| ]
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(EgPoint::new(300, 345), Size::new(65, 50)),
            Size::new(8, 8),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(3, 4, 6)))
        .draw(d);
        let _ =
            Text::with_alignment(">>|", EgPoint::new(332, 376), white, Alignment::Center).draw(d);

        // 6. Volume Row (y: 412..436)
        let _ = Text::new("-", EgPoint::new(55, 428), white).draw(d);
        let vol_bar_x = 85i32;
        let vol_bar_w = 240i32;
        let vol_bar_y = 422i32;
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(
                EgPoint::new(vol_bar_x, vol_bar_y),
                Size::new(vol_bar_w as u32, 4),
            ),
            Size::new(2, 2),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(4, 5, 6)))
        .draw(d);
        let vol_fill = ((self.volume_pct as i32 * vol_bar_w) / 100).min(vol_bar_w);
        if vol_fill > 0 {
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(
                    EgPoint::new(vol_bar_x, vol_bar_y),
                    Size::new(vol_fill as u32, 4),
                ),
                Size::new(2, 2),
            )
            .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
            .draw(d);
        }
        let _ = Text::new("+", EgPoint::new(340, 428), white).draw(d);

        // 7. HERO BUTTON: SoundCloud "Add to Playlist"
        let btn_y = 446i32;
        let btn_h = 48u32;
        let btn_w = 350u32;
        let btn_x = 30i32;

        if in_playlist || self.toast_timer_ms > 0 {
            // Already Saved / Success State
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(btn_x, btn_y), Size::new(btn_w, btn_h)),
                Size::new(12, 12),
            )
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0, 20, 8))) // Forest green
            .draw(d);
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(btn_x, btn_y), Size::new(btn_w, btn_h)),
                Size::new(12, 12),
            )
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::GREEN, 2))
            .draw(d);

            let _ = Text::with_alignment(
                "✓ SAVED TO PLAYLIST",
                EgPoint::new(SCREEN_W / 2, btn_y + 30),
                bold_white,
                Alignment::Center,
            )
            .draw(d);
        } else {
            // Hero Action State
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(btn_x, btn_y), Size::new(btn_w, btn_h)),
                Size::new(12, 12),
            )
            .into_styled(PrimitiveStyle::with_fill(accent))
            .draw(d);
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(EgPoint::new(btn_x, btn_y), Size::new(btn_w, btn_h)),
                Size::new(12, 12),
            )
            .into_styled(PrimitiveStyle::with_stroke(Rgb565::WHITE, 1))
            .draw(d);

            let label = match self.source {
                MediaSource::SoundCloud => "+ ADD TO SC PLAYLIST",
                MediaSource::Spotify => "+ ADD TO SPOTIFY",
                MediaSource::AppleMusic => "+ ADD TO APPLE MUSIC",
                MediaSource::LocalSd => "+ SAVE TO FAVORITES",
            };

            let _ = Text::with_alignment(
                label,
                EgPoint::new(SCREEN_W / 2, btn_y + 30),
                bold_white,
                Alignment::Center,
            )
            .draw(d);
        }
    }
}

// ── Unit Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_and_track_catalog() {
        let player = MediaController::new();
        assert_eq!(player.source, MediaSource::SoundCloud);
        assert_eq!(player.state, PlaybackState::Playing);
        assert_eq!(player.volume_pct, 75);
        assert_eq!(player.tracks.len(), 4);
        assert_eq!(player.current_track, 0);

        let t0 = player.active_track().unwrap();
        assert_eq!(t0.title, "Strobe (Club Edit)");
        assert_eq!(t0.artist, "deadmau5");
        assert_eq!(t0.duration_secs, 355);
        assert!(!t0.in_playlist);
    }

    #[test]
    fn play_pause_toggle_via_center_button() {
        let mut player = MediaController::new();
        assert_eq!(player.state, PlaybackState::Playing);

        // Center tap (x: 205, y: 370) -> Pause
        let action = player.handle_touch_point(TouchPoint { x: 205, y: 370 });
        assert_eq!(action, MediaAction::SendCommand(MediaCommand::Pause));
        assert_eq!(player.state, PlaybackState::Paused);

        // Center tap again -> Play
        let action2 = player.handle_touch_point(TouchPoint { x: 205, y: 370 });
        assert_eq!(action2, MediaAction::SendCommand(MediaCommand::Play));
        assert_eq!(player.state, PlaybackState::Playing);
    }

    #[test]
    fn track_navigation_next_and_prev() {
        let mut player = MediaController::new();
        assert_eq!(player.current_track, 0);

        // Next tap (x: 330, y: 370)
        let act_next = player.handle_touch_point(TouchPoint { x: 330, y: 370 });
        assert_eq!(act_next, MediaAction::SendCommand(MediaCommand::NextTrack));
        assert_eq!(player.current_track, 1);
        assert_eq!(player.active_track().unwrap().title, "Sunset Lover");

        // Prev tap (x: 75, y: 370)
        let act_prev = player.handle_touch_point(TouchPoint { x: 75, y: 370 });
        assert_eq!(
            act_prev,
            MediaAction::SendCommand(MediaCommand::PreviousTrack)
        );
        assert_eq!(player.current_track, 0);
    }

    #[test]
    fn volume_up_and_down_controls() {
        let mut player = MediaController::new();
        assert_eq!(player.volume_pct, 75);

        // Vol Down (x: 60, y: 425)
        let act_down = player.handle_touch_point(TouchPoint { x: 60, y: 425 });
        assert_eq!(act_down, MediaAction::SendCommand(MediaCommand::VolumeDown));
        assert_eq!(player.volume_pct, 65);

        // Vol Up (x: 345, y: 425)
        let act_up = player.handle_touch_point(TouchPoint { x: 345, y: 425 });
        assert_eq!(act_up, MediaAction::SendCommand(MediaCommand::VolumeUp));
        assert_eq!(player.volume_pct, 75);
    }

    #[test]
    fn add_to_soundcloud_playlist_hero_button() {
        let mut player = MediaController::new();
        assert!(!player.active_track().unwrap().in_playlist);

        // Tap Hero button (x: 200, y: 470)
        let action = player.handle_touch_point(TouchPoint { x: 200, y: 470 });
        assert_eq!(
            action,
            MediaAction::AddToPlaylist(MediaSource::SoundCloud, String::from("Strobe (Club Edit)"))
        );
        assert!(player.active_track().unwrap().in_playlist);
        assert!(player.toast_timer_ms > 0);
    }

    #[test]
    fn cycle_sources_via_header_pill() {
        let mut player = MediaController::new();
        assert_eq!(player.source, MediaSource::SoundCloud);

        // Tap Source Pill (x: 80, y: 35)
        player.handle_touch_point(TouchPoint { x: 80, y: 35 });
        assert_eq!(player.source, MediaSource::Spotify);

        player.handle_touch_point(TouchPoint { x: 80, y: 35 });
        assert_eq!(player.source, MediaSource::AppleMusic);

        player.handle_touch_point(TouchPoint { x: 80, y: 35 });
        assert_eq!(player.source, MediaSource::LocalSd);

        player.handle_touch_point(TouchPoint { x: 80, y: 35 });
        assert_eq!(player.source, MediaSource::SoundCloud);
    }
}
