use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::Duration;

#[cfg(target_arch = "xtensa")]
use esp_println::println;
#[cfg(not(target_arch = "xtensa"))]
macro_rules! println {
    ($($arg:tt)*) => {};
}

use crate::peripherals::rtc::DateTime;

pub fn days_to_date(days_since_epoch: i32) -> (u32, u32, u32) {
    if days_since_epoch < 0 {
        return (2000, 1, 1);
    }
    // Simplified date calculation from days since 1970-01-01
    let mut y = 1970i32;
    let mut remaining = days_since_epoch;
    loop {
        if y > 2255 {
            return (2000, 1, 1);
        }
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while m < 12 && remaining >= month_days[m] {
        remaining -= month_days[m];
        m += 1;
    }
    (y as u32, (m + 1) as u32, (remaining + 1) as u32)
}

/// NTP sync — returns a parsed DateTime on success, Err(()) on failure.
/// Runs fully off the main loop.
pub async fn ntp_sync_inner(stack: embassy_net::Stack<'static>) -> Result<DateTime, ()> {
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 256];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    // Port 0 -> kernel assigns a free ephemeral port automatically.
    socket.bind(0).map_err(|_| ())?;

    // NTP request packet (simplified: 48 bytes, first byte = 0x1B for client mode)
    let mut ntp_request = [0u8; 48];
    ntp_request[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)

    // Try resolving pool.ntp.org via DNS first, falling back to static Google NTP IP.
    let fallback_ntp = embassy_net::Ipv4Address::new(216, 239, 35, 0);
    let ntp_addr = match embassy_time::with_timeout(
        Duration::from_secs(3),
        stack.dns_query("pool.ntp.org", embassy_net::dns::DnsQueryType::A),
    )
    .await
    {
        Ok(Ok(addrs)) => {
            if let Some(embassy_net::IpAddress::Ipv4(ipv4)) = addrs.into_iter().next() {
                println!("[NTP] resolved pool.ntp.org -> {:?}", ipv4);
                ipv4
            } else {
                fallback_ntp
            }
        }
        _ => {
            println!(
                "[NTP] DNS query timed out or failed, using static fallback {:?}",
                fallback_ntp
            );
            fallback_ntp
        }
    };
    println!("[NTP] sending request to {:?}", ntp_addr);

    socket
        .send_to(&ntp_request, (ntp_addr, 123))
        .await
        .map_err(|_| ())?;

    println!("[NTP] request sent");

    // Wait for response (timeout 5s)
    let mut response = [0u8; 48];
    match embassy_time::with_timeout(Duration::from_secs(5), socket.recv_from(&mut response)).await
    {
        Ok(Ok((len, _addr))) if len >= 48 => {
            println!("[NTP] response: {} bytes", len);
            // Reject unsynchronised/invalid server replies before touching the
            // RTC. LI=3 means alarm, server mode is 4, and stratum 0/16 is
            // unsynchronised/reserved.
            let li = response[0] >> 6;
            let version = (response[0] >> 3) & 0x07;
            let mode = response[0] & 0x07;
            let stratum = response[1];
            if li == 3 || !(3..=4).contains(&version) || mode != 4 || !(1..=15).contains(&stratum) {
                println!(
                    "[NTP] rejected invalid server reply li={} version={} mode={} stratum={}",
                    li, version, mode, stratum
                );
                return Err(());
            }
            // Parse NTP timestamp (bytes 40-43 = seconds since 1900-01-01)
            let ntp_secs =
                u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
            // Convert epochs with checked arithmetic. Time-zone presentation is
            // a UI/settings concern; RTC is kept in UTC to avoid hard-coded DST.
            let unix_secs = ntp_secs.checked_sub(2_208_988_800).ok_or(())?;
            let time_of_day = unix_secs % 86400;
            let hours = (time_of_day / 3600) as u8;
            let minutes = ((time_of_day % 3600) / 60) as u8;
            let seconds = (time_of_day % 60) as u8;

            // Calculate date (days since epoch)
            let total_days = (unix_secs / 86400) as i32;
            let (year, month, day) = days_to_date(total_days);

            println!(
                "[NTP] Time: {:02}:{:02}:{:02} {:02}/{:02}/{}",
                hours, minutes, seconds, day, month, year
            );

            if !(2000..=2255).contains(&year) {
                println!("[NTP] rejected RTC-unrepresentable year {}", year);
                return Err(());
            }
            Ok(DateTime::new(
                (year - 2000) as u8,
                month as u8,
                day as u8,
                hours,
                minutes,
                seconds,
            ))
        }
        Ok(Ok((_len, _addr))) => {
            println!("[NTP] short response");
            Err(())
        }
        Ok(Err(_)) => {
            println!("[NTP] UDP receive failed");
            Err(())
        }
        Err(_) => {
            println!("[NTP] receive timeout");
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_to_date_epoch_zero() {
        assert_eq!(days_to_date(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_date_negative_fallback() {
        assert_eq!(days_to_date(-1), (2000, 1, 1));
    }

    #[test]
    fn days_to_date_leap_day_2024() {
        // 2024-02-29:
        // Days from 1970 to 2024: 54 years, with 13 leap years (1972, 76, 80, 84, 88, 92, 96, 2000, 04, 08, 12, 16, 20)
        // 54 * 365 + 13 = 19710 + 13 = 19723 days to 2024-01-01
        // January has 31 days -> 19723 + 31 = 19754 is 2024-02-01
        // + 28 days = 19782 is 2024-02-29
        assert_eq!(days_to_date(19782), (2024, 2, 29));
    }

    #[test]
    fn days_to_date_year_cap() {
        // Far future beyond 2255
        assert_eq!(days_to_date(200_000), (2000, 1, 1));
    }
}
