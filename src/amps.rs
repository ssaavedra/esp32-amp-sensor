use esp_idf_svc::hal::adc;
use esp_idf_svc::hal::gpio::ADCPin;
use esp_idf_svc::sys::{adc_atten_t, EspError};
use esp_idf_svc::{
    hal::{
        adc::{attenuation, AdcChannelDriver, AdcDriver},
    },
};
use std::time::SystemTime;



// SCT-013-030 has a 1V output for 30A
// 30A = 1V
// 1A = 0.0333V
const FACTOR: f32 = 1. / 0.0333;

pub fn read_amps<const A: adc_atten_t, T, ADC: adc::Adc>(
    driver: &mut AdcDriver<ADC>,
    chan_driver: &mut AdcChannelDriver<A, T>,
) -> Result<f32, EspError> where
T: ADCPin<Adc = ADC>,
    {
    // Since we are working with 50Hz AC, we have a cycle every 20ms
    // We will sample for 500ms to get 25 samples

    let mut count: usize = 0;
    let start = SystemTime::now();
    let mut end = SystemTime::now();
    let mut highest_peak = 0.0f32;

    while end.duration_since(start).unwrap().as_millis() < 100 {
        let val = driver.read(chan_driver)?;
        highest_peak = highest_peak.max(val as f32).max(40f32);
        count += 1;
        // FreeRtos::delay_ms(1u32);
        end = SystemTime::now();
    }

    log::info!("Read {} samples", count);
    log::info!("Highest peak: {}", highest_peak);
    let peak = float_remap(highest_peak, 40.0, 1250.0, 0.0, 1.250);
    log::info!("Peak: {}V", peak);

    Ok(peak * FACTOR)
}

fn float_remap(value: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> f32 {
    return (value - in_min) * (out_max - out_min) / (in_max - in_min) + out_min;
}
