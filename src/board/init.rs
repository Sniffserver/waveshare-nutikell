//! Hardware peripheral initialization helpers for Waveshare ESP32-S3 AMOLED

use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::peripherals::{
    GPIO0, GPIO1, GPIO17, GPIO2, GPIO3, GPIO38, GPIO9, SPI3, SW_INTERRUPT, TIMG0,
};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::{MwdtStage, TimerGroup, Wdt};
use esp_println::println;

/// Initialize the 45-second Task Watchdog Timer (TWDT) and start the Embassy timer.
pub fn init_watchdog(
    timg0_periph: TIMG0<'static>,
    sw_interrupt_periph: SW_INTERRUPT<'static>,
) -> Wdt<TIMG0<'static>> {
    let timg0 = TimerGroup::new(timg0_periph);
    let mut wdt0 = timg0.wdt;
    wdt0.set_timeout(MwdtStage::Stage0, esp_hal::time::Duration::from_secs(45));
    wdt0.enable();
    let software_interrupt = SoftwareInterruptControl::new(sw_interrupt_periph);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);
    wdt0
}

/// Reset pulse for FT3168 touch controller and configure INT / BOOT pins.
pub fn init_touch_pins<'d>(
    gpio9: GPIO9<'d>,
    gpio38: GPIO38<'d>,
    gpio0: GPIO0<'d>,
    delay: &Delay,
) -> (Input<'d>, Input<'d>) {
    let mut touch_rst = Output::new(gpio9, Level::High, OutputConfig::default());
    let touch_int = Input::new(gpio38, InputConfig::default().with_pull(Pull::Up));
    touch_rst.set_low();
    delay.delay_millis(10);
    touch_rst.set_high();
    delay.delay_millis(50);
    let boot_button = Input::new(gpio0, InputConfig::default().with_pull(Pull::Up));
    (touch_int, boot_button)
}

/// Initialize the MicroSD card on SPI3 and scan for /MP3/ audio directory.
pub fn init_sd_card<'d>(
    spi3: SPI3<'d>,
    clk: GPIO2<'d>,
    mosi: GPIO1<'d>,
    miso: GPIO3<'d>,
    cs: GPIO17<'d>,
    delay: Delay,
) -> bool {
    println!("[SD] Init...");
    let sd_spi_config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(4))
        .with_mode(SpiMode::_0);
    let sd_spi = Spi::new(spi3, sd_spi_config)
        .expect("SPI3 failed")
        .with_sck(clk)
        .with_mosi(mosi)
        .with_miso(miso);
    let sd_cs = Output::new(cs, Level::High, OutputConfig::default());

    use embedded_hal_bus::spi::ExclusiveDevice;
    let sd_spi_dev = ExclusiveDevice::new_no_delay(sd_spi, sd_cs).unwrap();
    let sd_card = embedded_sdmmc::SdCard::new(sd_spi_dev, delay);
    let mut sd_ok = false;
    match sd_card.num_bytes() {
        Ok(size) => {
            println!("[SD] Card {}MB", size / 1024 / 1024);

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
                        let scan_folder = |dir_name: &str,
                                           mgr: &mut embedded_sdmmc::VolumeManager<_, _>|
                         -> bool {
                            if let Ok(mp3_dir) = mgr.open_dir(root_dir, dir_name) {
                                println!("[SD] Found /{:?}/ folder", dir_name);
                                let mut file_count = 0u16;
                                let _ = mgr.iterate_dir(mp3_dir, |entry| {
                                    if !entry.attributes.is_directory() {
                                        let name = core::str::from_utf8(entry.name.base_name())
                                            .unwrap_or("?");
                                        let ext = core::str::from_utf8(entry.name.extension())
                                            .unwrap_or("");
                                        println!("[SD]   {}.{}", name.trim(), ext.trim());
                                        file_count = file_count.saturating_add(1);
                                    }
                                });
                                println!("[SD] {} files found", file_count);
                                let _ = mgr.close_dir(mp3_dir);
                                true
                            } else {
                                false
                            }
                        };

                        if scan_folder("MP3", &mut volume_mgr)
                            || scan_folder("mp3", &mut volume_mgr)
                        {
                            sd_ok = true;
                        } else {
                            println!("[SD] No /mp3/ or /MP3/ folder");
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
    sd_ok
}
