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

pub fn align_to_even_region(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    panel_width: usize,
    panel_height: usize,
) -> Option<(usize, usize, usize, usize)> {
    if w == 0 || h == 0 || panel_width < 2 || panel_height < 2 {
        return None;
    }

    let x0 = x.min(panel_width.saturating_sub(2)) & !1;
    let y0 = y.min(panel_height.saturating_sub(2)) & !1;

    let raw_x1 = x.saturating_add(w).min(panel_width);
    let raw_y1 = y.saturating_add(h).min(panel_height);

    let x1 = raw_x1.saturating_add(1) & !1;
    let y1 = raw_y1.saturating_add(1) & !1;
    let x1 = x1.min(panel_width);
    let y1 = y1.min(panel_height);

    let flush_w = x1.saturating_sub(x0).max(2);
    let flush_h = y1.saturating_sub(y0).max(2);

    Some((x0, y0, flush_w, flush_h))
}

impl DisplayWindow {
    #[allow(clippy::too_many_arguments)]
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
