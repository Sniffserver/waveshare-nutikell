#![no_std]
#![no_main]

extern crate alloc;

fn log_memory(label: &'static str) {
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

use embedded_hal_bus::i2c::RefCellDevice;
use esp_alloc as _;
use esp_backtrace as _;

#[unsafe(export_name = "esp_app_desc")]
#[unsafe(link_section = ".flash.appdesc")]
#[used]
pub static ESP_APP_DESC: esp_bootloader_esp_idf::EspAppDesc =
    esp_bootloader_esp_idf::EspAppDesc::new_internal(
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_NAME"),
        esp_bootloader_esp_idf::BUILD_TIME,
        esp_bootloader_esp_idf::BUILD_DATE,
        esp_bootloader_esp_idf::ESP_IDF_COMPATIBLE_VERSION,
        0,
        u16::MAX,
        esp_bootloader_esp_idf::MMU_PAGE_SIZE,
    );

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
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
use crate::apps::media_controller::MediaController;
use crate::apps::settings::SettingsApp;
use crate::apps::snake::SnakeGame;
use crate::apps::tetris::TetrisGame;
use crate::apps::wifi_setup::{SetupPhase, WifiSetupApp, AP_SSID};
use crate::apps::work_tracker::{TrackerState, WorkSession, WorkTracker};
use crate::apps::work_tracker_storage::{
    checkpoint_work_session, finish_work_session, start_work_session, WorkSessionStoreError,
};
use crate::apps::{App, AppInput, AppResult, AppState};
use crate::ble_analyzer::BleAnalyzerApp;
use crate::drivers::co5300::Co5300Display;
use crate::drivers::co5300::DisplayError;
use crate::drivers::framebuffer::Framebuffer;
use crate::drivers::qspi_bus::QspiBus;
use crate::peripherals::active_work::{calendar_to_unix, clear_active_session, load_active_session};
use crate::peripherals::audio::{fill_beep_buffer, fill_chime_buffer, Es8311};
use crate::peripherals::imu::Qmi8658Imu;
use crate::peripherals::nvs::{read_credentials, write_credentials, StoredCredentials};
use crate::peripherals::power::Axp2101Power;
use crate::peripherals::rtc::Pcf85063aRtc;
use crate::peripherals::session_log::{
    contains_session_id, session_log_health, summarize_day, LogSeverity, SessionLogError,
};
fn refresh_work_tracker_daily_summary(tracker: &mut WorkTracker) {
    match summarize_day(tracker.date_ymd) {
        Ok(summary) => tracker.set_daily_summary(summary),
        Err(error) => {
            tracker.set_storage_error(true);
            println!("[WORKLOG] daily summary read failed: {:?}", error);
        }
    }
}

use crate::peripherals::settings::load_settings;
use crate::peripherals::touch::{Ft3168Touch, SwipeDirection, TouchPoint};
use crate::ui::launcher::Launcher;
use crate::ui::pages::Page;
use crate::ui::watchface::WatchFace;

// Network runner task (must be spawned for WiFi to work)
#[embassy_executor::task(pool_size = 1)]
async fn net_task(
    mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>,
) -> ! {
    runner.run().await
}

// ── WiFi connection task ──────────────────────────────────────────────────────
//
// Runs the full start → connect sequence off the main loop so touch, rendering
// and sensor reads are never blocked.  Results are signalled back via two
// static Signals:
//
//   WIFI_RESULT  — posted once per connection attempt:
//       Ok(())    = connected
//       Err(true) = failed but should retry (attempt < MAX_WIFI_ATTEMPTS)
//       Err(false)= failed, max retries reached, user must intervene
//
//   WIFI_DISCONNECT_REQ — set by the main loop to request a graceful stop.
//
// The task exits after a successful connect OR after exhausting retries OR
// after receiving a disconnect request.
//
// DESIGN: The WifiController is placed in WIFI_CTRL_MUTEX at startup so both
// the task and the settings-reconnect path in the main loop can reach it.
// The mutex is held only during synchronous calls (start/stop/set_config);
// it is NOT held across `.await` points so the main loop can reconfigure the
// controller between task runs.

/// Maximum automatic retries before giving up and requiring the user to
/// manually toggle WiFi again.
const MAX_WIFI_ATTEMPTS: u8 = 5;

static WIFI_RESULT: Signal<CriticalSectionRawMutex, Result<(), bool>> = Signal::new();
static WIFI_DISCONNECT_REQ: Signal<CriticalSectionRawMutex, ()> = Signal::new();

// The WifiController is stored here so both wifi_connect_task and the
// settings-reconnect path in the main loop can reach it without unsafe code.
use embassy_sync::mutex::Mutex as EmbassyMutex;
static WIFI_CTRL_MUTEX: EmbassyMutex<
    CriticalSectionRawMutex,
    Option<esp_radio::wifi::WifiController<'static>>,
> = EmbassyMutex::new(None);

#[embassy_executor::task(pool_size = 1)]
async fn wifi_connect_task() {
    use embassy_futures::select::{select, Either};

    for attempt in 1u8..=MAX_WIFI_ATTEMPTS {
        // Honour a disconnect request that arrived while we were sleeping
        // between retry backoffs.
        if WIFI_DISCONNECT_REQ.signaled() {
            // Consume the signal so the next run starts clean.
            let _ = WIFI_DISCONNECT_REQ.try_take();
            println!("[WIFI-TASK] disconnect requested before attempt {}", attempt);
            {
                let mut g = WIFI_CTRL_MUTEX.lock().await;
                if let Some(c) = g.as_mut() {
                    c.disconnect_async().await.ok();
                }
            }
            WIFI_RESULT.signal(Err(false));
            return;
        }

        println!("[WIFI-TASK] attempt {}/{}", attempt, MAX_WIFI_ATTEMPTS);

        // ── connect_async() — WiFi is started automatically when
        //    WifiController was created via esp_radio::wifi::new().
        //    No explicit start() call is needed in esp-radio 0.18.0.
        //
        //    We hold the MutexGuard across connect_async() because the
        //    future borrows &mut WifiController.  The only other locker
        //    is the settings reconnect path, which runs only after the
        //    task has exited (after WIFI_DISCONNECT_REQ + wifi_task_running
        //    cleared), so there is no real contention.
        let connect_result = {
            let mut g = WIFI_CTRL_MUTEX.lock().await;
            if let Some(c) = g.as_mut() {
                let connect_fut = embassy_time::with_timeout(
                    Duration::from_secs(15),
                    c.connect_async(),
                );
                // Interruptible by a disconnect request.
                Some(select(connect_fut, WIFI_DISCONNECT_REQ.wait()).await)
            } else {
                None
            }
        };

        match connect_result {
            None => {
                println!("[WIFI-TASK] controller missing");
            }
            Some(Either::Second(_)) => {
                println!("[WIFI-TASK] disconnect requested during connect");
                {
                    let mut g = WIFI_CTRL_MUTEX.lock().await;
                    if let Some(c) = g.as_mut() {
                        c.disconnect_async().await.ok();
                    }
                }
                WIFI_RESULT.signal(Err(false));
                return;
            }
            Some(Either::First(Ok(Ok(_info)))) => {
                println!("[WIFI-TASK] connected on attempt {}", attempt);
                WIFI_RESULT.signal(Ok(()));
                return;
            }
            Some(Either::First(Ok(Err(e)))) => {
                println!("[WIFI-TASK] connect failed: {:?}", e);
            }
            Some(Either::First(Err(_))) => {
                println!("[WIFI-TASK] connect timeout");
            }
        }

        if attempt == MAX_WIFI_ATTEMPTS {
            println!("[WIFI-TASK] max retries reached — giving up");
            WIFI_RESULT.signal(Err(false)); // no more auto-retry
            return;
        }

        // Exponential backoff (1 s, 2 s, 4 s, 8 s, 16 s), interruptible.
        let backoff_secs = (1u64 << attempt.min(4)).min(16);
        println!("[WIFI-TASK] retry in {} s", backoff_secs);
        match select(
            Timer::after(Duration::from_secs(backoff_secs)),
            WIFI_DISCONNECT_REQ.wait(),
        )
        .await
        {
            Either::Second(_) => {
                println!("[WIFI-TASK] disconnect requested during backoff");
                WIFI_RESULT.signal(Err(false));
                return;
            }
            Either::First(_) => {}
        }

        // Signal "retrying" so the UI can keep showing "connecting…"
        WIFI_RESULT.signal(Err(true));
    }
}

// ── NTP sync task ─────────────────────────────────────────────────────────────
//
// Runs the full NTP exchange off the main loop so the 5-second UDP timeout
// never blocks touch, rendering, or sensor reads.
// Posts the parsed DateTime via NTP_RESULT when done.

static NTP_RESULT: Signal<CriticalSectionRawMutex, Option<crate::peripherals::rtc::DateTime>> =
    Signal::new();

#[embassy_executor::task(pool_size = 1)]
async fn ntp_task(stack: embassy_net::Stack<'static>) {
    match ntp_sync_inner(stack).await {
        Ok(dt) => {
            println!(
                "[NTP-TASK] synced {:02}:{:02}:{:02} {:02}/{:02}/{}",
                dt.hours, dt.minutes, dt.seconds, dt.day, dt.month, dt.year
            );
            NTP_RESULT.signal(Some(dt));
        }
        Err(()) => {
            println!("[NTP-TASK] sync failed");
            NTP_RESULT.signal(None);
        }
    }
}

fn report_display(result: Result<(), DisplayError>) {
    if let Err(error) = result {
        println!("[DISPLAY] runtime operation failed: {:?}", error);
    }
}

// NTP sync — returns a parsed DateTime on success, Err(()) on failure.
// Called by ntp_task; runs fully off the main loop.
async fn ntp_sync_inner(stack: embassy_net::Stack<'static>) -> Result<crate::peripherals::rtc::DateTime, ()> {
    use embassy_net::udp::{PacketMetadata, UdpSocket};

    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 256];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    // Port 0 → kernel assigns a free ephemeral port automatically.
    socket.bind(0).map_err(|_| ())?;

    // NTP request packet (simplified: 48 bytes, first byte = 0x1B for client mode)
    let mut ntp_request = [0u8; 48];
    ntp_request[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)

    // Try resolving pool.ntp.org via DNS first, falling back to static Google NTP IP.
    let fallback_ntp = embassy_net::Ipv4Address::new(216, 239, 35, 0);
    let ntp_addr = match embassy_time::with_timeout(
        Duration::from_secs(3),
        stack.dns_query("pool.ntp.org", embassy_net::dns::DnsQueryType::A),
    )
    .await
    {
        Ok(Ok(addrs)) => {
            if let Some(embassy_net::IpAddress::Ipv4(ipv4)) = addrs.into_iter().next() {
                println!("[NTP] resolved pool.ntp.org -> {:?}", ipv4);
                ipv4
            } else {
                fallback_ntp
            }
        }
        _ => {
            println!("[NTP] DNS query timed out or failed, using static fallback {:?}", fallback_ntp);
            fallback_ntp
        }
    };
    println!("[NTP] sending request to {:?}", ntp_addr);

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
            // Build the validated DateTime and return it to the caller.
            // The main loop is responsible for writing it to the RTC so this
            // task does not need to hold a mutable reference to the peripheral.
            Ok(crate::peripherals::rtc::DateTime::new(
                (year - 2000) as u8,
                month as u8,
                day as u8,
                hours,
                minutes,
                seconds,
            ))
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
    if days_since_epoch < 0 {
        return (2000, 1, 1);
    }
    // Simplified date calculation from days since 1970-01-01
    let mut y = 1970i32;
    let mut remaining = days_since_epoch;
    loop {
        if y > 2255 {
            return (2000, 1, 1);
        }
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
    let software_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

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
    let mut te_pin = Input::new(peripherals.GPIO13, InputConfig::default());
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
    // Power down immediately after sampling to save ~2.5 mA while Wi-Fi and BLE initialize.
    let _ = imu.power_down();

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
    let sd_card = embedded_sdmmc::SdCard::new(sd_spi_dev, delay);
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
    // Store the I2S TX handle in a StaticCell so it lives for the duration of
    // the program and can be passed into an audio task when MP3 playback is
    // wired up.  Using `_i2s_tx` (underscore prefix) would let the value drop
    // at end-of-scope and silently disable DMA — do NOT rename it back.
    static I2S_TX_HANDLE: StaticCell<esp_hal::i2s::master::I2sTx<'static, esp_hal::Blocking>> =
        StaticCell::new();
    let i2s_tx = I2S_TX_HANDLE.init(
        i2s_periph
            .i2s_tx
            .with_bclk(peripherals.GPIO41)
            .with_ws(peripherals.GPIO45)
            .with_dout(peripherals.GPIO40)
            .build(i2s_tx_desc),
    );
    // `i2s_tx` is a `&'static mut I2sTx` obtained from the StaticCell.
    // Pass it to the audio task when needed.  It is intentionally unused
    // until MP3 decoding is wired; the `#[allow]` prevents a spurious warning.
    #[allow(unused_variables)]
    let i2s_tx = i2s_tx;

    // Pre-generate beep sound (800Hz, 50ms, stereo 16-bit @ 16kHz = 3200 bytes)
    static BEEP_BUF: StaticCell<[u8; 4000]> = StaticCell::new();
    let beep_buf = BEEP_BUF.init([0u8; 4000]);
    let beep_len = fill_beep_buffer(beep_buf, 800, 16000, 50);
    println!("[AUDIO] I2S OK ({} bytes beep)", beep_len);

    static CHIME_BUF: StaticCell<[u8; 4480]> = StaticCell::new();
    let chime_buf = CHIME_BUF.init([0u8; 4480]);
    let chime_len = fill_chime_buffer(chime_buf, 16000);
    println!("[AUDIO] Chime OK ({} bytes chime)", chime_len);

    // === WiFi + BLE init (esp-radio 0.18.0) — RADIO STAYS IDLE AT BOOT ===
    //
    // POWER BUDGET NOTE:
    // Up to v0.4 the watch booted straight into a connected WiFi STA with
    // no power-save configured. The 2.4 GHz radio + PA alone sustains
    // ~90 mA continuous — the single biggest drain by far, roughly half
    // of the total ~1-hour runtime the user complained about.
    //
    // In esp-radio 0.18.0 there is no separate start()/stop() on
    // WifiController.  The controller is created once here; the radio is
    // activated by calling connect_async() (STA) or set_config(AP) and is
    // deactivated by calling disconnect_async().  Power save is set
    // immediately after creation so every transmission uses DTIM-based sleep.
    //
    // DHCP + NTP are deferred to the first user-toggle of the WiFi button,
    // so a watch that never touches WiFi pays exactly zero network cost.
    println!("[RADIO] Init radio stack (WiFi OFF, BLE OFF)");

    // === WiFi init (OFF by default) ===
    use embassy_net::{Config as NetConfig, StackResources};
    use esp_radio::wifi::{
        ap::AccessPointConfig, AuthenticationMethod, Config as WifiConfig,
        ControllerConfig, PowerSaveMode, sta::StationConfig,
    };
    let (mut wifi_controller, wifi_interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, ControllerConfig::default())
            .expect("WiFi init failed");
    // Enable DTIM-based power save immediately — reduces idle radio current
    // from ~90 mA to ~20 mA average.
    wifi_controller.set_power_saving(PowerSaveMode::Maximum).ok();

    let mut ble_connector = esp_radio::ble::controller::BleConnector::new(
        peripherals.BT,
        esp_radio::ble::Config::default(),
    )
    .expect("BLE init failed");
    println!("[BLE] HCI connector ready");
    println!("[HIL][PASS] ble-hci");
    log_memory("after-radio");

    // === NVS: load stored WiFi credentials ===
    // If no credentials are stored, enter AP provisioning mode.
    let nvs_creds: Option<StoredCredentials> = read_credentials();
    let wifi_has_nvs_creds = nvs_creds
        .as_ref()
        .map(|c| c.is_configured())
        .unwrap_or(false);

    // Load persistent watch settings (hourly rate, display prefs, feature flags).
    // Falls back to defaults if flash is blank or CRC fails (first boot).
    let mut watch_settings = load_settings();
    println!(
        "[SETTINGS] rate={}c/h timeout={}s ble={} wifi={}",
        watch_settings.hourly_rate_cents,
        watch_settings.display_timeout_secs,
        watch_settings.ble_enabled,
        watch_settings.wifi_enabled,
    );
    if watch_settings.ble_enabled {
        if let Some(auth_key) = crate::peripherals::nvs::read_device_auth_key() {
            if let Ok(task) = crate::peripherals::ble::companion_sync_task(ble_connector, auth_key)
            {
                _spawner.spawn(task);
                println!("[BLE] companion GATT service started");
            } else {
                println!("[BLE] companion GATT task could not start");
            }
        } else if let Err(error) = crate::peripherals::ble::start_advertising(&mut ble_connector) {
            println!("[BLE] advertising start failed: {:?}", error);
        } else {
            println!("[BLE] advertising enabled without sync key");
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

        // Generate a random 8-hex-digit suffix using hardware RNG so the
        // provisioning password is not guessable from boot timing.
        let rand_suffix: u32 = esp_hal::rng::Rng::new().random();
        let ap_password = alloc::format!("sbie-{:08X}", rand_suffix);

        // Reconfigure WiFi controller as Access Point
        // Never expose credential provisioning on an open AP. The password is
        // shown by the setup UI/QR flow and protects the plaintext HTTP form
        // from arbitrary nearby clients.
        let ap_config = AccessPointConfig::default()
            .with_ssid(alloc::string::String::from(AP_SSID))
            .with_password(ap_password.clone())
            .with_auth_method(AuthenticationMethod::Wpa2Personal);
        let ap_mode = WifiConfig::AccessPoint(ap_config);
        if wifi_controller.set_config(&ap_mode).is_err() {
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
        let ap_seed = ((esp_hal::rng::Rng::new().random() as u64) << 32)
            | (esp_hal::rng::Rng::new().random() as u64);
        let (ap_stack, ap_runner) =
            embassy_net::new(wifi_interfaces.access_point, ap_net_config, ap_resources, ap_seed);
        if let Ok(token) = net_task(ap_runner) {
            _spawner.spawn(token);
        }

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
                wifi_controller.disconnect_async().await.ok();
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
    let is_open_network = wifi_pass.is_empty();
    let client_config = StationConfig::default()
        .with_ssid(wifi_ssid)
        .with_password(wifi_pass)
        .with_auth_method(if is_open_network {
            AuthenticationMethod::None
        } else {
            AuthenticationMethod::Wpa2Personal
        });
    let mode_config = WifiConfig::Station(client_config);
    wifi_controller
        .set_config(&mode_config)
        .expect("WiFi config failed");
    // NOTE: In esp-radio 0.18.0 there is no separate start() — the radio is
    // activated by connect_async().  The controller is idle until the user
    // taps the WiFi button.

    // Place the controller in WIFI_CTRL_MUTEX so both wifi_connect_task and
    // the settings-reconnect path in the main loop can reach it without
    // unsafe code.  After this point all radio interaction goes through the
    // mutex or through the Signal channels.
    *WIFI_CTRL_MUTEX.lock().await = Some(wifi_controller);

    // === Network Stack scaffolding (idle until radio starts) ===
    let net_config = NetConfig::dhcpv4(Default::default());
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let resources = RESOURCES.init(StackResources::new());

    let sta_seed = ((esp_hal::rng::Rng::new().random() as u64) << 32)
        | (esp_hal::rng::Rng::new().random() as u64);
    let (stack, runner) = embassy_net::new(wifi_interfaces.station, net_config, resources, sta_seed);

    // Spawn network runner task. It does nothing useful until the radio
    // has been started, but spawning it here avoids having to hot-spawn
    // a task from inside the main loop.
    if let Ok(token) = net_task(runner) {
        _spawner.spawn(token);
    }

    println!("=== All systems GO! (Embassy async, WiFi OFF) ===");

    // === State ===
    let mut watchface = WatchFace::new();
    watchface.set_context_row("Offline");
    let mut snapshot_store = crate::peripherals::companion_sync::RamSnapshotStore::new();
    let mut snapshot_received_at: Option<Instant> = None;
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
    let mut active_work_session_id = None;
    let mut active_recovery_blocked = false;
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
    let mut settings_app = SettingsApp::with_settings(watch_settings);
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
    let active_record = load_active_session();
    if let Ok(raw_dt) = rtc.get_time() {
        let dt = raw_dt.with_timezone_offset(watch_settings.timezone_offset_mins);
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

        match active_record {
            Ok(Some(active)) => match contains_session_id(active.session_id) {
                Ok(true) => {
                    if clear_active_session().is_ok() {
                        println!("[WORK] reconciled completed session after interrupted stop");
                    } else {
                        active_recovery_blocked = true;
                        println!("[WORK] recovery record could not be cleared; clock-in blocked");
                    }
                }
                Ok(false) => {
                    let now_unix = calendar_to_unix(
                        (2000 + dt.year as u16, dt.month, dt.day),
                        (dt.hours, dt.minutes, dt.seconds),
                    );
                    if let Some((elapsed_secs, break_secs)) =
                        now_unix.and_then(|now| active.recover_elapsed_and_break_secs(now))
                    {
                        work_tracker.restore_active_session(
                            WorkSession::new(
                                active.start_date_ymd,
                                active.start_time_hms,
                                active.hourly_rate_cents,
                            ),
                            elapsed_secs,
                            break_secs,
                            active.paused,
                        );
                        active_work_session_id = Some(active.session_id);
                        println!("[WORK] restored active session ({} seconds)", elapsed_secs);
                    } else {
                        active_recovery_blocked = true;
                        println!("[WORK] invalid RTC or session time; active session retained");
                    }
                }
                Err(error) => {
                    active_recovery_blocked = true;
                    println!("[WORK] session-log reconciliation failed: {:?}", error);
                }
            },
            Ok(None) => {}
            Err(error) => {
                active_recovery_blocked = true;
                println!("[WORK] active-session read failed: {:?}", error);
            }
        }
    } else if !matches!(active_record, Ok(None)) {
        active_recovery_blocked = true;
        println!("[WORK] RTC unavailable; active session retained for recovery");
    }
    let worklog_health = session_log_health();
    work_tracker.set_log_health(worklog_health);
    refresh_work_tracker_daily_summary(&mut work_tracker);
    work_tracker.set_start_allowed(
        !active_recovery_blocked
            && worklog_health.free_records > 0
            && worklog_health.severity() != LogSeverity::Error,
    );
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
    let mut next_imu_sample = Instant::now();

    // ── WiFi / NTP state ─────────────────────────────────────────────────────
    //
    // The heavy lifting (start, connect_async, NTP UDP exchange) is now done
    // inside dedicated Embassy tasks so the main loop is never blocked.
    // The main loop only manages these boolean flags and spawns/resets tasks.
    //
    //   wifi_task_running  : a wifi_connect_task is currently executing
    //   wifi_connected     : last WIFI_RESULT was Ok(())
    //   ntp_task_running   : an ntp_task is currently executing
    //   ntp_synced         : NTP_RESULT returned Some(dt) at least once
    //   wifi_toggle_request: user tapped the WiFi button
    let mut wifi_task_running: bool = false;
    let mut wifi_connected: bool = false;
    let mut ntp_task_running: bool = false;
    let mut ntp_synced: bool = false;
    let mut last_wifi_toggle = Instant::now();
    // Request pending from persistent settings or a UI tap on the WiFi button.
    let mut wifi_toggle_request: bool = watch_settings.wifi_enabled;
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
        let policy = crate::outdoor::PowerPolicy::for_mode(power_mode);

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
        if let Some(snapshot) = crate::peripherals::ble::SNAPSHOT_RESULT.try_take() {
            let generated_at = snapshot.generated_at_unix;
            if crate::peripherals::companion_sync::SnapshotStore::save(
                &mut snapshot_store,
                snapshot,
                generated_at,
            )
            .is_ok()
            {
                snapshot_received_at = Some(now);
            }
        }
        if let Some(snapshot) =
            crate::peripherals::companion_sync::SnapshotStore::load(&snapshot_store)
        {
            let elapsed_secs = snapshot_received_at
                .map(|received| (now - received).as_secs() as i64)
                .unwrap_or(0);
            let context = crate::peripherals::companion_sync::format_context_row(
                Some(snapshot),
                snapshot.generated_at_unix.saturating_add(elapsed_secs),
            );
            watchface.set_context_row(context.as_str());
        }
        if let Some(packet) = crate::peripherals::ble::POSITION_RESULT.try_take() {
            let encoded = packet.encode();
            let now_unix = calendar_to_unix(outdoor_state.date_ymd, outdoor_state.time_hms)
                .unwrap_or(packet.timestamp_unix);
            let _ = outdoor_state.handle_position_write(&encoded, now_unix);
        }
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

        let consumer_min_imu_hz = if need_gyro {
            if app_state == AppState::Watchface && current_page == Page::Sensors {
                10
            } else {
                30
            }
        } else if need_accel {
            if hike_running || app_state == AppState::Outdoor {
                20
            } else {
                1
            }
        } else {
            0
        };
        let imu_interval = policy
            .imu_interval_ms(consumer_min_imu_hz)
            .map(Duration::from_millis);
        let imu_sample_due = need_accel && imu_interval.is_some_and(|_| now >= next_imu_sample);

        if imu_sample_due {
            if let Some(interval) = imu_interval {
                next_imu_sample = now + interval;
            }
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

        if imu_sample_due && need_gyro {
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
            if let Ok(raw_dt) = rtc.get_time() {
                let dt = raw_dt.with_timezone_offset(watch_settings.timezone_offset_mins);
                watchface.update_time(dt.hours, dt.minutes, dt.seconds);
                watchface.update_date(dt.day, dt.month, dt.year);
                outdoor_state.time_hms = (dt.hours, dt.minutes, dt.seconds);
                outdoor_state.date_ymd = (2000 + dt.year as u16, dt.month, dt.day);
                work_tracker.update_time((dt.hours, dt.minutes, dt.seconds));
                let new_date = (2000 + dt.year as u16, dt.month, dt.day);
                let date_changed = work_tracker.date_ymd != new_date;
                work_tracker.update_date(new_date);
                if date_changed {
                    refresh_work_tracker_daily_summary(&mut work_tracker);
                }
                authenticator_app.update_time(
                    (dt.hours, dt.minutes, dt.seconds),
                    (2000 + dt.year as u16, dt.month, dt.day),
                );
            }
            next_rtc = now + Duration::from_secs(1);
        }

        // ── WiFi toggle ──────────────────────────────────────────────────────
        //
        // The actual radio bring-up runs in wifi_connect_task.  The main loop
        // only spawns / cancels the task and reads results from WIFI_RESULT.
        // This means the loop is never blocked for the ~17 s that
        // start + connect_async used to take.

        // Poll for a result if a task is in flight.
        if wifi_task_running && WIFI_RESULT.signaled() {
            match WIFI_RESULT.wait().await {
                Ok(()) => {
                    // Task connected successfully.
                    wifi_connected = true;
                    wifi_task_running = false;
                    ntp_synced = false; // trigger NTP
                    watchface.wifi_connected = true;
                    println!("[WIFI] connected");
                }
                Err(true) => {
                    // Task is retrying; stay in task_running state.
                    // The UI already shows "connecting…" via wifi_on=true.
                }
                Err(false) => {
                    // Task gave up (max retries or disconnect request).
                    wifi_connected = false;
                    wifi_task_running = false;
                    watchface.wifi_on = false;
                    watchface.wifi_connected = false;
                    println!("[WIFI] task ended without connection — user must retry");
                }
            }
        }

        // Handle a user toggle request (debounced: 2 s minimum between toggles).
        if wifi_toggle_request
            && now.duration_since(last_wifi_toggle) > Duration::from_secs(2)
        {
            last_wifi_toggle = now;
            wifi_toggle_request = false;

            if !wifi_task_running && !wifi_connected {
                // User wants WiFi on: spawn the connect task.
                // Clear any stale signals from a previous session before spawning.
                WIFI_RESULT.reset();
                WIFI_DISCONNECT_REQ.reset();
                println!("[WIFI] spawning connect task");
                watchface.wifi_on = true;
                wifi_task_running = true;
                watch_settings.wifi_enabled = true;
                let _ = crate::peripherals::settings::save_settings(watch_settings);
                if let Ok(token) = wifi_connect_task() {
                    _spawner.spawn(token);
                }
            } else if wifi_connected || wifi_task_running {
                // User wants WiFi off: signal the running task to stop, or
                // mark as disconnected if no task is running.
                println!("[WIFI] requesting disconnect");
                WIFI_DISCONNECT_REQ.signal(());
                wifi_connected = false;
                wifi_task_running = false; // task will exit on next iteration
                watchface.wifi_on = false;
                watchface.wifi_connected = false;
                ntp_synced = false;
                watch_settings.wifi_enabled = false;
                let _ = crate::peripherals::settings::save_settings(watch_settings);
            }
        }

        // ── NTP sync ─────────────────────────────────────────────────────────
        //
        // The 5-second UDP exchange runs in ntp_task.  The main loop spawns it
        // once after WiFi connects and applies the result when it arrives.
        if wifi_connected && !ntp_synced && !ntp_task_running {
            println!("[NTP] spawning NTP task");
            ntp_task_running = true;
            if let Ok(token) = ntp_task(stack) {
                _spawner.spawn(token);
            }
        }

        if ntp_task_running && NTP_RESULT.signaled() {
            ntp_task_running = false;
            match NTP_RESULT.wait().await {
                Some(dt) => {
                    ntp_synced = true;
                    if rtc.set_time(&dt).is_ok() {
                        println!("[NTP] RTC updated");
                    }
                    // Immediately push fresh time to watchface / apps.
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
                    watchface.force_redraw();
                    page_dirty = true;
                }
                None => {
                    println!("[NTP] sync failed; will retry on next WiFi connect");
                    // ntp_synced stays false → will retry if still connected
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
        if touch_int.is_low() || touch.is_tracking() {
            if let Ok((point, swipe_event)) = touch.poll() {
                if point.is_some() {
                    frame_touch = point;
                    last_interaction = now;
                    // Wake screen if off/AOD or restore full brightness from dim.
                    if screen_state < 2 {
                        screen_state = 3;
                        page_dirty = true;
                        report_display(display.display_on());
                        report_display(display.set_brightness(watchface.brightness));
                    } else if screen_state == 2 {
                        screen_state = 3;
                        report_display(display.set_brightness(watchface.brightness));
                        page_dirty = true;
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
                    } else if screen_state == 2 {
                        screen_state = 3;
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
                            } else if let Some(brightness) =
                                WatchFace::brightness_from_tap(tp.x, tp.y)
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
                        if let Some(updated) = settings_app.take_settings_update() {
                            watch_settings = updated;
                            page_dirty = true;
                        }
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
            AppState::WorkTracker => {
                let touched_work_button = app_input.tap
                    && app_touch.is_some_and(|touch| {
                        (50..=360).contains(&(touch.x as i32))
                            && (330..=400).contains(&(touch.y as i32))
                    });
                let touched_pause_button = app_input.tap
                    && app_touch.is_some_and(|touch| {
                        (30..=195).contains(&(touch.x as i32))
                            && (330..=400).contains(&(touch.y as i32))
                    });
                if touched_pause_button && work_tracker.state != TrackerState::Idle {
                    let pause = work_tracker.state == TrackerState::Working;
                    match (
                        active_work_session_id,
                        work_tracker.active_session,
                        calendar_to_unix(work_tracker.date_ymd, work_tracker.time_hms),
                    ) {
                        (Some(session_id), Some(session), Some(now_unix)) => {
                            match checkpoint_work_session(session_id, session, pause, now_unix) {
                                Ok(break_secs) => {
                                    if pause {
                                        work_tracker.pause();
                                    } else {
                                        work_tracker.resume_with_break_secs(break_secs);
                                    }
                                }
                                Err(error) => {
                                    work_tracker.set_storage_error(true);
                                    println!("[WORK] pause/resume checkpoint failed: {:?}", error);
                                }
                            }
                        }
                        _ => {
                            work_tracker.set_storage_error(true);
                            println!("[WORK] pause/resume missing session or valid RTC time");
                        }
                    }
                    page_dirty = true;
                    Some(AppResult::Continue)
                } else if touched_work_button && work_tracker.state == TrackerState::Idle {
                    if let Some(session) = work_tracker.pending_completed_session() {
                        match active_work_session_id {
                            Some(session_id) => match finish_work_session(session_id, session) {
                                Ok(()) => {
                                    active_work_session_id = None;
                                    work_tracker.confirm_completed_session_saved();
                                    let health = session_log_health();
                                    work_tracker.set_log_health(health);
                                    refresh_work_tracker_daily_summary(&mut work_tracker);
                                    work_tracker.set_start_allowed(
                                        health.free_records > 0
                                            && health.severity() != LogSeverity::Error,
                                    );
                                }
                                Err(error) => {
                                    work_tracker.set_storage_error(true);
                                    println!("[WORK] completion retry failed: {:?}", error);
                                }
                            },
                            None => {
                                work_tracker.set_storage_error(true);
                                println!("[WORK] pending completion has no active session ID");
                            }
                        }
                    } else if work_tracker.start_allowed {
                        match start_work_session(
                            work_tracker.date_ymd,
                            work_tracker.time_hms,
                            work_tracker.hourly_rate_cents,
                        ) {
                            Ok(active) => {
                                let session = WorkSession::new(
                                    active.start_date_ymd,
                                    active.start_time_hms,
                                    active.hourly_rate_cents,
                                );
                                active_work_session_id = Some(active.session_id);
                                if !work_tracker.clock_in_with_session(session) {
                                    work_tracker.restore_active_session(session, 0, 0, false);
                                    println!(
                                        "[WORK] tracker rejected a committed start; restored it"
                                    );
                                }
                            }
                            Err(error) => {
                                work_tracker.set_storage_error(true);
                                if matches!(
                                    error,
                                    WorkSessionStoreError::SessionLog(SessionLogError::Full)
                                ) {
                                    let health = session_log_health();
                                    work_tracker.set_log_health(health);
                                    work_tracker.set_start_allowed(false);
                                }
                                println!("[WORK] clock-in persistence failed: {:?}", error);
                            }
                        }
                    }
                    page_dirty = true;
                    Some(AppResult::Continue)
                } else {
                    let had_pending_session = work_tracker.pending_completed_session().is_some();
                    let result = work_tracker.update(&app_input);
                    if !had_pending_session {
                        if let Some(session) = work_tracker.pending_completed_session() {
                            match active_work_session_id {
                                Some(session_id) => {
                                    match finish_work_session(session_id, session) {
                                        Ok(()) => {
                                            active_work_session_id = None;
                                            work_tracker.confirm_completed_session_saved();
                                            let health = session_log_health();
                                            work_tracker.set_log_health(health);
                                            refresh_work_tracker_daily_summary(&mut work_tracker);
                                            work_tracker.set_start_allowed(
                                                health.free_records > 0
                                                    && health.severity() != LogSeverity::Error,
                                            );
                                        }
                                        Err(error) => {
                                            work_tracker.set_storage_error(true);
                                            println!("[WORK] completion save failed: {:?}", error);
                                        }
                                    }
                                }
                                None => {
                                    work_tracker.set_storage_error(true);
                                    println!("[WORK] completed session has no active session ID");
                                }
                            }
                            page_dirty = true;
                        }
                    }
                    Some(result)
                }
            }
            AppState::Authenticator => Some(authenticator_app.update(&app_input)),
            AppState::BleAnalyzer => Some(ble_analyzer_app.update(&app_input)),
            AppState::Mp3Player => Some(media_controller.update(&app_input)),
            AppState::Snake => {
                page_dirty = true;
                Some(snake_game.update(&app_input))
            }
            AppState::Game2048 => {
                page_dirty = true;
                Some(game_2048.update(&app_input))
            }
            AppState::Tetris => {
                page_dirty = true;
                Some(tetris_game.update(&app_input))
            }
            AppState::Flappy => {
                page_dirty = true;
                Some(flappy_game.update(&app_input))
            }
            AppState::Maze => {
                page_dirty = true;
                Some(maze_game.update(&app_input))
            }
            AppState::Watchface => None,
        };

        if work_tracker.hourly_rate_cents != watch_settings.hourly_rate_cents {
            watch_settings.hourly_rate_cents = work_tracker.hourly_rate_cents;
            let _ = crate::peripherals::settings::save_settings(watch_settings);
        }

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
                    watch_settings.wifi_enabled = true;
                    let _ = crate::peripherals::settings::save_settings(watch_settings);

                    // If the connect task is running, tell it to stop so we
                    // can reconfigure the controller for the new SSID.
                    if wifi_task_running {
                        WIFI_DISCONNECT_REQ.signal(());
                        // Task will drain and signal Err(false); we reset below.
                        wifi_task_running = false;
                        wifi_connected = false;
                    }

                    // Reconfigure with new credentials via the shared mutex.
                    // The connect task is not running at this point (we cleared
                    // wifi_task_running above), so the mutex is uncontested.
                    let client = StationConfig::default()
                        .with_ssid(alloc::string::String::from(config.ssid_str()))
                        .with_password(alloc::string::String::from(config.pass_str()))
                        .with_auth_method(if config.pass_len == 0 {
                            AuthenticationMethod::None
                        } else {
                            AuthenticationMethod::Wpa2Personal
                        });
                    let reconfig_ok = {
                        let mut g = WIFI_CTRL_MUTEX.lock().await;
                        match g.as_mut() {
                            Some(c) => c.set_config(&WifiConfig::Station(client)).is_ok(),
                            None => false,
                        }
                    };
                    if reconfig_ok {
                        wifi_connected = false;
                        ntp_synced = false;
                        // Trigger a fresh connect via the task.
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
            let watchface_outcome = match app_state {
                AppState::Watchface => Some(watchface.render(&mut fb)),
                AppState::Launcher => {
                    launcher.render(&mut fb);
                    None
                }
                AppState::Settings => {
                    settings_app.render(&mut fb);
                    None
                }
                AppState::Outdoor => {
                    outdoor_app.render(&mut fb, &outdoor_state);
                    None
                }
                AppState::WorkTracker => {
                    work_tracker.render(&mut fb);
                    None
                }
                AppState::Authenticator => {
                    authenticator_app.render(&mut fb);
                    None
                }
                AppState::BleAnalyzer => {
                    ble_analyzer_app.render(&mut fb);
                    None
                }
                AppState::Mp3Player => {
                    media_controller.render(&mut fb);
                    None
                }
                AppState::Snake => {
                    snake_game.render(&mut fb);
                    None
                }
                AppState::Game2048 => {
                    game_2048.render(&mut fb);
                    None
                }
                AppState::Tetris => {
                    tetris_game.render(&mut fb);
                    None
                }
                AppState::Flappy => {
                    flappy_game.render(&mut fb);
                    None
                }
                AppState::Maze => {
                    maze_game.render(&mut fb);
                    None
                }
            };
            let is_game = matches!(
                app_state,
                AppState::Snake
                    | AppState::Game2048
                    | AppState::Tetris
                    | AppState::Flappy
                    | AppState::Maze
            );
            if is_game {
                report_display(fb.swap_and_flush(&mut display, &mut te_pin).await);
                page_dirty = false;
                let policy_interval_ms = policy.display_interval_ms().unwrap_or(33);
                next_watchface_flush =
                    now + Duration::from_millis(core::cmp::max(33, policy_interval_ms));
            } else if let Some(outcome) = watchface_outcome {
                match outcome {
                    Ok(outcome) if outcome.full_redraw => report_display(fb.flush(&mut display)),
                    Ok(outcome) => {
                        for region in [
                            outcome.time_region,
                            outcome.battery_region,
                            outcome.gyro_region,
                            outcome.context_region,
                        ]
                        .into_iter()
                        .flatten()
                        {
                            report_display(fb.flush_region(
                                &mut display,
                                region.x,
                                region.y,
                                region.w,
                                region.h,
                            ));
                        }
                    }
                    Err(error) => report_display(Err(error)),
                }
                page_dirty = false;
                let policy_interval_ms = policy.display_interval_ms().unwrap_or(100);
                next_watchface_flush =
                    now + Duration::from_millis(core::cmp::max(100, policy_interval_ms));
            } else {
                report_display(fb.flush(&mut display));
                page_dirty = false;
                let policy_interval_ms = policy.display_interval_ms().unwrap_or(100);
                next_watchface_flush =
                    now + Duration::from_millis(core::cmp::max(100, policy_interval_ms));
            }
        }
    }
}
