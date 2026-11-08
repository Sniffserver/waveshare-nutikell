#[path = "../src/drivers/display_window.rs"]
mod display_window;

use display_window::{align_to_even_region, DisplayWindow, WindowError};

#[test]
fn aligns_dirty_regions_to_even_panel_bounds() {
    assert_eq!(align_to_even_region(3, 5, 4, 7, 10, 12), Some((2, 4, 6, 8)));
    assert_eq!(
        align_to_even_region(9, 11, 1, 1, 10, 12),
        Some((8, 10, 2, 2))
    );
    assert_eq!(align_to_even_region(0, 0, 0, 1, 10, 12), None);
}

#[test]
fn accepts_full_panel_and_applies_offsets() {
    assert_eq!(
        DisplayWindow::checked(0, 0, 410, 502, 410, 502, 22, 0).unwrap(),
        DisplayWindow {
            x_start: 22,
            x_end: 431,
            y_start: 0,
            y_end: 501,
        }
    );
}

#[test]
fn rejects_empty_outside_and_overflowing_windows() {
    assert_eq!(
        DisplayWindow::checked(0, 0, 0, 1, 410, 502, 0, 0),
        Err(WindowError::Empty)
    );
    assert_eq!(
        DisplayWindow::checked(409, 0, 2, 1, 410, 502, 0, 0),
        Err(WindowError::OutsidePanel)
    );
    assert_eq!(
        DisplayWindow::checked(0, 0, 410, 1, 410, 502, u16::MAX, 0),
        Err(WindowError::CoordinateOverflow)
    );
}
