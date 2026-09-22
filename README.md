# blackpill_sandbox

Rust/Embassy firmware for the [WeAct STM32F4x1Cx "Blackpill"](https://github.com/weactstudio/weactstudio.ministm32f4x1) board (STM32F411CEU6, 512KB flash). Blinks the on-board LED (`PC13`), tests a W25Q32JV SPI NOR flash chip soldered onto the board's on-board flash footprint (via the [`w25q32jv`](https://crates.io/crates/w25q32jv) driver crate), and reports the results both over `defmt`/RTT and as a USB CDC-ACM virtual COM port — no debug probe needed to see the results.

See `.claude/skills/blackpill-firmware/SKILL.md` for toolchain/hardware details and `reference/board-pinout.md` for the full pinout.

## Prerequisites

- Rust target: `rustup target add thumbv7em-none-eabi`
- [`probe-rs`](https://probe.rs/) CLI, for the ST-Link path: `cargo install probe-rs-tools`
- [`dfu-util`](https://dfu-util.sourceforge.net/), for the USB-DFU path: `brew install dfu-util`
- `cargo-binutils`, to convert ELF → `.bin` for DFU: `cargo install cargo-binutils` (+ `rustup component add llvm-tools-preview`)

## Build

```sh
cargo build --release
```

## Upload to the board

There are two ways to get firmware onto the Blackpill:

### Option A — ST-Link / SWD (recommended for development)

Gives live `defmt` logs and debugging. Requires an ST-Link probe wired to the board's `PA13`(SWDIO)/`PA14`(SWCLK)/`GND`/`3V3` header.

```sh
cargo run --release
```

This flashes, resets, and streams `defmt` logs live (runner is already configured in `.cargo/config.toml` as `probe-rs run --chip STM32F411CE`).

### Option B — USB DFU (no debug probe needed)

The F411 has a built-in ROM bootloader that shows up as a USB DFU device — flash it straight over the same USB-C cable used for power, no ST-Link required. Downside: no `defmt`/RTT logging this way (that needs a probe attached).

1. **Enter DFU mode**: hold down the `BOOT0` button, tap `NRST` (reset) while still holding `BOOT0`, then release `BOOT0`. The board re-enumerates as a DFU device.
2. **Check it's detected**:
   ```sh
   dfu-util -l
   # look for: Found DFU: [0483:df11] ... "@Internal Flash  /0x08000000/..."
   ```
3. **Convert the build to a raw binary**:
   ```sh
   cargo objcopy --release --bin blackpill_sandbox -- -O binary target/thumbv7em-none-eabi/release/blackpill_sandbox.bin
   ```
4. **Flash it**:
   ```sh
   dfu-util -a 0 -s 0x08000000:leave -D target/thumbv7em-none-eabi/release/blackpill_sandbox.bin
   ```
   `:leave` tells the bootloader to reset into the new firmware immediately after flashing.

Only one flashing method is needed per session — they write to the same flash, so whichever ran last is what's on the board.

## On-board SPI flash test

The WeAct Blackpill has an unpopulated footprint on the back of the board (SOIC-8 pads + a decoupling capacitor) for an optional SPI NOR flash chip. If you've soldered a **W25Q32JV** chip there, this firmware tests it automatically on every boot using the [`w25q32jv`](https://crates.io/crates/w25q32jv) driver crate:

1. Reads the chip's factory-unique 64-bit ID over **SPI1** (`CS=PA4`, `SCK=PA5`, `MISO=PA6`, `MOSI=PA7`) as a presence check.
2. If a chip responds, erases the first 4KB sector, programs a 16-byte test pattern, reads it back, and compares (the crate's `readback-check` feature also double-checks the write internally).

The result is reported three ways at once — pick whichever is easiest to check:

- **USB serial (recommended, no probe needed)** — see "USB serial output" below.
- **`defmt`/RTT**, if flashing via ST-Link (`cargo run --release`) — logs the unique ID bytes and pass/fail as text.
- **On-board `PC13` LED blink rate**, always active as a fallback if you can't get a terminal open:

  | Blink rate | Meaning |
  |---|---|
  | Fast (~50ms) | Chip detected, write/read test passed — flash is working |
  | Medium (~300ms) | Chip detected (valid unique ID) but the write/read test failed |
  | Slow (~1000ms) | No chip detected (unique ID came back all `0x00`/`0xFF`) — check wiring/soldering |

**Note:** some early V2.0 boards route the flash footprint's `MISO` to `PB4` instead of `PA6` — if you get a slow blink but are confident the chip is soldered correctly, try swapping that pin in `src/main.rs`. Also note: the flash footprint's `HOLD#`/`WP#` lines aren't wired to the MCU on this board (tied high on the PCB) — `src/main.rs` passes the `w25q32jv` driver a no-op stand-in pin for both.

## USB serial output (CDC-ACM)

The board also enumerates as a USB virtual COM port on the same USB-C connector used for DFU flashing (separate device from the DFU bootloader — it only shows up once your firmware, not the ROM bootloader, is running). No ST-Link required.

1. Flash the firmware (either upload method above) and let the board boot normally (i.e. **not** in DFU mode).
2. It should enumerate as a serial device, e.g. on macOS: `/dev/cu.usbmodem*`. On Linux: `/dev/ttyACM*`.
3. Open it with any serial terminal (baud rate doesn't matter for USB CDC-ACM):
   ```sh
   cat /dev/cu.usbmodem11      # macOS, read-only
   screen /dev/ttyACM0         # Linux
   ```
4. The board re-sends the boot report (unique ID, flash-detected, write/read pass/fail) every 2 seconds while a terminal is connected.

This is a one-way log stream (board → host only) — USB VID:PID `0xc0de:0xcafe`, defined in `src/main.rs`.
