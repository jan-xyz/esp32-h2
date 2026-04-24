# Matter-over-Thread on ESP32-C6: Fixes Summary

Getting the `light_thread` example working with Apple Home on an ESP32-C6 required fixes across four repositories. This document describes each fix, why it was needed, and the corresponding diff.

## Overview

The ESP32-C6 `light_thread` example uses non-concurrent BLE commissioning (BLE first, then Thread). The commissioning flow is:

1. **BLE phase**: PASE pairing, certificate exchange, AddNOC, AddOrUpdateThreadNetwork, ConnectNetwork
2. **Thread phase**: Device joins Thread network, registers SRP/mDNS services
3. **CASE over Thread**: Commissioner discovers device via DNS-SD, establishes CASE session, sends `CommissioningComplete`

Multiple bugs prevented this flow from completing.

---

## Fix 1: WirelessMgr Commissioning Deadlock

**Repo**: `project-chip/rs-matter`
**File**: `rs-matter/src/dm/networks/wireless/mgr.rs`

### Problem

After BLE commissioning completes, the device transitions to the Thread phase. `WirelessMgr::run()` is responsible for connecting to the Thread network using stored credentials. However, it gated on `commissioned()` — a flag only set by the `CommissioningComplete` command.

Per the Matter spec, `CommissioningComplete` must be sent over a CASE session on the operational network (Thread/IP). But the device can't join Thread until `WirelessMgr` calls `connect()`, and `WirelessMgr` won't call `connect()` until `commissioned == true`. **This is a deadlock.**

### Fix

Replace the `commissioned()` gate with a check for whether stored network credentials exist. The device should connect to Thread as soon as credentials are available, allowing the commissioner to reach it over IP and send `CommissioningComplete`.

```diff
     pub async fn run(&mut self) -> Result<(), Error> {
         loop {
-            // Don't try to connect to any network until we are commissioned, just wait for the commissioning to complete.
-            Self::wait_while_not_commissioned(&self.networks).await?;
+            Self::wait_while_no_creds(&self.networks, self.buf).await?;
 
-            // The commissioning status changed to commissioned, so start trying to connect to the networks
-            // Do it while the networks don't change.
             let mut changed = pin!(Self::wait_while_not_changed(&self.networks));
             let mut connect = pin!(Self::run_connect(&self.networks, &self.net_ctl, self.buf));
 
@@ -230,6 +227,17 @@
         }
     }
 
+    async fn wait_while_no_creds(networks: &W, buf: &mut [u8]) -> Result<(), Error> {
+        loop {
+            let has_creds = Self::next_creds(networks, None, buf)?.is_some();
+            if has_creds {
+                break Ok(());
+            }
+
+            Self::wait_while_not_changed(networks).await?;
+        }
+    }
+
     async fn wait_while_not_commissioned(networks: &W) -> Result<(), Error> {
```

---

## Fix 2: esp-hal 1.1 Ecosystem Upgrade

**Repos**: `rs-matter-embassy`, `openthread`, example `Cargo.toml`

### Problem

The old `esp-radio 0.17` driver had a hard-coded RX queue size of 10. During Thread operation, the 802.15.4 radio ISR queues frames faster than the OpenThread stack can drain them, causing `"Receive queue full"` warnings. This dropped incoming CASE handshake packets, preventing `CommissioningComplete` from arriving.

### Fix

Upgrade to `esp-radio 0.18` / `esp-hal 1.1` which has an overhauled IEEE 802.15.4 driver aligned with ESP-IDF 5.5.2.

```diff
 # rs-matter-embassy/Cargo.toml
-esp-radio = { version = "0.17", optional = true, features = ["ble"] }
-esp-hal = { version = "1", optional = true, features = ["unstable"] }
+esp-radio = { version = "0.18", optional = true, features = ["ble"] }
+esp-hal = { version = "1.1", optional = true, features = ["unstable"] }
```

