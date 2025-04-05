#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::analog::adc::Adc;
use esp_hal::analog::adc::AdcCalBasic;
use esp_hal::analog::adc::AdcConfig;
use esp_hal::analog::adc::Attenuation;
use esp_hal::peripherals::ADC1;
use esp_hal::{delay::Delay, main};
use esp_println::println;

type AdcCal<'a> = AdcCalBasic<ADC1<'a>>;

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let analog_pin = peripherals.GPIO2;
    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin_with_cal::<_, AdcCal>(analog_pin, Attenuation::_11dB);

    let mut light_pin = Adc::new(peripherals.ADC1, adc1_config);

    // Initialize the Delay peripheral, and use it to toggle the LED state in a
    // loop.
    let delay = Delay::new();

    let mut i = 0;
    loop {
        println!("------- {}", i);
        let pin_value = nb::block!(light_pin.read_oneshot(&mut pin)).unwrap();
        println!("pin_value: {}", pin_value);
        let lux = analog_to_lux(pin_value.into());
        println!("lux: {:.2}", lux);
        delay.delay_millis(500);
        i += 1
    }
}

const ADC_VOLTAGE_RANGE: f64 = 3.3;
const ADC_MAX: f64 = 4095.;

fn analog_to_lux(analog_value: f64) -> f64 {
    let volts = analog_value * ADC_VOLTAGE_RANGE / ADC_MAX;
    let amps = volts / 10_000.0; // resistor has 10k Ohm
    let microamps = amps * 1_000_000.;
    let lux = microamps * 2.0;
    return lux;
}
