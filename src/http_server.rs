use esp_idf_svc::{
    http::server::{Configuration, EspHttpServer},
    nvs,
    sys::EspError,
};
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

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

#[inline(always)]
pub fn configure_setup_http_server<'a>() -> Result<EspHttpServer<'a>, EspError> {
    let server_config = Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");
    server.fn_handler(
        "/",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            let mut server_msg = String::new();
            write!(
                server_msg,
                "<!DOCTYPE html>
        <html><head><title>Coarse watt-o-meter</title></head>
        <body>
        <form action=\"/save\" method=\"post\">
        <label for=\"wifi_ssid\">Wi-Fi SSID:</label><br>
        <input type=\"text\" id=\"wifi_ssid\" name=\"wifi_ssid\"><br>
        <label for=\"wifi_psk\">Wi-Fi Password:</label><br>
        <input type=\"password\" id=\"wifi_psk\" name=\"wifi_psk\"><br><br>
        <input type=\"submit\" value=\"Submit\">
        </body></html>"
            )
            .unwrap();
            req.into_response(200, Some("OK"), &[("Content-Type", "text/html")])?
                .write(server_msg.as_bytes())?;
            Ok(())
        },
    )?;

    server.fn_handler(
        "/save",
        esp_idf_svc::http::Method::Post,
        |mut req| -> Result<(), esp_idf_svc::io::EspIOError> {
            // Check that we have received wifi_ssid and wifi_psk as form data
            let mut buf = [0u8; 128];
            let read_bytes = req.read(&mut buf)?;
            let form_data = std::str::from_utf8(&buf).unwrap();
            let mut wifi_ssid = String::new();
            let mut wifi_psk = String::new();
            // Form data is in the format "wifi_ssid=SSID&wifi_psk=PSK\0\0..."
            for (key, value) in form_data.split('&').map(split_urlencoded_kv) {
                match key {
                    "wifi_ssid" => wifi_ssid = value,
                    "wifi_psk" => wifi_psk = value,
                    _ => (),
                }
            }

            log::info!("Received {} bytes.\nBody: {:?}", read_bytes, form_data);

            // Check that we have received both values
            if wifi_ssid.is_empty() || wifi_psk.is_empty() {
                req.into_response(400, Some("Bad Request"), &[("Content-Type", "text/plain")])?
                    .write("Missing Wi-Fi SSID or Password".as_bytes())?;
                return Ok(());
            } else {
                log::info!(
                    "Received Wi-Fi SSID: {:?}, Password: {:?}",
                    wifi_ssid,
                    wifi_psk
                );

                // Send the response before restarting the device!
                req.into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                    .write("Saved Wi-Fi credentials and restarting system".as_bytes())?;

                // Save the values to NVS
                let nvs = nvs::EspNvsPartition::<nvs::NvsDefault>::take()?;
                let mut default_partition = nvs::EspNvs::new(nvs.clone(), "ssaa", true)?;
                default_partition.set_str("wifi_ssid", &wifi_ssid)?;
                default_partition.set_str("wifi_psk", &wifi_psk)?;

                // Reboot the device
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
        },
    )?;

    Ok(server)
}

#[inline(always)]
pub fn configure_http_server<'a>(
    expose_value: &'a Arc<Mutex<f32>>,
) -> Result<EspHttpServer<'a>, EspError> {
    // // Start Http Server
    let server_config = Configuration::default();
    let mut server = EspHttpServer::new(&server_config).expect("Failed to create server");
    server
        .fn_handler(
            "/",
            esp_idf_svc::http::Method::Get,
            |req| -> Result<(), esp_idf_svc::io::EspIOError> {
                log::info!("Got request");
                let mut server_msg = String::new();
                let mutex_handle = expose_value.lock().unwrap();
                let amps: f32 = *mutex_handle;
                write!(server_msg, "<!DOCTYPE html><html><head><title>Coarse watt-o-meter</title></head><body><a href=\"/amps\">Amps: {:.5}A</a><br /><a href=\"/watts\">{:.5}W</a></body></html>", amps, AC_VOLTS * amps).unwrap();
                req.into_response(
                    200,
                    Some("OK"),
                    &[("Content-Type", "text/html")],
                )?.write(server_msg.as_bytes())?;

                Ok(())
            },
        )?;

    server.fn_handler(
        "/amps",
        esp_idf_svc::http::Method::Get,
        |req| -> Result<(), esp_idf_svc::io::EspIOError> {
            let mut server_msg = String::new();
            let mutex_handle = expose_value.lock().unwrap();
            let amps: f32 = *mutex_handle;
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
            let mutex_handle = expose_value.lock().unwrap();
            let amps: f32 = *mutex_handle;
            write!(server_msg, "{:.12}", AC_VOLTS * amps).unwrap();
            req.into_response(200, Some("OK"), &[("Content-Type", "text/plain")])?
                .write(server_msg.as_bytes())?;

            Ok(())
        },
    )?;

    Ok(server)
}
