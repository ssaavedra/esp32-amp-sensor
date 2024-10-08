use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use esp_idf_svc::hal::modem::WifiModemPeripheral;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::handle::RawHandle as _;
use esp_idf_svc::sys::esp_netif_set_hostname;
pub use esp_idf_svc::wifi::{AccessPointConfiguration, WifiEvent};
pub use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        adc::{attenuation, AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        gpio::Gpio35,
        peripherals::Peripherals,
    },
    sys::EspError,
    wifi::{self, AuthMethod, ClientConfiguration, Configuration, EspWifi},
};
use esp_idf_svc::{hal, http, nvs};
use ssd1306::size::DisplaySize128x32;

use crate::state::AsGlobalState;

pub fn non_empty_string_or_fail(s: String) -> Result<String, EspError> {
    if s.len() == 0 {
        Err(EspError::from_non_zero(
            core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_INVALID_ARG).unwrap(),
        ))
    } else {
        Ok(s)
    }
}

pub fn get_ssid_psk_from_nvs(
    app_config: &crate::Config,
    nvs: &nvs::EspNvs<nvs::NvsDefault>,
    force_setup: bool,
) -> Result<(String, String, String, bool), EspError> {
    let mut setup_mode = force_setup;
    let wifi_ssid =
        match crate::nvs::read_str_from_nvs(nvs, "wifi_ssid").and_then(non_empty_string_or_fail) {
            Ok(ssid) => ssid,
            Err(_) => {
                setup_mode = true;
                app_config.wifi_ssid.to_string()
            }
        };
    let wifi_psk =
        match crate::nvs::read_str_from_nvs(nvs, "wifi_psk").and_then(non_empty_string_or_fail) {
            Ok(psk) => psk,
            Err(_) => {
                setup_mode = true;
                app_config.wifi_psk.to_string()
            }
        };
    let hostname =
        match crate::nvs::read_str_from_nvs(nvs, "hostname").and_then(non_empty_string_or_fail) {
            Ok(hostname) => hostname,
            Err(_) => app_config.default_hostname.to_string(),
        };

    if setup_mode {
        Ok((
            app_config.wifi_ssid.to_string(),
            app_config.wifi_psk.to_string(),
            hostname,
            true,
        ))
    } else {
        Ok((wifi_ssid, wifi_psk, hostname, setup_mode))
    }
}

pub fn render_wifi_config(
    app_config: &crate::Config,
    ssid: String,
    psk: String,
    setup_mode: bool,
) -> wifi::Configuration {
    if setup_mode {
        wifi::Configuration::Mixed(
            ClientConfiguration {
                ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
                password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
                ..Default::default()
            },
            AccessPointConfiguration {
                ssid: heapless::String::try_from(app_config.wifi_ssid).expect("SSID too long"),
                password: heapless::String::try_from(app_config.wifi_psk)
                    .expect("Password too long"),
                auth_method: wifi::AuthMethod::WPA2Personal,
                ..Default::default()
            },
        )
    } else {
        wifi::Configuration::Client(ClientConfiguration {
            ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
            password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
            ..Default::default()
        })
    }
}

pub fn setup_wifi<'d, M: WifiModemPeripheral>(
    app_config: &crate::Config,
    modem: impl Peripheral<P = M> + 'd,
    ssid: String,
    psk: String,
    hostname: String,
    setup_mode: bool,
    nvs: &nvs::EspNvsPartition<nvs::NvsDefault>,
    sysloop: &EspSystemEventLoop,
) -> Result<EspWifi<'d>, EspError> {
    let mut wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))?;

    let wifi_config = render_wifi_config(app_config, ssid, psk, setup_mode);
    {
        set_wifi_hostname_once(hostname, &wifi);
        if let Err(err) = wifi.set_configuration(&wifi_config) {
            log::info!("Wifi not started, error={}, starting now", err);
        }
        wifi.start()?;
        let raw_handle = wifi.sta_netif().handle();
        log::info!("Setting hostname to {}!", app_config.default_hostname);
        unsafe {
            esp_netif_set_hostname(raw_handle, app_config.default_hostname.as_ptr() as _);
        }
        if !setup_mode {
            wifi.connect()?;
        }
    }
    Ok(wifi)
}

