use alloc::boxed::Box;
use embassy_time::Duration;

use crate::apps::authenticator::AuthenticatorApp;
use crate::apps::flappy::FlappyGame;
use crate::apps::game2048::Game2048;
use crate::apps::maze::MazeGame;
use crate::apps::media_controller::MediaController;
use crate::apps::outdoor_app::OutdoorApp;
use crate::apps::settings::SettingsApp;
use crate::apps::snake::SnakeGame;
use crate::apps::tetris::TetrisGame;
use crate::apps::work_tracker::WorkTracker;
use crate::apps::{App, AppState};
use crate::ble_analyzer::BleAnalyzerApp;
use crate::drivers::co5300::DisplayError;
use crate::drivers::framebuffer::Framebuffer;
use crate::outdoor::OutdoorState;
use crate::peripherals::settings::WatchSettings;
use crate::ui::launcher::Launcher;
use crate::ui::pages::Page;
use crate::ui::watchface::{RenderOutcome, WatchFace};

pub struct UiRouter {
    pub current_page: Page,
    pub app_state: AppState,
    pub launcher: Launcher,
    pub outdoor_app: OutdoorApp,
    pub work_tracker: WorkTracker,
    pub authenticator_app: AuthenticatorApp,
    pub ble_analyzer_app: Box<BleAnalyzerApp>,
    pub settings_app: SettingsApp,
    pub media_controller: MediaController,
    pub snake_game: SnakeGame,
    pub game_2048: Game2048,
    pub tetris_game: TetrisGame,
    pub flappy_game: FlappyGame,
    pub maze_game: MazeGame,
}

impl UiRouter {
    pub fn new(watch_settings: WatchSettings, stored_ssid: Option<&str>) -> Self {
        let mut settings_app = SettingsApp::with_settings(watch_settings);
        if let Some(ssid) = stored_ssid {
            settings_app.set_stored_ssid(ssid);
        }

        Self {
            current_page: Page::Clock,
            app_state: AppState::Watchface,
            launcher: Launcher::new(),
            outdoor_app: OutdoorApp::new(),
            work_tracker: WorkTracker::with_rate(watch_settings.hourly_rate_cents),
            authenticator_app: AuthenticatorApp::new(),
            ble_analyzer_app: Box::new(BleAnalyzerApp::new()),
            settings_app,
            media_controller: MediaController::new(),
            snake_game: SnakeGame::new(),
            game_2048: Game2048::new(),
            tetris_game: TetrisGame::new(),
            flappy_game: FlappyGame::new(),
            maze_game: MazeGame::new(),
        }
    }

    pub fn switch_to(&mut self, state: AppState) {
        match state {
            AppState::Outdoor => self.outdoor_app.setup(),
            AppState::Settings => {
                self.settings_app.wifi_state = crate::peripherals::wifi::WifiState::Disconnected;
            }
            AppState::WorkTracker => self.work_tracker.setup(),
            AppState::Authenticator => self.authenticator_app.setup(),
            AppState::BleAnalyzer => self.ble_analyzer_app.setup(),
            AppState::Mp3Player => self.media_controller.setup(),
            AppState::Snake => self.snake_game.setup(),
            AppState::Game2048 => self.game_2048.setup(),
            AppState::Tetris => self.tetris_game.setup(),
            AppState::Flappy => self.flappy_game.setup(),
            AppState::Maze => self.maze_game.setup(),
            _ => {}
        }
        self.app_state = state;
    }

    pub fn is_game(&self) -> bool {
        matches!(
            self.app_state,
            AppState::Snake
                | AppState::Game2048
                | AppState::Tetris
                | AppState::Flappy
                | AppState::Maze
        )
    }

    pub fn need_gyro(&self, watchface_gyro_enabled: bool, screen_state: u8) -> bool {
        screen_state >= 2
            && (watchface_gyro_enabled
                || self.app_state == AppState::Maze
                || self.app_state == AppState::Tetris
                || self.app_state == AppState::Flappy
                || (self.app_state == AppState::Outdoor
                    && self.outdoor_app.screen == crate::apps::outdoor_app::OutdoorScreen::Level)
                || (self.app_state == AppState::Watchface && self.current_page == Page::Sensors))
    }

    pub fn need_accel(
        &self,
        hike_running: bool,
        watchface_gyro_enabled: bool,
        screen_state: u8,
    ) -> bool {
        self.need_gyro(watchface_gyro_enabled, screen_state)
            || hike_running
            || (screen_state >= 2
                && (self.app_state == AppState::Outdoor || self.app_state == AppState::Watchface))
    }

    pub fn tick_budget(
        &self,
        screen_state: u8,
        touch_held: bool,
        button_held: bool,
        hike_running: bool,
        watchface_gyro_enabled: bool,
    ) -> Duration {
        if touch_held || button_held {
            Duration::from_millis(16)
        } else if screen_state == 0 {
            if hike_running {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(20)
            }
        } else if screen_state == 1 {
            if hike_running {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(10)
            }
        } else {
            match self.app_state {
                AppState::Watchface => match self.current_page {
                    Page::Clock => {
                        if watchface_gyro_enabled {
                            Duration::from_millis(33)
                        } else {
                            Duration::from_secs(1)
                        }
                    }
                    Page::Sensors => Duration::from_millis(100),
                    Page::System => Duration::from_secs(2),
                    Page::Power => Duration::from_secs(1),
                },
                AppState::Launcher
                | AppState::Settings
                | AppState::Mp3Player
                | AppState::WorkTracker
                | AppState::Authenticator
                | AppState::BleAnalyzer => Duration::from_millis(100),
                AppState::Outdoor => {
                    if self.outdoor_app.screen == crate::apps::outdoor_app::OutdoorScreen::Level {
                        Duration::from_millis(50)
                    } else {
                        Duration::from_millis(100)
                    }
                }
                AppState::Flappy
                | AppState::Snake
                | AppState::Game2048
                | AppState::Tetris
                | AppState::Maze => Duration::from_millis(33),
            }
        }
    }

    pub fn render(
        &mut self,
        fb: &mut Framebuffer,
        watchface: &mut WatchFace,
        outdoor_state: &OutdoorState,
    ) -> Option<Result<RenderOutcome, DisplayError>> {
        match self.app_state {
            AppState::Watchface => Some(watchface.render(fb)),
            AppState::Launcher => {
                self.launcher.render(fb);
                None
            }
            AppState::Settings => {
                self.settings_app.render(fb);
                None
            }
            AppState::Outdoor => {
                self.outdoor_app.render(fb, outdoor_state);
                None
            }
            AppState::WorkTracker => {
                self.work_tracker.render(fb);
                None
            }
            AppState::Authenticator => {
                self.authenticator_app.render(fb);
                None
            }
            AppState::BleAnalyzer => {
                self.ble_analyzer_app.render(fb);
                None
            }
            AppState::Mp3Player => {
                self.media_controller.render(fb);
                None
            }
            AppState::Snake => {
                self.snake_game.render(fb);
                None
            }
            AppState::Game2048 => {
                self.game_2048.render(fb);
                None
            }
            AppState::Tetris => {
                self.tetris_game.render(fb);
                None
            }
            AppState::Flappy => {
                self.flappy_game.render(fb);
                None
            }
            AppState::Maze => {
                self.maze_game.render(fb);
                None
            }
        }
    }
}
