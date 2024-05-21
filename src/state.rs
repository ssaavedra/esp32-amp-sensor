use std::sync::{Arc, Mutex, MutexGuard};

use esp_idf_svc::hal::{adc::attenuation, *};
use ssd1306::{mode::TerminalDisplaySize, prelude::WriteOnlyDataCommand};

use crate::display;

pub trait AsGlobalState<'a, DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize> {
    fn as_global_state(&self) -> &GlobalState<'a, DI, SIZE>;
}

pub struct GlobalState<'a, DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize> {
    pub wifi: Arc<Mutex<esp_idf_svc::wifi::EspWifi<'a>>>,
    pub setup_mode: Arc<Mutex<bool>>,
    pub adc_value: Arc<Mutex<f32>>,
    pub display_handler: Arc<Mutex<display::DisplayHandler<DI, SIZE>>>,
    pub webhook_url: Arc<Mutex<String>>,
    pub adc_driver: Arc<Mutex<adc::AdcDriver<'a, adc::ADC1>>>,
    pub adc_chan_driver: Arc<Mutex<adc::AdcChannelDriver<'a, { attenuation::DB_2_5 }, gpio::Gpio35>>>,
    pub gpio_btn_boot: gpio::PinDriver<'a, gpio::Gpio0, gpio::Input>,
    /**
     * Quiet mode pin. If set to low, do not blink the LED
     * If set to high, blink the LED to indicate that the device is running
     */
    pub quiet_mode_pin: gpio::PinDriver<'a, gpio::Gpio34, gpio::Input>,
    pub blink_led: Arc<Mutex<gpio::PinDriver<'a, gpio::Gpio2, gpio::Output>>>,
}

impl<'a, DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize> AsGlobalState<'a, DI, SIZE> for GlobalState<'a, DI, SIZE> {
    fn as_global_state(&self) -> &GlobalState<'a, DI, SIZE> {
        self
    }
}

impl<'a, DI, SIZE> GlobalState<'a, DI, SIZE> where DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize {
    pub fn adc_driver_mut(&self) -> Result<MutexGuard<adc::AdcDriver<'a, adc::ADC1>>, sys::EspError> {
        self.adc_driver.lock().map_err(|_| sys::EspError::from_non_zero(
            core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_INVALID_STATE).unwrap(),
        ))
    }

    pub fn adc_chan_driver_mut(&self) -> Result<MutexGuard<adc::AdcChannelDriver<'a, { attenuation::DB_2_5 }, gpio::Gpio35>>, sys::EspError> {
        self.adc_chan_driver.lock().map_err(|_| sys::EspError::from_non_zero(
            core::num::NonZeroI32::new(esp_idf_svc::sys::ESP_ERR_INVALID_STATE).unwrap(),
        ))
    }
}

pub trait PinDriverOutputArcExt<PIN: gpio::Pin> {
    fn set_high(&self) -> Result<(), sys::EspError>;
    fn set_low(&self) -> Result<(), sys::EspError>;
    fn set_level(&self, level: gpio::Level) -> Result<(), sys::EspError>;
    fn with_locked_value<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut gpio::PinDriver<'_, PIN, gpio::Output>) -> R;
}

impl<Pin> PinDriverOutputArcExt<Pin> for Arc<Mutex<gpio::PinDriver<'_, Pin, gpio::Output>>>
where Pin : gpio::Pin
{
    fn set_high(&self) -> Result<(), sys::EspError> {
        self.with_locked_value(|pin| pin.set_high())
    }

    fn set_low(&self) -> Result<(), sys::EspError> {
        self.with_locked_value(|pin| pin.set_low())
    }

    fn set_level(&self, level: gpio::Level) -> Result<(), sys::EspError> {
        self.with_locked_value(|pin| pin.set_level(level))
    }

    fn with_locked_value<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut gpio::PinDriver<'_, Pin, gpio::Output>) -> R {
        match self.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(_) => {
                panic!("Failed to lock mutex")
            }
        }
    }
}

