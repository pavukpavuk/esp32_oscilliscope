#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_sync::channel::{Channel, Sender, Receiver};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use core::future::pending;
use embassy_time::{Duration, Timer};

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::DriveMode;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::ledc::{
    LSGlobalClkSource, Ledc, LowSpeed, channel, channel::ChannelIFace, timer, timer::TimerIFace,
};

use esp_hal::timer::timg::TimerGroup;

use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::peripherals::{ADC1, GPIO4, GPIO7, LEDC};
use esp_println::println;

use esp_hal::spi::{
    Mode,
    master::{Config, Spi},
};
use esp_hal::time::Rate;
#[allow(unused_imports)]
use esp_radio::ble::controller::BleConnector;
use log::{error, info};

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use embedded_hal_bus::spi::ExclusiveDevice;

use mipidsi::{
    Builder,
    models::ST7789, 
    options::ColorInversion,
    interface::SpiInterface,
    options::Orientation,
    options::Rotation
};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

use heapless::Vec;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

const DISPLAY_WIDTH: u32 = 320;
const DISPLAY_HEIGHT: u32 = 240;

const SAMPLE_BUFFER_SIZE: usize = DISPLAY_WIDTH as usize;
//static ADC_CHANNEL: Channel<CriticalSectionRawMutex, [u16; SAMPLE_BUFFER_SIZE], 2> = Channel::new();
static ADC_CHANNEL: Channel<CriticalSectionRawMutex, u16, 1> = Channel::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[embassy_executor::task]
async fn adc_read(adc1: ADC1<'static>, adc_pin: GPIO7<'static>, sender: Sender<'static, CriticalSectionRawMutex, u16, 1>) {
    // takes ownership of the pin

    let mut adc1_config = AdcConfig::new();
    let mut pin = adc1_config.enable_pin(adc_pin, Attenuation::_11dB);
    let mut adc1 = Adc::new(adc1, adc1_config);

    let mut value;

    //320px = 4095 adc output
    //0px = 0 adc output
    const GRAPH_Y_HEIGHT: u32 = 240;
    let mut samples = [0u16; SAMPLE_BUFFER_SIZE as usize];
    
    
    let mut graph_y_points = [0u16; GRAPH_Y_HEIGHT as usize];


    //println!("length: {}", samples.len());

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
        let normalised_adc: f32 = 1.0 / avg as f32;
        let graph_y_point: f32 = normalised_adc * GRAPH_Y_HEIGHT as f32;
        let graph_y_point = graph_y_point * GRAPH_Y_HEIGHT as f32;
        // match graph_y_points.push(graph_y_point) {
        //     Ok(_) => {},
        //     Err(item) => {
        //         //send all samples to drawing function
        //         //clear samples

        //     }
        // }
        // println!("{}", graph_y_point);
        sender.send(graph_y_point as u16).await;
        Timer::after_millis(10).await;
    }
}

#[embassy_executor::task]
async fn fade_led(led_pwm_pin: GPIO4<'static>, ledc: LEDC<'static>) {
    //led PWM
    let led_pwm_pin = Output::new(led_pwm_pin, Level::High, OutputConfig::default());

    //get the singleton
    let mut ledc = Ledc::new(ledc);

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

    //LCD screen
    let lcd_brightness_level_pin =
        Output::new(peripherals.GPIO9, Level::High, OutputConfig::default());
    let lcd_chip_select_pin = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
    let lcd_din_pin = peripherals.GPIO11;
    let lcd_clock_pin = peripherals.GPIO12;
    let _lcd_reset_pin = Output::new(peripherals.GPIO13, Level::High, OutputConfig::default());
    let lcd_data_command_pin = Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default());
    let mut delay = Delay::new();

    //initialise peripheral
    let spi = Spi::new(
        peripherals.SPI2,
        Config::default()
            .with_frequency(Rate::from_mhz(8))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(lcd_clock_pin)
    .with_mosi(lcd_din_pin);

    let spi_device = ExclusiveDevice::new_no_delay(spi, lcd_chip_select_pin).unwrap();
    let mut buffer = [0_u8; 512];
    let di = SpiInterface::new(spi_device, lcd_data_command_pin, &mut buffer);
    

    

    let mut display = Builder::new(ST7789, di)
        .display_size(DISPLAY_HEIGHT as u16, DISPLAY_WIDTH as u16)
        .invert_colors(ColorInversion::Inverted)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .init(&mut delay)
        .unwrap();
    display.clear(Rgb565::BLACK).unwrap();

    task_spawner.spawn(fade_led(peripherals.GPIO4, peripherals.LEDC).unwrap());

    let sender = ADC_CHANNEL.sender();
    let receiver = ADC_CHANNEL.receiver();
    task_spawner.spawn(adc_read(peripherals.ADC1, peripherals.GPIO7, sender).unwrap()); 
 

    let mut sample_x = DISPLAY_WIDTH/2;
    let sample_y = DISPLAY_HEIGHT/2;


    loop {
       
        
        let sample_y: u16 = receiver.receive().await;
        display.set_pixel(sample_x as u16,sample_y as u16,Rgb565::GREEN );
        Timer::after(Duration::from_millis(3)).await;
        let sample_x_prev = sample_x; 
        let sample_y_prev = sample_y;

        println!("{}", sample_y);
        if sample_x > DISPLAY_WIDTH {
            sample_x = 0;
        }

        display.set_pixel(sample_x_prev as u16,sample_y_prev as u16,Rgb565::BLACK );
    }
}
