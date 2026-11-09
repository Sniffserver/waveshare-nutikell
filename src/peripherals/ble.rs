//! BLE advertising control via raw HCI commands.
//!
//! esp-radio's `BleConnector` is a low-level HCI transport — it doesn't
//! provide a GATT server or high-level advertising API. For the smartwatch
//! we only need the device to be discoverable (advertising its name), so
//! we send 3 HCI commands directly:
//!
//!   1. LE Set Advertising Parameters (slow interval = power-friendly)
//!   2. LE Set Advertising Data (Flags + Complete Local Name)
//!   3. LE Set Advertising Enable (on / off)
//!
//! The VHCI interface in ESP-IDF expects H4 transport framing, so every
//! command is prefixed with 0x01 (HCI Command Packet).

use embedded_io::Write;

#[cfg(target_arch = "xtensa")]
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
#[cfg(target_arch = "xtensa")]
use static_cell::StaticCell;
#[cfg(target_arch = "xtensa")]
use trouble_host::prelude::*;

use crate::outdoor::ble::handle_position_packet;
use crate::outdoor::{BleStatus, OutdoorState};

#[cfg(target_arch = "xtensa")]
use crate::outdoor::packet::PositionPacket;
#[cfg(target_arch = "xtensa")]
use crate::peripherals::companion_sync::{
    SnapshotAssembler, SyncSnapshot, SNAPSHOT_IDLE_TIMEOUT_SECS,
};
#[cfg(target_arch = "xtensa")]
use crate::peripherals::nvs::DeviceAuthKey;

#[cfg(target_arch = "xtensa")]
pub static SNAPSHOT_RESULT: Signal<CriticalSectionRawMutex, SyncSnapshot> = Signal::new();
#[cfg(target_arch = "xtensa")]
pub static POSITION_RESULT: Signal<CriticalSectionRawMutex, PositionPacket> = Signal::new();

/// Authenticated GATT write dispatcher for the companion position
/// characteristic. The pinned radio crate supplies HCI transport only; a
/// GATT server task must call this for every position-characteristic write.
pub fn dispatch_position_write(
    value: &[u8],
    auth_key: &[u8],
    status: &mut BleStatus,
    outdoor: &mut OutdoorState,
) -> Option<PositionPacket> {
    handle_position_packet(value, auth_key, status, outdoor)
}

/// Start BLE advertising as "Rust Watch".
/// Sends HCI commands synchronously via the BleConnector's Write impl.
pub fn start_advertising<W: Write>(hci: &mut W) -> Result<(), W::Error> {
    // 1) LE Set Advertising Parameters
    //    Opcode 0x2006, 15 bytes of params
    //    Interval: 0x0800 (1.28s) — slow to save power
    //    Type: ADV_IND (connectable, undirected)
    //    Channels: all 3 (37, 38, 39)
    hci.write_all(&[
        0x01, // H4: HCI command
        0x06, 0x20, // opcode: LE Set Advertising Parameters
        15,   // param length
        0x00, 0x08, // interval min: 0x0800 (1280 * 0.625ms = 800ms)
        0x00, 0x08, // interval max: 0x0800
        0x00, // type: ADV_IND
        0x00, // own addr type: public
        0x00, // peer addr type
        0, 0, 0, 0, 0, 0,    // peer addr (unused)
        0x07, // channel map: all
        0x00, // filter policy: any
    ])?;

    // 2) LE Set Advertising Data
    //    Opcode 0x2008, always 32 bytes of param (1 len + 31 data)
    let name = b"Rust Watch";
    let flags_len: u8 = 3; // AD: [len=2, type=0x01 Flags, val=0x06]
    let name_ad_len: u8 = 1 + name.len() as u8; // [type + name bytes]
    let sig_octets = flags_len + 1 + name_ad_len; // total significant

    let mut cmd = [0u8; 36]; // 1 (H4) + 2 (opcode) + 1 (plen) + 32 (data) = 36
    cmd[0] = 0x01; // H4
    cmd[1] = 0x08;
    cmd[2] = 0x20; // opcode: LE Set Advertising Data
    cmd[3] = 32; // param length (always 32)
    cmd[4] = sig_octets; // significant octets count
                         // Flags AD structure
    cmd[5] = 2; // length of this AD
    cmd[6] = 0x01; // AD type: Flags
    cmd[7] = 0x06; // General Discoverable + BR/EDR Not Supported
                   // Complete Local Name AD structure
    cmd[8] = name_ad_len;
    cmd[9] = 0x09; // AD type: Complete Local Name
    cmd[10..10 + name.len()].copy_from_slice(name);
    // Remaining bytes are zero (padding)
    hci.write_all(&cmd)?;

    // 3) LE Set Advertising Enable
    //    Opcode 0x200A, 1 byte param = 0x01 (enable)
    hci.write_all(&[
        0x01, // H4
        0x0A, 0x20, // opcode: LE Set Advertising Enable
        1,    // param length
        0x01, // enable
    ])?;

    Ok(())
}

/// Stop BLE advertising.
pub fn stop_advertising<W: Write>(hci: &mut W) -> Result<(), W::Error> {
    hci.write_all(&[
        0x01, // H4
        0x0A, 0x20, // opcode: LE Set Advertising Enable
        1,    // param length
        0x00, // disable
    ])?;
    Ok(())
}

