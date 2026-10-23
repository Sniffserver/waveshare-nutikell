// QMI8658 6-axis IMU driver (Accelerometer + Gyroscope)
// Reference: SensorLib/src/SensorQMI8658.hpp
// I2C address 0x6B

use embedded_hal::i2c::I2c;

const QMI8658_ADDR: u8 = 0x6B;

// Registers
const REG_WHO_AM_I: u8 = 0x00;
const REG_CTRL1: u8 = 0x02; // Serial interface and sensor enable
const REG_CTRL2: u8 = 0x03; // Accelerometer settings
const REG_CTRL3: u8 = 0x04; // Gyroscope settings
const REG_CTRL5: u8 = 0x06; // Low-pass filter
const REG_CTRL7: u8 = 0x08; // Enable sensors
const REG_AX_L: u8 = 0x35; // Accel X low byte
const REG_GX_L: u8 = 0x3B; // Gyro X low byte
const REG_TEMP_L: u8 = 0x33; // Temperature low byte
const REG_STATUS0: u8 = 0x2E; // Status 0 register

const QMI8658_WHO_AM_I: u8 = 0x05; // Expected chip ID

#[derive(Debug, Clone, Copy, Default)]
pub struct AccelData {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GyroData {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub struct Qmi8658Imu<I> {
    i2c: I,
    accel_scale: f32,
    gyro_scale: f32,
}

impl<I: I2c> Qmi8658Imu<I> {
    pub fn new(i2c: I) -> Self {
        Self {
            i2c,
            accel_scale: 1.0 / 4096.0, // ±8g default
            gyro_scale: 1.0 / 64.0,    // ±512 dps default
        }
    }

    fn read_reg(&mut self, reg: u8) -> Result<u8, I::Error> {
        let mut buf = [0u8];
        self.i2c.write_read(QMI8658_ADDR, &[reg], &mut buf)?;
        Ok(buf[0])
    }

    fn write_reg(&mut self, reg: u8, val: u8) -> Result<(), I::Error> {
        self.i2c.write(QMI8658_ADDR, &[reg, val])
    }

    fn read_regs(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), I::Error> {
        self.i2c.write_read(QMI8658_ADDR, &[reg], buf)
    }

    /// Initialize the IMU. Returns true if chip ID matches.
    pub fn init(&mut self) -> Result<bool, I::Error> {
        let id = self.read_reg(REG_WHO_AM_I)?;
        if id != QMI8658_WHO_AM_I {
            return Ok(false);
        }

        // Reset: CTRL1 bit 6 = soft reset (auto-clear)
        self.write_reg(REG_CTRL1, 0x40)?;
        // Wait for reset (no delay available here, just write next configs)

        // CTRL1: address auto-increment enabled
        self.write_reg(REG_CTRL1, 0x60)?;

        // CTRL2: Accelerometer ODR=119Hz, Full scale=±8g
        // ODR[3:0]=0b0101 (119Hz), FS[6:4]=0b010 (±8g)
        self.write_reg(REG_CTRL2, 0x25)?;
        self.accel_scale = 8.0 / 32768.0; // ±8g

        // CTRL3: Gyroscope ODR=119Hz, Full scale=±512dps
        // ODR[3:0]=0b0101 (119Hz), FS[6:4]=0b011 (±512dps)
        self.write_reg(REG_CTRL3, 0x35)?;
        self.gyro_scale = 512.0 / 32768.0; // ±512dps

        // CTRL5: Low-pass filter enabled for both accel and gyro
        self.write_reg(REG_CTRL5, 0x11)?;

        // CTRL7: Enable accelerometer and gyroscope
        // Bit 0: accel enable, Bit 1: gyro enable
        self.write_reg(REG_CTRL7, 0x03)?;

        Ok(true)
    }

    /// Power down accelerometer + gyroscope. Call when sensors are not needed.
    /// Reduces idle current significantly (gyro alone draws ~1.5 mA).
    pub fn power_down(&mut self) -> Result<(), I::Error> {
        self.write_reg(REG_CTRL7, 0x00)
    }

    /// Power up accelerometer + gyroscope. Call before reading.
    pub fn power_up(&mut self) -> Result<(), I::Error> {
        self.write_reg(REG_CTRL7, 0x03)
    }

    /// Enable accelerometer only (power down gyroscope to save ~1.5mA).
    pub fn enable_accel_only(&mut self) -> Result<(), I::Error> {
        self.write_reg(REG_CTRL7, 0x01)
    }

    /// Check if accelerometer data is ready in STATUS0.
    pub fn accel_ready(&mut self) -> Result<bool, I::Error> {
        let status = self.read_reg(REG_STATUS0)?;
        Ok(status & 0x01 != 0)
    }

    /// Initialize accelerometer only (low-power navigation mode).
    pub fn init_accel_only(&mut self) -> Result<bool, I::Error> {
        let id = self.read_reg(REG_WHO_AM_I)?;
        if id != QMI8658_WHO_AM_I {
            return Ok(false);
        }
        self.write_reg(REG_CTRL7, 0x00)?;
        self.write_reg(REG_CTRL1, 0x60)?;
        self.write_reg(REG_CTRL2, 0x25)?;
        self.accel_scale = 8.0 / 32768.0;
        self.write_reg(REG_CTRL3, 0x00)?;
        self.write_reg(REG_CTRL5, 0x11)?;
        self.write_reg(REG_CTRL7, 0x01)?;
        Ok(true)
    }

