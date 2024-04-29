use esp_idf_svc::hal::adc;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::nvs;
use esp_idf_svc::sys::adc_atten_t;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        prelude::*,
        adc::{attenuation, AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        gpio::Gpio35,
        peripherals::Peripherals,
    },
    sys::EspError,
    wifi::{self, ClientConfiguration},
};
use ssd1306::rotation::DisplayRotation;
use ssd1306::size::DisplaySize128x32;
use ssd1306::{I2CDisplayInterface, Ssd1306};
use ssd1306::mode::DisplayConfig;
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
    time::SystemTime,
};

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
    log::info!("Hello, world!");

    // Set Pin 14 as OUTPUT and HIGH so that we have a VCC for the screen in the same side of everything
    // as the 3V3 pin is on the other side of the board
    let mut gpio14 = PinDriver::output(peripherals.pins.gpio14).unwrap();
    gpio14.set_high()?;


    let app_config = CONFIG;

    // Initialize nvs before starting wifi
    let nvs = nvs::EspNvsPartition::<nvs::NvsDefault>::take()?;

    let mut wifi = wifi::EspWifi::new(peripherals.modem, sysloop.clone(), Some(nvs))?;
    log::info!("Create wifi structure");
    // wifi.start()?;
    log::info!("Connect wifi structure");
    let wifi_config = wifi::Configuration::Client(ClientConfiguration {
        ssid: heapless::String::try_from(app_config.wifi_ssid).expect("SSID too long"),
        password: heapless::String::try_from(app_config.wifi_psk).expect("Password too long"),
        ..Default::default()
    });
    log::info!("Create wifi config");
    if let Err(err) = wifi.set_configuration(&wifi_config)
    {
        log::info!("Wifi not started, error={}, starting now", err);
    }
    wifi.start()?;
    log::info!("Wifi started. Connecting");
    wifi.connect()?;
    log::info!("Wifi connected");
    log::info!("Wifi CONFIG: {:?}", wifi.driver().get_configuration()?);
    log::info!("Wifi SSID: {:?}", wifi.driver_mut().get_ap_info().map(|ap| ap.ssid));
    log::info!("Wifi IP: {:?}", wifi.sta_netif().get_ip_info().map(|info| info.ip));


    let pin_in = peripherals.pins.gpio35;
    let adc_config = adc::config::Config::new();
    let mut chan_driver: AdcChannelDriver<{ attenuation::DB_2_5 }, Gpio35> =
        AdcChannelDriver::new(pin_in)?;
    let mut driver = AdcDriver::new(peripherals.adc1, &adc_config)?;

    let adc_value = Arc::new(Mutex::new(0f32));

    // // Start Http Server
    let server_config = esp_idf_svc::http::server::Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");
    server
        .fn_handler(
            "/",
            esp_idf_svc::http::Method::Get,
            |req| -> Result<(), esp_idf_svc::io::EspIOError> {
                log::info!("Got request");
                let mut server_msg = String::new();
                let mutex_handle = adc_value.lock().unwrap();
                let amps: f32 = *mutex_handle;
                write!(server_msg, "Amps: {:.5}A ; {:.5}W", amps, 230.0 * amps).unwrap();
                req.into_response(
                    200,
                    Some("OK"),
                    &[("Content-Type", "text/plain")],
                )?.write(server_msg.as_bytes())?;

                Ok(())
            },
        )
        .expect("Failed to add handler");

    let i2c = peripherals.i2c0;
    let sda = peripherals.pins.gpio12;
    let scl = peripherals.pins.gpio13;
    let i2c_config = I2cConfig::new().baudrate(100.kHz().into());
    let i2c = I2cDriver::new(i2c, sda, scl, &i2c_config)?;
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(
        interface,
        DisplaySize128x32,
        DisplayRotation::Rotate0,
    ).into_terminal_mode();
    display.init().expect("Failed to initialize display");
    display.clear().unwrap();

    loop {
        {
            let guard = adc_value.lock();
            let mut adc_value = guard.unwrap();
            *adc_value = read_amps(&mut driver, &mut chan_driver).unwrap();
            log::info!(";;;;;Updated ADC Value: {:.32}", adc_value);

            // 30A = 1V
            // 1A = 0.0333V
            // AC Voltage is 230V
            let amps = *adc_value / 0.0333;
            log::info!("Amps: {:.5}A ; {:.5}W", amps, 230.0 * amps);
            display.clear().unwrap();
            write!(display, "{:.5}A\n{:.5}W\n", amps, 230.0 * amps).unwrap();
            if wifi.is_connected()? {
                let ip = wifi.sta_netif().get_ip_info()?.ip;
                write!(display, "WIFI @ {}", ip).unwrap();
            } else {
                write!(display, "WIFI NO CONN").unwrap();
            }
        }

        /*

        log::info!("Wifi is: {:?}", wifi.is_connected());
        let wifi_client = wifi.sta_netif();
        log::info!(
            "Listening on IP: {:?} (hostname={:?})",
            wifi_client.get_ip_info(),
            wifi_client.get_hostname()
        );

        */

        // Sleep 500ms
        FreeRtos::delay_ms(1500u32);
    }
}

fn read_amps<const A: adc_atten_t>(
    driver: &mut AdcDriver<adc::ADC1>,
    chan_driver: &mut AdcChannelDriver<A, Gpio35>,
) -> Result<f32, EspError>
{
    // Since we are working with 50Hz AC, we have a cycle every 20ms
    // We will sample for 500ms to get 25 samples

    let mut count: usize = 0;
    let start = SystemTime::now();
    let mut end = SystemTime::now();
    let mut highest_peak = 0.0f32;

    while end.duration_since(start).unwrap().as_millis() < 100 {
        let val = driver.read(chan_driver)?;
        highest_peak = highest_peak.max(val as f32);
        count += 1;
        // FreeRtos::delay_ms(1u32);
        end = SystemTime::now();
    }

    log::info!("Read {} samples", count);
    log::info!("Highest peak: {}", highest_peak);
    let peak = float_remap(highest_peak, 40.0, 1250.0, 0.0, 1.150);
    log::info!("Peak: {}V", peak);

    Ok(peak)
}

fn float_remap(value: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> f32 {
    return (value - in_min) * (out_max - out_min) / (in_max - in_min) + out_min;
}
