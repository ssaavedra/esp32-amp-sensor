use esp_idf_svc::hal::adc;
use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::nvs;
use esp_idf_svc::sys::ESP_ERR_WIFI_NOT_STARTED;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        adc::{attenuation::adc_atten_t_ADC_ATTEN_DB_0, AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        gpio::Gpio35,
        peripherals::Peripherals,
    },
    sys::EspError,
    wifi::{self, ClientConfiguration},
};
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
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
        auth_method: wifi::AuthMethod::WPA2WPA3Personal,
        bssid: None,
        channel: None,
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



    let pin_in = peripherals.pins.gpio35;
    let adc_config = adc::config::Config::new();
    let mut chan_driver: AdcChannelDriver<adc_atten_t_ADC_ATTEN_DB_0, Gpio35> =
        AdcChannelDriver::new(pin_in)?;
    let mut driver = AdcDriver::new(peripherals.adc1, &adc_config)?;

    let adc_value = Arc::new(Mutex::new(0));

    // // Start Http Server
    let server_config = esp_idf_svc::http::server::Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");
    server
        .fn_handler(
            "/",
            esp_idf_svc::http::Method::Get,
            |req| -> Result<(), esp_idf_svc::io::EspIOError> {
                log::info!("Got request");
                let mut server_msg = heapless::String::<64>::try_from("Hello, World!\nADC Value: ")
                    .expect("Failed to create string");
                let val = adc_value.lock().unwrap();
                write!(server_msg, "{}\n", val).unwrap();
                let _response = req.into_response(
                    200,
                    Some(server_msg.as_str()),
                    &[("Content-Type", "text/plain")],
                )?;
                Ok(())
            },
        )
        .expect("Failed to add handler");

    loop {
        {
            let guard = adc_value.lock();
            let mut adc_value = guard.unwrap();
            *adc_value = driver.read(&mut chan_driver).unwrap();
            log::info!("Updated ADC Value: {}", adc_value);
        }

        log::info!("Wifi is: {:?}", wifi.is_connected());
        let wifi_client = wifi.sta_netif();
        log::info!(
            "Listening on IP: {:?} (hostname={:?})",
            wifi_client.get_ip_info(),
            wifi_client.get_hostname()
        );

        // Sleep 500ms
        FreeRtos::delay_ms(500u32);
    }
}
