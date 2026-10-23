//! NOAA Solar position calculations (Azimuth, Elevation, Sunrise, Sunset).
//!
//! Uses pure `libm` math, suitable for `#![no_std]` embedded targets.

use core::f32::consts::PI;

const DEG_TO_RAD: f32 = PI / 180.0;
const RAD_TO_DEG: f32 = 180.0 / PI;

#[derive(Clone, Copy, Debug, Default)]
pub struct SolarPosition {
    /// Solar azimuth in degrees (0..360, 0 = North, 90 = East, 180 = South, 270 = West)
    pub azimuth_deg: f32,
    /// Solar elevation in degrees (-90..+90, positive above horizon, negative below)
    pub elevation_deg: f32,
    /// Sunrise in minute of day UTC (0..1440), or None for polar conditions
    pub sunrise_utc_min: Option<u16>,
    /// Sunset in minute of day UTC (0..1440), or None for polar conditions
    pub sunset_utc_min: Option<u16>,
}

impl SolarPosition {
    pub fn cardinal_str(&self) -> &'static str {
        let mut az = self.azimuth_deg + 22.5;
        while az >= 360.0 {
            az -= 360.0;
        }
        while az < 0.0 {
            az += 360.0;
        }
        let idx = (az / 45.0) as usize;
        match idx {
            0 => "N",
            1 => "NE",
            2 => "E",
            3 => "SE",
            4 => "S",
            5 => "SW",
            6 => "W",
            7 => "NW",
            _ => "N",
        }
    }
}

fn wrap_360(mut deg: f32) -> f32 {
    while deg >= 360.0 {
        deg -= 360.0;
    }
    while deg < 0.0 {
        deg += 360.0;
    }
    deg
}

fn wrap_1440(mut min: f32) -> f32 {
    while min >= 1440.0 {
        min -= 1440.0;
    }
    while min < 0.0 {
        min += 1440.0;
    }
    min
}

/// Calculate solar azimuth, elevation, and sunrise/sunset from Gregorian date, time, and coordinates.
pub fn calculate_solar(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    lat: f32,
    lon: f32,
) -> SolarPosition {
    let y = year as i32;
    let m = month as i32;
    let d = day as i32;

    // Julian Day Number
    let a = (14 - m) / 12;
    let y_adj = y + 4800 - a;
    let m_adj = m + 12 * a - 3;
    let jdn = d + (153 * m_adj + 2) / 5 + 365 * y_adj + y_adj / 4 - y_adj / 100 + y_adj / 400 - 32045;
    let time_frac = ((hour as f32) + (minute as f32) / 60.0 + (second as f32) / 3600.0 - 12.0) / 24.0;
    let jd = (jdn as f32) + time_frac;
    let t = (jd - 2451545.0) / 36525.0;

    // Geometric mean longitude of the Sun
    let l0 = wrap_360(280.46646 + t * (36000.77 + 0.0003032 * t));
    // Sun mean anomaly
    let m_sun = wrap_360(357.52911 + t * (35999.05 - 0.0001537 * t));
    // Earth orbit eccentricity
    let e = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);

    let m_rad = m_sun * DEG_TO_RAD;
    let c = libm::sinf(m_rad) * (1.914602 - t * (0.004817 + 0.000014 * t))
        + libm::sinf(2.0 * m_rad) * (0.019993 - 0.000101 * t)
        + libm::sinf(3.0 * m_rad) * 0.000289;

    let sun_true_lon = l0 + c;
    let sun_app_lon = sun_true_lon - 0.00569 - 0.00478 * libm::sinf((125.04 - 1934.136 * t) * DEG_TO_RAD);

    let obliq = 23.439291 - 0.0130042 * t;
    let obliq_corr = obliq + 0.00256 * libm::cosf((125.04 - 1934.136 * t) * DEG_TO_RAD);

    let sin_dec = libm::sinf(obliq_corr * DEG_TO_RAD) * libm::sinf(sun_app_lon * DEG_TO_RAD);
    let dec_rad = libm::asinf(sin_dec);

    // Equation of Time (EOT in minutes)
    let half_obliq = (obliq_corr * 0.5) * DEG_TO_RAD;
    let tan_half = libm::tanf(half_obliq);
    let y_var = tan_half * tan_half;
    let l0_rad = l0 * DEG_TO_RAD;
    let eot_min = 4.0 * RAD_TO_DEG * (
        y_var * libm::sinf(2.0 * l0_rad)
            - 2.0 * e * libm::sinf(m_rad)
            + 4.0 * e * y_var * libm::sinf(m_rad) * libm::cosf(2.0 * l0_rad)
            - 0.5 * (y_var * y_var) * libm::sinf(4.0 * l0_rad)
            - 1.25 * (e * e) * libm::sinf(2.0 * m_rad)
    );

    // Solar Time & Hour Angle
    let time_min = (hour as f32) * 60.0 + (minute as f32) + (second as f32) / 60.0;
    let solar_time_min = wrap_1440(time_min + eot_min + 4.0 * lon);
    let ha_deg = solar_time_min / 4.0 - 180.0;

    // Solar Elevation
    let lat_rad = lat * DEG_TO_RAD;
    let ha_rad = ha_deg * DEG_TO_RAD;
    let sin_elev = libm::sinf(lat_rad) * libm::sinf(dec_rad)
        + libm::cosf(lat_rad) * libm::cosf(dec_rad) * libm::cosf(ha_rad);
    let sin_elev_clamped = if sin_elev < -1.0 { -1.0 } else if sin_elev > 1.0 { 1.0 } else { sin_elev };
    let elev_deg = libm::asinf(sin_elev_clamped) * RAD_TO_DEG;

    // Solar Azimuth
    let elev_rad = elev_deg * DEG_TO_RAD;
    let cos_zenith = libm::sinf(elev_rad);
    let sin_zenith = libm::cosf(elev_rad);
    let az_deg = if sin_zenith > 0.0001 {
        let num = libm::sinf(lat_rad) * cos_zenith - libm::sinf(dec_rad);
        let den = libm::cosf(lat_rad) * sin_zenith;
        let cos_az = if den.abs() > 1e-6 { (num / den).clamp(-1.0, 1.0) } else { 0.0 };
        let mut angle = 180.0 - libm::acosf(cos_az) * RAD_TO_DEG;
        if ha_deg > 0.0 {
            angle = 360.0 - angle;
        }
        wrap_360(angle)
    } else {
        180.0
    };

    // Sunrise & Sunset (Zenith 90.833 deg)
    let cos_90_833 = libm::cosf(90.833 * DEG_TO_RAD);
    let denom_rise = libm::cosf(lat_rad) * libm::cosf(dec_rad);
    let (sunrise_utc_min, sunset_utc_min) = if denom_rise.abs() > 1e-6 {
        let cos_h0 = (cos_90_833 - libm::sinf(lat_rad) * libm::sinf(dec_rad)) / denom_rise;
        if cos_h0 > 1.0 || cos_h0 < -1.0 {
            // Polar night or midnight sun
            (None, None)
        } else {
            let h0_deg = libm::acosf(cos_h0) * RAD_TO_DEG;
            let solar_noon_utc = 720.0 - 4.0 * lon - eot_min;
            let rise = wrap_1440(solar_noon_utc - 4.0 * h0_deg);
            let sset = wrap_1440(solar_noon_utc + 4.0 * h0_deg);
            (Some(rise as u16), Some(sset as u16))
        }
    } else {
        (None, None)
    };

    SolarPosition {
        azimuth_deg: az_deg,
        elevation_deg: elev_deg,
        sunrise_utc_min,
        sunset_utc_min,
    }
}


// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tallinn_summer_solstice_noon() {
        // Tallinn approx: 59.437 N, 24.753 E
        // Summer Solstice: June 21, 10:20 UTC (solar noon in Tallinn ~10:21 UTC)
        let pos = calculate_solar(2026, 6, 21, 10, 21, 0, 59.437, 24.753);

        // At solar noon, azimuth should be nearly due South (approx 180 deg)
        assert!((pos.azimuth_deg - 180.0).abs() < 5.0, "Azimuth expected ~180, got {}", pos.azimuth_deg);
        assert_eq!(pos.cardinal_str(), "S");

        // Solar elevation in Tallinn at summer solstice noon is approx:
        // 90 - 59.437 + 23.44 = ~54 degrees
        assert!((pos.elevation_deg - 54.0).abs() < 2.0, "Elevation expected ~54, got {}", pos.elevation_deg);

        // Sunrise and sunset should be present
        assert!(pos.sunrise_utc_min.is_some());
        assert!(pos.sunset_utc_min.is_some());
        let rise = pos.sunrise_utc_min.unwrap();
        let sset = pos.sunset_utc_min.unwrap();
        // Summer solstice in Tallinn: very long day (>18 hours)
        assert!(sset > rise);
        let day_len_min = sset - rise;
        assert!(day_len_min > 18 * 60, "Expected >18h day, got {} min", day_len_min);
    }

    #[test]
    fn tallinn_winter_solstice_noon() {
        // Winter Solstice: Dec 21, 10:20 UTC
        let pos = calculate_solar(2026, 12, 21, 10, 20, 0, 59.437, 24.753);

        // Solar elevation in Tallinn at winter solstice noon is approx:
        // 90 - 59.437 - 23.44 = ~7.1 degrees
        assert!((pos.elevation_deg - 7.1).abs() < 2.0, "Elevation expected ~7.1, got {}", pos.elevation_deg);
        assert!((pos.azimuth_deg - 180.0).abs() < 5.0, "Azimuth expected ~180, got {}", pos.azimuth_deg);
    }

    #[test]
    fn cardinal_directions() {
        let mut pos = SolarPosition::default();

        pos.azimuth_deg = 0.0;
        assert_eq!(pos.cardinal_str(), "N");
        pos.azimuth_deg = 45.0;
        assert_eq!(pos.cardinal_str(), "NE");
        pos.azimuth_deg = 90.0;
        assert_eq!(pos.cardinal_str(), "E");
        pos.azimuth_deg = 135.0;
        assert_eq!(pos.cardinal_str(), "SE");
        pos.azimuth_deg = 180.0;
        assert_eq!(pos.cardinal_str(), "S");
        pos.azimuth_deg = 225.0;
        assert_eq!(pos.cardinal_str(), "SW");
        pos.azimuth_deg = 270.0;
        assert_eq!(pos.cardinal_str(), "W");
        pos.azimuth_deg = 315.0;
        assert_eq!(pos.cardinal_str(), "NW");
        pos.azimuth_deg = 355.0;
        assert_eq!(pos.cardinal_str(), "N");
    }

    #[test]
    fn polar_night_returns_none() {
        // North Pole (85 N) in winter (Dec 21)
        let pos = calculate_solar(2026, 12, 21, 12, 0, 0, 85.0, 0.0);
        assert_eq!(pos.sunrise_utc_min, None);
        assert_eq!(pos.sunset_utc_min, None);
        assert!(pos.elevation_deg < -10.0);
    }
}