```diff
 # openthread/openthread/Cargo.toml
-esp-radio = { version = "0.17", features = ["unstable", "ieee802154"], optional = true }
+esp-radio = { version = "0.18", features = ["unstable", "ieee802154"], optional = true }
```

The example `Cargo.toml` also needs updated dependency versions for `esp-backtrace`, `esp-rtos`, `esp-alloc`, `esp-println`, `esp-storage`, `esp-bootloader-esp-idf`, `esp-metadata-generated`, `esp-sync`, `embassy-executor`, and `embassy-embedded-hal` to match esp-hal 1.1.

---

## Fix 3: Restart Receiver After Config Change

**Repo**: `esp-rs/openthread`
**File**: `openthread/src/esp.rs`

### Problem

When OpenThread scans channels during network attachment, it calls `set_config()` to change the radio channel. In `esp-radio 0.18`, `set_config()` updates the PIB (Protocol Information Base) settings but does **not** restart the receiver. The hardware stays idle after a channel change, so no frames are received on the new channel.

This caused the device to transmit beacon requests successfully but never receive any responses — Thread attachment always failed.

### Fix

Call `start_receive()` after updating the driver configuration to restart the receiver on the new channel.

```diff
     async fn set_config(&mut self, config: &Config) -> Result<(), Self::Error> {
         if self.config != *config {
             debug!("Setting radio config: {:?}", config);

             self.config = config.clone();
             self.update_driver_config();
+            self.driver.start_receive();
         }

         Ok(())
     }
```

---

## Fix 4: Extended Address Byte Order

**Repo**: `esp-rs/openthread`
**File**: `openthread/src/esp.rs`

### Problem

`esp-radio 0.18` changed the IEEE 802.15.4 extended address byte order from big-endian to little-endian (matching the hardware register layout). However, OpenThread stores extended addresses in network byte order (big-endian). The openthread crate passed the address directly without conversion, so the hardware's address filter had the bytes reversed.

This caused all **unicast** frames (like Parent Responses during Thread attachment) to be silently dropped by the MAC hardware filter. Only broadcast frames were received.

### Fix

Swap the byte order when passing the extended address to the ESP radio driver.

```diff
-            ext_addr: config.ext_addr,
+            ext_addr: config.ext_addr.map(|a| a.swap_bytes()),
```

---

## Fix 5: Increase RX Queue Size

**Repo**: `esp-rs/openthread`
**File**: `openthread/src/esp.rs`

### Problem

Even with `esp-radio 0.18`, the default RX queue size of 50 was insufficient during heavy Thread traffic (MLE advertisements, SRP updates, Matter data exchanges). The queue would fill up, dropping incoming packets.

### Fix

Increase the queue to 200 entries (~26KB heap).

```diff
-            rx_queue_size: 50,
+            rx_queue_size: 200,
```

---

## Fix 6: Skip SRP Removal on First Registration

**Repo**: `ivmarkov/rs-matter-embassy`
**File**: `rs-matter-embassy/src/ot.rs`

### Problem

The `OtMdns::run()` loop unconditionally called `srp_remove_all()` and waited for the SRP server to confirm removal before registering new services. On first startup after commissioning, there are no existing SRP records to remove, but the removal request still had to round-trip to the SRP server — consuming 10-30 seconds of the fail-safe timer window.

### Fix

Skip the removal step if SRP is already empty.

```diff
-            let _ = self.ot.srp_remove_all(false);
+            if !self.ot.srp_is_empty().unwrap_or(true) {
+                let _ = self.ot.srp_remove_all(false);
 
-            // TODO: Something is still not quite right with the SRP
-            // We seem to get stuck here
-            while !self.ot.srp_is_empty()? {
-                debug!("Waiting for SRP records to be removed...");
-                select(
-                    Timer::after(Duration::from_secs(1)),
-                    self.ot.srp_wait_changed(),
-                )
-                .await;
+                while !self.ot.srp_is_empty().unwrap_or(true) {
+                    debug!("Waiting for SRP records to be removed...");
+                    select(
+                        Timer::after(Duration::from_secs(1)),
+                        self.ot.srp_wait_changed(),
+                    )
+                    .await;
+                }
             }
```

