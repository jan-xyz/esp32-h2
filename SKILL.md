---
name: matter-thread-esp32
description: Debugging and developing Matter-over-Thread on ESP32-C6/H2 using the Rust ecosystem (rs-matter, openthread, esp-hal). Use when the user is working on Matter commissioning, Thread networking, ESP32 802.15.4 radio issues, or any combination of these technologies. Covers the full stack from hardware radio drivers through OpenThread to the Matter application layer, including Apple Home / Home Assistant commissioning flows.
license: MIT
metadata:
  author: jan-xyz
  version: "1.0"
---

# Matter-over-Thread ESP32 Debugging Skill

This skill covers debugging and developing Matter-over-Thread devices on ESP32-C6 and ESP32-H2 using the Rust embedded ecosystem.

## Technology Stack

The stack has five layers, bottom to top:

```
┌─────────────────────────────────┐
│  Apple Home / Home Assistant    │  Commissioner
├─────────────────────────────────┤
│  rs-matter + rs-matter-stack    │  Matter protocol (CASE, IM, clusters)
├─────────────────────────────────┤
│  rs-matter-embassy              │  Glue: BLE commissioning, Thread driver, mDNS/SRP
├─────────────────────────────────┤
│  openthread (esp-rs/openthread) │  Thread networking (MLE, 6LoWPAN, MAC)
├─────────────────────────────────┤
│  esp-radio (esp-rs/esp-hal)     │  IEEE 802.15.4 radio driver + BLE
└─────────────────────────────────┘
```

### Key Repositories

| Repo | What it does | Key files |
|------|-------------|-----------|
| `project-chip/rs-matter` | Matter protocol implementation | `src/dm/networks/wireless/mgr.rs` (WirelessMgr), `src/dm/clusters/net_comm.rs` (NetworkCommissioning), `src/dm/clusters/gen_comm.rs` (GeneralCommissioning) |
| `sysgrok/rs-matter-stack` | Stack orchestration, commissioning flows | `src/lib.rs`, `src/wireless/thread.rs` (non-concurrent & coex flows) |
| `ivmarkov/rs-matter-embassy` | Embassy async integration, ESP/nRF drivers | `rs-matter-embassy/src/wireless/thread.rs` (ThreadDriverTaskImpl), `rs-matter-embassy/src/ot.rs` (OtNetCtl, OtMdns, OtPersist), `rs-matter-embassy/src/wireless/thread/esp_thread.rs` |
| `esp-rs/openthread` | OpenThread Rust bindings + ESP radio | `openthread/src/esp.rs` (EspRadio — TX/RX/config), `openthread/src/lib.rs` (OpenThread instance, platform callbacks), `openthread-sys/gen/builder.rs` (CMake build config) |
| `esp-rs/esp-hal` | ESP32 HAL including 802.15.4 radio | `esp-radio/src/ieee802154/raw.rs` (ISR, RX queue, state machine), `esp-radio/src/ieee802154/mod.rs` (Config, Ieee802154 driver) |

### Dependency Chain for Patching

The example app uses `[patch.crates-io]` in `Cargo.toml` to override git dependencies. When patching:

```
examples/esp/Cargo.toml
  [patch.crates-io]        ← patches crates.io deps
  [patch.'https://...']    ← patches specific git URL deps (needed for transitive deps)
```

To patch `rs-matter` (which is a transitive dep via `rs-matter-stack`):
1. Clone it locally
2. Add BOTH `[patch.crates-io]` AND `[patch.'https://github.com/sysgrok/rs-matter']` pointing to the local path
3. The git URL patch is needed because `rs-matter-stack` depends on `rs-matter` via git, not crates.io

## Matter Commissioning Flow (Non-Concurrent, Thread)

This is the flow for `SupportsConcurrentConnection=false`:

```
Phase 1: BLE (PASE)
  1. BLE advertising → Phone connects
  2. PBKDFParamRequest/Response
  3. PASEPake1/2/3 → PASE session established
  4. ArmFailSafe (starts 120-177s timer!)
  5. SetRegulatoryConfig
  6. CertificateChainRequest (PAI + DAC)
  7. AttestationRequest
  8. CSRRequest
  9. AddTrustedRootCertificate
  10. AddNOC → Fabric created
  11. AddOrUpdateThreadNetwork → Thread credentials stored
  12. ArmFailSafe (re-armed with 177s)
  13. ConnectNetwork → NoopWirelessNetCtl returns Ok, BLE phase exits

Phase 2: Thread
  14. BLE teardown, Thread radio init
  15. WirelessMgr reads stored creds, calls OtNetCtl::connect()
  16. OpenThread: enable_thread(true), scans, attaches as child
  17. Netif up, SRP registration, mDNS services published

Phase 3: CASE over Thread/IP
  18. Commissioner discovers device via DNS-SD (through border router)
  19. CASESigma1/2/3 → CASE session established
  20. CommissioningComplete → sets commissioned=true, persists
  21. ACL write, subscription setup
```

