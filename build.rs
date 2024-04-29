#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,

    #[default("")]
    wifi_psk: &'static str,

    #[default("")]
    hostname: &'static str,
}

fn main() {
    // Check that cfg.toml exists
    if !std::path::Path::new("cfg.toml").exists() {
        panic!("You need to create a `cfg.toml` file with your Wi-Fi credentials");
    }

    let app_config = CONFIG;
    if app_config.wifi_ssid == "" || app_config.wifi_psk == "" {
        panic!("You need to set your Wi-Fi credentials in `cfg.toml`");
    }

    embuild::espidf::sysenv::output();
}
