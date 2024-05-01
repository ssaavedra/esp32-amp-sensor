use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use esp_idf_svc::hal::modem::WifiModemPeripheral;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::handle::RawHandle as _;
use esp_idf_svc::nvs;
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

pub fn get_ssid_psk_from_nvs(
    app_config: &crate::Config,
    nvs: &nvs::EspNvs<nvs::NvsDefault>,
    force_setup: bool,
) -> Result<(String, String, bool), EspError> {
    let mut setup_mode = force_setup;
    let wifi_ssid = {
        let mut buf = [0u8; 32];
        if let Err(e) = nvs.get_str("wifi_ssid", &mut buf) {
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
        if let Err(e) = nvs.get_str("wifi_psk", &mut buf) {
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
    if setup_mode {
        Ok((app_config.wifi_ssid.to_string(), app_config.wifi_psk.to_string(), true))
    } else {
        Ok((wifi_ssid, wifi_psk, setup_mode))
    }
}

pub fn setup_wifi<'d, M: WifiModemPeripheral>(
    modem: impl Peripheral<P = M> + 'd,
    ssid: String,
    psk: String,
    setup_mode: bool,
    nvs: &nvs::EspNvsPartition<nvs::NvsDefault>,
    sysloop: &EspSystemEventLoop,
) -> Result<Arc<Mutex<EspWifi<'d>>>, EspError> {
    let mut wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))?;

    let wifi_config = if setup_mode {
        wifi::Configuration::AccessPoint(AccessPointConfiguration {
            ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
            password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
            auth_method: wifi::AuthMethod::WPA2Personal,
            ..Default::default()
        })
    } else {
        wifi::Configuration::Client(ClientConfiguration {
            ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
            password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
            ..Default::default()
        })
    };
    {
        if let Err(err) = wifi.set_configuration(&wifi_config) {
            log::info!("Wifi not started, error={}, starting now", err);
        }
        wifi.start()?;
        if !setup_mode {
            wifi.connect()?;
        }
    }
    let wifi = Arc::new(Mutex::new(wifi));
    Ok(wifi)
}

pub fn reset_wifi<'a>(
    wifi: Arc<Mutex<EspWifi<'a>>>,
    ssid: String,
    psk: String,
    setup_mode: bool,
) -> Result<(), EspError> {
    match wifi.try_lock() {
        Ok(mut wifi) => {
            wifi.disconnect()?;
            wifi.stop()?;

            // Reset configuration
            let wifi_config = if setup_mode {
                wifi::Configuration::AccessPoint(AccessPointConfiguration {
                    ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
                    password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
                    auth_method: wifi::AuthMethod::WPA2Personal,
                    ..Default::default()
                })
            } else {
                wifi::Configuration::Client(ClientConfiguration {
                    ssid: heapless::String::try_from(ssid.as_str()).expect("SSID too long"),
                    password: heapless::String::try_from(psk.as_str()).expect("Password too long"),
                    ..Default::default()
                })
            };

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
        Err(_) => return Err(EspError::from_non_zero(
            core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_WIFI_NOT_CONNECT).unwrap(),
        )),
    };
    Ok(())
}


pub fn set_wifi_hostname<'a>(
    hostname: String,
    wifi: std::sync::Weak<Mutex<EspWifi<'static>>>,
    sysloop: &'a EspSystemEventLoop,
) {
    let hostname = hostname.clone();
    let wifi = wifi.clone();

    sysloop.subscribe::<WifiEvent, _>(move |event| {
        let hostname_copy = hostname.clone();
        if let WifiEvent::ApStaConnected = event {
            log::info!("Connected to Wi-Fi");
            let raw_handle = {
                match wifi.upgrade() {
                    Some(wifi) => match wifi.try_lock() {
                        Ok(wifi) => wifi.sta_netif().handle(),
                        Err(_) => return,
                    },
                    None => return,
                }
            };
            // Set hostname now!
            unsafe {
                // Safe because we are passing a null-terminated string and sta_netif_mut must exist when connected to wifi
                esp_netif_set_hostname(raw_handle, hostname_copy.as_ptr() as _);
                log::info!("Hostname set to {}", hostname_copy);
            }
        }
    }).map_err(|err| log::error!("Failed to subscribe to WifiEvent: {:?}", err)).unwrap();
}

pub trait AppWifi {
    fn is_connected(&self) -> Result<bool, EspError>;
    fn get_client_ip(&self) -> Result<Ipv4Addr, EspError>;
}

impl<'d> AppWifi for Arc<Mutex<EspWifi<'d>>> {
    fn is_connected(&self) -> Result<bool, EspError> {
        match self.try_lock() {
            Ok(wifi) => wifi.is_connected(),
            Err(_) => Ok(false),
        }
    }
    fn get_client_ip(&self) -> Result<Ipv4Addr, EspError> {
        if self.is_connected()? {
            match self.try_lock() {
                Ok(wifi) => Ok(wifi.sta_netif().get_ip_info()?.ip),
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
