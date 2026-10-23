//! BLE supervisor states, reconnect backoff, and packet handling.

use crate::outdoor::packet::PositionPacket;
use crate::outdoor::state::OutdoorState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BleState {
    #[default]
    Disabled,
    Advertising,
    Connected,
    Disconnected,
    Backoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BleStatus {
    pub state: BleState,
    pub reconnect_attempt: u8,
    pub last_sequence: u32,
    pub packets_received: u32,
    pub packets_dropped: u32,
}

impl Default for BleStatus {
    fn default() -> Self {
        Self {
            state: BleState::Disabled,
            reconnect_attempt: 0,
            last_sequence: 0,
            packets_received: 0,
            packets_dropped: 0,
        }
    }
}

/// Calculate exponential reconnect backoff delay in seconds.
/// 0 => 1s, 1 => 2s, 2 => 4s, 3 => 8s, 4 => 16s, _ => 30s.
pub const fn reconnect_delay_secs(attempt: u8) -> u64 {
    match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        _ => 30,
    }
}

/// Process a raw GATT write containing a PositionPacket.
/// Validates sequence monotonicity and updates both BleStatus and OutdoorState.
pub fn handle_position_packet(
    data: &[u8],
    status: &mut BleStatus,
    outdoor: &mut OutdoorState,
) -> bool {
    let packet = match PositionPacket::decode(data) {
        Ok(p) => p,
        Err(_) => {
            status.packets_dropped = status.packets_dropped.saturating_add(1);
            return false;
        }
    };

    if packet.sequence <= status.last_sequence && status.last_sequence != 0 {
        status.packets_dropped = status.packets_dropped.saturating_add(1);
        return false;
    }

    status.last_sequence = packet.sequence;
    status.packets_received = status.packets_received.saturating_add(1);

    let _ = outdoor.handle_position_write(data, packet.timestamp_unix);
    true
}
