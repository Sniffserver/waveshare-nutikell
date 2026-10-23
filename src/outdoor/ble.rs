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

impl BleStatus {
    pub const fn new() -> Self {
        Self {
            state: BleState::Disabled,
            reconnect_attempt: 0,
            last_sequence: 0,
            packets_received: 0,
            packets_dropped: 0,
        }
    }

    /// User or policy enabled BLE advertising
    pub fn enable_advertising(&mut self) {
        self.state = BleState::Advertising;
    }

    /// User or policy disabled BLE
    pub fn disable(&mut self) {
        self.state = BleState::Disabled;
        self.reconnect_attempt = 0;
    }

    /// Central (iOS companion) connected
    pub fn on_connected(&mut self) {
        self.state = BleState::Connected;
        self.reconnect_attempt = 0;
    }

    /// Link loss detected: enters backoff and returns backoff delay in seconds
    pub fn on_disconnected(&mut self) -> u64 {
        self.state = BleState::Backoff;
        let delay = reconnect_delay_secs(self.reconnect_attempt);
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        delay
    }

    /// Backoff timer elapsed: re-enter advertising state
    pub fn on_backoff_expired(&mut self) {
        if self.state == BleState::Backoff {
            self.state = BleState::Advertising;
        }
    }

    pub fn is_connected(&self) -> bool {
        self.state == BleState::Connected
    }

    pub fn is_advertising(&self) -> bool {
        self.state == BleState::Advertising
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outdoor::packet::{
        FLAG_ACCURACY_VALID, FLAG_ALTITUDE_VALID, FLAG_COORDS_VALID, FLAG_COURSE_VALID,
        FLAG_SPEED_VALID,
    };

    #[test]
    fn reconnect_backoff_schedule() {
        assert_eq!(reconnect_delay_secs(0), 1);
        assert_eq!(reconnect_delay_secs(1), 2);
        assert_eq!(reconnect_delay_secs(2), 4);
        assert_eq!(reconnect_delay_secs(3), 8);
        assert_eq!(reconnect_delay_secs(4), 16);
        assert_eq!(reconnect_delay_secs(5), 30);
        assert_eq!(reconnect_delay_secs(10), 30);
    }

    #[test]
    fn state_machine_lifecycle() {
        let mut status = BleStatus::new();
        assert_eq!(status.state, BleState::Disabled);

        status.enable_advertising();
        assert_eq!(status.state, BleState::Advertising);
        assert!(status.is_advertising());

        status.on_connected();
        assert_eq!(status.state, BleState::Connected);
        assert!(status.is_connected());
        assert_eq!(status.reconnect_attempt, 0);

        // Link loss 1 -> 1s backoff
        let delay1 = status.on_disconnected();
        assert_eq!(delay1, 1);
        assert_eq!(status.state, BleState::Backoff);
        assert_eq!(status.reconnect_attempt, 1);

        // Timer expires -> resume advertising
        status.on_backoff_expired();
        assert_eq!(status.state, BleState::Advertising);

        // Link loss 2 -> 2s backoff
        let delay2 = status.on_disconnected();
        assert_eq!(delay2, 2);
        assert_eq!(status.reconnect_attempt, 2);

        // Reconnect established -> resets backoff attempts to 0
        status.on_connected();
        assert_eq!(status.reconnect_attempt, 0);
        assert_eq!(status.state, BleState::Connected);

        // Manual disable
        status.disable();
        assert_eq!(status.state, BleState::Disabled);
    }

    #[test]
    fn packet_ingestion_and_drop_tracking() {
        let mut status = BleStatus::new();
        let mut outdoor = OutdoorState::new();

        let pkt1 = PositionPacket {
            version: 1,
            flags: FLAG_COORDS_VALID | FLAG_ALTITUDE_VALID | FLAG_SPEED_VALID | FLAG_COURSE_VALID | FLAG_ACCURACY_VALID,
            sequence: 10,
            timestamp_unix: 1718000010,
            latitude_e7: 594370000,
            longitude_e7: 247535000,
            altitude_cm: 4500,
            speed_cms: 120,
            course_cdeg: 9000,
            accuracy_cm: 300,
        };

        // First packet: valid sequence
        let ok1 = handle_position_packet(&pkt1.encode(), &mut status, &mut outdoor);
        assert!(ok1);
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 0);
        assert_eq!(status.last_sequence, 10);
        assert_eq!(outdoor.latitude_deg, 59.437);

        // Duplicate sequence (10 <= 10): dropped
        let ok_dup = handle_position_packet(&pkt1.encode(), &mut status, &mut outdoor);
        assert!(!ok_dup);
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 1);

        // Invalid packet (bad length): dropped
        let corrupt = [0u8; 12];
        let ok_corrupt = handle_position_packet(&corrupt, &mut status, &mut outdoor);
        assert!(!ok_corrupt);
        assert_eq!(status.packets_dropped, 2);

        // Monotonic sequence (11 > 10): accepted
        let mut pkt2 = pkt1;
        pkt2.sequence = 11;
        let ok2 = handle_position_packet(&pkt2.encode(), &mut status, &mut outdoor);
        assert!(ok2);
        assert_eq!(status.packets_received, 2);
        assert_eq!(status.last_sequence, 11);
    }
}
