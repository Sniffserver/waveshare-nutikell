//! Bounded, provider-neutral data received from the LAN companion.
//!
//! This is deliberately the normalized representation, not a mirror of a
//! weather or calendar provider response. Keeping the boundary here lets the
//! companion change providers without requiring a watch firmware update.

use heapless::{String, Vec};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const SCHEMA_VERSION: u16 = 1;
/// Maximum encoded JSON snapshot accepted from the companion.
pub const MAX_SNAPSHOT_JSON_BYTES: usize = 12 * 1024;
pub const MAX_CONTEXT_ROW_BYTES: usize = 25;
const JSON_STRING_BUFFER_BYTES: usize = 64;
pub const SNAPSHOT_FRAME_HEADER_BYTES: usize = 10;
pub const SNAPSHOT_AUTH_TAG_BYTES: usize = 16;
const MAX_SNAPSHOT_TRANSFER_BYTES: usize = MAX_SNAPSHOT_JSON_BYTES + SNAPSHOT_AUTH_TAG_BYTES;
const SNAPSHOT_FRAME_MAGIC: u8 = 0x53;
const SNAPSHOT_PROTOCOL_VERSION: u8 = 1;
const SNAPSHOT_HMAC_DOMAIN: &[u8] = b"sbie-watch.sync.v1\0";
/// Maximum hourly entries accepted from `sbie-watch.sync.v1`.
pub const MAX_HOURLY_FORECASTS: usize = 24;
/// Maximum today/tomorrow agenda entries accepted from `sbie-watch.sync.v1`.
pub const MAX_AGENDA_EVENTS: usize = 20;
/// Maximum UTF-8 bytes in the weather location name.
pub const MAX_WEATHER_LOCATION_BYTES: usize = 32;
pub const MAX_EVENT_ID_LEN: usize = 40;
/// Maximum UTF-8 bytes in an agenda title.
pub const MAX_TITLE_BYTES: usize = 64;
/// Maximum UTF-8 bytes in an agenda location.
pub const MAX_LOCATION_BYTES: usize = 64;

// Compatibility names for code written against the first v1 model.
pub const MAX_LOCATION_LEN: usize = MAX_WEATHER_LOCATION_BYTES;
pub const MAX_EVENT_TITLE_LEN: usize = MAX_TITLE_BYTES;
pub const MAX_EVENT_LOCATION_LEN: usize = MAX_LOCATION_BYTES;

