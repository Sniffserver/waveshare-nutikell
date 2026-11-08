//! Deterministic property-style tests for untrusted protocol input.
//!
//! These intentionally use no external fuzzing runtime so they run in the
//! normal host CI job. The same invariants can be promoted to cargo-fuzz
//! targets later without changing the production decoder API.

use waveshare_watch_rs::outdoor::packet::{
    PositionError, PositionPacket, FLAG_ACCURACY_VALID, FLAG_ALTITUDE_VALID, FLAG_COORDS_VALID,
    FLAG_COURSE_VALID, FLAG_SPEED_VALID, POSITION_PACKET_LEN,
};
use waveshare_watch_rs::peripherals::companion_sync::{
    format_context_row, parse_snapshot_json, CompanionSyncError, SnapshotAssembler,
    MAX_SNAPSHOT_JSON_BYTES, SNAPSHOT_AUTH_TAG_BYTES, SNAPSHOT_FRAME_HEADER_BYTES,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const SYNC_HMAC_DOMAIN: &[u8] = b"sbie-watch.sync.v1\0";

fn snapshot_json() -> &'static [u8] {
    br#"{"schema":1,"generated_at_unix":1000,"weather":{"location":"Tallinn","observed_at_unix":990,"temperature_tenths_c":124,"condition":"partly_cloudy","high_tenths_c":160,"low_tenths_c":80,"precipitation_probability_percent":35,"hourly":[]},"agenda":{"events":[{"id":"event-1","title":"Standup \u2601","start_unix":1100,"end_unix":1200,"location":"Office","all_day":false}]}}"#
}

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

#[test]
fn snapshot_parser_checks_schema_and_formats_context_row() {
    let snapshot = parse_snapshot_json(snapshot_json()).unwrap();
    assert_eq!(snapshot.agenda.events[0].title.as_str(), "Standup ☁");
    assert_eq!(format_context_row(Some(&snapshot), 1000).as_str(), "12C Partly | Standup");
    assert_eq!(format_context_row(None, 1000).as_str(), "Offline");

    let mut stale = snapshot;
    stale.generated_at_unix = 2_000;
    assert!(format_context_row(Some(&stale), 7_000).starts_with('~'));
    assert_eq!(
        parse_snapshot_json(&[b' '; MAX_SNAPSHOT_JSON_BYTES + 1]),
        Err(CompanionSyncError::BodyTooLarge)
    );
    assert_eq!(
        parse_snapshot_json(br#"{"schema":}"#),
        Err(CompanionSyncError::InvalidJson)
    );
}

#[test]
fn snapshot_assembler_accepts_only_ordered_authenticated_fragments() {
    let key = b"property-test-sync-key";
    let sequence = 7u32;
    let json = br#"{"schema":1,"generated_at_unix":1000,"weather":null,"agenda":{"events":[]}}"#;
    let mut transfer = json.to_vec();
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(SYNC_HMAC_DOMAIN);
    mac.update(&sequence.to_le_bytes());
    mac.update(json);
    transfer.extend_from_slice(&mac.finalize().into_bytes()[..SNAPSHOT_AUTH_TAG_BYTES]);

    let split = 24;
    let first = sync_frame(sequence, transfer.len(), 0, &transfer[..split]);
    let second = sync_frame(sequence, transfer.len(), split, &transfer[split..]);
    let mut assembler = SnapshotAssembler::new();
    assert!(assembler.push_fragment(&first, key).unwrap().is_none());
    assert_eq!(
        assembler
            .push_fragment(&second, key)
            .unwrap()
            .unwrap()
            .generated_at_unix,
        1000
    );
    assert_eq!(
        assembler.push_fragment(&first, key),
        Err(CompanionSyncError::Replay)
    );

    let mut invalid = sync_frame(8, transfer.len(), 0, &transfer);
    *invalid.last_mut().unwrap() ^= 0x80;
    assert_eq!(
        assembler.push_fragment(&invalid, key),
        Err(CompanionSyncError::InvalidAuthentication)
    );
}

fn sync_frame(sequence: u32, total: usize, offset: usize, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(SNAPSHOT_FRAME_HEADER_BYTES + payload.len());
    frame.push(0x53);
    frame.push(1);
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&(total as u16).to_le_bytes());
    frame.extend_from_slice(&(offset as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}
