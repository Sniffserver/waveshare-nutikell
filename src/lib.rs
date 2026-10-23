// Host-side library target — used for host-side unit testing (`cargo test-host`).
// The embedded binary entry-point is in src/main.rs.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod outdoor {
    pub mod steps;
    pub mod packet;
    pub mod solar;
    pub mod state;
    pub mod power_policy;
    pub mod ble;
}

pub mod apps {
    pub mod authenticator;
    pub mod work_tracker;
    pub use authenticator::{AuthenticatorApp, TotpAccount};
    pub use work_tracker::{TrackerState, WorkSession, WorkTracker};

    pub use crate::peripherals::touch::{SwipeDirection, TouchPoint};

    pub struct AppInput {
        pub touch: Option<TouchPoint>,
        pub swipe: Option<SwipeDirection>,
        pub tap: bool,
        pub accel: (f32, f32, f32),
        pub dt_ms: u32,
    }

    pub enum AppResult {
        Continue,
        Exit,
    }

    pub trait App {
        fn name(&self) -> &str;
        fn setup(&mut self);
        fn update(&mut self, input: &AppInput) -> AppResult;
        fn render<D: embedded_graphics::prelude::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>>(&self, d: &mut D);
    }
}

pub mod peripherals {
    pub mod touch {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct TouchPoint {
            pub x: u16,
            pub y: u16,
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum SwipeDirection {
            Left,
            Right,
            Up,
            Down,
        }
    }
}

pub mod ui {
    pub mod notification;
    pub use notification::{Notification, NotificationManager, Urgency};
}
