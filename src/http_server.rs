use esp_idf_svc::{
    http::server::{Configuration, EspHttpServer},
    sys::EspError,
};
use std::{
    fmt::Write,
    sync::{Arc, Mutex},
};

use crate::AC_VOLTS;


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