pub fn reset_wifi<'a>(
    app_config: &crate::Config,
    wifi: &Arc<Mutex<EspWifi<'a>>>,
    ssid: String,
    psk: String,
    setup_mode: bool,
) -> Result<(), EspError> {
    match wifi.try_lock() {
        Ok(mut wifi) => {
            // We don't care if disconnecting fails at all
            let _ = wifi.disconnect();
            let _ = wifi.stop();

            // Reset configuration
            let wifi_config = render_wifi_config(app_config, ssid, psk, setup_mode);
            {
                if let Err(err) = wifi.set_configuration(&wifi_config) {
                    log::info!("Wifi not started, error={}, starting now", err);
                }
                wifi.start()?;
                if !setup_mode {
                    wifi.connect()?;
                }
            }
        }
        Err(_) => {
            return Err(EspError::from_non_zero(
                core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
            ))
        }
    };
    Ok(())
}

#[embassy_executor::task]
pub async fn wifi_handle_task(
    app_config: crate::Config,
    nvs: nvs::EspNvsPartition<nvs::NvsDefault>,
    global_state: impl AsGlobalState<
            'static,
            ssd1306::prelude::I2CInterface<esp_idf_svc::hal::i2c::I2cDriver<'static>>,
            DisplaySize128x32,
        > + 'static,
) -> ! {
    loop {
        let _ = wifi_handle_task_worker(&app_config, &nvs, &global_state).await;
    }
}

pub async fn wifi_handle_task_worker(
    app_config: &crate::Config,
    nvs: &nvs::EspNvsPartition<nvs::NvsDefault>,
    global_state: &impl AsGlobalState<
        'static,
        ssd1306::prelude::I2CInterface<esp_idf_svc::hal::i2c::I2cDriver<'static>>,
        DisplaySize128x32,
    >,
) -> Result<(), EspError> {
    let mut seconds_disconnected = 0;
    let global_state = global_state.as_global_state();
    loop {
        let nvs_partition = nvs::EspNvs::new(nvs.clone(), "ssaa", false)?;
        let (ssid, psk, hostname, rendered_setup_mode) = get_ssid_psk_from_nvs(
            app_config,
            &nvs_partition,
            *global_state.setup_mode.lock().unwrap(),
        )?;
        let wifi_config =
            render_wifi_config(app_config, ssid.clone(), psk.clone(), rendered_setup_mode);
        if let Ok(mut wifi) = global_state.wifi.lock() {
            set_wifi_hostname_once(hostname, &wifi);
            if let Err(err) = wifi.set_configuration(&wifi_config) {
                log::info!("Wifi not started, error={}, starting now", err);
            }
            wifi.start()?;
            if !rendered_setup_mode {
                wifi.connect()?;
            }
        }
        FreeRtos::delay_ms(1000);
        if let Ok(mut setup_mode) = global_state.setup_mode.lock() {
            if !*setup_mode {
                if !global_state.wifi.is_connected()? {
                    seconds_disconnected += 1;
                    log::info!("Wi-Fi disconnected for {} seconds", seconds_disconnected);
                    if seconds_disconnected > 3 && seconds_disconnected % 5 == 0 {
                        // Issue the connect command again
                        if let Ok(mut wifi) = global_state.wifi.lock() {
                            wifi.connect()?;
                        }
                    }
                    if seconds_disconnected > 30 {
                        log::info!("Resetting Wi-Fi");
                        *setup_mode = true;
                        // Release the lock early since we don't really need it anymore
                        drop(setup_mode);
                        reset_wifi(app_config, &global_state.wifi, ssid, psk, true)?;
                    }
                } else {
                    seconds_disconnected = 0;
                }
            }
        }
    }
}

