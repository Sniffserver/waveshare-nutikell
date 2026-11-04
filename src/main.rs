#![no_std]
#![no_main]

extern crate alloc;

pub fn log_memory(label: &'static str) {
    let stats = esp_alloc::HEAP.stats();
    let mut internal_free = 0;
    let mut internal_used = 0;
    let mut psram_free = 0;
    let mut psram_used = 0;

    for r in stats.region_stats.iter().flatten() {
        if r.capabilities
            .contains(esp_alloc::MemoryCapability::Internal)
        {
            internal_free += r.free;
            internal_used += r.used;
        } else if r
            .capabilities
            .contains(esp_alloc::MemoryCapability::External)
        {
            psram_free += r.free;
            psram_used += r.used;
        }
    }

    println!(
        "[MEM:{}] internal: free={} used={} | psram: free={} used={}",
        label, internal_free, internal_used, psram_free, psram_used
    );

    if internal_free < 16 * 1024 {
        println!(
            "[MEM:{}] WARNING: Low internal heap headroom (<16KB)!",
            label
        );
    }
}

mod apps;
mod ble_analyzer;
mod board;
mod drivers;
mod outdoor;
mod peripherals;
mod ui;

use core::cell::RefCell;

use embedded_graphics_core::prelude::RawData;
use embedded_hal_bus::i2c::RefCellDevice;
use esp_alloc as _;
use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::RgbColor;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
// use esp_hal::i2s::master::{I2s, Config as I2sConfig, DataFormat}; // TODO: wire I2S
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use static_cell::StaticCell;

use crate::apps::authenticator::AuthenticatorApp;
use crate::apps::flappy::FlappyGame;
use crate::apps::game2048::Game2048;
use crate::apps::maze::MazeGame;
use crate::apps::media_controller::{MediaAction, MediaCommand, MediaController};
use crate::apps::settings::SettingsApp;
use crate::apps::snake::SnakeGame;
use crate::apps::tetris::TetrisGame;
use crate::apps::wifi_setup::{SetupPhase, WifiSetupApp, AP_SSID};
use crate::apps::work_tracker::WorkTracker;
use crate::apps::{App, AppInput, AppResult, AppState};
use crate::ble_analyzer::BleAnalyzerApp;
use crate::drivers::co5300::Co5300Display;
use crate::drivers::co5300::DisplayError;
use crate::drivers::framebuffer::Framebuffer;
use crate::drivers::qspi_bus::QspiBus;
use crate::peripherals::active_work::{
    calendar_to_unix, clear_active_session, load_active_session, save_active_session,
    ActiveWorkSession,
};
use crate::peripherals::audio::{fill_beep_buffer, fill_chime_buffer, Es8311};
use crate::peripherals::captive::run_ap_services;
use crate::peripherals::imu::Qmi8658Imu;
use crate::peripherals::nvs::{read_credentials, write_credentials, StoredCredentials};
use crate::peripherals::power::Axp2101Power;
use crate::peripherals::rtc::{DateTime, Pcf85063aRtc};
use crate::peripherals::session_log::{
    append_session, contains_session_id, next_session_id, session_log_health, LogSeverity,
    SessionRecord,
};
use crate::peripherals::settings::load_settings;
use crate::peripherals::touch::{Ft3168Touch, SwipeDirection, TouchPoint};
use crate::ui::launcher::Launcher;
use crate::ui::pages::{self, Page};
use crate::ui::power_page;
use crate::ui::watchface::{WatchFace, SLEEP_DISSOLVE_FRAMES};

// Network runner task (must be spawned for WiFi to work)
#[embassy_executor::task(pool_size = 1)]
async fn net_task(
    mut runner: embassy_net::Runner<'static, esp_radio::wifi::WifiDevice<'static>>,
) -> ! {
    runner.run().await
}

fn report_display(result: Result<(), DisplayError>) {
    if let Err(error) = result {
        println!("[DISPLAY] runtime operation failed: {:?}", error);
    }
}

// Simple NTP sync (UDP to pool.ntp.org:123)
async fn ntp_sync(
    stack: embassy_net::Stack<'static>,
    rtc: &mut crate::peripherals::rtc::Pcf85063aRtc<impl embedded_hal::i2c::I2c>,
) -> Result<(), ()> {
    use embassy_net::udp::{PacketMetadata, UdpSocket};

    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 256];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(12345).map_err(|_| ())?;

    // NTP request packet (simplified: 48 bytes, first byte = 0x1B for client mode)
    let mut ntp_request = [0u8; 48];
    ntp_request[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)

    // Resolve pool.ntp.org (use Google's NTP IP directly: 216.239.35.0)
    let ntp_addr = embassy_net::Ipv4Address::new(216, 239, 35, 0);
    println!("[NTP] sending request");

    socket
        .send_to(&ntp_request, (ntp_addr, 123))
        .await
        .map_err(|_| ())?;

    println!("[NTP] request sent");

    // Wait for response (timeout 5s)
    let mut response = [0u8; 48];
    match embassy_time::with_timeout(Duration::from_secs(5), socket.recv_from(&mut response)).await
    {
        Ok(Ok((len, addr))) if len >= 48 => {
            println!("[NTP] response: {} bytes from {:?}", len, addr);
            // Reject unsynchronised/invalid server replies before touching the
            // RTC. LI=3 means alarm, server mode is 4, and stratum 0/16 is
            // unsynchronised/reserved.
            let li = response[0] >> 6;
            let version = (response[0] >> 3) & 0x07;
            let mode = response[0] & 0x07;
            let stratum = response[1];
            if li == 3 || !(3..=4).contains(&version) || mode != 4 || !(1..=15).contains(&stratum) {
                println!(
                    "[NTP] rejected invalid server reply li={} version={} mode={} stratum={}",
                    li, version, mode, stratum
                );
                return Err(());
            }
            // Parse NTP timestamp (bytes 40-43 = seconds since 1900-01-01)
            let ntp_secs =
                u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
            // Convert epochs with checked arithmetic. Time-zone presentation is
            // a UI/settings concern; RTC is kept in UTC to avoid hard-coded DST.
            let unix_secs = ntp_secs.checked_sub(2_208_988_800).ok_or(())?;
            let time_of_day = unix_secs % 86400;
            let hours = (time_of_day / 3600) as u8;
            let minutes = ((time_of_day % 3600) / 60) as u8;
            let seconds = (time_of_day % 60) as u8;

            // Calculate date (simplified: days since epoch)
            let total_days = (unix_secs / 86400) as i32;
            // Simple date from days since 1970-01-01
            let (year, month, day) = days_to_date(total_days);

            println!(
                "[NTP] Time: {:02}:{:02}:{:02} {:02}/{:02}/{}",
                hours, minutes, seconds, day, month, year
            );

            if !(2000..=2255).contains(&year) {
                println!("[NTP] rejected RTC-unrepresentable year {}", year);
                return Err(());
            }
            // Set RTC only after all validity checks pass.
            let dt = crate::peripherals::rtc::DateTime::new(
                (year - 2000) as u8,
                month as u8,
                day as u8,
                hours,
                minutes,
                seconds,
            );
            rtc.set_time(&dt).map_err(|_| ())
        }
        Ok(Ok((len, addr))) => {
            println!("[NTP] short response: {} bytes from {:?}", len, addr);
            Err(())
        }
        Ok(Err(_)) => {
            println!("[NTP] UDP receive failed");
            Err(())
        }
        Err(_) => {
            println!("[NTP] receive timeout");
            Err(())
        }
    }
}

