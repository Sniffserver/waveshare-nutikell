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
