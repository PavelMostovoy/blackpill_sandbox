# blackpill_sandbox

Rust/Embassy firmware for the [WeAct STM32F4x1Cx "Blackpill"](https://github.com/weactstudio/weactstudio.ministm32f4x1) board (STM32F411CEU6, 512KB flash). Blinks the on-board LED (`PC13`), with `defmt` logging over RTT.

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
