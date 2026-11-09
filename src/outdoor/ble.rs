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
/// Returns the authenticated and validated PositionPacket on success.
pub fn handle_position_packet(
    data: &[u8],
    auth_key: &[u8],
    status: &mut BleStatus,
    outdoor: &mut OutdoorState,
) -> Option<PositionPacket> {
    let packet = match PositionPacket::decode_authenticated(data, auth_key) {
        Ok(p) => p,
        Err(_) => {
            status.packets_dropped = status.packets_dropped.saturating_add(1);
            return None;
        }
    };

    if status.last_sequence != 0 && !is_newer_sequence(packet.sequence, status.last_sequence) {
        status.packets_dropped = status.packets_dropped.saturating_add(1);
        return None;
    }

    match outdoor.handle_position_write(
        &data[..crate::outdoor::packet::POSITION_PACKET_LEN],
        packet.timestamp_unix,
    ) {
        Ok(()) => {
            status.last_sequence = packet.sequence;
            status.packets_received = status.packets_received.saturating_add(1);
            Some(packet)
        }
        Err(_err) => {
            status.packets_dropped = status.packets_dropped.saturating_add(1);
            None
        }
    }
}

/// RFC 1982-style serial arithmetic for a wrapping u32 sequence number.
pub const fn is_newer_sequence(candidate: u32, current: u32) -> bool {
    candidate != current && candidate.wrapping_sub(current) < 0x8000_0000
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
            flags: FLAG_COORDS_VALID
                | FLAG_ALTITUDE_VALID
                | FLAG_SPEED_VALID
                | FLAG_COURSE_VALID
                | FLAG_ACCURACY_VALID,
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
        let key = b"test-only-position-key";
        let bytes1 = pkt1.encode_authenticated(key).unwrap();
        let res1 = handle_position_packet(&bytes1, key, &mut status, &mut outdoor);
        assert_eq!(res1, Some(pkt1));
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 0);
        assert_eq!(status.last_sequence, 10);
        assert_eq!(outdoor.latitude_deg, 59.437);

        // Duplicate sequence (10 <= 10): dropped
        let res_dup = handle_position_packet(&bytes1, key, &mut status, &mut outdoor);
        assert_eq!(res_dup, None);
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 1);

        // Invalid packet (bad length): dropped
        let corrupt = [0u8; 12];
        let res_corrupt = handle_position_packet(&corrupt, key, &mut status, &mut outdoor);
        assert_eq!(res_corrupt, None);
        assert_eq!(status.packets_dropped, 2);

        // Monotonic sequence (11 > 10): accepted
        let mut pkt2 = pkt1;
        pkt2.sequence = 11;
        let bytes2 = pkt2.encode_authenticated(key).unwrap();
        let res2 = handle_position_packet(&bytes2, key, &mut status, &mut outdoor);
        assert_eq!(res2, Some(pkt2));
        assert_eq!(status.packets_received, 2);
        assert_eq!(status.last_sequence, 11);
    }

    #[test]
    fn malformed_and_out_of_order_packets_preserve_state_and_increment_dropped() {
        let mut status = BleStatus::new();
        let mut outdoor = OutdoorState::new();
        let key = b"test-only-position-key";

        // Initial valid packet
        let valid_pkt = PositionPacket {
            version: 1,
            flags: FLAG_COORDS_VALID,
            sequence: 50,
            timestamp_unix: 1718000050,
            latitude_e7: 594370000,
            longitude_e7: 247535000,
            altitude_cm: 0,
            speed_cms: 0,
            course_cdeg: 0,
            accuracy_cm: 200,
        };
        let valid_bytes = valid_pkt.encode_authenticated(key).unwrap();
        assert!(handle_position_packet(
            &valid_bytes,
            key,
            &mut status,
            &mut outdoor
        )
        .is_some());
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 0);
        assert_eq!(outdoor.latitude_deg, 59.437);
        let orig_lat = outdoor.latitude_deg;
        let orig_lon = outdoor.longitude_deg;

        // 1. Out-of-order packet (sequence 40 < 50): rejected, state unchanged
        let mut stale_pkt = valid_pkt;
        stale_pkt.sequence = 40;
        stale_pkt.latitude_e7 = 600000000;
        let stale_bytes = stale_pkt.encode_authenticated(key).unwrap();
        assert!(handle_position_packet(
            &stale_bytes,
            key,
            &mut status,
            &mut outdoor
        )
        .is_none());
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 1);
        assert_eq!(status.last_sequence, 50);
        assert_eq!(outdoor.latitude_deg, orig_lat);
        assert_eq!(outdoor.longitude_deg, orig_lon);

        // 2. Malformed packet with bad HMAC: rejected, state unchanged
        let mut bad_mac_bytes = valid_bytes;
        bad_mac_bytes[0] ^= 0xFF;
        assert!(handle_position_packet(
            &bad_mac_bytes,
            key,
            &mut status,
            &mut outdoor
        )
        .is_none());
        assert_eq!(status.packets_received, 1);
        assert_eq!(status.packets_dropped, 2);
        assert_eq!(status.last_sequence, 50);
        assert_eq!(outdoor.latitude_deg, orig_lat);
        assert_eq!(outdoor.longitude_deg, orig_lon);
    }
}