**Critical timing**: The fail-safe timer (177s max) starts at step 4 and must not expire before step 20. Everything between BLE teardown and CommissioningComplete must complete within ~150s.

## Debugging Decision Tree

### Device won't join Thread after BLE commissioning

**Symptom**: `Netif down` stays forever, no `Role detached -> child`

1. Check if `WirelessMgr` logs `"Connecting to network with ID ..."` — if not, it's blocked
2. Check `commissioned()` flag — the deadlock bug (Fix #1 in MATTER_THREAD_ESP32C6_FIXES.md)
3. Check if `enable_thread(true)` is called — only happens from `OtNetCtl::connect()`

### Thread attaches but no CASE arrives

**Symptom**: `Role detached -> child`, `Netif up`, SRP registered, but no `SC::CASESigma1`

1. Check SRP registration succeeded (not stuck in retry loop)
2. Check if mDNS services double-cycle (remove→re-register wastes time)
3. Check fail-safe hasn't expired (`Fail-Safe timeout expired for fabric X, disarming`)
4. Check border router is publishing SRP records to local network

### Radio RX not working (esp-radio 0.18)

**Symptom**: TX succeeds but no RX frames logged

1. Check `start_receive()` is called after `set_config()` — channel changes stop the receiver
2. Check `ext_addr` byte order — esp-radio 0.18 changed to little-endian, OpenThread uses big-endian
3. Check for `Frame rx failed, error:Parse` on first frame — stale RX buffer

### RX Queue Full

**Symptom**: `WARN - Receive queue full` spam

1. Check `rx_queue_size` in EspRadio config — default 10 is too small
2. Set to 200 for Matter-over-Thread workloads
3. On esp-radio 0.18, this is set in `EspConfig` passed to `set_config()`

### ChannelAccessFailure on large messages

**Symptom**: `Failed to send IPv6 UDP msg, error:ChannelAccessFailure` for 1200+ byte messages

1. This is 6LoWPAN fragmentation — large UDP packets fragment into 10+ MAC frames
2. CCA threshold 0 in Carrier mode may be too sensitive (but changing to -60 didn't help on H2)
3. ESP32-H2 is particularly affected — may be hardware limitation
4. Increase OpenThread message buffers (`OPENTHREAD_CONFIG_NUM_MESSAGE_BUFFERS=256`)
5. Disable unnecessary traffic (e.g., `TestOnOffDeviceLogic::new(false)` to stop auto-toggle)

### SRP Registration Slow

**Symptom**: `SRP callback error: code=28, host_state=X` with increasing retry intervals

1. Code 28 = `RESPONSE_TIMEOUT` — SRP server not responding
2. Check if unnecessary remove→re-register cycle is happening
3. Skip removal if `srp_is_empty()` on first registration
4. Add debounce after `wait_mdns()` to prevent double-cycle

## Building on macOS

### Cross-compilation toolchain

The OpenThread C library needs a RISC-V GCC cross-compiler:

```bash
# Install via espup
cargo install espup
espup install

# Add to PATH (espup doesn't do this automatically)
export PATH="$HOME/.espressif/tools/riscv32-esp-elf/esp-15.1.0_20250607/riscv32-esp-elf/bin:$PATH"
```

### CMake macOS workaround

macOS CMake injects `-arch arm64` which breaks cross-compilation. Create a toolchain file:

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

### Clang vs GCC for OpenThread

The `openthread-sys` build defaults to clang. On macOS:
- Apple Clang doesn't support RISC-V targets
- Homebrew LLVM works but needs `stdint.h` in the sysroot
- Best approach: use `CARGO_FEATURE_USE_GCC=true` with the ESP RISC-V GCC toolchain

### Build command

```bash
export PATH="$HOME/.espressif/tools/riscv32-esp-elf/esp-15.1.0_20250607/riscv32-esp-elf/bin:$PATH"
export CMAKE_TOOLCHAIN_FILE=/tmp/riscv32-toolchain.cmake
DEFMT_LOG=debug ESP_LOG=debug CARGO_FEATURE_USE_GCC=true \
  cargo +nightly espflash flash \
  --no-default-features --features esp32c6 \
  --target riscv32imac-unknown-none-elf \
  --release --bin light_thread \
  --monitor --baud 1500000
```

## Key Log Messages and What They Mean

| Log | Meaning | Action |
|-----|---------|--------|
| `Role disabled -> detached` | Thread starting, searching for network | Normal |
| `Role detached -> child` | Successfully joined Thread network | Good |
| `Netif up` | IPv6 interface active, can receive IP traffic | Good |
| `Netif down` | Lost Thread connection | Check radio/parent |
| `Receive queue full` | ISR queuing faster than drain | Increase `rx_queue_size` |
| `ChannelAccessFailure` | CCA fails, can't transmit | RF congestion or H2 limitation |
| `OtError(3)` | `NO_BUFS` — out of message buffers | Increase `NUM_MESSAGE_BUFFERS` |
| `Fabric Index mismatch` | CASE for expired/removed fabric | Fail-safe expired, need re-commission |
| `SRP callback error: code=28` | SRP server timeout | Retry will happen, check border router |
| `Frame rx failed, error:Parse` | Received garbage frame | Usually first frame after init, harmless |
| `Frame rx failed, error:Duplicated` | Same frame received twice | MAC retransmission, harmless |
| `Frame rx failed, error:UnknownNeighbor` | Frame from unknown router | Broadcast from router not yet discovered, harmless |
| `Failed to process Parent Response: Security` | Key mismatch during reattach | Device was detached, keys out of sync, will retry |
| `Fail-Safe timeout expired` | Commissioning took too long | Optimize SRP/mDNS registration time |
| `Busy Responder` | Max concurrent exchanges reached | Too many simultaneous CASE sessions |
| `No TX space, chunking` | IM response too large for one message | Normal Matter chunked transfer |

## esp-radio 0.17 vs 0.18 Differences

| Aspect | 0.17 | 0.18 |
|--------|------|------|
| RX queue size | Hard-coded 10 | Configurable via `Config::rx_queue_size` |
| ext_addr byte order | Big-endian | Little-endian (needs `swap_bytes()`) |
| set_config() | Restarts receiver implicitly | Does NOT restart receiver (need manual `start_receive()`) |
| CCA default | `CcaMode::Carrier` | `CcaMode::Ed` with threshold -60 |
| ISR handling | Simpler state machine | Aligned with ESP-IDF 5.5.2 C driver |
| ACK handling | Basic | Timer0-based ACK timeout, TX deferral |
| Dependencies | esp-hal 1.0 ecosystem | esp-hal 1.1 ecosystem (esp-sync, esp-phy v2, etc.) |

## ESP32-C6 vs ESP32-H2

| Aspect | ESP32-C6 | ESP32-H2 |
|--------|----------|----------|
| Radio | WiFi + BLE + 802.15.4 | BLE + 802.15.4 only |
| RAM | 512KB | 320KB |
| Matter commissioning | Works reliably | Commissioning works, post-commission subscription fails |
| Large packet TX | Reliable | `ChannelAccessFailure` on 1200+ byte fragmented packets |
| Thread attachment | Fast (1st attempt) | Fast (1st attempt) |
| Known issue | None | Radio can't reliably transmit large 6LoWPAN fragments |

## Common Pitfalls

1. **Editing Cargo git checkouts** — Changes to `~/.cargo/git/checkouts/` are NOT picked up by Cargo. Use `[patch]` sections with local paths instead.

2. **Cargo.lock regeneration** — Changing `[patch]` sections or dependency versions regenerates the lock file, potentially pulling in incompatible versions. Delete `Cargo.lock` and rebuild from scratch when changing patches.

3. **OpenThread submodules** — When cloning the openthread repo, run `git submodule update --init --recursive` to get the C OpenThread source.

4. **esp-hal ecosystem version coupling** — All `esp-*` crates must be from the same release. Mixing 0.17/0.18 esp-radio with different esp-hal versions causes `esp-sync` feature gate errors.

5. **Flash state** — Stale commissioning state persists across reflashes. Use `espflash erase-flash` before re-commissioning.

6. **BLE + Thread radio sharing** — ESP32-C6/H2 share the radio between BLE and 802.15.4. Non-concurrent mode (BLE first, then Thread) is more reliable than coex mode.
