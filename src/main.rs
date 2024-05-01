use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::{adc, gpio};
use esp_idf_svc::nvs;
use esp_idf_svc::{
    hal::{
        adc::{attenuation, AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        gpio::Gpio35,
        peripherals::Peripherals,
    },
    sys::EspError,
};
use http_server::{configure_http_server, configure_setup_http_server};
use ssd1306::prelude::Brightness;
use ssd1306::size::DisplaySize128x32;
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

pub mod amps;
pub mod display;
pub mod http_server;
pub mod wifi;
use crate::wifi::AppWifi as _;

// AC Voltage is 220V
const AC_VOLTS: f32 = 220.0;

/// This configuration is picked up at compile time by `build.rs` from the
/// file `cfg.toml`.
#[toml_cfg::toml_config]
pub struct Config {
    #[default("Wokwi-GUEST")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_psk: &'static str,
    #[default("wattometer")]
    hostname: &'static str,
}

fn main() -> Result<(), EspError> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = nvs::EspNvsPartition::<nvs::NvsDefault>::take()?;
    let mut nvs_partition = nvs::EspNvs::new(nvs.clone(), "ssaa", true)?;

    // Set Pin 26 as OUTPUT and HIGH so that we have a VCC for the screen in the same side of everything
    // as the 3V3 pin is on the other side of the board
    let mut gpio26 = PinDriver::output(peripherals.pins.gpio26).unwrap();
    gpio26.set_high()?;

    let mut display_handler = display::init_display_i2c(
        peripherals.pins.gpio25,
        peripherals.pins.gpio14,
        peripherals.i2c0,
        DisplaySize128x32,
    )?;

    let pin_in = peripherals.pins.gpio35;

    let app_config = CONFIG;

    let (wifi_ssid, wifi_psk, mut setup_mode) =
        wifi::get_ssid_psk_from_nvs(&app_config, &nvs_partition, false)?;
    log::info!(
        "SSID: {:?} (len={}), PSK: {:?} (len={}) (setup={})",
        wifi_ssid.as_bytes(),
        wifi_ssid.chars().count(),
        wifi_psk,
        wifi_psk.chars().count(),
        setup_mode
    );
    let wifi = wifi::setup_wifi(
        peripherals.modem,
        wifi_ssid,
        wifi_psk,
        setup_mode,
        &nvs,
        &sysloop,
    )?;
    wifi::set_wifi_hostname("wattometer".to_string(), Arc::downgrade(&wifi), &sysloop);

    let adc_config = adc::config::Config::new();
    let mut chan_driver: AdcChannelDriver<{ attenuation::DB_2_5 }, Gpio35> =
        AdcChannelDriver::new(pin_in)?;
    let mut driver = AdcDriver::new(peripherals.adc1, &adc_config)?;

    let adc_value = Arc::new(Mutex::new(0f32));
    let mut server = if setup_mode {
        configure_setup_http_server(&mut nvs_partition)?
    } else {
        configure_http_server(&adc_value)?
    };

    let gpio0 = peripherals.pins.gpio0;
    let gpio0 = PinDriver::input(gpio0)?;

    let gpio2 = peripherals.pins.gpio2;
    let mut gpio2 = PinDriver::output(gpio2)?;
    gpio2.set_drive_strength(gpio::DriveStrength::I5mA)?;

    let mut setup_mode_changed;
    let mut last_setup_mode = setup_mode;

    loop {
        if last_setup_mode != setup_mode {
            setup_mode_changed = true;
            last_setup_mode = setup_mode;
        } else {
            setup_mode_changed = false;
        }

        if setup_mode_changed {
            drop(server);

            let (wifi_ssid, wifi_psk, _setup_mode) =
                wifi::get_ssid_psk_from_nvs(&app_config, &nvs_partition, setup_mode)?;
            log::info!(
                "SSID: {:?} (len={}), PSK: {:?} (len={}) (setup={})",
                wifi_ssid.as_bytes(),
                wifi_ssid.chars().count(),
                wifi_psk,
                wifi_psk.chars().count(),
                setup_mode
            );
            wifi::reset_wifi(wifi.clone(), wifi_ssid, wifi_psk, setup_mode)?;
            // wifi::set_wifi_hostname("wattometer".to_string(), Arc::downgrade(&wifi), &sysloop);

            server = if setup_mode {
                configure_setup_http_server(&mut nvs_partition)?
            } else {
                configure_http_server(&adc_value)?
            };
        };

        if setup_mode {
            display_handler.run(|d| d.clear());
            display_handler.run(|d| write!(d, "SETUP MODE AP:\n{}\n", app_config.wifi_ssid));
            display_handler.run(|d| write!(d, "KEY:\n{}\n", app_config.wifi_psk));

            // Blink the LED
            gpio2.set_high()?;
            FreeRtos::delay_ms(1000u32);
            gpio2.set_low()?;
            FreeRtos::delay_ms(1000u32); // Wait for longer, since this will just refresh the screen

            if gpio0.is_low() {
                // If the BOOT button is pressed, we will exit setup mode
                setup_mode = false;
                // Blink twice to confirm
                gpio2.set_high()?;
                FreeRtos::delay_ms(100u32);
                gpio2.set_low()?;
                FreeRtos::delay_ms(100u32);
                gpio2.set_high()?;
                FreeRtos::delay_ms(100u32);
                gpio2.set_low()?;
                FreeRtos::delay_ms(500u32);
                continue;
            }
        } else {
            // If the BOOT button is pressed, we will enter setup mode
            if gpio0.is_low() {
                setup_mode = true;
                continue;
            } else {
                log::info!("Normal mode");
            }

            // Tiny blink of LED if normal mode and wifi is connected
            if wifi.is_connected()? {
                gpio2.set_high()?;
                FreeRtos::delay_ms(100u32);
                gpio2.set_low()?;
            }

            display_handler.init(Brightness::DIM);
            let amps = amps::read_amps(&mut driver, &mut chan_driver).unwrap();
            {
                let guard = adc_value.try_lock();
                match guard {
                    Ok(mut guard) => *guard = amps,
                    Err(_) => log::warn!("ADC value is locked"),
                }
            };

            log::info!("Amps: {:.5}A ; {:.5}W", amps, AC_VOLTS * amps);
            display_handler.run(|d| d.set_position(0, 0));
            display_handler.run(|d| write!(d, "{:.5}A    \n{:.5}W    \n", amps, AC_VOLTS * amps));

            if wifi.is_connected()? {
                let ip = wifi.get_client_ip()?;
                display_handler.run(|d| write!(d, "{}\n", ip));
            } else {
                display_handler.run(|d| write!(d, "CONNECTING..."));
            }
        }

        // Sleep 1500ms
        FreeRtos::delay_ms(1500u32);
    }
}
