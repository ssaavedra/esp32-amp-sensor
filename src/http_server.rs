use esp_idf_svc::{
    http::server::{Configuration, EspHttpServer},
    io::EspIOError,
    nvs,
    sys::EspError,
};
use once_cell::sync::Lazy;
use std::fmt::Write;
use std::sync::{Arc, Mutex};

use crate::AC_VOLTS;

fn percent_decode_str(input: &str) -> String {
    // Implement percent decoding
    let mut output = String::new();
    let mut input = input.as_bytes();
    while !input.is_empty() {
        match input[0] {
            b'%' => {
                if input.len() < 3 {
                    break;
                }
                let hex = u8::from_str_radix(std::str::from_utf8(&input[1..3]).unwrap(), 16)
                    .unwrap_or(b'?');
                output.push(hex as char);
                input = &input[3..];
            }
            b'+' => {
                output.push(' ');
                input = &input[1..];
            }
            _ => {
                output.push(input[0] as char);
                input = &input[1..];
            }
        }
    }

    output
}

#[inline(always)]
fn split_urlencoded_kv<'a>(input: &'a str) -> (&'a str, String) {
    let eq = input.find('=').unwrap_or(input.len());
    let (key, value) = input.split_at(eq);
    let nul = value.find('\0').unwrap_or(value.len());
    // Urldecode %xx
    let value = percent_decode_str(&value[1..nul]);
    let nul = value.find('\0').unwrap_or(value.len());
    (key, value[..nul].to_string())
}

pub(crate) static CURRENT_KNOWN_WIFI_SSID: Lazy<Arc<Mutex<String>>> =
    Lazy::new(|| Arc::new(Mutex::new(String::new())));
pub(crate) static CURRENT_KNOWN_WEBHOOK: Lazy<Arc<Mutex<String>>> =
    Lazy::new(|| Arc::new(Mutex::new(String::new())));

fn render_setup_page<'r>(
    req: esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection<'r>>,
) -> Result<(), EspIOError> {
    let mut server_msg = String::new();
    write!(
        server_msg,
        "<!DOCTYPE html>
        <html><head><title>Coarse watt-o-meter</title></head>
        <body>
        <form action=\"/save\" method=\"post\">
        <label for=\"wifi_ssid\">Wi-Fi SSID:</label><br>
        <input type=\"text\" id=\"wifi_ssid\" name=\"wifi_ssid\" value=\"{}\"><br>
        <label for=\"wifi_psk\">Wi-Fi Password:</label><br>
        <input type=\"password\" id=\"wifi_psk\" name=\"wifi_psk\" value\"{}\"><br><br>
        <label for=\"webhook\">URL to POST with the Amps in {{amps}} (if non-empty)</label><br>
        <input type=\"text\" id=\"webhook\" name=\"webhook\" value=\"{}\"><br><br>
        <input type=\"submit\" value=\"Submit\">
        </body></html>",
        with_locked_value(&CURRENT_KNOWN_WIFI_SSID.clone(), identity),
        "",
        with_locked_value(&CURRENT_KNOWN_WEBHOOK.clone(), identity),
    )
    .unwrap();
    req.into_response(200, Some("OK"), &[("Content-Type", "text/html")])?
        .write(server_msg.as_bytes())?;
    Ok(())
}

