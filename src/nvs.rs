use esp_idf_svc::nvs;
pub use esp_idf_svc::nvs::*;
use esp_idf_svc::sys::EspError;

pub fn read_str_from_nvs<T: NvsPartitionId>(nvs: &nvs::EspNvs<T>, key: &str) -> Result<String, EspError> {
    let mut buf = [0u8; 128];
    match nvs.get_str(key, &mut buf) {
        Ok(_) => {
            let nul = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let buf = String::from_utf8_lossy(&buf[..nul]);
            Ok(buf.to_string())
        }
        Err(e) => {
            log::info!("Error reading {} from NVS: {:?}", key, e);
            Err(e)
        }
    }
}

pub fn read_str_from_nvs_or_default<T: NvsPartitionId>(nvs: &nvs::EspNvs<T>, key: &str, default: &str) -> String {
    match read_str_from_nvs(nvs, key) {
        Ok(val) => val,
        Err(_) => default.to_string(),
    }
}