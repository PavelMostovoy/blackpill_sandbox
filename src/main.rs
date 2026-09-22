#![no_std]
#![no_main]

use core::fmt::Write as _;

use defmt::info;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::peripherals;
use embassy_stm32::rcc::{AHBPrescaler, APBPrescaler, Hse, HseMode, Pll, PllMul, PllPDiv, PllPreDiv, PllQDiv, PllSource, Sysclk};
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::Driver as UsbDriver;
use embassy_stm32::{bind_interrupts, usb, Config};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::{Builder, Config as UsbConfig};
use embedded_hal_bus::spi::ExclusiveDevice;
use panic_probe as _;
use w25q32jv::W25q32jv;

bind_interrupts!(struct Irqs {
    OTG_FS => usb::InterruptHandler<peripherals::USB_OTG_FS>;
});

/// Fixed-size buffer for building a report string with `write!` — avoids
/// pulling in `heapless` just for this.
struct Cursor<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for Cursor<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.len + bytes.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

/// Stand-in for the flash footprint's HOLD#/WP# lines, which aren't wired
/// to any GPIO on this board (they're tied high on the PCB) — `w25q32jv`
/// still wants pin objects to drive, so these just no-op.
struct NoPin;

impl embedded_hal::digital::ErrorType for NoPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::OutputPin for NoPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Blackpill HSE crystal is 25 MHz. PLL: /25 -> 1 MHz VCO input, x384 -> 384 MHz VCO,
    // /4 (PLLP) -> 96 MHz SYSCLK, /8 (PLLQ) -> 48 MHz for USB OTG FS. (Dropped from the
    // previous x400/100MHz config: that VCO doesn't divide down to an exact 48MHz for USB.)
    // If clocks look wrong, check the crystal's printed part marking — some clone boards
    // ship 8 MHz instead.
    let mut config = Config::default();
    config.rcc.hse = Some(Hse {
        freq: Hertz(25_000_000),
        mode: HseMode::Oscillator,
    });
    config.rcc.pll_src = PllSource::HSE;
    config.rcc.pll = Some(Pll {
        prediv: PllPreDiv::DIV25,
        mul: PllMul::from_bits(384),
        divp: Some(PllPDiv::DIV4),
        divq: Some(PllQDiv::DIV8),
        divr: None,
    });
    config.rcc.sys = Sysclk::PLL1_P;
    config.rcc.ahb_pre = AHBPrescaler::DIV1;
    config.rcc.apb1_pre = APBPrescaler::DIV2;
    config.rcc.apb2_pre = APBPrescaler::DIV1;

    let p = embassy_stm32::init(config);

    info!("Hello from the Blackpill (STM32F411CEU6)!");

    let clocks = embassy_stm32::rcc::clocks(&p.RCC);
    info!("SYSCLK: {} Hz", clocks.sys);

    // On-board SPI flash footprint (WeAct Blackpill V2.1+): SPI1 with
    // CS=PA4, SCK=PA5, MISO=PA6, MOSI=PA7. (V2.0 boards briefly routed
    // MISO to PB4 instead — swap if detection fails below.)
    let flash_cs = Output::new(p.PA4, Level::High, Speed::VeryHigh);
    let spi = Spi::new_blocking(p.SPI1, p.PA5, p.PA7, p.PA6, SpiConfig::default());
    // Infallible: NoDelay only panics if a driver issues Operation::DelayNs,
    // which w25q32jv's blocking API never does.
    let spi_device = ExclusiveDevice::new_no_delay(spi, flash_cs).unwrap();
    let mut flash = W25q32jv::new(spi_device, NoPin, NoPin).unwrap();

    // Read the chip's factory-unique 64-bit ID as a presence check.
    let device_id = flash.device_id().unwrap_or([0u8; 8]);
    info!("SPI flash unique ID: {:02x}", device_id);
    // All-0x00 or all-0xFF means no chip responded — either the wiring is
    // wrong or this board doesn't have the flash footprint populated.
    let flash_detected = device_id != [0u8; 8] && device_id != [0xFFu8; 8];
    info!("Flash detected: {}", flash_detected);

    // Write/read test: erase the first 4KB sector, program a known pattern,
    // then read it back and compare. Only meaningful if a chip responded.
    let mut write_read_ok = false;
    if flash_detected {
        const TEST_ADDR: u32 = 0x000_000;
        let pattern: [u8; 16] = *b"BLACKPILL_TEST!!";

        let erased = flash.erase_sector(TEST_ADDR).is_ok();
        let written = erased && flash.write_blocking(TEST_ADDR, &pattern).is_ok();

        let mut readback = [0u8; 16];
        let read_ok = written && flash.read(TEST_ADDR, &mut readback).is_ok();

        write_read_ok = read_ok && readback == pattern;
        info!("Flash write/read test passed: {}", write_read_ok);
    }

    // On-board LED on PC13 is active-low. Blink rate still doubles as a
    // no-terminal-needed status indicator:
    //   fast (50ms)   = write/read test passed
    //   medium (300ms) = chip detected but write/read test failed
    //   slow (1000ms)  = no chip detected at all
    let mut led = Output::new(p.PC13, Level::High, Speed::Low);
    let blink_period_ms: u64 = if !flash_detected {
        1000
    } else if !write_read_ok {
        300
    } else {
        50
    };

    // Build the boot report once, as text, to re-send over USB CDC-ACM
    // every time a terminal is connected (or stays connected).
    let mut report_buf = [0u8; 256];
    let mut report = Cursor { buf: &mut report_buf, len: 0 };
    let _ = write!(
        report,
        "Blackpill SPI flash test\r\n\
         Unique ID: {:02x?}\r\n\
         Flash detected: {}\r\n\
         Write/read test passed: {}\r\n\
         ---\r\n",
        device_id, flash_detected, write_read_ok
    );
    let report = &report.buf[..report.len];

    // USB CDC-ACM (virtual COM port) on the same USB-C connector used for
    // DFU flashing — reachable with any serial terminal (e.g. `screen`,
    // `minicom`, `picocom`) once the board enumerates, no ST-Link needed.
    let mut ep_out_buffer = [0u8; 256];
    let usb_driver = UsbDriver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        &mut ep_out_buffer,
        Default::default(),
    );

    let mut usb_config = UsbConfig::new(0xc0de, 0xcafe);
    usb_config.manufacturer = Some("blackpill_sandbox");
    usb_config.product = Some("Flash test log");
    usb_config.serial_number = Some("1");
    // Required for Windows to recognize the device as a serial port.
    usb_config.device_class = 0xEF;
    usb_config.device_sub_class = 0x02;
    usb_config.device_protocol = 0x01;
    usb_config.composite_with_iads = true;

    let mut config_descriptor = [0u8; 256];
    let mut bos_descriptor = [0u8; 256];
    let mut control_buf = [0u8; 64];
    let mut cdc_state = CdcState::new();

    let mut builder = Builder::new(
        usb_driver,
        usb_config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [],
        &mut control_buf,
    );

    let mut class = CdcAcmClass::new(&mut builder, &mut cdc_state, 64);
    let mut usb_device = builder.build();

    let usb_fut = usb_device.run();

    let led_fut = async {
        loop {
            led.toggle();
            Timer::after_millis(blink_period_ms).await;
        }
    };

    let log_fut = async {
        loop {
            class.wait_connection().await;
            for chunk in report.chunks(63) {
                let _ = class.write_packet(chunk).await;
            }
            Timer::after_millis(2000).await;
        }
    };

    join3(usb_fut, led_fut, log_fut).await;
}