#[cfg(target_arch = "xtensa")]
#[gatt_server(
    connections_max = 1,
    mutex_type = embassy_sync::blocking_mutex::raw::NoopRawMutex,
    attribute_table_size = 32
)]
struct OutdoorGattServer {
    outdoor: OutdoorGattService,
}

#[cfg(target_arch = "xtensa")]
#[gatt_service(uuid = "7A7E0001-5B8A-4C9C-9A4C-6B5E5F001001")]
struct OutdoorGattService {
    #[characteristic(
        uuid = "7A7E0002-5B8A-4C9C-9A4C-6B5E5F001001",
        write,
        write_without_response,
        value = [0; 50]
    )]
    position: [u8; 50],
    #[characteristic(uuid = "7A7E0004-5B8A-4C9C-9A4C-6B5E5F001001", read, notify, value = 0)]
    status: u8,
    #[characteristic(
        uuid = "7A7E0005-5B8A-4C9C-9A4C-6B5E5F001001",
        write,
        write_without_response,
        value = [0; 512]
    )]
    snapshot: [u8; 512],
}

#[cfg(target_arch = "xtensa")]
#[embassy_executor::task(pool_size = 1)]
pub async fn companion_sync_task(
    connector: esp_radio::ble::controller::BleConnector<'static>,
    auth_key: DeviceAuthKey,
) -> ! {
    const SERVICE_UUID_LE: [[u8; 16]; 1] = [[
        0x01, 0x10, 0x00, 0x5f, 0x5e, 0x6b, 0x4c, 0x9a, 0x9c, 0x4c, 0x8a, 0x5b, 0x01, 0x00, 0x7e,
        0x7a,
    ]];
    const CONNECTIONS: usize = 1;
    const CHANNELS: usize = 2;

    static RESOURCES: StaticCell<HostResources<DefaultPacketPool, CONNECTIONS, CHANNELS>> =
        StaticCell::new();

    let controller = bt_hci::controller::ExternalController::<_, 4>::new(connector);
    let resources = RESOURCES.init(HostResources::new());
    let stack = trouble_host::new(controller, resources);
    let host = stack.build();
    let mut peripheral = host.peripheral;
    let mut runner = host.runner;
    let server = OutdoorGattServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "Rust Watch",
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .expect("valid GATT service configuration");

    let runner_task = async move {
        loop {
            let _ = runner.run().await;
        }
    };
    let peripheral_task = async move {
        let mut assembler = SnapshotAssembler::new();
        let mut last_snapshot_fragment = None;
        let mut status = BleStatus::new();
        let mut outdoor = OutdoorState::new();
        let position = server.outdoor.position;
        let snapshot = server.outdoor.snapshot;
        loop {
            let mut adv_data = [0; 31];
            let structures = [
                AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                AdStructure::ServiceUuids128(&SERVICE_UUID_LE),
                AdStructure::ShortenedLocalName(b"SBIE"),
            ];
            let Ok(adv_len) = AdStructure::encode_slice(&structures, &mut adv_data) else {
                continue;
            };
            let advertiser = match peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..adv_len],
                        scan_data: &[],
                    },
                )
                .await
            {
                Ok(advertiser) => advertiser,
                Err(_) => continue,
            };
            let connection = match advertiser.accept().await {
                Ok(connection) => connection,
                Err(_) => continue,
            };
            let connection = match connection.with_attribute_server(&server) {
                Ok(connection) => connection,
                Err(_) => continue,
            };

            loop {
                let event = embassy_futures::select::select(
                    connection.next(),
                    embassy_time::Timer::after_secs(5),
                )
                .await;
                match event {
                    embassy_futures::select::Either::First(GattConnectionEvent::Disconnected {
                        ..
                    }) => {
                        assembler.reset();
                        last_snapshot_fragment = None;
                        break;
                    }
                    embassy_futures::select::Either::First(GattConnectionEvent::Gatt { event }) => {
                        if let GattEvent::Write(write) = &event {
                            if write.handle() == snapshot.handle {
                                match assembler.push_fragment(write.data(), auth_key.as_bytes()) {
                                    Ok(Some(value)) => {
                                        last_snapshot_fragment = None;
                                        SNAPSHOT_RESULT.signal(value);
                                    }
                                    Ok(None) => {
                                        last_snapshot_fragment = Some(embassy_time::Instant::now())
                                    }
                                    Err(_) => last_snapshot_fragment = None,
                                }
                            } else if write.handle() == position.handle {
                                if let Some(packet) = dispatch_position_write(
                                    write.data(),
                                    auth_key.as_bytes(),
                                    &mut status,
                                    &mut outdoor,
                                ) {
                                    POSITION_RESULT.signal(packet);
                                }
                            }
                        }
                        if let Ok(reply) = event.accept() {
                            reply.send().await;
                        }
                    }
                    embassy_futures::select::Either::Second(_) => {
                        if last_snapshot_fragment.is_some_and(|last| {
                            embassy_time::Instant::now().duration_since(last).as_secs()
                                >= SNAPSHOT_IDLE_TIMEOUT_SECS
                        }) {
                            assembler.reset();
                            last_snapshot_fragment = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = embassy_futures::join::join(runner_task, peripheral_task).await;
    loop {
        embassy_time::Timer::after_secs(60).await;
    }
}
