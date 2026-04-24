# Matter-over-Thread Light (ESP32-C6 / ESP32-H2)

A working Matter-over-Thread On-Off Light example using the Rust embedded ecosystem. Tested with Apple Home on ESP32-C6.

## Prerequisites

1. **Rust nightly** (handled by `rust-toolchain.toml`)
2. **RISC-V GCC toolchain**:
   ```bash
   cargo install espup
   espup install
   export PATH="$HOME/.espressif/tools/riscv32-esp-elf/esp-15.1.0_20250607/riscv32-esp-elf/bin:$PATH"
   ```
3. **CMake toolchain file** (macOS only — prevents `-arch arm64` injection):
   ```bash
   cat > /tmp/riscv32-toolchain.cmake << 'EOF'
   set(CMAKE_SYSTEM_NAME Generic)
   set(CMAKE_SYSTEM_PROCESSOR riscv32)
   set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)
   set(CMAKE_C_COMPILER_WORKS 1)
   set(CMAKE_CXX_COMPILER_WORKS 1)
   set(CMAKE_OSX_ARCHITECTURES "" CACHE STRING "" FORCE)
   set(CMAKE_OSX_DEPLOYMENT_TARGET "" CACHE STRING "" FORCE)
   set(CMAKE_OSX_SYSROOT "" CACHE STRING "" FORCE)
   EOF
   export CMAKE_TOOLCHAIN_FILE=/tmp/riscv32-toolchain.cmake
   ```
4. **espflash**: `cargo install espflash`

## Build & Flash

### ESP32-C6 (recommended, fully working)

```bash
export PATH="$HOME/.espressif/tools/riscv32-esp-elf/esp-15.1.0_20250607/riscv32-esp-elf/bin:$PATH"
export CMAKE_TOOLCHAIN_FILE=/tmp/riscv32-toolchain.cmake

espflash erase-flash
DEFMT_LOG=debug ESP_LOG=debug CARGO_FEATURE_USE_GCC=true \
  cargo espflash flash --no-default-features --features esp32c6 --release --monitor --baud 1500000
```

### ESP32-H2 (commissioning works, post-commission reliability issues)

```bash
export PATH="$HOME/.espressif/tools/riscv32-esp-elf/esp-15.1.0_20250607/riscv32-esp-elf/bin:$PATH"
export CMAKE_TOOLCHAIN_FILE=/tmp/riscv32-toolchain.cmake

espflash erase-flash
DEFMT_LOG=debug ESP_LOG=debug CARGO_FEATURE_USE_GCC=true \
  cargo espflash flash --no-default-features --features esp32h2 --release --monitor --baud 1500000
```

## Commission with Apple Home

1. Open the **Home** app on your iPhone
2. Tap **+** → **Add Accessory**
3. When the device starts, it will print a pairing code like `3840-2020-1234` in the log
4. Use the default test code: **3840-2020-1234** (password 20202021)
5. Apple Home will commission over BLE, then switch to Thread

## What's Included

This example includes fixes across 4 repositories to make Matter-over-Thread work on ESP32:

| Fix | Description |
|-----|-------------|
| **WirelessMgr deadlock** | Device joins Thread after BLE instead of waiting for CommissioningComplete |
| **esp-hal 1.1 upgrade** | New 802.15.4 radio driver eliminates RX queue overflow |
| **RX restart after config** | Receiver restarts after channel changes |
| **ext_addr byte swap** | Fixes unicast frame filtering for esp-radio 0.18 |
| **SRP optimization** | Skips unnecessary removal, adds debounce |
| **Message buffers** | Increased to 256 for large Matter messages |

See [MATTER_THREAD_ESP32C6_FIXES.md](MATTER_THREAD_ESP32C6_FIXES.md) for details.

## Known Issues

- **ESP32-H2**: Commissioning completes but large subscription responses fail with `ChannelAccessFailure`. This is a radio hardware limitation — the H2 can't reliably transmit large 6LoWPAN fragmented packets.
- **Thread detach/reattach**: The device may occasionally detach from Thread and reattach to a different router. This is normal Thread behavior.
