//! Battery state calculation and drawing helpers.

use crate::outdoor::ui::{OutdoorDisplay, GREEN, ORANGE, RED, WHITE};
pub use crate::peripherals::power::BatteryState;

/// Map battery percentage to a level bar segment count (0..=5).
pub const fn battery_level(percent: u8) -> u8 {
    match percent {
        0..=5 => 0,
        6..=20 => 1,
        21..=45 => 2,
        46..=70 => 3,
        71..=90 => 4,
        _ => 5,
    }
}

/// Draw a battery icon with outline, terminal nub, fill bars, and charge bolt.
pub fn draw_battery_icon<D>(
    display: &mut D,
    x: i32,
    y: i32,
    battery: &BatteryState,
) -> Result<(), D::Error>
where
    D: OutdoorDisplay,
{
    let width = 30;
    let height = 14;
    // Outline & positive terminal nub
    display.rect(x, y, width, height, WHITE)?;
    display.rect(x + width, y + 4, 3, 6, WHITE)?;

    let level = battery_level(battery.percent);
    let fill_width = match level {
        0 => 0,
        1 => 5,
        2 => 10,
        3 => 16,
        4 => 22,
        _ => 26,
    };

    let color = if battery.percent <= 15 {
        RED
    } else if battery.percent <= 30 {
        ORANGE
    } else {
        GREEN
    };

    if fill_width > 0 {
        display.rect(x + 2, y + 2, fill_width, height - 4, color)?;
    }

    if battery.charging {
        display.text(x - 18, y - 2, "+", ORANGE, 1)?;
    }

    Ok(())
}
