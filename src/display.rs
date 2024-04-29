use esp_idf_svc::hal::gpio;
use esp_idf_svc::hal::i2c;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::prelude::*;
use ssd1306::mode::DisplayConfig;
use ssd1306::mode::TerminalDisplaySize;
use ssd1306::mode::TerminalMode;
use ssd1306::prelude::Brightness;
use ssd1306::prelude::WriteOnlyDataCommand;
use ssd1306::I2CDisplayInterface;
use ssd1306::Ssd1306;

pub struct DisplayHandler<DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize> {
    pub display: Ssd1306<DI, SIZE, TerminalMode>,
    pub available: bool,
}

impl<DI, SIZE> DisplayHandler<DI, SIZE>
where
    DI: WriteOnlyDataCommand,
    SIZE: TerminalDisplaySize,
{
    pub fn new(display: Ssd1306<DI, SIZE, TerminalMode>) -> Self {
        DisplayHandler {
            display,
            available: false,
        }
    }

    // Run a FnOnce closure on the display, if it is available
    // Set as unavailable if the closure panics
    #[inline(always)]
    pub fn run<E: std::fmt::Debug>(
        &mut self,
        f: impl FnOnce(&mut Ssd1306<DI, SIZE, TerminalMode>) -> Result<(), E>,
    ) {
        if self.available {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut self.display)));
            if result.is_err() {
                self.available = false;
                // Log error
                log::info!("Panic: {:?}", result.err());
            } else if let Ok(inner) = result {
                if inner.is_err() {
                    self.available = false;
                    // Log error
                    log::info!("Display error: {:?}", inner.err());
                }
            }
        }
    }
}

impl<DI: WriteOnlyDataCommand, SIZE: TerminalDisplaySize> Drop for DisplayHandler<DI, SIZE> {
    fn drop(&mut self) {
        self.run(|d| d.clear());
    }
}

impl<DI, SIZE> DisplayHandler<DI, SIZE>
where
    DI: WriteOnlyDataCommand,
    SIZE: TerminalDisplaySize,
{
    #[inline(always)]
    pub fn init(&mut self, brightness: Brightness) {
        if self.available {
            return;
        }

        if self.display.init().is_ok() {
            self.available = true;
            self.run(|d| d.clear());
            self.run(|d| d.set_brightness(brightness));
        }
    }
}

pub fn init_display_i2c<'a, I2C: i2c::I2c, SIZE: TerminalDisplaySize>(
    sda: impl Peripheral<P = impl gpio::InputPin + gpio::OutputPin> + 'a,
    scl: impl Peripheral<P = impl gpio::InputPin + gpio::OutputPin> + 'a,
    i2c: impl Peripheral<P = I2C> + 'a,
    size: SIZE,
) -> Result<DisplayHandler<ssd1306::prelude::I2CInterface<i2c::I2cDriver<'a>>, SIZE>, esp_idf_svc::sys::EspError> {
    // Display
    let i2c_config = i2c::I2cConfig::new().baudrate(100.kHz().into());
    let i2c = i2c::I2cDriver::new(i2c, sda, scl, &i2c_config)?;
    let interface = I2CDisplayInterface::new(i2c);
    let mut display_handler = DisplayHandler::new(
        Ssd1306::new(interface, size, ssd1306::rotation::DisplayRotation::Rotate0).into_terminal_mode(),
    );
    display_handler.init(Brightness::DIM);
    Ok(display_handler)
}