pub fn set_wifi_hostname_once(mut hostname: String, wifi: &EspWifi) {
    let raw_handle = wifi.sta_netif().handle();
    if hostname.len() > 32 {
        log::warn!("Hostname too long, truncating to 32 characters");
        hostname = hostname.chars().take(32).collect();
    }
    log::info!("Setting hostname to {}!", hostname);
    unsafe {
        // Safe because we are passing a null-terminated string and sta_netif must exist (as it is safe Rust)
        esp_netif_set_hostname(raw_handle, hostname.as_ptr() as _);
    };
    log::info!("Hostname set to {}", hostname);
}

pub fn set_wifi_hostname<'a, 'b>(
    hostname: String,
    wifi: std::sync::Weak<Mutex<EspWifi<'static>>>,
    sysloop: &'a EspSystemEventLoop,
) {
    let hostname = hostname.clone();

    sysloop
        .subscribe::<WifiEvent, _>(move |event| {
            let hostname_copy = hostname.clone();
            if let WifiEvent::StaConnected = event {
                log::info!("Connected to Wi-Fi");
                let _ = match wifi.upgrade() {
                    Some(wifi) => match wifi.lock() {
                        Ok(wifi) => set_wifi_hostname_once(hostname_copy, &wifi),
                        Err(_) => return,
                    },
                    None => return,
                };
            }
        })
        .map_err(|err| log::error!("Failed to subscribe to WifiEvent: {:?}", err))
        .unwrap();
}

pub trait AppWifi {
    fn is_connected(&self) -> Result<bool, EspError>;
    fn get_client_ip(&self) -> Result<Ipv4Addr, EspError>;
    fn connect(&self) -> Result<(), EspError>;
}

pub fn get_client_ip(wifi: &EspWifi) -> Result<Ipv4Addr, EspError> {
    wifi.sta_netif().get_ip_info().map(|info| info.ip)
}

impl<'d> AppWifi for Arc<Mutex<EspWifi<'d>>> {
    fn connect(&self) -> Result<(), EspError> {
        match self.try_lock() {
            Ok(mut wifi) => wifi.connect(),
            Err(_) => Err(EspError::from_non_zero(
                core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
            )),
        }
    }
    fn is_connected(&self) -> Result<bool, EspError> {
        match self.try_lock() {
            Ok(wifi) => wifi.is_connected(),
            Err(_) => Ok(false),
        }
    }
    fn get_client_ip(&self) -> Result<Ipv4Addr, EspError> {
        if self.is_connected()? {
            match self.try_lock() {
                Ok(wifi) => get_client_ip(&wifi),
                Err(_) => Err(EspError::from_non_zero(
                    core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
                )),
            }
        } else {
            Err(EspError::from_non_zero(
                core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
            ))
        }
    }
}

pub fn send_webhook<'a>(
    webhook_url: &String,
    wifi: &EspWifi<'a>,
    amps: f32,
    watts: f32,
) -> anyhow::Result<usize> {
    if !wifi.is_connected()? {
        return Err(EspError::from_non_zero(
            core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
        )
        .into());
    }
    log::info!("Sending webhook to {}", webhook_url);

    let datum = format!("{{\"amps\":{:.5},\"watts\":{:.5}}}", amps, watts);

    // Create HTTPS Connection Handle
    let httpconnection = http::client::EspHttpConnection::new(&http::client::Configuration {
        use_global_ca_store: true,
        crt_bundle_attach: Some(hal::sys::esp_crt_bundle_attach),
        ..Default::default()
    })?;
    let mut client = embedded_svc::http::client::Client::wrap(httpconnection);

    // If the URL contains {{amps}}, replace it with the actual amps
    let webhook_url = webhook_url.clone().replace("{{amps}}", &amps.to_string());

    // Send POST Request
    let response = client
        .post(
            &webhook_url,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", &datum.len().to_string()),
            ],
        )?
        .write(datum.as_bytes())?;

    Ok(response)
}