---

## Fix 7: mDNS Debounce

**Repo**: `ivmarkov/rs-matter-embassy`
**File**: `rs-matter-embassy/src/ot.rs`

### Problem

After registering SRP services, the `matter.wait_mdns()` signal was already pending (triggered by the registration itself), causing the loop to immediately restart with an unnecessary remove → wait → re-register cycle. This double cycle wasted 30-60 seconds of the fail-safe timer, causing Apple Home to time out before the device became discoverable.

### Fix

Add a 10-second debounce after `wait_mdns()` to coalesce rapid mDNS change notifications.

```diff
             matter.wait_mdns().await;
+
+            // Debounce: the registration above likely triggered an mDNS change
+            // notification. Wait long enough for it to settle, then drain any
+            // stale signal so we don't immediately re-trigger a remove cycle.
+            Timer::after(Duration::from_secs(10)).await;
         }
```

---

## Fix 8: Increase OpenThread Message Buffers

**Repo**: `esp-rs/openthread`
**File**: `openthread-sys/gen/builder.rs`

### Problem

Matter messages can be large (1200+ bytes), which fragment into 10+ Thread MAC frames. Each fragment needs an OpenThread message buffer. With 128 buffers, concurrent Matter responses + SRP updates + MLE traffic exhausted the pool, causing `OT_ERROR_NO_BUFS` errors.

### Fix

Double the message buffer count to 256.

```diff
-            .cflag("-DOPENTHREAD_CONFIG_NUM_MESSAGE_BUFFERS=128")
-            .cxxflag("-DOPENTHREAD_CONFIG_NUM_MESSAGE_BUFFERS=128");
+            .cflag("-DOPENTHREAD_CONFIG_NUM_MESSAGE_BUFFERS=256")
+            .cxxflag("-DOPENTHREAD_CONFIG_NUM_MESSAGE_BUFFERS=256");
```

---

## Fix 9: AttrChangeNotifier API Update

**Repo**: `sysgrok/rs-matter-stack`
**File**: `src/lib.rs`

### Problem

The latest `rs-matter` added three new required methods to the `AttrChangeNotifier` trait (`notify_cluster_changed`, `notify_endpoint_changed`, `notify_all_changed`) and made `notify_attribute_changed` on `Subscriptions` private (`pub(crate)`). The `rs-matter-stack` crate needed to be updated to compile.

### Fix

```diff
         self.subscriptions
-            .notify_attribute_changed(endpoint_id, cluster_id, attr_id);
+            .notify_attr_changed(endpoint_id, cluster_id, attr_id);
```

```diff
 impl AttrChangeNotifier for DummyAttrNotifier {
     fn notify_attr_changed(&self, _endpoint_id: EndptId, _cluster_id: ClusterId, _attr_id: AttrId) {
     }
+
+    fn notify_cluster_changed(&self, _endpoint_id: EndptId, _cluster_id: ClusterId) {
+    }
+
+    fn notify_endpoint_changed(&self, _endpoint_id: EndptId) {
+    }
+
+    fn notify_all_changed(&self) {
+    }
 }
```

---

## Known Issue: ESP32-H2

The ESP32-H2 completes commissioning (including `CommissioningComplete`) but fails during the post-commissioning subscription data dump. The H2's radio cannot reliably transmit large 6LoWPAN fragmented packets (1200+ bytes), causing persistent `ChannelAccessFailure` errors. This is a hardware/radio driver issue specific to the H2 and needs investigation in `esp-rs/esp-hal`.

---

## Build Instructions

With all fixes applied, the build requires:

1. **RISC-V GCC toolchain** in PATH (`riscv32-esp-elf-gcc`)
2. **CMake toolchain file** to prevent macOS `-arch arm64` injection:
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
   ```

3. **Build and flash**:
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
