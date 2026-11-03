//! Deterministic property-style tests for untrusted protocol input.
//!
//! These intentionally use no external fuzzing runtime so they run in the
//! normal host CI job. The same invariants can be promoted to cargo-fuzz
//! targets later without changing the production decoder API.

use waveshare_watch_rs::outdoor::packet::{
    PositionError, PositionPacket, FLAG_ACCURACY_VALID, FLAG_ALTITUDE_VALID, FLAG_COORDS_VALID,
    FLAG_COURSE_VALID, FLAG_SPEED_VALID,
    POSITION_PACKET_LEN,
};

fn packet() -> PositionPacket {
    PositionPacket {
        version: 1,
        flags: FLAG_COORDS_VALID
            | FLAG_ALTITUDE_VALID
            | FLAG_SPEED_VALID
            | FLAG_COURSE_VALID
            | FLAG_ACCURACY_VALID,
        sequence: 7,
        timestamp_unix: 1_718_000_000,
        latitude_e7: 594_372_000,
        longitude_e7: 247_535_000,
        altitude_cm: 4500,
        speed_cms: 140,
        course_cdeg: 18000,
        accuracy_cm: 250,
    }
}

#[test]
fn arbitrary_lengths_never_decode_as_a_position_packet() {
    for len in 0..=128 {
        let bytes = vec![0xA5; len];
        if len != POSITION_PACKET_LEN {
            assert!(matches!(
                PositionPacket::decode(&bytes),
                Err(PositionError::InvalidLength)
            ));
        }
    }
}

#[test]
fn every_authenticated_body_bit_flip_is_rejected() {
    let key = b"property-test-position-key";
    let encoded = packet().encode_authenticated(key).unwrap();
    for index in 0..encoded.len() {
        for bit in 0..8 {
            let mut mutated = encoded;
            mutated[index] ^= 1 << bit;
            assert!(
                PositionPacket::decode_authenticated(&mutated, key).is_err(),
                "mutation at byte {index}, bit {bit} was accepted"
            );
        }
    }
}

#[test]
fn authentication_is_key_and_sequence_independent() {
    let key = b"property-test-position-key";
    let other_key = b"different-position-key";
    let mut first = packet();
    let first_bytes = first.encode_authenticated(key).unwrap();
    assert!(PositionPacket::decode_authenticated(&first_bytes, key).is_ok());
    assert!(PositionPacket::decode_authenticated(&first_bytes, other_key).is_err());

    first.sequence = first.sequence.wrapping_add(1);
    let second_bytes = first.encode_authenticated(key).unwrap();
    assert_ne!(first_bytes, second_bytes);
    assert!(PositionPacket::decode_authenticated(&second_bytes, key).is_ok());
}
