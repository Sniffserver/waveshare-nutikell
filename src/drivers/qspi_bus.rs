// QSPI bus driver for CO5300 AMOLED display - DMA version
// Uses SpiDmaBus for large transfers via DMA

use alloc::vec;
use alloc::vec::Vec;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDmaBus};
use esp_hal::Blocking;

// Max bytes per DMA transfer (must fit in DMA TX buffer)
//
// ARCHITECTURAL NOTE:
// On ESP32-S3, SPI GDMA descriptors require buffers located in internal SRAM.
// External PSRAM cannot be directly addressed by the GDMA controller for SPI master
// transfers. Therefore, this 8 KB scratch buffer MUST be allocated in internal SRAM
// (160 KB total heap), while full framebuffers reside in PSRAM.
const DMA_CHUNK: usize = 8000;

pub struct QspiBus<'d> {
    spi: SpiDmaBus<'d, Blocking>,
    cs: Output<'d>,
    scratch: Vec<u8>, // heap-allocated scratch buffer for pixel conversion (internal SRAM)
    error_latched: bool,
}

impl<'d> QspiBus<'d> {
    pub fn new(spi: SpiDmaBus<'d, Blocking>, cs: Output<'d>) -> Self {
        Self {
            spi,
            cs,
            scratch: vec![0u8; DMA_CHUNK],
            error_latched: false,
        }
    }

    pub fn scratch_capacity_bytes(&self) -> usize {
        self.scratch.capacity()
    }

    fn cs_low(&mut self) {
        // Clear any latched errors from previous transactions so stale failures
        // do not poison new operations.
        self.error_latched = false;
        self.cs.set_low();
    }
    fn cs_high(&mut self) {
        self.cs.set_high();
    }

    pub fn write_command(&mut self, reg: u8) {
        self.cs_low();
        if self
            .spi
            .half_duplex_write(
                DataMode::Single,
                Command::_8Bit(0x02, DataMode::Single),
                Address::_24Bit((reg as u32) << 8, DataMode::Single),
                0,
                &[],
            )
            .is_err()
        {
            self.error_latched = true;
        }
        self.cs_high();
    }

    pub fn write_c8d8(&mut self, reg: u8, data: u8) {
        self.cs_low();
        if self
            .spi
            .half_duplex_write(
                DataMode::Single,
                Command::_8Bit(0x02, DataMode::Single),
                Address::_24Bit((reg as u32) << 8, DataMode::Single),
                0,
                &[data],
            )
            .is_err()
        {
            self.error_latched = true;
        }
        self.cs_high();
    }

    pub fn write_c8d16d16(&mut self, reg: u8, d1: u16, d2: u16) {
        let data = [(d1 >> 8) as u8, d1 as u8, (d2 >> 8) as u8, d2 as u8];
        self.cs_low();
        if self
            .spi
            .half_duplex_write(
                DataMode::Single,
                Command::_8Bit(0x02, DataMode::Single),
                Address::_24Bit((reg as u32) << 8, DataMode::Single),
                0,
                &data,
            )
            .is_err()
        {
            self.error_latched = true;
        }
        self.cs_high();
    }

    pub fn begin_pixels(&mut self) {
        self.cs_low();
        if self
            .spi
            .half_duplex_write(
                DataMode::Quad,
                Command::_8Bit(0x32, DataMode::Single),
                Address::_24Bit(0x003C00, DataMode::Single),
                0,
                &[],
            )
            .is_err()
        {
            self.error_latched = true;
        }
    }

    pub fn stream_pixels(&mut self, pixels: &[u16]) {
        if pixels.is_empty() {
            return;
        }
        let max_px = DMA_CHUNK / 2;
        let mut remaining = pixels;
        while !remaining.is_empty() {
            let n = remaining.len().min(max_px);
            copy_swap_pixels(&remaining[..n], &mut self.scratch[..n * 2]);
            if self
                .spi
                .half_duplex_write(
                    DataMode::Quad,
                    Command::None,
                    Address::None,
                    0,
                    &self.scratch[..n * 2],
                )
                .is_err()
            {
                self.error_latched = true;
            }
            remaining = &remaining[n..];
        }
    }

