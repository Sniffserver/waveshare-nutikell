//! Host-testable checked address-window contract for the CO5300 panel.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayWindow {
    pub x_start: u16,
    pub x_end: u16,
    pub y_start: u16,
    pub y_end: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    Empty,
    OutsidePanel,
    CoordinateOverflow,
}

impl DisplayWindow {
    pub fn checked(
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        panel_width: u16,
        panel_height: u16,
        col_offset: u16,
        row_offset: u16,
    ) -> Result<Self, WindowError> {
        if w == 0 || h == 0 {
            return Err(WindowError::Empty);
        }
        if x >= panel_width || y >= panel_height || w > panel_width - x || h > panel_height - y {
            return Err(WindowError::OutsidePanel);
        }
        let x_start = x
            .checked_add(col_offset)
            .ok_or(WindowError::CoordinateOverflow)?;
        let x_end = x_start
            .checked_add(w - 1)
            .ok_or(WindowError::CoordinateOverflow)?;
        let y_start = y
            .checked_add(row_offset)
            .ok_or(WindowError::CoordinateOverflow)?;
        let y_end = y_start
            .checked_add(h - 1)
            .ok_or(WindowError::CoordinateOverflow)?;
        Ok(Self {
            x_start,
            x_end,
            y_start,
            y_end,
        })
    }
}