pub const WEATHER_FRESH_SECS: u64 = 60 * 60;
pub const AGENDA_FRESH_SECS: u64 = 15 * 60;
/// RAM allocation ceiling for the normalized, validated cache.
pub const SYNC_CACHE_MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeatherCondition {
    Clear,
    PartlyCloudy,
    Cloudy,
    Rain,
    Snow,
    Thunderstorm,
    Fog,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct HourlyForecast {
    pub start_unix: i64,
    /// Degrees Celsius multiplied by ten. This avoids floating point and
    /// preserves the 12.4 C precision used by the companion JSON contract.
    pub temperature_tenths_c: i16,
    pub condition: WeatherCondition,
    pub precipitation_probability_percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct WeatherSnapshot {
    pub location: String<MAX_LOCATION_LEN>,
    pub observed_at_unix: i64,
    pub temperature_tenths_c: i16,
    pub condition: WeatherCondition,
    pub high_tenths_c: i16,
    pub low_tenths_c: i16,
    pub precipitation_probability_percent: u8,
    pub hourly: Vec<HourlyForecast, MAX_HOURLY_FORECASTS>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct CalendarEvent {
    pub id: String<MAX_EVENT_ID_LEN>,
    pub title: String<MAX_EVENT_TITLE_LEN>,
    pub start_unix: i64,
    pub end_unix: i64,
    pub location: String<MAX_EVENT_LOCATION_LEN>,
    pub all_day: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Deserialize)]
pub struct AgendaSnapshot {
    pub events: Vec<CalendarEvent, MAX_AGENDA_EVENTS>,
}

impl AgendaSnapshot {
    /// Returns the nearest future event. Events are validated as ordered.
    pub fn next_event(&self, now_unix: i64) -> Option<&CalendarEvent> {
        self.events.iter().find(|event| event.end_unix > now_unix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct SyncSnapshot {
    pub schema: u16,
    /// Time at which the companion generated this complete snapshot.
    pub generated_at_unix: i64,
    pub weather: Option<WeatherSnapshot>,
    pub agenda: AgendaSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionSyncError {
    BadSchema,
    /// The response exceeded the companion JSON body limit before parsing.
    BodyTooLarge,
    /// A bounded collection would hide information, so reject the snapshot.
    TooManyHourlyForecasts,
    TooManyAgendaEvents,
    InvalidJson,
    InvalidAuthentication,
    InvalidTransfer,
    Replay,
    Truncated,
    InvalidTimestamp,
    InvalidProbability,
    InvalidTemperatureRange,
    InvalidWeatherRange,
    InvalidWeatherCondition,
    TitleTooLong,
    LocationTooLong,
    EmptyEventId,
    DuplicateEventId,
    UnorderedAgenda,
    UnorderedForecast,
}

/// The former public name remains available for existing consumers.
pub type SnapshotError = CompanionSyncError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotFreshness {
    Fresh,
    Stale,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    InvalidSnapshot(CompanionSyncError),
}

/// Storage boundary for sync consumers. A future CRC-protected flash store can
/// implement this trait without changing refresh, widget, or reminder code.
pub trait SnapshotStore {
    fn load(&self) -> Option<&SyncSnapshot>;
    fn generation(&self) -> u32;
    fn written_at_unix(&self) -> Option<i64>;
    fn save(&mut self, snapshot: SyncSnapshot, written_at_unix: i64) -> Result<(), StoreError>;
}

/// Last known-good snapshot kept in explicitly owned RAM.
///
/// It deliberately validates before replacement: a failed HTTP fetch or bad
/// companion response cannot erase the current weather or agenda data.
pub struct RamSnapshotStore {
    snapshot: Option<SyncSnapshot>,
    generation: u32,
    written_at_unix: Option<i64>,
}

impl RamSnapshotStore {
    pub const fn new() -> Self {
        Self {
            snapshot: None,
            generation: 0,
            written_at_unix: None,
        }
    }
}

impl Default for RamSnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotStore for RamSnapshotStore {
    fn load(&self) -> Option<&SyncSnapshot> {
        self.snapshot.as_ref()
    }

    fn generation(&self) -> u32 {
        self.generation
    }

    fn written_at_unix(&self) -> Option<i64> {
        self.written_at_unix
    }

    fn save(&mut self, snapshot: SyncSnapshot, written_at_unix: i64) -> Result<(), StoreError> {
        snapshot.validate().map_err(StoreError::InvalidSnapshot)?;
        self.snapshot = Some(snapshot);
        self.generation = self.generation.wrapping_add(1);
        self.written_at_unix = Some(written_at_unix);
        Ok(())
    }
}

const _: () = assert!(core::mem::size_of::<RamSnapshotStore>() <= SYNC_CACHE_MAX_BYTES);

impl SyncSnapshot {
    pub fn empty(generated_at_unix: i64) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            generated_at_unix,
            weather: None,
            agenda: AgendaSnapshot::default(),
        }
    }

    /// Age is clamped at zero so an RTC adjustment into the past never shows
    /// a nonsensical negative cache age.
    pub fn age_secs(&self, now_unix: i64) -> u64 {
        if now_unix <= self.generated_at_unix {
            0
        } else {
            (now_unix - self.generated_at_unix) as u64
        }
    }

    pub fn weather_freshness(&self, now_unix: i64) -> SnapshotFreshness {
        freshness(self.age_secs(now_unix), WEATHER_FRESH_SECS, 6 * 60 * 60)
    }

    pub fn agenda_freshness(&self, now_unix: i64) -> SnapshotFreshness {
        freshness(self.age_secs(now_unix), AGENDA_FRESH_SECS, 2 * 60 * 60)
    }

    pub fn validate(&self) -> Result<(), CompanionSyncError> {
        if self.schema != SCHEMA_VERSION {
            return Err(CompanionSyncError::BadSchema);
        }
        if self.generated_at_unix < 0 {
            return Err(CompanionSyncError::InvalidTimestamp);
        }

        if let Some(weather) = &self.weather {
            if weather.observed_at_unix < 0 || weather.hourly.iter().any(|hour| hour.start_unix < 0)
            {
                return Err(CompanionSyncError::InvalidTimestamp);
            }
            if weather.precipitation_probability_percent > 100
                || weather
                    .hourly
                    .iter()
                    .any(|hour| hour.precipitation_probability_percent > 100)
            {
                return Err(CompanionSyncError::InvalidProbability);
            }
            // Sanity bounds protect rendering and reject accidental units such
            // as a provider sending Fahrenheit or raw tenths incorrectly.
            if !(-900..=700).contains(&weather.temperature_tenths_c)
                || !(-900..=700).contains(&weather.high_tenths_c)
                || !(-900..=700).contains(&weather.low_tenths_c)
                || weather
                    .hourly
                    .iter()
                    .any(|hour| !(-900..=700).contains(&hour.temperature_tenths_c))
            {
                return Err(CompanionSyncError::InvalidTemperatureRange);
            }
            if weather.low_tenths_c > weather.high_tenths_c {
                return Err(CompanionSyncError::InvalidWeatherRange);
            }
            if weather
                .hourly
                .windows(2)
                .any(|pair| pair[0].start_unix >= pair[1].start_unix)
            {
                return Err(CompanionSyncError::UnorderedForecast);
            }
        }

        if self
            .agenda
            .events
            .iter()
            .any(|event| event.start_unix < 0 || event.end_unix < event.start_unix)
        {
            return Err(CompanionSyncError::InvalidTimestamp);
        }
        if self.agenda.events.iter().any(|event| event.id.is_empty()) {
            return Err(CompanionSyncError::EmptyEventId);
        }
        for (index, event) in self.agenda.events.iter().enumerate() {
            if self.agenda.events[index + 1..]
                .iter()
                .any(|other| other.id == event.id)
            {
                return Err(CompanionSyncError::DuplicateEventId);
            }
        }
        if self
            .agenda
            .events
            .windows(2)
            .any(|pair| pair[0].start_unix > pair[1].start_unix)
        {
            return Err(CompanionSyncError::UnorderedAgenda);
        }
        Ok(())
    }
}

/// Decode and validate one complete normalized companion snapshot.
///
/// The caller owns the input buffer; this function never allocates and rejects
/// oversized payloads before invoking the JSON decoder.
pub fn parse_snapshot_json(data: &[u8]) -> Result<SyncSnapshot, CompanionSyncError> {
    if data.len() > MAX_SNAPSHOT_JSON_BYTES {
        return Err(CompanionSyncError::BodyTooLarge);
    }

    let mut string_buffer = [0; JSON_STRING_BUFFER_BYTES];
    let (snapshot, _) = serde_json_core::from_slice_escaped::<SyncSnapshot>(
        data,
        &mut string_buffer,
    )
    .map_err(|_| CompanionSyncError::InvalidJson)?;
    snapshot.validate()?;
    Ok(snapshot)
}

/// Bounded receiver for framed, authenticated snapshot transfers.
///
/// Frame layout: magic, protocol version, sequence (LE u32), total transfer
/// length (LE u16), byte offset (LE u16), then payload. The transfer payload
/// is the JSON document followed by a truncated HMAC-SHA-256 tag.
pub struct SnapshotAssembler {
    bytes: [u8; MAX_SNAPSHOT_TRANSFER_BYTES],
    sequence: u32,
    total_len: usize,
    received: usize,
    last_sequence: Option<u32>,
    active: bool,
}

impl SnapshotAssembler {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_SNAPSHOT_TRANSFER_BYTES],
            sequence: 0,
            total_len: 0,
            received: 0,
            last_sequence: None,
            active: false,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.total_len = 0;
        self.received = 0;
    }

    pub fn push_fragment(
        &mut self,
        frame: &[u8],
        auth_key: &[u8],
    ) -> Result<Option<SyncSnapshot>, CompanionSyncError> {
        if frame.len() <= SNAPSHOT_FRAME_HEADER_BYTES || frame[0] != SNAPSHOT_FRAME_MAGIC
            || frame[1] != SNAPSHOT_PROTOCOL_VERSION
        {
            self.reset();
            return Err(CompanionSyncError::InvalidTransfer);
        }

        let sequence = u32::from_le_bytes([frame[2], frame[3], frame[4], frame[5]]);
        let total_len = u16::from_le_bytes([frame[6], frame[7]]) as usize;
        let offset = u16::from_le_bytes([frame[8], frame[9]]) as usize;
        let payload = &frame[SNAPSHOT_FRAME_HEADER_BYTES..];
        let Some(end) = offset.checked_add(payload.len()) else {
            self.reset();
            return Err(CompanionSyncError::InvalidTransfer);
        };

        if total_len <= SNAPSHOT_AUTH_TAG_BYTES
            || total_len > MAX_SNAPSHOT_TRANSFER_BYTES
            || end > total_len
            || payload.is_empty()
        {
            self.reset();
            return Err(CompanionSyncError::InvalidTransfer);
        }

        if offset == 0 {
            if self.active {
                self.reset();
                return Err(CompanionSyncError::InvalidTransfer);
            }
            if self
                .last_sequence
                .is_some_and(|last| !crate::outdoor::ble::is_newer_sequence(sequence, last))
            {
                return Err(CompanionSyncError::Replay);
            }
            self.sequence = sequence;
            self.total_len = total_len;
            self.received = 0;
            self.active = true;
        }

        if !self.active
            || sequence != self.sequence
            || total_len != self.total_len
            || offset != self.received
        {
            self.reset();
            return Err(CompanionSyncError::InvalidTransfer);
        }

        self.bytes[offset..end].copy_from_slice(payload);
        self.received = end;
        if self.received != self.total_len {
            return Ok(None);
        }

        let json_len = self.total_len - SNAPSHOT_AUTH_TAG_BYTES;
        if auth_key.is_empty() {
            self.reset();
            return Err(CompanionSyncError::InvalidAuthentication);
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(auth_key)
            .map_err(|_| CompanionSyncError::InvalidAuthentication)?;
        mac.update(SNAPSHOT_HMAC_DOMAIN);
        mac.update(&self.sequence.to_le_bytes());
        mac.update(&self.bytes[..json_len]);
        let expected = mac.finalize().into_bytes();
        let mut difference = 0u8;
        for (actual, expected) in self.bytes[json_len..self.total_len]
            .iter()
            .zip(expected[..SNAPSHOT_AUTH_TAG_BYTES].iter())
        {
            difference |= actual ^ expected;
        }
        if difference != 0 {
            self.reset();
            return Err(CompanionSyncError::InvalidAuthentication);
        }

        let snapshot = parse_snapshot_json(&self.bytes[..json_len]);
        self.reset();
        let snapshot = snapshot?;
        self.last_sequence = Some(sequence);
        Ok(Some(snapshot))
    }
}

impl Default for SnapshotAssembler {
    fn default() -> Self {
        Self::new()
    }
}

fn freshness(age_secs: u64, fresh_until_secs: u64, offline_after_secs: u64) -> SnapshotFreshness {
    if age_secs <= fresh_until_secs {
        SnapshotFreshness::Fresh
    } else if age_secs <= offline_after_secs {
        SnapshotFreshness::Stale
    } else {
        SnapshotFreshness::Offline
    }
}

/// Build one short ASCII row for the watchface without allocating.
pub fn format_context_row(snapshot: Option<&SyncSnapshot>, now_unix: i64) -> String<MAX_CONTEXT_ROW_BYTES> {
    use core::fmt::Write;

    let mut row = String::new();
    let Some(snapshot) = snapshot else {
        let _ = row.push_str("Offline");
        return row;
    };

    let weather_freshness = snapshot.weather_freshness(now_unix);
    match (&snapshot.weather, weather_freshness) {
        (Some(weather), SnapshotFreshness::Fresh | SnapshotFreshness::Stale) => {
            if weather_freshness == SnapshotFreshness::Stale {
                let _ = row.push('~');
            }
            let temperature = weather.temperature_tenths_c;
            let rounded = if temperature < 0 {
                (temperature - 5) / 10
            } else {
                (temperature + 5) / 10
            };
            let _ = write!(
                row,
                "{}C {}",
                rounded,
                condition_short(weather.condition)
            );
        }
        (Some(_), SnapshotFreshness::Offline) => {
            let _ = row.push_str("Wx offline");
        }
        (None, _) => {
            let _ = row.push_str("Wx --");
        }
    }

    let _ = row.push_str(" | ");
    let agenda_freshness = snapshot.agenda_freshness(now_unix);
    match agenda_freshness {
        SnapshotFreshness::Offline => {
            let _ = row.push_str("Cal offline");
        }
        SnapshotFreshness::Fresh | SnapshotFreshness::Stale => {
            if agenda_freshness == SnapshotFreshness::Stale {
                let _ = row.push('~');
            }
            if let Some(event) = snapshot.agenda.next_event(now_unix) {
                let title = truncate_utf8(event.title.as_str(), 9);
                let _ = row.push_str(title);
            } else {
                let _ = row.push_str("No event");
            }
        }
    }

    row
}

fn condition_short(condition: WeatherCondition) -> &'static str {
    match condition {
        WeatherCondition::Clear => "Clear",
        WeatherCondition::PartlyCloudy => "Partly",
        WeatherCondition::Cloudy => "Cloud",
        WeatherCondition::Rain => "Rain",
        WeatherCondition::Snow => "Snow",
        WeatherCondition::Thunderstorm => "Storm",
        WeatherCondition::Fog => "Fog",
        WeatherCondition::Unknown => "Wx",
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text<const N: usize>(value: &str) -> String<N> {
        String::try_from(value).expect("fixture fits")
    }

    fn event(id: &str, start_unix: i64, end_unix: i64) -> CalendarEvent {
        CalendarEvent {
            id: text(id),
            title: text("Standup"),
            start_unix,
            end_unix,
            location: text("Teams"),
            all_day: false,
        }
    }

    #[test]
    fn empty_snapshot_has_current_schema_and_non_negative_age() {
        let snapshot = SyncSnapshot::empty(1_000);
        assert_eq!(snapshot.schema, SCHEMA_VERSION);
        assert_eq!(snapshot.age_secs(900), 0);
        assert_eq!(snapshot.age_secs(1_032), 32);
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn next_event_skips_completed_items() {
        let mut snapshot = SyncSnapshot::empty(1_000);
        snapshot
            .agenda
            .events
            .push(event("past", 800, 900))
            .unwrap();
        snapshot
            .agenda
            .events
            .push(event("next", 1_100, 1_200))
            .unwrap();

        assert_eq!(
            snapshot.agenda.next_event(1_000).unwrap().id.as_str(),
            "next"
        );
    }

    #[test]
    fn validation_rejects_invalid_weather_and_event_ordering() {
        let mut snapshot = SyncSnapshot::empty(1_000);
        snapshot.weather = Some(WeatherSnapshot {
            location: text("Helsinki"),
            observed_at_unix: 990,
            temperature_tenths_c: 124,
            condition: WeatherCondition::PartlyCloudy,
            high_tenths_c: 70,
            low_tenths_c: 140,
            precipitation_probability_percent: 35,
            hourly: Vec::new(),
        });
        assert_eq!(
            snapshot.validate(),
            Err(CompanionSyncError::InvalidWeatherRange)
        );

        snapshot.weather.as_mut().unwrap().low_tenths_c = 70;
        snapshot
            .agenda
            .events
            .push(event("late", 2_000, 2_100))
            .unwrap();
        snapshot
            .agenda
            .events
            .push(event("early", 1_000, 1_100))
            .unwrap();
        assert_eq!(
            snapshot.validate(),
            Err(CompanionSyncError::UnorderedAgenda)
        );
    }

    #[test]
    fn validation_rejects_duplicate_or_empty_event_ids() {
        let mut snapshot = SyncSnapshot::empty(1_000);
        snapshot
            .agenda
            .events
            .push(event("same", 1_100, 1_200))
            .unwrap();
        snapshot
            .agenda
            .events
            .push(event("same", 1_300, 1_400))
            .unwrap();
        assert_eq!(
            snapshot.validate(),
            Err(CompanionSyncError::DuplicateEventId)
        );

        snapshot.agenda.events.clear();
        snapshot
            .agenda
            .events
            .push(event("", 1_100, 1_200))
            .unwrap();
        assert_eq!(snapshot.validate(), Err(CompanionSyncError::EmptyEventId));
    }

    #[test]
    fn bounded_collections_reject_an_extra_entry() {
        let mut hourly = Vec::<HourlyForecast, MAX_HOURLY_FORECASTS>::new();
        for hour in 0..MAX_HOURLY_FORECASTS {
            hourly
                .push(HourlyForecast {
                    start_unix: hour as i64,
                    temperature_tenths_c: 100,
                    condition: WeatherCondition::Clear,
                    precipitation_probability_percent: 0,
                })
                .unwrap();
        }
        assert!(hourly
            .push(HourlyForecast {
                start_unix: 99,
                temperature_tenths_c: 100,
                condition: WeatherCondition::Clear,
                precipitation_probability_percent: 0,
            })
            .is_err());

        let mut events = Vec::<CalendarEvent, MAX_AGENDA_EVENTS>::new();
        for index in 0..MAX_AGENDA_EVENTS {
            events
                .push(event("id", index as i64, index as i64))
                .unwrap();
        }
        assert!(events.push(event("extra", 30, 30)).is_err());
    }

    #[test]
    fn freshness_thresholds_are_centralized_and_rtc_safe() {
        let snapshot = SyncSnapshot::empty(1_000);
        assert_eq!(snapshot.weather_freshness(900), SnapshotFreshness::Fresh);
        assert_eq!(snapshot.weather_freshness(4_601), SnapshotFreshness::Stale);
        assert_eq!(
            snapshot.weather_freshness(22_601),
            SnapshotFreshness::Offline
        );
        assert_eq!(snapshot.agenda_freshness(1_901), SnapshotFreshness::Stale);
        assert_eq!(snapshot.agenda_freshness(8_201), SnapshotFreshness::Offline);
    }

    #[test]
    fn invalid_save_preserves_last_good_snapshot() {
        let mut store = RamSnapshotStore::new();
        let valid = SyncSnapshot::empty(1_000);
        store.save(valid, 1_001).unwrap();

        let mut invalid = SyncSnapshot::empty(2_000);
        invalid.schema = 99;
        assert_eq!(
            store.save(invalid, 2_001),
            Err(StoreError::InvalidSnapshot(CompanionSyncError::BadSchema))
        );
        assert_eq!(store.generation(), 1);
        assert_eq!(store.written_at_unix(), Some(1_001));
        assert_eq!(store.load().unwrap().generated_at_unix, 1_000);
    }

    #[test]
    fn valid_save_replaces_snapshot_and_advances_generation() {
        let mut store = RamSnapshotStore::new();
        store.save(SyncSnapshot::empty(1_000), 1_001).unwrap();
        store.save(SyncSnapshot::empty(2_000), 2_001).unwrap();

        assert_eq!(store.generation(), 2);
        assert_eq!(store.written_at_unix(), Some(2_001));
        assert_eq!(store.load().unwrap().generated_at_unix, 2_000);
    }

    #[test]
    fn parser_decodes_valid_snapshot_and_escaped_strings() {
        let json = br#"{
            "schema":1,
            "generated_at_unix":1000,
            "weather":{
                "location":"Tallinn",
                "observed_at_unix":990,
                "temperature_tenths_c":124,
                "condition":"partly_cloudy",
                "high_tenths_c":160,
                "low_tenths_c":80,
                "precipitation_probability_percent":35,
                "hourly":[]
            },
            "agenda":{"events":[{
                "id":"event-1",
                "title":"Standup \u2601",
                "start_unix":1100,
                "end_unix":1200,
                "location":"Office",
                "all_day":false
            }]}
        }"#;

        let snapshot = parse_snapshot_json(json).unwrap();
        assert_eq!(snapshot.weather.as_ref().unwrap().condition, WeatherCondition::PartlyCloudy);
        assert_eq!(snapshot.agenda.events[0].title.as_str(), "Standup ☁");
        assert_eq!(snapshot.validate(), Ok(()));
    }

    #[test]
    fn parser_rejects_malformed_and_oversized_json() {
        assert_eq!(
            parse_snapshot_json(br#"{"schema":}"#),
            Err(CompanionSyncError::InvalidJson)
        );

        let oversized = [b' '; MAX_SNAPSHOT_JSON_BYTES + 1];
        assert_eq!(
            parse_snapshot_json(&oversized),
            Err(CompanionSyncError::BodyTooLarge)
        );
    }

    #[test]
    fn assembler_requires_ordered_authenticated_non_replayed_transfer() {
        let json = br#"{"schema":1,"generated_at_unix":1000,"weather":null,"agenda":{"events":[]}}"#;
        let key = b"test-only-sync-key";
        let sequence = 7u32;
        let mut transfer = alloc::vec::Vec::from(json.as_slice());
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(SNAPSHOT_HMAC_DOMAIN);
        mac.update(&sequence.to_le_bytes());
        mac.update(json);
        transfer.extend_from_slice(&mac.finalize().into_bytes()[..SNAPSHOT_AUTH_TAG_BYTES]);

        let mut assembler = SnapshotAssembler::new();
        let first = snapshot_frame(sequence, transfer.len(), 0, &transfer[..24]);
        assert_eq!(assembler.push_fragment(&first, key), Ok(None));
        let second = snapshot_frame(sequence, transfer.len(), 24, &transfer[24..]);
        let snapshot = assembler.push_fragment(&second, key).unwrap().unwrap();
        assert_eq!(snapshot.generated_at_unix, 1000);

        assert_eq!(
            assembler.push_fragment(&first, key),
            Err(CompanionSyncError::Replay)
        );
    }

    #[test]
    fn assembler_rejects_authentication_failure_and_out_of_order_fragments() {
        let json = br#"{"schema":1,"generated_at_unix":1000,"weather":null,"agenda":{"events":[]}}"#;
        let key = b"test-only-sync-key";
        let mut transfer = alloc::vec::Vec::from(json.as_slice());
        transfer.extend_from_slice(&[0; SNAPSHOT_AUTH_TAG_BYTES]);

        let mut assembler = SnapshotAssembler::new();
        let first = snapshot_frame(1, transfer.len(), 0, &transfer[..24]);
        assert_eq!(assembler.push_fragment(&first, key), Ok(None));
        let gap = snapshot_frame(1, transfer.len(), 25, &transfer[25..]);
        assert_eq!(
            assembler.push_fragment(&gap, key),
            Err(CompanionSyncError::InvalidTransfer)
        );

        let complete = snapshot_frame(1, transfer.len(), 0, &transfer);
        assert_eq!(
            assembler.push_fragment(&complete, key),
            Err(CompanionSyncError::InvalidAuthentication)
        );
    }

    fn snapshot_frame(sequence: u32, total_len: usize, offset: usize, payload: &[u8]) -> alloc::vec::Vec<u8> {
        let mut frame = alloc::vec::Vec::with_capacity(SNAPSHOT_FRAME_HEADER_BYTES + payload.len());
        frame.push(SNAPSHOT_FRAME_MAGIC);
        frame.push(SNAPSHOT_PROTOCOL_VERSION);
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(&(total_len as u16).to_le_bytes());
        frame.extend_from_slice(&(offset as u16).to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }
}