    pub fn end_pixels(&mut self) {
        self.cs_high();
    }

    pub fn write_pixels(&mut self, pixels: &[u16]) {
        if pixels.is_empty() {
            return;
        }
        self.cs_low();
        let max_px = DMA_CHUNK / 2;
        let mut remaining = pixels;
        let mut first = true;
        while !remaining.is_empty() {
            let n = remaining.len().min(max_px);
            copy_swap_pixels(&remaining[..n], &mut self.scratch[..n * 2]);
            if first {
                if self
                    .spi
                    .half_duplex_write(
                        DataMode::Quad,
                        Command::_8Bit(0x32, DataMode::Single),
                        Address::_24Bit(0x003C00, DataMode::Single),
                        0,
                        &self.scratch[..n * 2],
                    )
                    .is_err()
                {
                    self.error_latched = true;
                }
                first = false;
            } else {
                if self
                    .spi
                    .half_duplex_write(
                        DataMode::Quad,
                        Command::None,
                        Address::None,
                        0,
                        &self.scratch[..n * 2],
                    )
                    .is_err()
                {
                    self.error_latched = true;
                }
            }
            remaining = &remaining[n..];
        }
        self.cs_high();
    }

    pub fn write_repeat(&mut self, color: u16, count: u32) {
        if count == 0 {
            return;
        }
        let hi = (color >> 8) as u8;
        let lo = color as u8;
        let max_px = DMA_CHUNK / 2;
        // Fill scratch with repeated color
        for i in 0..max_px {
            self.scratch[i * 2] = hi;
            self.scratch[i * 2 + 1] = lo;
        }
        self.cs_low();
        let mut remaining = count;
        let mut first = true;
        while remaining > 0 {
            let n = remaining.min(max_px as u32);
            let bytes = (n as usize) * 2;
            if first {
                if self
                    .spi
                    .half_duplex_write(
                        DataMode::Quad,
                        Command::_8Bit(0x32, DataMode::Single),
                        Address::_24Bit(0x003C00, DataMode::Single),
                        0,
                        &self.scratch[..bytes],
                    )
                    .is_err()
                {
                    self.error_latched = true;
                }
                first = false;
            } else {
                if self
                    .spi
                    .half_duplex_write(
                        DataMode::Quad,
                        Command::None,
                        Address::None,
                        0,
                        &self.scratch[..bytes],
                    )
                    .is_err()
                {
                    self.error_latched = true;
                }
            }
            remaining -= n;
        }
        self.cs_high();
    }

    /// Return and clear a latched SPI/DMA failure. Hardware errors are not
    /// silently converted into a successful display update.
    pub fn take_error(&mut self) -> bool {
        let failed = self.error_latched;
        self.error_latched = false;
        failed
    }
}

/// Convert native little-endian `u16` RGB565 pixels to big-endian bytes for the AMOLED panel.
/// Unrolled 4 pixels (8 bytes) at a time to eliminate per-element bounds checks and allow
/// the compiler to use 32-bit word stores on Xtensa.
#[inline(always)]
fn copy_swap_pixels(src: &[u16], dst: &mut [u8]) {
    let chunks_src = src.chunks_exact(4);
    let mut chunks_dst = dst.chunks_exact_mut(8);
    let rem_src = chunks_src.remainder();

    for (s, d) in chunks_src.zip(&mut chunks_dst) {
        let b0 = s[0].to_be_bytes();
        let b1 = s[1].to_be_bytes();
        let b2 = s[2].to_be_bytes();
        let b3 = s[3].to_be_bytes();
        d[0] = b0[0];
        d[1] = b0[1];
        d[2] = b1[0];
        d[3] = b1[1];
        d[4] = b2[0];
        d[5] = b2[1];
        d[6] = b3[0];
        d[7] = b3[1];
    }

    let rem_dst = chunks_dst.into_remainder();
    for (i, &px) in rem_src.iter().enumerate() {
        let b = px.to_be_bytes();
        rem_dst[i * 2] = b[0];
        rem_dst[i * 2 + 1] = b[1];
    }
}
