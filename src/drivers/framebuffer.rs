// PSRAM Framebuffer for CO5300 display
// 410x502 RGB565 = 411,640 bytes (~402KB)
// Draws to RAM, then flushes entire screen via DMA QSPI

use embedded_graphics_core::draw_target::DrawTarget;
use embedded_graphics_core::geometry::{OriginDimensions, Size};
use embedded_graphics_core::pixelcolor::raw::RawU16;
use embedded_graphics_core::pixelcolor::Rgb565;
use embedded_graphics_core::prelude::*;
use embedded_graphics_core::primitives::Rectangle;

use crate::board;
use crate::drivers::co5300::Co5300Display;
use crate::drivers::co5300::DisplayError;

use alloc::vec;
use alloc::vec::Vec;

const WIDTH: usize = board::LCD_WIDTH as usize;
const HEIGHT: usize = board::LCD_HEIGHT as usize;
const PIXEL_COUNT: usize = WIDTH * HEIGHT;

pub struct Framebuffer {
    front: Vec<u16>,
    back: Vec<u16>,
}

impl Framebuffer {
    /// Allocate true front and back framebuffers in PSRAM (via global allocator).
    /// Provides zero-tearing double-buffered rendering for games and animations.
    pub fn new() -> Self {
        let front = vec![0u16; PIXEL_COUNT];
        let back = vec![0u16; PIXEL_COUNT];
        Self { front, back }
    }

    pub fn allocated_bytes(&self) -> usize {
        (self.front.capacity() + self.back.capacity()) * core::mem::size_of::<u16>()
    }

    /// Clear the back framebuffer with a color.
    pub fn clear_color(&mut self, color: Rgb565) {
        let raw = RawU16::from(color).into_inner();
        self.back.fill(raw);
    }

    /// Set a single pixel in back buffer (no bounds check for speed).
    #[inline(always)]
    pub fn set_pixel(&mut self, x: usize, y: usize, color: u16) {
        if x < WIDTH && y < HEIGHT {
            self.back[y * WIDTH + x] = color;
        }
    }

    /// Fill a rectangular region in back buffer.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u16) {
        let x_end = x.saturating_add(w).min(WIDTH);
        let y_end = y.saturating_add(h).min(HEIGHT);
        for row in y..y_end {
            let start = row * WIDTH + x;
            let end = row * WIDTH + x_end;
            self.back[start..end].fill(color);
        }
    }

    /// Wait for TE VSync signal (rising edge / VBlank) asynchronously without CPU busy-wait.
    pub async fn wait_vsync(&self, te: &mut esp_hal::gpio::Input<'_>) {
        let _ = embassy_time::with_timeout(
            embassy_time::Duration::from_millis(20),
            te.wait_for_rising_edge(),
        )
        .await;
    }

    /// True double-buffer swap + async VSync flush.
    /// 1. Swap front and back buffers (instant Vec pointer swap)
    /// 2. Wait for TE signal (VBlank) asynchronously (executor remains unblocked)
    /// 3. Flush the new front buffer to display via DMA QSPI
    /// 4. Synchronize back buffer to maintain baseline for partial redraws
    /// Result: display always shows a complete frame, zero tearing.
    pub async fn swap_and_flush(
        &mut self,
        display: &mut Co5300Display<'_>,
        te: &mut esp_hal::gpio::Input<'_>,
    ) -> Result<(), DisplayError> {
        core::mem::swap(&mut self.front, &mut self.back);
        self.wait_vsync(te).await;
        display.set_addr_window(0, 0, WIDTH as u16, HEIGHT as u16)?;
        display.bus_mut().write_pixels(&self.front);
        self.back.copy_from_slice(&self.front);
        if display.bus_mut().take_error() {
            Err(DisplayError::BusError)
        } else {
            Ok(())
        }
    }

    /// VSync flush for watchface / menus using async edge wait.
    pub async fn flush_vsync(
        &mut self,
        display: &mut Co5300Display<'_>,
        te: &mut esp_hal::gpio::Input<'_>,
    ) -> Result<(), DisplayError> {
        self.wait_vsync(te).await;
        self.flush(display)
    }

    /// Flush the entire back framebuffer to the display via DMA QSPI.
    pub fn flush(&self, display: &mut Co5300Display<'_>) -> Result<(), DisplayError> {
        display.set_addr_window(0, 0, WIDTH as u16, HEIGHT as u16)?;
        display.bus_mut().write_pixels(&self.back);
        if display.bus_mut().take_error() {
            Err(DisplayError::BusError)
        } else {
            Ok(())
        }
    }

    /// Flush only a rectangular region (dirty rect optimization).
    pub fn flush_region(&self, display: &mut Co5300Display<'_>, x: u16, y: u16, w: u16, h: u16) {
        let Some((x0, y0, flush_w, flush_h)) = align_to_even_region(x, y, w, h) else {
            return;
        };

        if display
            .set_addr_window(x0 as u16, y0 as u16, flush_w as u16, flush_h as u16)
            .is_err()
        {
            return;
        }
        display.bus_mut().begin_pixels();
        for row in y0..(y0 + flush_h) {
            let start = row * WIDTH + x0;
            let end = start + flush_w;
            display.bus_mut().stream_pixels(&self.back[start..end]);
        }
        display.bus_mut().end_pixels();
    }

    /// Get raw back buffer for direct access.
    pub fn buffer(&self) -> &[u16] {
        &self.back
    }

    /// Get mutable raw back buffer for direct access (snapshot restore).
    pub fn buffer_mut(&mut self) -> &mut [u16] {
        &mut self.back
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb565;
    type Error = DisplayError;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels.into_iter() {
            if coord.x >= 0 && coord.x < WIDTH as i32 && coord.y >= 0 && coord.y < HEIGHT as i32 {
                let raw = RawU16::from(color).into_inner();
                self.back[coord.y as usize * WIDTH + coord.x as usize] = raw;
            }
        }
        Ok(())
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let area = area.intersection(&Rectangle::new(
            Point::zero(),
            Size::new(WIDTH as u32, HEIGHT as u32),
        ));
        if area.size.width == 0 || area.size.height == 0 {
            return Ok(());
        }

        let x = area.top_left.x as usize;
        let y = area.top_left.y as usize;
        let w = area.size.width as usize;
        let mut row = y;
        let mut col = 0;

        for color in colors.into_iter() {
            if col < w && row < HEIGHT {
                self.back[row * WIDTH + x + col] = RawU16::from(color).into_inner();
            }
            col += 1;
            if col >= w {
                col = 0;
                row += 1;
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let area = area.intersection(&Rectangle::new(
            Point::zero(),
            Size::new(WIDTH as u32, HEIGHT as u32),
        ));
        if area.size.width == 0 || area.size.height == 0 {
            return Ok(());
        }
        let raw = RawU16::from(color).into_inner();
        self.fill_rect(
            area.top_left.x as usize,
            area.top_left.y as usize,
            area.size.width as usize,
            area.size.height as usize,
            raw,
        );
        Ok(())
    }
}