fn days_to_date(days_since_epoch: i32) -> (u32, u32, u32) {
    // Simplified date calculation from days since 1970-01-01
    let mut y = 1970i32;
    let mut remaining = days_since_epoch;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while m < 12 && remaining >= month_days[m] {
        remaining -= month_days[m];
        m += 1;
    }
    (y as u32, (m + 1) as u32, (remaining + 1) as u32)
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    // Reserve a bounded internal heap and keep large allocations in PSRAM.
    // The ESP32-S3 data region is shared with Wi-Fi/radio statics, so a 320 KiB
    // heap leaves no room for the firmware image to link. 160 KiB preserves
    // radio headroom for BLE + WiFi while the framebuffer and application allocations use
    // the PSRAM allocator below.
    esp_alloc::heap_allocator!(size: 160 * 1024);
    // Power-aware: default to 160MHz instead of 240MHz.
    // Saves ~30% CPU power without noticeable impact on UI/sensor work.
    // Game code can still trigger short bursts via DMA/peripherals at 80MHz QSPI which is unchanged.
    let peripherals =
        esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_160MHz));

    // PSRAM
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
    log_memory("init");

    // Embassy timer
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    esp_println::logger::init_logger_from_env();
    println!("=== Waveshare Watch RS v0.4 (Embassy) ===");

    let delay = Delay::new();

    // === I2C Bus ===
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_hz(crate::board::I2C_FREQ_HZ)),
    )
    .expect("I2C failed")
    .with_sda(peripherals.GPIO15)
    .with_scl(peripherals.GPIO14);
    let i2c_ref = RefCell::new(i2c);

    // === Power ===
    let mut power = Axp2101Power::new(RefCellDevice::new(&i2c_ref));
    if power.init().is_err() {
        println!("[POWER] initialization failed");
        return;
    }
    // Drop the die-temp ADC channel we never show on the UI — shaves a
    // few hundred µA off the AXP2101 housekeeping load.
    let _ = power.trim_adc_channels();
    println!("[POWER] OK");
    println!("[HIL][PASS] pmic");

    // === Display 80MHz DMA ===
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(80))
        .with_mode(SpiMode::_0);
    let (rx_buf, rx_desc, tx_buf, tx_desc) = dma_buffers!(8000);
    let dma_rx = DmaRxBuf::new(rx_desc, rx_buf).unwrap();
    let dma_tx = DmaTxBuf::new(tx_desc, tx_buf).unwrap();
    let spi = Spi::new(peripherals.SPI2, spi_config)
        .expect("SPI failed")
        .with_sck(peripherals.GPIO11)
        .with_sio0(peripherals.GPIO4)
        .with_sio1(peripherals.GPIO5)
        .with_sio2(peripherals.GPIO6)
        .with_sio3(peripherals.GPIO7)
        .with_dma(peripherals.DMA_CH0)
        .with_buffers(dma_rx, dma_tx);
    let cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let reset = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let mut display = Co5300Display::new(QspiBus::new(spi, cs), reset);
    if let Err(error) = display.init() {
        println!("[DISPLAY] initialization failed: {:?}", error);
        return;
    }

    // Enable Tearing Effect output on CO5300 (TE pin = GPIO13)
    // Command 0x35 = TEARON, param 0x00 = VBlank only
    display.bus_mut().write_c8d8(0x35, 0x00);
    let te_pin = Input::new(peripherals.GPIO13, InputConfig::default());
    println!("[DISPLAY] OK (TE VSync enabled)");
    println!("[HIL][PASS] display");

    // === Framebuffer PSRAM ===
    let mut fb = Framebuffer::new();
    fb.clear_color(Rgb565::BLACK);
    if let Err(error) = fb.flush(&mut display) {
        println!("[DISPLAY] initial framebuffer flush failed: {:?}", error);
        return;
    }
    println!("[FB] OK");
    log_memory("after-display");

    // === Touch ===
    let mut touch_rst = Output::new(peripherals.GPIO9, Level::High, OutputConfig::default());
    // GPIO38 is the FT3168 INT line: held high by pull-up, pulled low by the controller
    // when a finger is on the screen. We use it both for level checks and as an async wake source.
    let mut touch_int = Input::new(
        peripherals.GPIO38,
        InputConfig::default().with_pull(Pull::Up),
    );
    touch_rst.set_low();
    delay.delay_millis(10);
    touch_rst.set_high();
    delay.delay_millis(50);
    let mut touch = Ft3168Touch::new(RefCellDevice::new(&i2c_ref));
    if touch.init().is_err() {
        println!("[TOUCH] initialization failed");
        return;
    }
    println!("[TOUCH] OK");
    println!("[HIL][PASS] touch");
    let mut boot_button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    // === RTC ===
    let mut rtc = Pcf85063aRtc::new(RefCellDevice::new(&i2c_ref));
    if rtc.init().is_err() {
        println!("[RTC] initialization failed");
        return;
    }
    println!("[RTC] OK");
    println!("[HIL][PASS] rtc");

    // === I2C Bus Scan ===
    {
        let mut scan_dev = RefCellDevice::new(&i2c_ref);
        crate::peripherals::imu::scan_i2c(&mut scan_dev);
    }

    // === IMU ===
    let mut imu = Qmi8658Imu::new(RefCellDevice::new(&i2c_ref));
    let _ = imu.probe_addresses();
    let chip_id = match imu.read_chip_id() {
        Ok(id) => Some(id),
        Err(_) => {
            println!("[IMU] WHO_AM_I read failed");
            None
        }
    };
    let init_ok = match imu.init() {
        Ok(ok) => ok,
        Err(_) => {
            println!("[IMU] initialization failed");
            false
        }
    };
    println!(
        "[IMU] WHO_AM_I = {:?} (expected 0x05), init_ok: {}",
        chip_id, init_ok
    );
    imu.dump_config();
    if let Ok(raw) = imu.read_accel_raw() {
        println!(
            "[IMU] Raw accel bytes: {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]
        );
        if init_ok {
            println!("[HIL][PASS] imu");
        }
    }

    // Sample resting acceleration to verify scale and |a| ~ 1.0g
    let _ = imu.power_up();
    for i in 1..=5 {
        Timer::after(Duration::from_millis(25)).await;
        if let Ok(Some(a)) = imu.read_accel_if_ready() {
            let mag = libm::sqrtf(a.x * a.x + a.y * a.y + a.z * a.z);
            println!(
                "[IMU] Rest sample #{}: ax={:+.3}g, ay={:+.3}g, az={:+.3}g | |a|={:.3}g",
                i, a.x, a.y, a.z, mag
            );
        }
    }

    // === SD Card (SPI3) ===
    println!("[SD] Init...");
    let sd_spi_config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(4))
        .with_mode(SpiMode::_0);
    let sd_spi = Spi::new(peripherals.SPI3, sd_spi_config)
        .expect("SPI3 failed")
        .with_sck(peripherals.GPIO2)
        .with_mosi(peripherals.GPIO1)
        .with_miso(peripherals.GPIO3);
    let sd_cs = Output::new(peripherals.GPIO17, Level::High, OutputConfig::default());

    use embedded_hal_bus::spi::ExclusiveDevice;
    let sd_spi_dev = ExclusiveDevice::new_no_delay(sd_spi, sd_cs).unwrap();
    let mut sd_card = embedded_sdmmc::SdCard::new(sd_spi_dev, delay);
    let mut _sd_ok = false;
    match sd_card.num_bytes() {
        Ok(size) => {
            println!("[SD] Card {}MB", size / 1024 / 1024);

            // Scan /mp3/ directory
            struct DummyTime;
            impl embedded_sdmmc::TimeSource for DummyTime {
                fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
                    embedded_sdmmc::Timestamp::from_calendar(2026, 4, 6, 12, 0, 0).unwrap()
                }
            }

            let mut volume_mgr = embedded_sdmmc::VolumeManager::new(sd_card, DummyTime);
            match volume_mgr.open_raw_volume(embedded_sdmmc::VolumeIdx(0)) {
                Ok(volume) => {
                    if let Ok(root_dir) = volume_mgr.open_root_dir(volume) {
                        if let Ok(mp3_dir) = volume_mgr.open_dir(root_dir, "MP3") {
                            println!("[SD] Found /MP3/ folder");
                            let mut file_count = 0u16;
                            let _ = volume_mgr.iterate_dir(mp3_dir, |entry| {
                                if !entry.attributes.is_directory() {
                                    let name = core::str::from_utf8(&entry.name.base_name())
                                        .unwrap_or("?");
                                    let ext =
                                        core::str::from_utf8(&entry.name.extension()).unwrap_or("");
                                    println!("[SD]   {}.{}", name.trim(), ext.trim());
                                    file_count = file_count.saturating_add(1);
                                }
                            });
                            println!("[SD] {} files found", file_count);
                            let _ = volume_mgr.close_dir(mp3_dir);
                            _sd_ok = true;
                        } else {
                            // Try lowercase
                            if let Ok(mp3_dir) = volume_mgr.open_dir(root_dir, "mp3") {
                                println!("[SD] Found /mp3/ folder");
                                let mut file_count = 0u16;
                                let _ = volume_mgr.iterate_dir(mp3_dir, |entry| {
                                    if !entry.attributes.is_directory() {
                                        let name = core::str::from_utf8(&entry.name.base_name())
                                            .unwrap_or("?");
                                        let ext = core::str::from_utf8(&entry.name.extension())
                                            .unwrap_or("");
                                        println!("[SD]   {}.{}", name.trim(), ext.trim());
                                        file_count = file_count.saturating_add(1);
                                    }
                                });
                                println!("[SD] {} files found", file_count);
                                let _ = volume_mgr.close_dir(mp3_dir);
                                _sd_ok = true;
                            } else {
                                println!("[SD] No /mp3/ or /MP3/ folder");
                            }
                        }
                        let _ = volume_mgr.close_dir(root_dir);
                    } else {
                        println!("[SD] Can't open root dir");
                    }
                }
                Err(e) => {
                    println!("[SD] Can't open volume: {:?}", e);
                }
            }
        }
        Err(_) => println!("[SD] No card"),
    }

    // === Audio (ES8311 codec + I2S) ===
    // CRITICAL ORDER:
    // 1. Keep PA amplifier DISABLED (GPIO46 LOW) - prevents white noise from floating I2S line
    // 2. Init codec (codec init leaves DAC powered but no input yet)
    // 3. Immediately mute the codec DAC + HP output
    // 4. Init I2S bus
    // Only when we actually beep: unmute codec -> raise PA_EN -> write DMA -> lower PA_EN -> mute
    println!("[AUDIO] Init codec...");
    let mut audio_codec = Es8311::new(RefCellDevice::new(&i2c_ref));
    let _pa_en = Output::new(peripherals.GPIO46, Level::Low, OutputConfig::default());
    match audio_codec.init() {
        Ok(()) => println!("[AUDIO] Codec OK"),
        Err(_) => println!("[AUDIO] Codec FAILED"),
    }
    // Full power-down of the analog blocks (not just mute). The PGA, DAC
    // and HP driver are explicitly cut — saves ~20 mA versus mute() which
    // only zeroes the volume register. `unmute()` brings them back on
    // demand at playback time.
    let _ = audio_codec.shutdown();

    // === I2S Audio Output (using public write_dma) ===
    println!("[AUDIO] Init I2S...");
    use esp_hal::dma::DmaDescriptor;
    use esp_hal::i2s::master::{Config as I2sConfig, DataFormat, I2s};
    let i2s_config = I2sConfig::default()
        .with_sample_rate(Rate::from_hz(16000))
        .with_data_format(DataFormat::Data16Channel16);
    let i2s_periph = I2s::new(peripherals.I2S0, peripherals.DMA_CH1, i2s_config)
        .expect("I2S failed")
        .with_mclk(peripherals.GPIO16);
    // StaticCell gives the DMA driver unique, `'static` ownership without
    // manufacturing references to `static mut` storage.
    static I2S_TX_DESC: StaticCell<[DmaDescriptor; 8]> = StaticCell::new();
    let i2s_tx_desc = I2S_TX_DESC.init([DmaDescriptor::EMPTY; 8]);
    let _i2s_tx = i2s_periph
        .i2s_tx
        .with_bclk(peripherals.GPIO41)
        .with_ws(peripherals.GPIO45)
        .with_dout(peripherals.GPIO40)
        .build(i2s_tx_desc);

    // Pre-generate beep sound (800Hz, 50ms, stereo 16-bit @ 16kHz = 3200 bytes)
    static BEEP_BUF: StaticCell<[u8; 4000]> = StaticCell::new();
    let beep_buf = BEEP_BUF.init([0u8; 4000]);
    let beep_len = fill_beep_buffer(beep_buf, 800, 16000, 50);
    println!("[AUDIO] I2S OK ({} bytes beep)", beep_len);

    static CHIME_BUF: StaticCell<[u8; 4000]> = StaticCell::new();
    let chime_buf = CHIME_BUF.init([0u8; 4000]);
    let chime_len = fill_chime_buffer(chime_buf, 16000);
    println!("[AUDIO] Chime OK ({} bytes chime)", chime_len);

    // === WiFi init (esp-radio) — RADIO STAYS OFF AT BOOT ===
    //
    // POWER BUDGET NOTE:
    // Up to v0.4 the watch booted straight into a connected WiFi STA with
    // no power-save configured. The 2.4 GHz radio + PA alone sustains
    // ~90 mA continuous — the single biggest drain by far, roughly half
    // of the total ~1-hour runtime the user complained about.
    //
    // We now keep the esp-radio stack *initialized* (so we can spin it up
    // at any time without re-allocating 300 KB of radio buffers) but we
    // never call `wifi_controller.start()` on boot. The user toggles the
    // radio on/off with the 'W' button in the top-left of the watchface.
    // When toggled on, we also immediately set PowerSave::MaxModem so the
    // radio sleeps between DTIM beacons (~20 mA average instead of 90).
    //
    // DHCP + NTP are deferred to the first wake of the radio, so a watch
    // that never toggles WiFi pays exactly zero network cost.
    println!("[RADIO] Init radio stack (WiFi OFF, BLE OFF)");
    static RADIO: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    // Coerce &'static mut → &'static so both WiFi and BLE can share the controller.
    let radio_controller: &'static esp_radio::Controller<'static> =
        RADIO.init(esp_radio::init().expect("esp-radio init failed"));

    // === WiFi init (OFF by default) ===
    let wifi_config = esp_radio::wifi::Config::default()
        .with_power_save_mode(esp_radio::wifi::PowerSaveMode::Maximum);
    let (mut wifi_controller, wifi_interfaces) =
        esp_radio::wifi::new(radio_controller, peripherals.WIFI, wifi_config)
            .expect("WiFi init failed");

    let mut ble_connector = esp_radio::ble::controller::BleConnector::new(
        radio_controller,
        peripherals.BT,
        esp_radio::ble::Config::default(),
    )
    .expect("BLE init failed");
    println!("[BLE] HCI connector ready");
    println!("[HIL][PASS] ble-hci");
    log_memory("after-radio");

    // === NVS: load stored WiFi credentials ===
    // If no credentials are stored, enter AP provisioning mode.
    use embassy_net::{Config as NetConfig, StackResources};
    use esp_radio::wifi::{AccessPointConfig, AuthMethod, ClientConfig, ModeConfig};
    let nvs_creds: Option<StoredCredentials> = read_credentials();
    let wifi_has_nvs_creds = nvs_creds
        .as_ref()
        .map(|c| c.is_configured())
        .unwrap_or(false);

    // Load persistent watch settings (hourly rate, display prefs, feature flags).
    // Falls back to defaults if flash is blank or CRC fails (first boot).
    let watch_settings = load_settings();
    println!(
        "[SETTINGS] rate={}c/h timeout={}s ble={} wifi={}",
        watch_settings.hourly_rate_cents,
        watch_settings.display_timeout_secs,
        watch_settings.ble_enabled,
        watch_settings.wifi_enabled,
    );
    if watch_settings.ble_enabled {
        if let Err(e) = crate::peripherals::ble::start_advertising(&mut ble_connector) {
            println!("[BLE] advertising start failed: {:?}", e);
        } else {
            println!("[BLE] advertising enabled");
        }
    }

    // Compile-time fallback credentials (for development without NVS provisioning)
    let fallback_ssid = option_env!("WIFI_SSID").unwrap_or("");
    let fallback_pass = option_env!("WIFI_PASS").unwrap_or("");
    let wifi_has_creds = wifi_has_nvs_creds || !fallback_ssid.is_empty();

    // Resolve which credentials to use
    let (wifi_ssid, wifi_pass): (alloc::string::String, alloc::string::String) =
        if let Some(ref creds) = nvs_creds {
            if creds.is_configured() {
                println!("[WIFI] Using NVS credentials for: {}", creds.ssid_str());
                (
                    alloc::string::String::from(creds.ssid_str()),
                    alloc::string::String::from(creds.pass_str()),
                )
            } else {
                (
                    alloc::string::String::from(fallback_ssid),
                    alloc::string::String::from(fallback_pass),
                )
            }
        } else {
            (
                alloc::string::String::from(fallback_ssid),
                alloc::string::String::from(fallback_pass),
            )
        };

    // === WiFi provisioning AP mode (runs BEFORE main watchface loop) ===
    //
    // If no credentials are configured anywhere (neither NVS nor compile-time
    // env vars), the watch becomes a protected AP named "Watch-Setup" and shows a
    // QR code on the display.  The phone connects, opens the captive portal,
    // submits credentials, and the watch saves them to NVS then reboots.
    if !wifi_has_creds {
        println!("[PROVISION] No WiFi creds — starting AP provisioning");

        let ap_password = alloc::format!("sbie-{:08X}", Instant::now().as_millis() as u32);

        // Reconfigure WiFi controller as Access Point
        // Never expose credential provisioning on an open AP. The password is
        // shown by the setup UI/QR flow and protects the plaintext HTTP form
        // from arbitrary nearby clients.
        let ap_config = AccessPointConfig::default()
            .with_ssid(alloc::string::String::from(AP_SSID))
            .with_password(ap_password.clone())
            .with_auth_method(AuthMethod::WpaWpa2Personal);
        let ap_mode = ModeConfig::AccessPoint(ap_config);
        if wifi_controller.set_config(&ap_mode).is_err() || wifi_controller.start().is_err() {
            println!("[WIFI] provisioning AP initialization failed");
            return;
        }
        println!("[HIL][PASS] wifi-radio");

        // AP embassy-net stack with static IP 192.168.4.1/24
        let ap_net_config = NetConfig::ipv4_static(embassy_net::StaticConfigV4 {
            address: embassy_net::Ipv4Cidr::new(embassy_net::Ipv4Address::new(192, 168, 4, 1), 24),
            gateway: Some(embassy_net::Ipv4Address::new(192, 168, 4, 1)),
            dns_servers: Default::default(),
        });
        static AP_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
        let ap_resources = AP_RESOURCES.init(StackResources::new());
        let (ap_stack, ap_runner) =
            embassy_net::new(wifi_interfaces.ap, ap_net_config, ap_resources, 54321u64);
        _spawner.spawn(net_task(ap_runner)).ok();

        // Provisioning UI
        let mut setup_app = WifiSetupApp::new(&ap_password);

        println!("[PROVISION] AP up. Open http://192.168.4.1 to configure WiFi.");
        fb.clear_color(Rgb565::BLACK);
        setup_app.render(&mut fb);
        report_display(fb.flush(&mut display));

        // Run AP services (DHCP + DNS + HTTP) alongside UI rendering & touch detection
        let mut ap_services =
            core::pin::pin!(crate::peripherals::captive::run_ap_services(ap_stack));

        loop {
            // Check for touch tap or boot button to skip provisioning
            if boot_button.is_low() {
                println!("[PROVISION] Button pressed — skipping WiFi setup");
                wifi_controller.stop().ok();
                break;
            }
            // Animate spinner and check AP services
            match embassy_futures::select::select(
                &mut ap_services,
                Timer::after(Duration::from_millis(100)),
            )
            .await
            {
                embassy_futures::select::Either::First(creds) => {
                    println!(
                        "[PROVISION] Credentials saved for: {}! Rebooting...",
                        creds.ssid_str()
                    );
                    setup_app.mark_done();
                    fb.clear_color(Rgb565::BLACK);
                    setup_app.render(&mut fb);
                    report_display(fb.flush(&mut display));
                    Timer::after(Duration::from_secs(2)).await;
                    esp_hal::system::software_reset();
                }
                embassy_futures::select::Either::Second(_) => {
                    setup_app.update(100);
                    if setup_app.phase == SetupPhase::Waiting {
                        fb.clear_color(Rgb565::BLACK);
                        setup_app.render(&mut fb);
                        report_display(fb.flush(&mut display));
                    }
                }
            }
        }
    }

    // === Normal STA mode setup ===
    // Credentials are available (NVS or compile-time). Pre-fill STA config.
    let client_config = ClientConfig::default()
        .with_ssid(wifi_ssid)
        .with_password(wifi_pass.clone())
        .with_auth_method(if wifi_pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WpaWpa2Personal
        });
    let mode_config = ModeConfig::Client(client_config);
    wifi_controller
        .set_config(&mode_config)
        .expect("WiFi config failed");
    // NOTE: we do NOT call wifi_controller.start() here. The radio stays
    // fully idle until the user taps the WiFi button.

    // === Network Stack scaffolding (idle until radio starts) ===
    let net_config = NetConfig::dhcpv4(Default::default());
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let resources = RESOURCES.init(StackResources::new());

    let (stack, runner) = embassy_net::new(wifi_interfaces.sta, net_config, resources, 12345u64);

    // Spawn network runner task. It does nothing useful until the radio
    // has been started, but spawning it here avoids having to hot-spawn
    // a task from inside the main loop.
    _spawner.spawn(net_task(runner)).ok();

    println!("=== All systems GO! (Embassy async, WiFi OFF) ===");

    // === State ===
    let mut watchface = WatchFace::new();
    watchface.wifi_connected = false;
    // Credentials being present does not mean the radio is running; Wi-Fi is
    // explicitly user-activated from the watchface.
    watchface.wifi_on = false;
    let mut current_page = Page::Clock;
    let mut app_state = AppState::Watchface;
    let mut snake_game = SnakeGame::new();
    let mut game_2048 = Game2048::new();
    let mut tetris_game = TetrisGame::new();
    let mut flappy_game = FlappyGame::new();
    let mut maze_game = MazeGame::new();
    let mut launcher = Launcher::new();
    let mut outdoor_app = crate::apps::outdoor_app::OutdoorApp::new();
    let mut work_tracker = WorkTracker::with_rate(watch_settings.hourly_rate_cents);
    let mut authenticator_app = AuthenticatorApp::new();
    log_memory("before-analyzer");
    println!(
        "[BLE] DeviceTracker size={} bytes, TrackedDevice size={} bytes, BleAnalyzerApp size={} bytes",
        core::mem::size_of::<crate::ble_analyzer::tracker::DeviceTracker>(),
        core::mem::size_of::<crate::ble_analyzer::tracker::TrackedDevice>(),
        core::mem::size_of::<crate::ble_analyzer::BleAnalyzerApp>(),
    );
    let mut ble_analyzer_app = alloc::boxed::Box::new(BleAnalyzerApp::new());
    log_memory("after-analyzer");
    let mut outdoor_state = crate::outdoor::OutdoorState::new();
    let mut step_detector = crate::outdoor::StepDetector::default();
    let mut next_outdoor_tick = Instant::now();
    let mut settings_app = SettingsApp::new();
    if let Some(ref creds) = nvs_creds {
        if creds.is_configured() {
            settings_app.set_stored_ssid(creds.ssid_str());
        }
    } else if !fallback_ssid.is_empty() {
        settings_app.set_stored_ssid(fallback_ssid);
    }
    let mut media_controller = MediaController::new();

    let mut accel = (0.0f32, 0.0f32, 0.0f32);
    let mut page_dirty = true;

    let mut last_interaction = Instant::now();
    // screen_state levels:
    //   3 = full bright (interactive)
    //   2 = dim (transition)
    //   1 = AOD (Always-On Display: super-dim, minimal HH:MM, 1 update / minute)
    //   0 = full off (DISPOFF + SLPIN)
    let mut screen_state: u8 = 3;
    // Tracks the last minute we rendered in AOD so we update the screen exactly
    // once per minute, not faster. Saves both DMA bandwidth and AMOLED current.
    let mut aod_last_minute: u8 = 99;

    println!(
        "[MEM:UI] framebuffer={}B qspi_scratch={}B ble_analyzer={}B auth_accounts_cap={}",
        fb.allocated_bytes(),
        display.bus_mut().scratch_capacity_bytes(),
        core::mem::size_of_val(&*ble_analyzer_app),
        authenticator_app.account_capacity(),
    );

    // Initial render
    match power.read_battery() {
        Ok(bat) => {
            watchface.update_battery(bat.percent, bat.voltage_mv, bat.charging);
            outdoor_state.update_battery(
                bat.percent,
                bat.voltage_mv,
                bat.charging,
                bat.vbus_present,
            );
            work_tracker.update_battery(bat.percent, bat.charging);
            authenticator_app.update_battery(bat.percent, bat.charging);
        }
        Err(_) => println!("[PMIC] battery telemetry invalid; retaining last valid sample"),
    }
    if let Ok(dt) = rtc.get_time() {
        watchface.update_time(dt.hours, dt.minutes, dt.seconds);
        watchface.update_date(dt.day, dt.month, dt.year);
        outdoor_state.time_hms = (dt.hours, dt.minutes, dt.seconds);
        outdoor_state.date_ymd = (2000 + dt.year as u16, dt.month, dt.day);
        work_tracker.update_time((dt.hours, dt.minutes, dt.seconds));
        work_tracker.update_date((2000 + dt.year as u16, dt.month, dt.day));
        authenticator_app.update_time(
            (dt.hours, dt.minutes, dt.seconds),
            (2000 + dt.year as u16, dt.month, dt.day),
        );

        if let Some(active) = load_active_session() {
            if contains_session_id(active.session_id) {
                // A power loss after append but before active-record erase is
                // already committed. Reconciliation prevents a duplicate.
                let _ = clear_active_session();
                println!("[WORK] reconciled completed session after interrupted stop");
            } else {
                if let Some(now_unix) = calendar_to_unix(
                    (2000 + dt.year as u16, dt.month, dt.day),
                    (dt.hours, dt.minutes, dt.seconds),
                ) {
                    let elapsed_secs = now_unix.saturating_sub(active.start_unix).max(0) as u32;
                    work_tracker.restore_active_session(
                        crate::apps::work_tracker::WorkSession::new(
                            active.start_date_ymd,
                            active.start_time_hms,
                            active.hourly_rate_cents,
                        ),
                        elapsed_secs,
                    );
                    println!("[WORK] restored active session ({} seconds)", elapsed_secs);
                } else {
                    println!("[WORK] invalid RTC date; active session retained for recovery");
                }
            }
        }
    }
    let worklog_health = session_log_health();
    work_tracker.set_log_health(worklog_health);
    println!(
        "[WORKLOG] v{} valid={} corrupt={} free={} tail={} dup_ids={} dup_records={} scan={}",
        crate::peripherals::session_log::LOG_FORMAT_VERSION,
        worklog_health.valid_records,
        worklog_health.corrupt_records,
        worklog_health.free_records,
        if worklog_health.tail_state == crate::peripherals::session_log::TailState::Ok {
            "ok"
        } else {
            "bad"
        },
        worklog_health.duplicate_session_ids,
        worklog_health.duplicate_records,
        if worklog_health.duplicate_scan_truncated {
            "partial"
        } else {
            "full"
        },
    );
    if worklog_health.severity() == LogSeverity::Warning {
        println!("[WORKLOG] WARNING: duplicate session IDs detected");
    } else if worklog_health.severity() == LogSeverity::Error {
        println!("[WORKLOG] ERROR: invalid or corrupt log record detected");
    }
    watchface.force_redraw();
    let _ = watchface.render(&mut fb);
    report_display(fb.flush(&mut display));

    // === Event-driven async main loop ===
    //
    // The loop sleeps the CPU between iterations using `select` over:
    //   * a periodic timer whose period depends on the current app/screen state
    //   * the touch interrupt line (GPIO38, async falling edge)
    //   * the boot button line (GPIO0, async falling edge)
    //
    // Tick budgets per state (only consumed when nothing else wakes us):
    //   * screen off                : 30 s   (deep idle, just to refresh battery%)
    //   * watchface clock, gyro off : 1 s    (only the seconds digit changes)
    //   * watchface clock, gyro on  : 33 ms  (smooth gyro ball)
    //   * sensors page              : 100 ms (10 Hz IMU)
    //   * power page                : 1 s    (diagnostic — must not self-skew)
    //   * launcher / settings / mp3 : 100 ms
    //   * Snake / 2048 / Tetris / Maze / Flappy : 33 ms (~30 Hz, panel cap)
    //
    // A finger held on the screen forces 16 ms tick regardless of state.
    //
    // When the user touches the screen or presses BOOT, the select returns immediately
    // and we process the input. With this design, the CPU is parked >99% of the time
    // while sitting on the watchface.
    use embassy_futures::select::select3;

    let mut next_rtc = Instant::now();
    let mut next_battery = Instant::now();
    let mut last_frame = Instant::now();
    let mut next_watchface_flush = Instant::now();
    // Radio state: we track both what the user *wants* and what the radio
    // actually is. They drift apart briefly during connect/disconnect.
    let mut wifi_started: bool = false; // controller.start() called
    let mut wifi_connected: bool = false; // connect_async succeeded
    let mut ntp_synced: bool = false;
    let mut last_wifi_toggle = Instant::now();
    let mut wifi_retry_at: Option<Instant> = None;
    let mut wifi_attempt: u8 = 0;
    // Request pending from a UI tap on the WiFi button.
    let mut wifi_toggle_request: bool = false;
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ImuPowerMode {
        Off,
        AccelOnly,
        Full6Axis,
    }

    // Power-down the IMU at boot — only enable when a consumer needs it.
    let _ = imu.power_down();
    let mut imu_mode = ImuPowerMode::Off;
    loop {
        // Pick a tick budget based on current state. This is the MAX time we'll
        // sleep without any external wake source.
        let touch_held = touch_int.is_low();
        let button_held = boot_button.is_low();

        let hike_running = outdoor_state.hike.status == crate::outdoor::HikeStatus::Running;
        let hike_paused = outdoor_state.hike.status == crate::outdoor::HikeStatus::Paused;
        let power_mode = crate::outdoor::power_policy::determine_power_mode(
            screen_state,
            hike_running,
            hike_paused,
        );
        let _policy = crate::outdoor::PowerPolicy::for_mode(power_mode);

        let tick = if touch_held || button_held {
            // Something is currently being held: wake fast enough to track motion / detect
            // long-press, but no faster than necessary.
            Duration::from_millis(16) // ~60 Hz
        } else if screen_state == 0 {
            // Screen completely off: if hike is running, sample accel at 20 Hz (50 ms)
            // for step detection. Otherwise only wake every 30 s for housekeeping.
            if hike_running {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(30)
            }
        } else if screen_state == 1 {
            // AOD mode: if hike is running, sample accel at 20 Hz (50 ms).
            // Otherwise wake every 10 s to check if minute changed.
            if hike_running {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(10)
            }
        } else {
            match app_state {
                AppState::Watchface => match current_page {
                    // Clock page: 1 Hz when gyro is off (only seconds change),
                    // 33 ms when gyro is on (smooth ball animation).
                    Page::Clock => {
                        if watchface.gyro_enabled {
                            Duration::from_millis(33)
                        } else {
                            Duration::from_secs(1)
                        }
                    }
                    Page::Sensors => Duration::from_millis(100), // 10 Hz IMU display
                    Page::System => Duration::from_secs(2),      // basically static
                    // Power page refreshes at 1 Hz — fast enough to see
                    // changes, slow enough not to skew the measurement.
                    Page::Power => Duration::from_secs(1),
                },
                AppState::Launcher
                | AppState::Settings
                | AppState::Mp3Player
                | AppState::WorkTracker
                | AppState::Authenticator
                | AppState::BleAnalyzer => Duration::from_millis(100),
                AppState::Outdoor => {
                    if outdoor_app.screen == crate::apps::outdoor_app::OutdoorScreen::Level {
                        Duration::from_millis(50)
                    } else {
                        Duration::from_millis(100)
                    }
                }
                // Games capped at 30 Hz (33 ms)
                AppState::Flappy
                | AppState::Snake
                | AppState::Game2048
                | AppState::Tetris
                | AppState::Maze => Duration::from_millis(33),
            }
        };

        // Sleep until the tick budget elapses OR a falling edge arrives on touch / boot button.
        // Notes:
        //   * If a pin is already low, wait_for_falling_edge will not fire (no edge to wait for),
        //     but the tick above is short (16 ms) so we still wake reactively.
        //   * The futures from esp-hal install GPIO interrupts on creation and remove them on
        //     drop, so the executor parks the CPU between events: this is the main power win.
        let _ = select3(
            Timer::after(tick),
            touch_int.wait_for_falling_edge(),
            boot_button.wait_for_falling_edge(),
        )
        .await;

        let now = Instant::now();
        let dt_ms = (now - last_frame).as_millis() as u32;
        last_frame = now;

        // === Sensors (power-optimized by consumer needs) ===
        // Full 6-axis (accel + gyro, ~2.5 mA) only when a dynamic 3D angle or gyro consumer is active.
        let need_gyro = screen_state >= 2
            && (watchface.gyro_enabled
                || app_state == AppState::Maze
                || app_state == AppState::Tetris
                || app_state == AppState::Flappy
                || (app_state == AppState::Outdoor
                    && outdoor_app.screen == crate::apps::outdoor_app::OutdoorScreen::Level)
                || (app_state == AppState::Watchface && current_page == Page::Sensors));

        // Accel only (~150 µA) is sufficient for step detection, hike tracking, and watchface tilt.
        let need_accel = need_gyro
            || hike_running
            || (screen_state >= 2
                && (app_state == AppState::Outdoor || app_state == AppState::Watchface));

        let target_imu_mode = if need_gyro {
            ImuPowerMode::Full6Axis
        } else if need_accel {
            ImuPowerMode::AccelOnly
        } else {
            ImuPowerMode::Off
        };

        if target_imu_mode != imu_mode {
            match target_imu_mode {
                ImuPowerMode::Full6Axis => {
                    let _ = imu.enable_6axis();
                }
                ImuPowerMode::AccelOnly => {
                    let _ = imu.enable_accel_only();
                }
                ImuPowerMode::Off => {
                    let _ = imu.power_down();
                }
            }
            imu_mode = target_imu_mode;
        }

        if need_accel {
            outdoor_state.level_valid = false;
            match imu.read_accel_if_ready() {
                Ok(Some(a)) => {
                    accel = (a.x, a.y, a.z);
                    if screen_state >= 2 {
                        watchface.update_accel(a.x, a.y, a.z);
                    }
                    step_detector.update(
                        crate::outdoor::AccelSample {
                            x_g: a.x,
                            y_g: a.y,
                            z_g: a.z,
                        },
                        now.as_millis(),
                    );
                    let att = crate::outdoor::estimate_attitude(a.x, a.y, a.z);
                    outdoor_state.steps = step_detector.steps();
                    outdoor_state.roll_deg = att.roll_deg;
                    outdoor_state.pitch_deg = att.pitch_deg;
                    outdoor_state.level_valid = att.valid;
                    outdoor_state.imu_motion = step_detector.motion_state();
                }
                Ok(None) => {}
                Err(_) => println!("[IMU] accelerometer read failed"),
            }
        }

        if need_gyro {
            match imu.read_gyro_if_ready() {
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(_) => println!("[IMU] gyroscope read failed"),
            }
            let _ = imu.read_temperature();
        }

        // Outdoor & Work Tracker periodic 1 Hz tick
        if now >= next_outdoor_tick {
            outdoor_state.tick_1s();
            work_tracker.tick_1s();
            next_outdoor_tick = now + Duration::from_secs(1);
        }

        // RTC: keep the model current in AOD as well, so the next minute is
        // rendered correctly after a long display-off interval.
        if screen_state >= 1 && now >= next_rtc {
            if let Ok(dt) = rtc.get_time() {
                watchface.update_time(dt.hours, dt.minutes, dt.seconds);
                watchface.update_date(dt.day, dt.month, dt.year);
                outdoor_state.time_hms = (dt.hours, dt.minutes, dt.seconds);
                outdoor_state.date_ymd = (2000 + dt.year as u16, dt.month, dt.day);
                work_tracker.update_time((dt.hours, dt.minutes, dt.seconds));
                work_tracker.update_date((2000 + dt.year as u16, dt.month, dt.day));
                authenticator_app.update_time(
                    (dt.hours, dt.minutes, dt.seconds),
                    (2000 + dt.year as u16, dt.month, dt.day),
                );
            }
            next_rtc = now + Duration::from_secs(1);
        }

        // WiFi toggle logic
        let retry_ready = wifi_retry_at.map_or(true, |deadline| now >= deadline);
        if wifi_toggle_request
            && retry_ready
            && now.duration_since(last_wifi_toggle) > Duration::from_secs(2)
        {
            last_wifi_toggle = now;
            wifi_toggle_request = false;

            if !wifi_started {
                // Start WiFi radio
                println!("[WIFI] Starting radio...");
                match wifi_controller.start() {
                    Ok(()) => {
                        wifi_started = true;
                    }
                    Err(e) => {
                        println!("[WIFI] Start failed: {:?}", e);
                        wifi_started = false;
                        continue;
                    }
                }

                // Give it a moment, then attempt connect
                Timer::after(Duration::from_secs(2)).await;

                // Try connecting
                match embassy_time::with_timeout(
                    Duration::from_secs(15),
                    wifi_controller.connect_async(),
                )
                .await
                {
                    Ok(Ok(())) => {
                        println!("[WIFI] Connected!");
                        wifi_connected = true;
                        ntp_synced = false; // will sync in next cycle
                        wifi_attempt = 0;
                        wifi_retry_at = None;
                    }
                    Ok(Err(e)) => {
                        println!("[WIFI] Connect failed: {:?}", e);
                        wifi_connected = false;
                        wifi_controller.stop().ok();
                        wifi_started = false;
                        wifi_attempt = wifi_attempt.saturating_add(1);
                        let backoff = (1u64 << wifi_attempt.min(5)).min(30);
                        wifi_retry_at = Some(now + Duration::from_secs(backoff));
                        wifi_toggle_request = true;
                    }
                    Err(_) => {
                        println!("[WIFI] Connect timeout");
                        wifi_connected = false;
                        wifi_controller.stop().ok();
                        wifi_started = false;
                        wifi_attempt = wifi_attempt.saturating_add(1);
                        let backoff = (1u64 << wifi_attempt.min(5)).min(30);
                        wifi_retry_at = Some(now + Duration::from_secs(backoff));
                        wifi_toggle_request = true;
                    }
                }
            } else {
                // Stop WiFi radio
                println!("[WIFI] Stopping radio...");
                wifi_controller.stop().ok();
                wifi_started = false;
                wifi_connected = false;
                wifi_retry_at = None;
                wifi_attempt = 0;
            }
        }

        // NTP sync after WiFi connects
        if wifi_connected && !ntp_synced {
            match ntp_sync(stack, &mut rtc).await {
                Ok(()) => {
                    ntp_synced = true;
                    println!("[NTP] synchronized");
                }
                Err(()) => {
                    println!("[NTP] synchronization failed; will retry");
                }
            }
        }

        // Battery check every 30s
        if now >= next_battery {
            match power.read_battery() {
                Ok(bat) => {
                    watchface.update_battery(bat.percent, bat.voltage_mv, bat.charging);
                    outdoor_state.update_battery(
                        bat.percent,
                        bat.voltage_mv,
                        bat.charging,
                        bat.vbus_present,
                    );
                    work_tracker.update_battery(bat.percent, bat.charging);
                    authenticator_app.update_battery(bat.percent, bat.charging);
                }
                Err(_) => println!("[PMIC] battery telemetry invalid; retaining last valid sample"),
            }
            next_battery = now + Duration::from_secs(30);
        }

        // === Touch / button input ===
        // The touch driver reports live points while pressed and a classified
        // SwipeEvent once the finger is lifted.
        let mut frame_touch = None;
        let mut frame_swipe = None;
        if let Ok((point, swipe_event)) = touch.poll() {
            if point.is_some() {
                frame_touch = point;
                last_interaction = now;
                // Wake screen if off/AOD.
                if screen_state < 2 {
                    screen_state = 3;
                    page_dirty = true;
                    report_display(display.display_on());
                    report_display(display.set_brightness(watchface.brightness));
                }
            }

            if let Some(event) = swipe_event {
                frame_swipe = Some(event.direction);
                let event_touch = TouchPoint {
                    x: event.end_x,
                    y: event.end_y,
                    fingers: 0,
                };
                frame_touch = Some(event_touch);
                last_interaction = now;
                if screen_state < 2 {
                    screen_state = 3;
                    report_display(display.display_on());
                    report_display(display.set_brightness(watchface.brightness));
                    page_dirty = true;
                }

                if app_state == AppState::Watchface && event.direction == SwipeDirection::Tap {
                    if let Some(tp) = frame_touch {
                        if WatchFace::is_apps_zone(tp.x, tp.y) {
                            app_state = AppState::Launcher;
                            launcher = Launcher::new();
                            page_dirty = true;
                        } else if WatchFace::is_wifi_zone(tp.x, tp.y) {
                            wifi_toggle_request = true;
                        } else if WatchFace::is_gyro_zone(tp.x, tp.y) {
                            watchface.toggle_gyro();
                            page_dirty = true;
                        } else if let Some(brightness) = WatchFace::brightness_from_tap(tp.x, tp.y)
                        {
                            watchface.brightness = brightness;
                            report_display(display.set_brightness(brightness));
                            watchface.force_redraw();
                            page_dirty = true;
                        } else if WatchFace::is_cpu_zone(tp.x, tp.y) {
                            watchface.cycle_cpu();
                            page_dirty = true;
                        }
                    }
                }

                match event.direction {
                    SwipeDirection::Right => {
                        // Swipe right → previous page.
                        current_page = match current_page {
                            Page::Clock => Page::Power,
                            Page::Sensors => Page::Clock,
                            Page::System => Page::Sensors,
                            Page::Power => Page::System,
                        };
                        page_dirty = true;
                    }
                    SwipeDirection::Left => {
                        // Swipe left → next page.
                        current_page = match current_page {
                            Page::Clock => Page::Sensors,
                            Page::Sensors => Page::System,
                            Page::System => Page::Power,
                            Page::Power => Page::Clock,
                        };
                        page_dirty = true;
                    }
                    SwipeDirection::Up | SwipeDirection::Down | SwipeDirection::Tap => {}
                }
            }
        }

        // Dispatch the normalized input to the active application. The
        // release point is retained so taps can hit-test app controls.
        let app_touch = frame_touch;
        let app_swipe = frame_swipe;
        let app_input = AppInput {
            touch: app_touch,
            swipe: app_swipe,
            tap: app_swipe == Some(SwipeDirection::Tap),
            accel,
            dt_ms,
        };
        let app_result = match app_state {
            AppState::Launcher => {
                if let Some(tp) = app_touch {
                    launcher.update(app_input.swipe, app_input.tap, tp.y)
                } else {
                    launcher.update(app_input.swipe, false, 0)
                }
                .map(|state| {
                    match state {
                        AppState::Outdoor => outdoor_app.setup(),
                        AppState::Settings => {
                            settings_app.wifi_state =
                                crate::peripherals::wifi::WifiState::Disconnected
                        }
                        AppState::WorkTracker => work_tracker.setup(),
                        AppState::Authenticator => authenticator_app.setup(),
                        AppState::BleAnalyzer => ble_analyzer_app.setup(),
                        AppState::Mp3Player => media_controller.setup(),
                        AppState::Snake => snake_game.setup(),
                        AppState::Game2048 => game_2048.setup(),
                        AppState::Tetris => tetris_game.setup(),
                        AppState::Flappy => flappy_game.setup(),
                        AppState::Maze => maze_game.setup(),
                        _ => {}
                    }
                    app_state = state;
                    page_dirty = true;
                });
                None
            }
            AppState::Settings => {
                settings_app.update(dt_ms);
                if app_input.swipe == Some(SwipeDirection::Right) {
                    Some(AppResult::Exit)
                } else if app_input.tap {
                    if let Some(tp) = app_touch {
                        settings_app.handle_tap(tp.x, tp.y);
                    }
                    None
                } else {
                    None
                }
            }
            AppState::Outdoor => {
                if app_input.tap {
                    if let Some(tp) = app_touch {
                        outdoor_app.handle_tap(tp.x, tp.y, &mut outdoor_state);
                    }
                }
                if app_input.swipe == Some(SwipeDirection::Right) {
                    Some(AppResult::Exit)
                } else {
                    None
                }
            }
            AppState::WorkTracker => Some(work_tracker.update(&app_input)),
            AppState::Authenticator => Some(authenticator_app.update(&app_input)),
            AppState::BleAnalyzer => Some(ble_analyzer_app.update(&app_input)),
            AppState::Mp3Player => Some(media_controller.update(&app_input)),
            AppState::Snake => Some(snake_game.update(&app_input)),
            AppState::Game2048 => Some(game_2048.update(&app_input)),
            AppState::Tetris => Some(tetris_game.update(&app_input)),
            AppState::Flappy => Some(flappy_game.update(&app_input)),
            AppState::Maze => Some(maze_game.update(&app_input)),
            AppState::Watchface => None,
        };

        // Settings owns the editing UX; the main loop owns radio lifecycle and
        // persistence. Apply a validated request atomically before retrying.
        if let Some(config) = settings_app.take_connect_request() {
            let mut creds = StoredCredentials::empty();
            creds.ssid_len = config.ssid_len;
            creds.ssid[..config.ssid_len].copy_from_slice(&config.ssid[..config.ssid_len]);
            creds.pass_len = config.pass_len;
            creds.password[..config.pass_len].copy_from_slice(&config.password[..config.pass_len]);
            match write_credentials(&creds) {
                Ok(()) => {
                    settings_app.set_stored_ssid(config.ssid_str());
                    if wifi_started {
                        let _ = wifi_controller.stop();
                        wifi_started = false;
                    }
                    let client = ClientConfig::default()
                        .with_ssid(alloc::string::String::from(config.ssid_str()))
                        .with_password(alloc::string::String::from(config.pass_str()))
                        .with_auth_method(if config.pass_len == 0 {
                            AuthMethod::None
                        } else {
                            AuthMethod::WpaWpa2Personal
                        });
                    if wifi_controller
                        .set_config(&ModeConfig::Client(client))
                        .is_ok()
                    {
                        wifi_connected = false;
                        wifi_attempt = 0;
                        wifi_retry_at = None;
                        wifi_toggle_request = true;
                    } else {
                        settings_app.wifi_state = crate::peripherals::wifi::WifiState::Error;
                    }
                }
                Err(error) => {
                    println!("[WIFI] credential persistence failed: {:?}", error);
                    settings_app.wifi_state = crate::peripherals::wifi::WifiState::Error;
                }
            }
        }
        if app_result == Some(AppResult::Exit) {
            app_state = AppState::Launcher;
            launcher = Launcher::new();
            page_dirty = true;
        }

        // Idle display policy. Keep AOD only on the clock page; other apps
        // go directly dark so an unattended app cannot burn the panel.
        let idle = now.duration_since(last_interaction);
        let aod_allowed = app_state == AppState::Watchface && current_page == Page::Clock;
        let target_state = if idle >= Duration::from_secs(600)
            || (!aod_allowed && idle >= Duration::from_secs(40))
        {
            0
        } else if aod_allowed && idle >= Duration::from_secs(40) {
            1
        } else if idle >= Duration::from_secs(20) {
            2
        } else {
            3
        };
        if target_state != screen_state {
            let previous = screen_state;
            screen_state = target_state;
            match (previous, screen_state) {
                (3, 2) => {
                    report_display(display.set_brightness(watchface.brightness.min(0x40)));
                }
                (_, 1) => {
                    fb.clear_color(Rgb565::BLACK);
                    let _ = watchface.render_aod(&mut fb);
                    report_display(display.set_brightness(0x18));
                    report_display(fb.flush(&mut display));
                    aod_last_minute = watchface.current_minute();
                }
                (_, 0) => {
                    report_display(display.display_off());
                }
                (_, 3) if previous == 0 => {
                    report_display(display.display_on());
                    report_display(display.set_brightness(watchface.brightness));
                    watchface.force_redraw();
                    page_dirty = true;
                }
                (_, 3) => {
                    report_display(display.set_brightness(watchface.brightness));
                    page_dirty = true;
                }
                _ => {}
            }
        }

        // === Render when dirty ===
        if screen_state == 1 {
            let minute = watchface.current_minute();
            if minute != aod_last_minute {
                fb.clear_color(Rgb565::BLACK);
                let _ = watchface.render_aod(&mut fb);
                report_display(fb.flush(&mut display));
                aod_last_minute = minute;
            }
        } else if screen_state >= 2 && (page_dirty || now >= next_watchface_flush) {
            match app_state {
                AppState::Watchface => {
                    let _ = watchface.render(&mut fb);
                }
                AppState::Launcher => launcher.render(&mut fb),
                AppState::Settings => settings_app.render(&mut fb),
                AppState::Outdoor => outdoor_app.render(&mut fb, &outdoor_state),
                AppState::WorkTracker => work_tracker.render(&mut fb),
                AppState::Authenticator => authenticator_app.render(&mut fb),
                AppState::BleAnalyzer => ble_analyzer_app.render(&mut fb),
                AppState::Mp3Player => media_controller.render(&mut fb),
                AppState::Snake => snake_game.render(&mut fb),
                AppState::Game2048 => game_2048.render(&mut fb),
                AppState::Tetris => tetris_game.render(&mut fb),
                AppState::Flappy => flappy_game.render(&mut fb),
                AppState::Maze => maze_game.render(&mut fb),
            }
            report_display(fb.flush(&mut display));
            page_dirty = false;
            next_watchface_flush = now + Duration::from_millis(100);
        }
    }
}
