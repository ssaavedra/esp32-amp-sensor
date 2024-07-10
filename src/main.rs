use crate::display::DisplayHandlerExt;
use crate::state::PinDriverOutputArcExt as _;
use embassy_executor::Spawner;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::{adc, gpio};
use esp_idf_svc::{
    hal::{
        adc::{AdcChannelDriver, AdcDriver},
        delay::FreeRtos,
        peripherals::Peripherals,
    },
    sys::EspError,
};
use http_server::{
    configure_http_server, configure_setup_http_server, CURRENT_KNOWN_WEBHOOK,
    CURRENT_KNOWN_WIFI_SSID,
};
use ssd1306::prelude::Brightness;
use ssd1306::size::DisplaySize128x32;
use state::AsGlobalState;
use std::borrow::BorrowMut;
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

pub mod amps;
pub mod display;
pub mod http_server;
pub mod nvs;
pub mod state;
pub mod wifi;
use crate::nvs::read_str_from_nvs_or_default;
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
    default_hostname: &'static str,
}

fn setup_peripherals<'a, 'b>(
    peripherals: Peripherals,
    app_config: &'b Config,
    nvs: &'b nvs::EspNvsPartition<nvs::NvsDefault>,
    sysloop: &'b EspSystemEventLoop,
    wifi_ssid: String,
    wifi_psk: String,
    hostname: String,
    webhook_url: String,
    setup_mode: bool,
) -> Result<
    state::GlobalState<
        'a,
        ssd1306::prelude::I2CInterface<esp_idf_svc::hal::i2c::I2cDriver<'a>>,
        DisplaySize128x32,
    >,
    EspError,
