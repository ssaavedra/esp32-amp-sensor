use esp_idf_svc::hal::adc;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::nvs;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        adc::{attenuation, AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        gpio::Gpio35,
        peripherals::Peripherals,
    },
    sys::EspError,
    wifi::{self, ClientConfiguration},
};
use http_server::configure_http_server;
use ssd1306::prelude::Brightness;
use ssd1306::size::DisplaySize128x32;
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

pub mod amps;
pub mod display;
pub mod http_server;

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
}

fn main() -> Result<(), EspError> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take().unwrap();

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

    // Initialize nvs before starting wifi
    let nvs = nvs::EspNvsPartition::<nvs::NvsDefault>::take()?;

    let mut wifi = wifi::EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs))?;
    let wifi_config = wifi::Configuration::Client(ClientConfiguration {
        ssid: heapless::String::try_from(app_config.wifi_ssid).expect("SSID too long"),
        password: heapless::String::try_from(app_config.wifi_psk).expect("Password too long"),
        ..Default::default()
    });
    if let Err(err) = wifi.set_configuration(&wifi_config) {
        log::info!("Wifi not started, error={}, starting now", err);
    }
    wifi.start()?;
    wifi.connect()?;

    let adc_config = adc::config::Config::new();
    let mut chan_driver: AdcChannelDriver<{ attenuation::DB_2_5 }, Gpio35> =
        AdcChannelDriver::new(pin_in)?;
    let mut driver = AdcDriver::new(peripherals.adc1, &adc_config)?;

    let adc_value = Arc::new(Mutex::new(0f32));
    let _server = configure_http_server(&adc_value)?;

    loop {
        {
            display_handler.init(Brightness::DIM);
            let guard = adc_value.lock();
            let mut amps = guard.unwrap();
            *amps = amps::read_amps(&mut driver, &mut chan_driver).unwrap();

            log::info!("Amps: {:.5}A ; {:.5}W", *amps, AC_VOLTS * *amps);
            display_handler.run(|d| d.set_position(0, 0));
            display_handler.run(|d| write!(d, "{:.5}A    \n{:.5}W    \n", *amps, AC_VOLTS * *amps));

            if wifi.is_connected()? {
                let ip = wifi.sta_netif().get_ip_info()?.ip;
                display_handler.run(|d| write!(d, "{}\n", ip));
            } else {
                display_handler.run(|d| write!(d, "CONNECTING..."));
            }
        }

        // Sleep 500ms
        FreeRtos::delay_ms(1500u32);
    }
}
