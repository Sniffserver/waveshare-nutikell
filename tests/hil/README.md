# ESP32-S3 hardware smoke test contract

These checks are intended for a self-hosted runner with a watch attached over
USB/UART. They are deliberately separate from host mocks: a missing or
unresponsive peripheral must fail the run rather than silently pass.

The fixture must exercise, in order:

1. display init, full-frame flush, edge-window writes, and brightness;
2. FT3168 touch interrupt plus a tap at a known coordinate;
3. QMI8658 reset, data-ready, accelerometer, and gyro reads;
4. AXP2101 rail enable and ADC/battery read;
5. Wi-Fi provisioning, credential persistence, reconnect timeout, and NTP;
6. BLE advertising plus an authenticated position-characteristic write, with a
   tampered/replayed write rejected;
7. flash recovery after an interrupted credential write.

The firmware boot log is the fixture protocol. A hardware runner should flash
the exact release artifact, collect the serial log, and require one explicit
`[HIL][PASS] <component>` marker for every component above. The current CI
host job covers the corresponding deterministic driver/protocol mocks; this
folder is the contract for the physical runner and prevents claiming HIL
coverage when no board is attached.

## Latest Validation (2026-11-08)

- **Device identification:** `espflash board-info --port /dev/ttyACM0` reported
   ESP32-S3 revision 0.2, a 40 MHz crystal, and 32 MB flash. This identifies the
   connected chip; it is not a peripheral smoke test.
- **Host work-storage tests:** `cargo test --test work_tracker_storage
   --target x86_64-unknown-linux-gnu --locked` passed (12 tests).
- **Xtensa release check:** the CI-configured `cargo check --release` passed.
- **Xtensa release build:** blocked at link time. `stack.x:11` reported that
   the location counter could not move backwards (`0x3fcdbe0c` to `0x3fcdb700`).
   The firmware ELF was therefore not rebuilt for this validation.
- **Flash and peripheral smoke tests:** not run. No image was flashed; the
   connected watch's firmware behavior remains unverified.

These are the results of this run, not a claim that the full checklist passed.
Re-run the release build and the complete hardware fixture before marking any
HIL component as passed.