fn add_server_setup_handlers<'a>(
    nvs: &'a mut nvs::EspNvs<nvs::NvsDefault>,
    server: &mut EspHttpServer<'a>,
) -> Result<(), EspError> {
    let nvs = Arc::new(Mutex::new(nvs));

    server.fn_handler("/save", esp_idf_svc::http::Method::Get, render_setup_page)?;

    server.fn_handler(
        "/save",
        esp_idf_svc::http::Method::Post,
        move |mut req| -> Result<(), esp_idf_svc::io::EspIOError> {
            // Check that we have received wifi_ssid and wifi_psk as form data
            let mut buf = [0u8; 500];
            let read_bytes = req.read(&mut buf)?;
            let form_data = std::str::from_utf8(&buf).unwrap();
            let mut wifi_ssid = String::new();
            let mut wifi_psk = String::new();
            let mut webhook = String::new();
            // Form data is in the format "wifi_ssid=SSID&wifi_psk=PSK&webhook=...\0\0..."
            for (key, value) in form_data.split('&').map(split_urlencoded_kv) {
                match key {
                    "wifi_ssid" => wifi_ssid = value,
                    "wifi_psk" => wifi_psk = value,
                    "webhook" => webhook = value,
                    _ => (),
                }
            }

            log::info!("Received {} bytes.\nBody: {:?}", read_bytes, form_data);

            // Check that we have received both values
            if wifi_ssid.is_empty() || wifi_psk.is_empty() {
                req.into_response(400, Some("Bad Request"), &[("Content-Type", "text/plain")])?
                    .write("Missing Wi-Fi SSID or Password".as_bytes())?;
                Ok(())
            } else {
                log::info!(
                    "Received Wi-Fi SSID: {:?}, Password: {:?}, Webhook: {:?}",
                    wifi_ssid,
                    wifi_psk,
                    webhook
                );

                let mut nvs = nvs.lock().unwrap();

                // Send the response before restarting the device!
                let written_bytes = req
                    .into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                    .write("Saved Wi-Fi credentials and restarting system".as_bytes())?;

                log::info!("Sent response of {} bytes", written_bytes);

                if let Err(x) = nvs.set_str("wifi_ssid", &wifi_ssid) {
                    log::warn!("Error setting wifi_ssid in NVS: {:?}", x);
                }
                log::info!("Setting Wi-Fi SSID in NVS");
                if let Err(x) = nvs.set_str("wifi_psk", &wifi_psk) {
                    log::warn!("Error setting wifi_psk in NVS: {:?}", x);
                }
                log::info!("Setting Wi-Fi PSK in NVS");
                log::info!("Saved Wi-Fi credentials to NVS");

                if let Err(x) = nvs.set_str("webhook", &webhook) {
                    log::warn!("Error setting webhook in NVS: {:?}", x);
                }
                log::info!("Setting Webhook in NVS");


                // Restart the device
                unsafe {
                    esp_idf_svc::sys::esp_restart();
                }
                #[allow(unreachable_code)]
                Ok(())
            }
        },
    )?;

    server.fn_handler(
        "/restart",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            req.into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                .write("Restarting system".as_bytes())?;

            unsafe {
                esp_idf_svc::sys::esp_restart();
            }
            #[allow(unreachable_code)]
            Ok(())
        },
    )?;

    Ok(())
}

#[inline(always)]
pub fn configure_setup_http_server<'a>(
    nvs: &'a mut nvs::EspNvs<nvs::NvsDefault>,
) -> Result<EspHttpServer<'a>, EspError> {
    let server_config = Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");


    server.fn_handler("/", esp_idf_svc::http::Method::Get, render_setup_page)?;
    add_server_setup_handlers(nvs, &mut server)?;
    Ok(server)
}

trait LockedValue<T> {
    fn with_locked_value<F, R>(self: &Self, f: F) -> R
    where
        F: FnOnce(T) -> R;
}

impl<'a, T: Clone> LockedValue<T> for Arc<Mutex<T>> {
    fn with_locked_value<F, R>(self: &Self, f: F) -> R
    where
        F: FnOnce(T) -> R,
    {
        match self.try_lock() {
            Ok(guard) => f(guard.clone()),
            Err(_) => {
                panic!("Failed to lock mutex")
            }
        }
    }
}

fn with_locked_value<T: Clone, R>(value: &Arc<Mutex<T>>, f: impl FnOnce(T) -> R) -> R {
    value.with_locked_value(f)
}

fn identity<T>(x: T) -> T {
    x
}

#[inline(always)]
pub fn configure_http_server<'a>(
    expose_value: &'a Arc<Mutex<f32>>,
    nvs: &'a mut nvs::EspNvs<nvs::NvsDefault>,
) -> Result<EspHttpServer<'a>, EspError> {
    // // Start Http Server
    let server_config = Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");
    server.fn_handler(
        "/",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            log::info!("Got request");
            let mut server_msg = String::new();
            write!(
                server_msg,
                "<!DOCTYPE html>
                    <html><head><title>Coarse watt-o-meter</title></head>
                    <body><a href=\"/amps\">Amps: {:.5}A</a><br />
                    <a href=\"/watts\">{:.5}W</a></body>
                    </html>",
                with_locked_value(expose_value, identity),
                with_locked_value(expose_value, identity) * AC_VOLTS
            )
            .expect("Failed to write");

            req.into_response(200, Some("OK"), &[("Content-Type", "text/html")])?
                .write(server_msg.as_bytes())?;

            Ok(())
        },
    )?;

    add_server_setup_handlers(nvs, &mut server)?;

    server.fn_handler(
        "/amps",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            let mut server_msg = String::new();
            let amps = with_locked_value(expose_value, identity);
            write!(server_msg, "{:.12}", amps).unwrap();
            req.into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                .write(server_msg.as_bytes())?;

            Ok(())
        },
    )?;

    server.fn_handler(
        "/watts",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            let mut server_msg = String::new();
            let amps = expose_value.with_locked_value(identity);
            let watts = amps * AC_VOLTS;
            write!(server_msg, "{:.12}", watts).unwrap();
            req.into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                .write(server_msg.as_bytes())?;

            Ok(())
        },
    )?;

    Ok(server)
}