    /// Enable both accelerometer and gyroscope (for Level / 6-axis mode).
    pub fn enable_6axis(&mut self) -> Result<(), I::Error> {
        self.write_reg(REG_CTRL7, 0x03)
    }

    /// Read accelerometer data in g.
    pub fn read_accel(&mut self) -> Result<AccelData, I::Error> {
        let mut buf = [0u8; 6];
        self.read_regs(REG_AX_L, &mut buf)?;

        let x = i16::from_le_bytes([buf[0], buf[1]]) as f32 * self.accel_scale;
        let y = i16::from_le_bytes([buf[2], buf[3]]) as f32 * self.accel_scale;
        let z = i16::from_le_bytes([buf[4], buf[5]]) as f32 * self.accel_scale;

        Ok(AccelData { x, y, z })
    }

    /// Read gyroscope data in degrees per second.
    pub fn read_gyro(&mut self) -> Result<GyroData, I::Error> {
        let mut buf = [0u8; 6];
        self.read_regs(REG_GX_L, &mut buf)?;

        let x = i16::from_le_bytes([buf[0], buf[1]]) as f32 * self.gyro_scale;
        let y = i16::from_le_bytes([buf[2], buf[3]]) as f32 * self.gyro_scale;
        let z = i16::from_le_bytes([buf[4], buf[5]]) as f32 * self.gyro_scale;

        Ok(GyroData { x, y, z })
    }

    /// Read chip temperature in °C.
    pub fn read_temperature(&mut self) -> Result<f32, I::Error> {
        let mut buf = [0u8; 2];
        self.read_regs(REG_TEMP_L, &mut buf)?;
        let raw = i16::from_le_bytes([buf[0], buf[1]]);
        Ok(raw as f32 / 256.0)
    }

    /// Read chip ID.
    pub fn read_chip_id(&mut self) -> Result<u8, I::Error> {
        self.read_reg(REG_WHO_AM_I)
    }
}

/// Calculate magnitude of a 3-axis acceleration vector in g.
pub fn magnitude(sample: AccelData) -> f32 {
    libm::sqrtf(sample.x * sample.x + sample.y * sample.y + sample.z * sample.z)
}

// ── Debug helpers ────────────────────────────────────────────────────────────

impl<I: I2c> Qmi8658Imu<I> {
    /// Read 6 raw accelerometer bytes without any scaling.
    /// Useful to verify register address, auto-increment, and data ready.
    ///
    /// Expected: bytes are non-zero when sensor is enabled via CTRL7.
    /// If all bytes are 0x00 → check CTRL7 and REG_STATUS0.
    /// If bytes are 0xFF   → bus floating or wrong address.
    pub fn read_accel_raw(&mut self) -> Result<[u8; 6], I::Error> {
        let mut raw = [0u8; 6];
        self.read_regs(REG_AX_L, &mut raw)?;
        Ok(raw)
    }

    /// Read WHO_AM_I from both 0x6A and 0x6B and log results.
    /// Returns the address that responded with 0x05, or None.
    pub fn probe_addresses(&mut self) -> Option<u8> {
        for &addr in &[0x6A_u8, 0x6B_u8] {
            let mut who = [0u8; 1];
            match self.i2c.write_read(addr, &[0x00], &mut who) {
                Ok(()) => {
                    println!(
                        "[QMI] addr={:#04x} WHO_AM_I={:#04x}{}",
                        addr,
                        who[0],
                        if who[0] == 0x05 { " ✓" } else { " (unexpected)" }
                    );
                    if who[0] == 0x05 {
                        return Some(addr);
                    }
                }
                Err(_) => {
                    println!("[QMI] addr={:#04x} → no response", addr);
                }
            }
        }
        None
    }

    /// Dump key configuration registers for debugging.
    pub fn dump_config(&mut self) {
        let regs = [
            (REG_WHO_AM_I, "WHO_AM_I"),
            (REG_CTRL1,    "CTRL1   "),
            (REG_CTRL2,    "CTRL2   "),
            (REG_CTRL3,    "CTRL3   "),
            (REG_CTRL5,    "CTRL5   "),
            (REG_CTRL7,    "CTRL7   "),
            (REG_STATUS0,  "STATUS0 "),
        ];
        println!("[QMI] ── register dump ──────────────────");
        for (reg, name) in regs {
            match self.read_reg(reg) {
                Ok(v)  => println!("[QMI]  {name} ({:#04x}) = {:#04x}  ({:08b}b)", reg, v, v),
                Err(_) => println!("[QMI]  {name} ({:#04x}) = ERR", reg),
            }
        }
        println!("[QMI] ────────────────────────────────────");
    }
}

/// Scan the I²C bus for all responding devices (0x08–0x77).
/// Call once at startup during debugging.
pub fn scan_i2c<I: I2c>(i2c: &mut I) {
    println!("[I2C] ── bus scan ────────────────────────");
    let mut found = 0u8;
    for addr in 0x08_u8..=0x77 {
        let mut byte = [0u8; 1];
        if i2c.write_read(addr, &[0x00], &mut byte).is_ok() {
            println!("[I2C]  found {:#04x}  reg0={:#04x}", addr, byte[0]);
            found += 1;
        }
    }
    if found == 0 {
        println!("[I2C]  no devices found — check pull-ups, VCC, GND, SDA/SCL");
    }
    println!("[I2C] ─────────────────────────────────────");
}