> {
    let adc_config = adc::config::Config::new();

    // We'll set up these additional VCC and GND pins for the SSD1306 display,
    // in case you are using HW-394 and you want to route only one side of the
    // breadboard :)
    // Even though you should not do this as a long-term solution, it should be
    // probably OK for a prototype since the SSD1306 should draw <50mA
    #[cfg(feature = "hw-394-prototype")]
    {
        let mut gpio26 = PinDriver::output(peripherals.pins.gpio26).unwrap();
        gpio26.set_high()?;
        let mut gpio27 = PinDriver::output(peripherals.pins.gpio27).unwrap();
        gpio27.set_low()?;
    }

    // D2 is the builtin LED in HW-394 (when building your own board, you might
    // need to solder gpio2 to a LED)
    let mut gpio2 = PinDriver::output(peripherals.pins.gpio2)?;
    gpio2.set_drive_strength(gpio::DriveStrength::I5mA)?;

    let wifi = wifi::setup_wifi(
        app_config,
        peripherals.modem,
        wifi_ssid.clone(),
        wifi_psk,
        hostname,
        setup_mode,
        nvs,
        sysloop,
    )?;

    Ok(state::GlobalState {
        wifi: Arc::new(Mutex::new(wifi)),
        wifi_ssid: Arc::new(Mutex::new(wifi_ssid)),
        setup_mode: Arc::new(Mutex::new(setup_mode)),
        adc_value: Arc::new(Mutex::new(0f32)),
        display_handler: Arc::new(Mutex::new(display::init_display_i2c(
            peripherals.pins.gpio25,
            peripherals.pins.gpio14,
            peripherals.i2c0,
            DisplaySize128x32,
        )?)),
        webhook_url: Arc::new(Mutex::new(webhook_url)),
        gpio_btn_boot: PinDriver::input(peripherals.pins.gpio0)?,
        adc_driver: Arc::new(Mutex::new(AdcDriver::new(peripherals.adc1, &adc_config)?)),
        adc_chan_driver: Arc::new(Mutex::new(AdcChannelDriver::new(peripherals.pins.gpio35)?)),
        quiet_mode_pin: PinDriver::input(peripherals.pins.gpio34)?,
        blink_led: Arc::new(Mutex::new(gpio2)),
    })
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

    let app_config = CONFIG;

    let (wifi_ssid, wifi_psk, hostname, mut setup_mode) =
        wifi::get_ssid_psk_from_nvs(&app_config, &nvs_partition, false)?;
    log::info!(
        "SSID: {:?} (len={}), PSK: {:?} (len={}) (setup={})",
        wifi_ssid.as_bytes(),
        wifi_ssid.chars().count(),
        wifi_psk,
        wifi_psk.chars().count(),
        setup_mode
    );

    let mut webhook_url = read_str_from_nvs_or_default(&nvs_partition, "webhook", "");
    let global_state = setup_peripherals(
        peripherals,
        &app_config,
        &nvs,
        &sysloop,
        wifi_ssid,
        wifi_psk,
        hostname,
        webhook_url.clone(),
        setup_mode,
    )?;
    wifi::set_wifi_hostname(
        app_config.default_hostname.to_string(),
        Arc::downgrade(&global_state.wifi),
        &sysloop,
    );

    let mut server = {
        let setup_mode = global_state.setup_mode.lock().unwrap();
        if *setup_mode {
            log::info!("Starting EspHttpServer in setup mode");
            configure_setup_http_server(&mut nvs_partition)?
        } else {
            configure_http_server(&global_state.adc_value, &mut nvs_partition)?
        }
    };

    let display_handler = global_state.display_handler.clone();

    let mut wifi_disconnected_count = 0;
    let mut setup_mode_changed;
    let mut last_setup_mode = setup_mode;

    *CURRENT_KNOWN_WIFI_SSID.try_lock().unwrap() = global_state
        .as_global_state()
        .wifi_ssid
        .try_lock()
        .unwrap()
        .clone();

    *CURRENT_KNOWN_WEBHOOK.try_lock().unwrap() = webhook_url.clone();

    loop {
        if last_setup_mode != setup_mode {
            setup_mode_changed = true;
            last_setup_mode = setup_mode;
        } else {
            setup_mode_changed = false;
        }

        let high_level = if global_state.quiet_mode_pin.is_high() {
            gpio::Level::High
        } else {
            gpio::Level::Low
        };

        if setup_mode_changed {
            display_handler.run(|d| d.clear());
            drop(server);
            webhook_url = read_str_from_nvs_or_default(&nvs_partition, "webhook", "");

            let (wifi_ssid, wifi_psk, hostname, _setup_mode) =
                wifi::get_ssid_psk_from_nvs(&app_config, &nvs_partition, setup_mode)?;
            log::info!(
                "SSID: {:?} (len={}), PSK: {:?} (len={}) (setup={})",
                wifi_ssid,
                wifi_ssid.chars().count(),
                wifi_psk,
                wifi_psk.chars().count(),
                setup_mode
            );
            wifi::reset_wifi(
                &app_config,
                &global_state.wifi,
                wifi_ssid,
                wifi_psk,
                setup_mode,
            )?;
            wifi::set_wifi_hostname(hostname, Arc::downgrade(&global_state.wifi), &sysloop);

            server = if setup_mode {
                configure_setup_http_server(&mut nvs_partition)?
            } else {
                configure_http_server(&global_state.adc_value, &mut nvs_partition)?
            };
        };

        if setup_mode {
            display_handler.run(|d| d.set_position(0, 0));
            display_handler.run(|d| write!(d, "SETUP MODE AP:\n{}\n", app_config.wifi_ssid));
            display_handler.run(|d| write!(d, "KEY:\n{}\n", app_config.wifi_psk));

            // Forcefully blink the LED even if we are in "quiet" mode to identify that we are in setup mode
            global_state.blink_led.set_high()?;
            FreeRtos::delay_ms(1000u32);
            global_state.blink_led.set_low()?;
            FreeRtos::delay_ms(1000u32); // Wait for longer, since this will just refresh the screen

            if global_state.gpio_btn_boot.is_low() {
                // If the BOOT button is pressed, we will exit setup mode
                setup_mode = false;
                // Blink twice to confirm
                global_state.blink_led.set_high()?;
                FreeRtos::delay_ms(100u32);
                global_state.blink_led.set_low()?;
                FreeRtos::delay_ms(100u32);
                global_state.blink_led.set_high()?;
                FreeRtos::delay_ms(100u32);
                global_state.blink_led.set_low()?;
                FreeRtos::delay_ms(500u32);
                continue;
            }
        } else {
            // If the BOOT button is pressed, we will enter setup mode
            if global_state.gpio_btn_boot.is_low() {
                setup_mode = true;
                continue;
            } else {
                log::info!("Normal mode (setup={})", setup_mode);
            }

            // Tiny blink of LED if normal mode and wifi is connected
            if global_state.wifi.is_connected()? {
                wifi_disconnected_count = 0;
                global_state.blink_led.set_level(high_level)?;
            } else {
                wifi_disconnected_count += 1;
                if wifi_disconnected_count < 10 && wifi_disconnected_count % 10 == 0 {
                    // If we are disconnected for more than 5 iterations, we will issue .connect() again
                    global_state.wifi.connect()?;
                } else if wifi_disconnected_count >= 20 {
                    // If we are disconnected for more than 20 seconds, we will enter setup mode
                    log::info!("Entering setup mode due to no Wi-Fi connection");
                    setup_mode = true;
                    continue;
                }
            }

            display_handler.init(Brightness::DIM);
            let amps = amps::read_amps(
                global_state.adc_driver_mut().unwrap().borrow_mut(),
                global_state.adc_chan_driver_mut().unwrap().borrow_mut(),
            )
            .unwrap();
            {
                let guard = global_state.adc_value.try_lock();
                match guard {
                    Ok(mut guard) => *guard = amps,
                    Err(_) => log::warn!("ADC value is locked"),
                }
            };

            log::info!("Amps: {:.5}A ; {:.5}W", amps, AC_VOLTS * amps);
            display_handler.run(|d| d.set_position(0, 0));
            display_handler.run(|d| write!(d, "{:.5}A    \n{:.5}W    \n", amps, AC_VOLTS * amps));

            if let Ok(wifi) = global_state.wifi.try_lock() {
                if wifi.is_connected()? {
                    let ip = wifi::get_client_ip(&wifi)?;
                    display_handler.run(|d| write!(d, "{}\n", ip));

                    // Send via webhook
                    log::info!("Webhook: {:?}", webhook_url);
                    if webhook_url.is_empty() {
                        display_handler.run(|d| write!(d, "NO WEBHOOK"));
                    } else {
                        display_handler.run(|d| write!(d, "SENDING...  "));
                        let _ = wifi::send_webhook(&webhook_url, &wifi, amps, AC_VOLTS * amps);

                        display_handler.run(|d| {
                            let _ = d.set_column(80);
                            write!(d, "OK")
                        });
                    }
                } else {
                    display_handler.run(|d| write!(d, "CONNECTING..."));
                }
            }
        }

        // Sleep 1000ms
        FreeRtos::delay_ms(100u32);
        global_state.blink_led.set_low()?;
        FreeRtos::delay_ms(900u32);
    }
}
