//! Bounded, provider-neutral data received from the LAN companion.
//!
//! This is deliberately the normalized representation, not a mirror of a
//! weather or calendar provider response. Keeping the boundary here lets the
//! companion change providers without requiring a watch firmware update.

use heapless::{String, Vec};

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_HOURLY_FORECASTS: usize = 24;
pub const MAX_AGENDA_EVENTS: usize = 20;
pub const MAX_LOCATION_LEN: usize = 32;
pub const MAX_EVENT_ID_LEN: usize = 40;
pub const MAX_EVENT_TITLE_LEN: usize = 64;
pub const MAX_EVENT_LOCATION_LEN: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HourlyForecast {
    pub start_unix: i64,
    /// Degrees Celsius multiplied by ten. This avoids floating point and
    /// preserves the 12.4 C precision used by the companion JSON contract.
    pub temperature_tenths_c: i16,
    pub condition: WeatherCondition,
    pub precipitation_probability_percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String<MAX_EVENT_ID_LEN>,
    pub title: String<MAX_EVENT_TITLE_LEN>,
    pub start_unix: i64,
    pub end_unix: i64,
    pub location: String<MAX_EVENT_LOCATION_LEN>,
    pub all_day: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AgendaSnapshot {
    pub events: Vec<CalendarEvent, MAX_AGENDA_EVENTS>,
}

impl AgendaSnapshot {
    /// Returns the nearest future event. Events are validated as ordered.
    pub fn next_event(&self, now_unix: i64) -> Option<&CalendarEvent> {
        self.events.iter().find(|event| event.end_unix > now_unix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSnapshot {
    pub schema: u16,
    /// Time at which the companion generated this complete snapshot.
    pub generated_at_unix: i64,
    pub weather: Option<WeatherSnapshot>,
    pub agenda: AgendaSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    UnsupportedSchema,
    InvalidProbability,
    InvalidTemperatureRange,
    InvalidWeatherRange,
    InvalidEventTime,
    EventsNotOrdered,
    ForecastNotOrdered,
}

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

    pub fn validate(&self) -> Result<(), SnapshotError> {
        if self.schema != SCHEMA_VERSION {
            return Err(SnapshotError::UnsupportedSchema);
        }

        if let Some(weather) = &self.weather {
            if weather.precipitation_probability_percent > 100
                || weather
                    .hourly
                    .iter()
                    .any(|hour| hour.precipitation_probability_percent > 100)
            {
                return Err(SnapshotError::InvalidProbability);
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
                return Err(SnapshotError::InvalidTemperatureRange);
            }
            if weather.low_tenths_c > weather.high_tenths_c {
                return Err(SnapshotError::InvalidWeatherRange);
            }
            if weather
                .hourly
                .windows(2)
                .any(|pair| pair[0].start_unix >= pair[1].start_unix)
            {
                return Err(SnapshotError::ForecastNotOrdered);
            }
        }

        if self
            .agenda
            .events
            .iter()
            .any(|event| event.end_unix < event.start_unix)
        {
            return Err(SnapshotError::InvalidEventTime);
        }
        if self
            .agenda
            .events
            .windows(2)
            .any(|pair| pair[0].start_unix > pair[1].start_unix)
        {
            return Err(SnapshotError::EventsNotOrdered);
        }
        Ok(())
    }
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
        assert_eq!(snapshot.validate(), Err(SnapshotError::InvalidWeatherRange));

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
        assert_eq!(snapshot.validate(), Err(SnapshotError::EventsNotOrdered));
    }
}
