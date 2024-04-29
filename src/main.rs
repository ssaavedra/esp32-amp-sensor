use esp_idf_svc::hal::adc;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::nvs;
use esp_idf_svc::sys::esp_netif_set_hostname;
use esp_idf_svc::wifi::{AccessPointConfiguration, WifiEvent};
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
use http_server::{configure_http_server, configure_setup_http_server};
use ssd1306::prelude::Brightness;
use ssd1306::size::DisplaySize128x32;
use std::ops::Deref;
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
    let default_partition = nvs::EspNvs::new(nvs.clone(), "ssaa", true)?;
    let mut setup_mode = false;
    let wifi_ssid = {
        let mut buf = [0u8; 32];
        if let Err(e) = default_partition.get_str("wifi_ssid", &mut buf) {
            log::info!("Error reading wifi_ssid from NVS: {:?}", e);
            buf.copy_from_slice(app_config.wifi_ssid.as_bytes());
        } else {
            log::info!("Read wifi_ssid from NVS {:}", String::from_utf8_lossy(&buf));
        }
        let nul = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let buf = String::from_utf8_lossy(&buf[..nul]);
        if buf.chars().count() == 0 {
            setup_mode = true;
        }
        buf.to_string()
    };
    let wifi_psk = {
        let mut buf = [0u8; 64];
        if let Err(e) = default_partition.get_str("wifi_psk", &mut buf) {
            log::info!("Error reading wifi_psk from NVS: {:?}", e);
            buf.copy_from_slice(app_config.wifi_psk.as_bytes());
        } else {
            log::info!(
                "Read wifi_psk from NVS {:}",
                String::from_utf8_lossy(&buf).to_string()
            );
        }
        let nul = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let buf = String::from_utf8_lossy(&buf[..nul]);
        if buf.chars().count() == 0 {
            setup_mode = true;
        }
        buf.to_string()
    };
    log::info!(
        "SSID: {:?} (len={}), PSK: {:?} (len={}) (setup={})",
        wifi_ssid.as_bytes(),
        wifi_ssid.chars().count(),
        wifi_psk,
        wifi_psk.chars().count(),
        setup_mode
    );

    let wifi = Arc::new(Mutex::new(wifi::EspWifi::new(
        peripherals.modem,
        sysloop.clone(),
        Some(nvs),
    )?));
    let wifi_config = if setup_mode {
        wifi::Configuration::AccessPoint(AccessPointConfiguration {
            ssid: heapless::String::try_from(app_config.wifi_ssid).expect("SSID too long"),
            password: heapless::String::try_from(app_config.wifi_psk).expect("Password too long"),
            auth_method: wifi::AuthMethod::WPA2Personal,
            ..Default::default()
        })
    } else {
        wifi::Configuration::Client(ClientConfiguration {
            ssid: heapless::String::try_from(wifi_ssid.as_str()).expect("SSID too long"),
            password: heapless::String::try_from(wifi_psk.as_str()).expect("Password too long"),
            ..Default::default()
        })
    };
    {
        let wifi = wifi.lock();
        let mut wifi = wifi.unwrap();
        if let Err(err) = wifi.set_configuration(&wifi_config) {
            log::info!("Wifi not started, error={}, starting now", err);
        }
        wifi.start()?;
        if !setup_mode {
            wifi.connect()?;
        }
    }

    let w2 = wifi.clone();

    sysloop.subscribe::<WifiEvent, _>(move |event| {
        if let WifiEvent::ApStaConnected = event {
            log::info!("Connected to Wi-Fi");
            // Set hostname now!
            unsafe {
                // Safe because we are passing a null-terminated string and sta_netif_mut must exist when connected to wifi
                esp_netif_set_hostname(w2.lock().unwrap().sta_netif().handle(), "wattometer\0".as_ptr() as _);
            }
            log::info!("Hostname set to {}", app_config.hostname);
        }
    })?;

    let adc_config = adc::config::Config::new();
    let mut chan_driver: AdcChannelDriver<{ attenuation::DB_2_5 }, Gpio35> =
        AdcChannelDriver::new(pin_in)?;
    let mut driver = AdcDriver::new(peripherals.adc1, &adc_config)?;

    let adc_value = Arc::new(Mutex::new(0f32));
    let _server = if setup_mode {
        configure_setup_http_server()?
    } else {
        configure_http_server(&adc_value)?
    };

    let gpio0 = peripherals.pins.gpio0;
    let gpio0 = PinDriver::input(gpio0).unwrap();

    loop {
        if setup_mode {
            display_handler.run(|d| write!(d, "SETUP MODE AP:\n{}\n", app_config.wifi_ssid));
            display_handler.run(|d| write!(d, "KEY:\n{}\n", app_config.wifi_psk));
        } else {
            // If the BOOT button is pressed, we will enter setup mode
            if gpio0.is_low() {
                setup_mode = true;
                continue;
            } else {
                log::info!("Normal mode");
            }

            display_handler.init(Brightness::DIM);
            let guard = adc_value.lock();
            let mut amps = guard.unwrap();
            *amps = amps::read_amps(&mut driver, &mut chan_driver).unwrap();

            log::info!("Amps: {:.5}A ; {:.5}W", *amps, AC_VOLTS * *amps);
            display_handler.run(|d| d.set_position(0, 0));
            display_handler.run(|d| write!(d, "{:.5}A    \n{:.5}W    \n", *amps, AC_VOLTS * *amps));

            if wifi.lock().unwrap().is_connected()? {
                let ip = wifi.lock().unwrap().sta_netif().get_ip_info()?.ip;
                display_handler.run(|d| write!(d, "{}\n", ip));
            } else {
                display_handler.run(|d| write!(d, "CONNECTING..."));
            }
        }

        // Sleep 500ms
        FreeRtos::delay_ms(1500u32);
    }
}
