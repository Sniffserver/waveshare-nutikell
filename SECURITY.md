# Security release checklist

This firmware is not a production security release until the hardware and
provisioning steps below are completed on a fresh device.

## Boot and flash protection

1. Build and test the signed release image using the repository CI workflow.
2. Generate and retain the secure-boot signing key in an offline secret store.
3. Enable ESP32-S3 secure boot and flash encryption with the Espressif
   provisioning tooling, then verify the eFuses before shipping.
4. Do not run development images or leave UART download mode enabled after
   those fuses are burned. eFuse changes are irreversible.

The Rust application cannot safely burn these fuses. The release operator must
perform and record the device-specific provisioning step.

## Credentials

Wi-Fi credentials and a randomly generated per-device HMAC key are stored only
in the application-owned credentials sector. The sector is intended to be
protected by ESP flash encryption; writes and reads must be tested only after
flash encryption is enabled. Never ship a device with plaintext flash and real
credentials.

## BLE position packets

Position writes use a 34-byte payload plus a 16-byte truncated HMAC-SHA-256
tag. The watch rejects legacy unauthenticated 34-byte writes and replayed
sequence numbers. Provision the per-device position key through the secure
companion flow; do not add a key literal to firmware or the iOS app.

## Smart-home commands

Smart-home commands are disabled. The current transport is plaintext HTTP and
has no authorization or request signing. Re-enable only after adding TLS
certificate validation, per-device authorization, replay protection, and an
explicit user confirmation for state-changing commands.

## TOTP

Demo accounts exist only in test builds. Release builds must import real
accounts through the authenticated provisioning path and must not contain test
secrets.
