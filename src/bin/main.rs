#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::Timer;

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::DriveMode;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::ledc::{
    LSGlobalClkSource, Ledc, LowSpeed, channel, channel::ChannelIFace, timer, timer::TimerIFace,
};
use esp_hal::timer::OneShotTimer;
use esp_hal::timer::timg::TimerGroup;

use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::peripherals::{self, ADC1, GPIO12};
use esp_println::println;

use esp_hal::peripherals::GPIO7;
use esp_hal::spi::{
    Mode,
    master::{Config, Spi},
};
use esp_hal::time::Rate;
#[allow(unused_imports)]
use esp_radio::ble::controller::BleConnector;
use log::{Level as LogLevel, error, info, log};

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use embedded_hal_bus::spi::ExclusiveDevice;
use mipidsi::interface::SpiInterface;
use mipidsi::{Builder, models::ST7789, options::ColorInversion};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[embassy_executor::task]
async fn adc_read(adc1: ADC1<'static>, adc_pin: GPIO7<'static>) {
    // takes ownership of the pin

    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin(adc_pin, Attenuation::_11dB);
    let mut adc1 = Adc::new(adc1, adc1_config);

    let mut value;

    loop {
        let mut total: u32 = 0;

        for _ in 0..32 {
            value = match adc1.read_oneshot(&mut pin) {
                Ok(val) => val,
                Err(_) => 0,
            };

            total += value as u32;
        }

        let avg = total / 32;
        println!("{}", avg);
        Timer::after_millis(10).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32s3 -o unstable-hal -o alloc -o wifi -o ble-bleps -o embassy -o log

    esp_println::logger::init_logger_from_env();

    //configuration and initialisation of built in hardware
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // COEX needs more RAM - so we've added some more
    esp_alloc::heap_allocator!(size: 64 * 1024);

    //embassy needs a hardware timer to schedule tasks
    //it also uses a software interrupt to switch between tasks, the interrupt wakes it up.
    //embassy is like axum or tokio.
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    ////initialise the wifi peripheral
    // let (mut _wifi_controller, _interfaces) =
    //     esp_radio::wifi::new(peripherals.WIFI, Default::default())
    //         .expect("Failed to initialize Wi-Fi controller");
    // let _connector = BleConnector::new(peripherals.BT, Default::default());

    // TODO: Spawn some tasks
    let task_spawner = spawner;

    //pin definitions
    //led
    let led_pwm_pin = peripherals.GPIO4;

    //LCD screen
    let lcd_brightness_level_pin =
        Output::new(peripherals.GPIO9, Level::High, OutputConfig::default());
    let lcd_chip_select_pin = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
    let lcd_din_pin = peripherals.GPIO11;
    let lcd_clock_pin = peripherals.GPIO12;
    let lcd_reset_pin = Output::new(peripherals.GPIO13, Level::High, OutputConfig::default());
    let lcd_data_command_pin = Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default());
    let mut delay = Delay::new();

    //initialise peripheral
    let mut spi = Spi::new(
        peripherals.SPI2,
        Config::default()
            .with_frequency(Rate::from_mhz(1))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(lcd_clock_pin)
    .with_mosi(lcd_din_pin);
    //.with_cs(lcd_chip_select_pin);
    //.with_miso(peripherals.GPIO2);

    let spi_device = ExclusiveDevice::new_no_delay(spi, lcd_chip_select_pin).unwrap();
    let mut buffer = [0_u8; 512];
    let di = SpiInterface::new(spi_device, lcd_data_command_pin, &mut buffer);

    let mut display = Builder::new(ST7789, di)
        .display_size(240 as u16, 320 as u16)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .unwrap();

    display.clear(Rgb565::BLUE).unwrap();

    //led PWM
    let led_pwm_pin = Output::new(led_pwm_pin, Level::High, OutputConfig::default());

    //get the singleton
    let mut ledc = Ledc::new(peripherals.LEDC);

    //use the singleton
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let mut lstimer0 = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    lstimer0
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty5Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_khz(24),
        })
        .unwrap();

    let mut channel0 = ledc.channel(channel::Number::Channel0, led_pwm_pin);
    channel0
        .configure(channel::config::Config {
            timer: &lstimer0,
            duty_pct: 10,
            drive_mode: DriveMode::PushPull,
        })
        .unwrap();

    task_spawner.spawn(adc_read(peripherals.ADC1, peripherals.GPIO7).unwrap());

    loop {
        channel0.set_duty(0).unwrap();
        Timer::after_millis(1000).await;
        channel0.set_duty(20).unwrap();
        Timer::after_millis(1000).await;
        channel0.set_duty(40).unwrap();
        Timer::after_millis(1000).await;
        channel0.set_duty(60).unwrap();
        Timer::after_millis(1000).await;
        channel0.set_duty(80).unwrap();
        Timer::after_millis(1000).await;
        channel0.set_duty(100).unwrap();
        Timer::after_millis(1000).await;
    }
}
