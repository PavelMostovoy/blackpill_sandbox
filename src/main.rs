#![no_std]
#![no_main]

use defmt::info;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::mode::Blocking;
use embassy_stm32::rcc::{AHBPrescaler, APBPrescaler, Hse, HseMode, Pll, PllMul, PllPDiv, PllPreDiv, PllSource, Sysclk};
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::Config;
use embassy_time::Timer;
use panic_probe as _;

const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS1: u8 = 0x05;
const CMD_SECTOR_ERASE: u8 = 0x20;
const CMD_PAGE_PROGRAM: u8 = 0x02;
const CMD_READ_DATA: u8 = 0x03;

fn write_enable(spi: &mut Spi<'_, Blocking, Master>, cs: &mut Output<'_>) {
    cs.set_low();
    let _ = spi.blocking_write(&[CMD_WRITE_ENABLE]);
    cs.set_high();
}

async fn wait_busy(spi: &mut Spi<'_, Blocking, Master>, cs: &mut Output<'_>) {
    loop {
        let mut status = [CMD_READ_STATUS1, 0u8];
        cs.set_low();
        let _ = spi.blocking_transfer_in_place(&mut status);
        cs.set_high();
        if status[1] & 0x01 == 0 {
            break;
        }
        Timer::after_millis(1).await;
    }
}

async fn erase_sector(spi: &mut Spi<'_, Blocking, Master>, cs: &mut Output<'_>, addr: u32) {
    write_enable(spi, cs);
    cs.set_low();
    let _ = spi.blocking_write(&[CMD_SECTOR_ERASE, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8]);
    cs.set_high();
    wait_busy(spi, cs).await;
}

async fn page_program(spi: &mut Spi<'_, Blocking, Master>, cs: &mut Output<'_>, addr: u32, data: &[u8]) {
    write_enable(spi, cs);
    cs.set_low();
    let _ = spi.blocking_write(&[CMD_PAGE_PROGRAM, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8]);
    let _ = spi.blocking_write(data);
    cs.set_high();
    wait_busy(spi, cs).await;
}

fn read_flash(spi: &mut Spi<'_, Blocking, Master>, cs: &mut Output<'_>, addr: u32, buf: &mut [u8]) {
    cs.set_low();
    let _ = spi.blocking_write(&[CMD_READ_DATA, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8]);
    let _ = spi.blocking_read(buf);
    cs.set_high();
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Blackpill HSE crystal is 25 MHz. PLL: /25 -> 1 MHz VCO input, x400 -> 400 MHz VCO,
    // /4 -> 100 MHz SYSCLK (the STM32F411's maximum). If clocks look wrong, check the
    // crystal's printed part marking — some clone boards ship 8 MHz instead.
    let mut config = Config::default();
    config.rcc.hse = Some(Hse {
        freq: Hertz(25_000_000),
        mode: HseMode::Oscillator,
    });
    config.rcc.pll_src = PllSource::HSE;
    config.rcc.pll = Some(Pll {
        prediv: PllPreDiv::DIV25,
        mul: PllMul::from_bits(400),
        divp: Some(PllPDiv::DIV4),
        divq: None,
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
    // MISO to PB4 instead — swap if the JEDEC ID below reads all 0x00/0xFF.)
    let mut flash_cs = Output::new(p.PA4, Level::High, Speed::VeryHigh);
    let mut spi = Spi::new_blocking(p.SPI1, p.PA5, p.PA7, p.PA6, SpiConfig::default());

    let mut jedec = [0u8; 4];
    jedec[0] = 0x9F; // Read JEDEC ID
    flash_cs.set_low();
    let tx = jedec;
    spi.blocking_transfer(&mut jedec, &tx).ok();
    flash_cs.set_high();
    info!(
        "SPI flash JEDEC ID: manufacturer={=u8:02x} memtype={=u8:02x} capacity={=u8:02x}",
        jedec[1], jedec[2], jedec[3]
    );
    // 0x00 (all-zero) or 0xFF (all-one) manufacturer bytes mean no chip
    // responded — either the wiring is wrong or this board doesn't have the
    // flash footprint populated on this SPI bus.
    let flash_detected = jedec[1] != 0x00 && jedec[1] != 0xFF;
    info!("Flash detected: {}", flash_detected);

    // Write/read test: erase the first 4KB sector, program a known pattern,
    // then read it back and compare. Only meaningful if a chip responded.
    let mut write_read_ok = false;
    if flash_detected {
        const TEST_ADDR: u32 = 0x000_000;
        let pattern: [u8; 16] = *b"BLACKPILL_TEST!!";

        erase_sector(&mut spi, &mut flash_cs, TEST_ADDR).await;
        page_program(&mut spi, &mut flash_cs, TEST_ADDR, &pattern).await;

        let mut readback = [0u8; 16];
        read_flash(&mut spi, &mut flash_cs, TEST_ADDR, &mut readback);

        write_read_ok = readback == pattern;
        info!("Flash write/read test passed: {}", write_read_ok);
    }

    // On-board LED on PC13 is active-low. No ST-Link/RTT log available, so
    // signal the result via blink rate instead:
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

    loop {
        led.toggle();
        Timer::after_millis(blink_period_ms).await;
    }
}
