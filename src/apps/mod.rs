// App framework - common types and trait for all apps/games

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::DrawTarget;

use crate::peripherals::touch::{SwipeDirection, TouchPoint};

pub mod flappy;
pub mod game2048;
pub mod maze;
pub mod mp3player;
pub mod outdoor_app;
pub mod settings;
pub mod smarthome;
pub mod snake;
pub mod tetris;
pub mod wifi_setup;
pub mod authenticator;
pub mod work_tracker;

pub use authenticator::{AuthenticatorApp, TotpAccount};

pub use work_tracker::{TrackerState, WorkSession, WorkTracker};

/// Input state passed to apps each frame
pub struct AppInput {
    pub touch: Option<TouchPoint>,
    pub swipe: Option<SwipeDirection>,
    pub tap: bool,
    pub accel: (f32, f32, f32),
    pub dt_ms: u32, // milliseconds since last frame
}

/// Result of an app update
pub enum AppResult {
    Continue,
    Exit, // Return to launcher/watchface
}

/// Common trait for all apps/games
pub trait App {
    fn name(&self) -> &str;
    fn setup(&mut self);
    fn update(&mut self, input: &AppInput) -> AppResult;
    fn render<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D);
}

/// All available app states
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AppState {
    Watchface,
    Launcher,
    Snake,
    Game2048,
    Tetris,
    Flappy,
    Maze,
    Mp3Player,
    SmartHome,
    Settings,
    Outdoor,
    WorkTracker,
    Authenticator,
}
