// PCF85063A RTC driver
// Reference: OLEDS3Watch/components/bsp_extra/src/pcf85063a.c
// I2C address 0x51, BCD encoded time registers

use embedded_hal::i2c::I2c;

const PCF85063A_ADDR: u8 = 0x51;

// Registers
const REG_CTRL1: u8 = 0x00;
const REG_CTRL2: u8 = 0x01;
const REG_SECONDS: u8 = 0x04;
const REG_MINUTES: u8 = 0x05;
const REG_HOURS: u8 = 0x06;
const REG_DAYS: u8 = 0x07;
const REG_WEEKDAYS: u8 = 0x08;
const REG_MONTHS: u8 = 0x09;
const REG_YEARS: u8 = 0x0A;

#[derive(Debug, Clone, Copy)]
pub struct DateTime {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub day: u8,
    pub weekday: u8,
    pub month: u8,
    pub year: u8, // 0-99 (2000-2099)
}

impl DateTime {
    pub fn new(year: u8, month: u8, day: u8, hours: u8, minutes: u8, seconds: u8) -> Self {
        Self {
            seconds,
            minutes,
            hours,
            day,
            weekday: 0,
            month,
            year,
        }
    }

    /// Shift this DateTime by a timezone offset in minutes.
    pub fn with_timezone_offset(&self, offset_mins: i16) -> DateTime {
        if offset_mins == 0 {
            return *self;
        }
        let total_mins = (self.hours as i32) * 60 + (self.minutes as i32) + (offset_mins as i32);
        let (day_delta, rem_mins) = if total_mins >= 0 {
            (total_mins / 1440, total_mins % 1440)
        } else {
            let d = (total_mins - 1439) / 1440;
            (d, total_mins - d * 1440)
        };
        let hours = (rem_mins / 60) as u8;
        let minutes = (rem_mins % 60) as u8;

        if day_delta == 0 {
            DateTime {
                hours,
                minutes,
                seconds: self.seconds,
                day: self.day,
                weekday: self.weekday,
                month: self.month,
                year: self.year,
            }
        } else {
            let days_in_month = match self.month {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => {
                    let y = 2000 + self.year as u32;
                    if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                        29
                    } else {
                        28
                    }
                }
                _ => 31,
            };
            let mut day = self.day as i32 + day_delta;
            let mut month = self.month as i32;
            let mut year = self.year as i32;

            if day > days_in_month {
                day -= days_in_month;
                month += 1;
                if month > 12 {
                    month = 1;
                    year += 1;
                }
            } else if day < 1 {
                month -= 1;
                if month < 1 {
                    month = 12;
                    year -= 1;
                }
                let prev_days = match month {
                    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                    4 | 6 | 9 | 11 => 30,
                    2 => {
                        let y = 2000 + year as u32;
                        if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                            29
                        } else {
                            28
                        }
                    }
                    _ => 31,
                };
                day += prev_days;
            }

            DateTime {
                hours,
                minutes,
                seconds: self.seconds,
                day: day as u8,
                weekday: self.weekday,
                month: month as u8,
                year: year as u8,
            }
        }
    }
}

pub struct Pcf85063aRtc<I> {
    i2c: I,
}

impl<I: I2c> Pcf85063aRtc<I> {
    pub fn new(i2c: I) -> Self {
        Self { i2c }
    }

    fn read_reg(&mut self, reg: u8) -> Result<u8, I::Error> {
        let mut buf = [0u8];
        self.i2c.write_read(PCF85063A_ADDR, &[reg], &mut buf)?;
        Ok(buf[0])
    }

    fn write_reg(&mut self, reg: u8, val: u8) -> Result<(), I::Error> {
        self.i2c.write(PCF85063A_ADDR, &[reg, val])
    }

    /// Initialize RTC: ensure oscillator running, 24h mode.
    pub fn init(&mut self) -> Result<(), I::Error> {
        let ctrl1 = self.read_reg(REG_CTRL1)?;
        // Clear STOP bit (bit 5) to start oscillator
        // Clear 12_24 bit (bit 2) for 24-hour mode
        let new_ctrl1 = ctrl1 & !(0x20 | 0x04);
        if new_ctrl1 != ctrl1 {
            self.write_reg(REG_CTRL1, new_ctrl1)?;
        }
        Ok(())
    }

    /// Check whether the oscillator stop flag (OS) is set, indicating
    /// that oscillator stopped (e.g. power loss or battery depletion).
    pub fn is_oscillator_stopped(&mut self) -> Result<bool, I::Error> {
        let sec = self.read_reg(REG_SECONDS)?;
        Ok(sec & 0x80 != 0)
    }

    /// Read current date/time.
    pub fn get_time(&mut self) -> Result<DateTime, I::Error> {
        // Read all time registers in one burst (7 bytes from 0x04)
        let mut buf = [0u8; 7];
        self.i2c
            .write_read(PCF85063A_ADDR, &[REG_SECONDS], &mut buf)?;

        Ok(DateTime {
            seconds: bcd_to_dec(buf[0] & 0x7F), // mask OS bit
            minutes: bcd_to_dec(buf[1] & 0x7F),
            hours: bcd_to_dec(buf[2] & 0x3F), // 24h mode
            day: bcd_to_dec(buf[3] & 0x3F),
            weekday: buf[4] & 0x07,
            month: bcd_to_dec(buf[5] & 0x1F),
            year: bcd_to_dec(buf[6]),
        })
    }

    /// Set date/time using an atomic 7-byte burst write.
    pub fn set_time(&mut self, dt: &DateTime) -> Result<(), I::Error> {
        // Stop oscillator
        let ctrl1 = self.read_reg(REG_CTRL1)?;
        self.write_reg(REG_CTRL1, ctrl1 | 0x20)?;

        // Write time registers atomically in a single I2C burst transaction.
        // Bit 7 of seconds is written as 0, clearing the OS (oscillator stop) flag.
        let payload = [
            REG_SECONDS,
            dec_to_bcd(dt.seconds),
            dec_to_bcd(dt.minutes),
            dec_to_bcd(dt.hours),
            dec_to_bcd(dt.day),
            dt.weekday,
            dec_to_bcd(dt.month),
            dec_to_bcd(dt.year),
        ];
        self.i2c.write(PCF85063A_ADDR, &payload)?;

        // Restart oscillator
        self.write_reg(REG_CTRL1, ctrl1 & !0x20)?;
        Ok(())
    }
}

fn bcd_to_dec(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

fn dec_to_bcd(dec: u8) -> u8 {
    ((dec / 10) << 4) | (dec % 10)
}
