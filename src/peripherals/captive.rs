// Minimal captive-portal services (DHCP + DNS + HTTP) for WiFi provisioning
//
// The watch runs in AP mode with SSID "Watch-Setup" and an open password.
// It provides:
//   1. DHCP server on UDP port 67 (assigns 192.168.4.2 to connecting phones).
//   2. Captive DNS server on UDP port 53 (redirects all domain lookups to 192.168.4.1).
//   3. HTTP server on TCP port 80 (serves WiFi setup form, accepts POST /save, persists to NVS).

use embassy_net::{
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
    IpAddress, IpEndpoint, IpListenEndpoint, Stack,
};
use embassy_time::Duration;
use esp_println::println;

use crate::peripherals::nvs::{write_credentials, StoredCredentials};

const HTML_FORM: &[u8] = b"HTTP/1.1 200 OK
Content-Type: text/html
Connection: close

<!DOCTYPE html><html><head><meta name='viewport' content='width=device-width,initial-scale=1'><style>body{font-family:sans-serif;background:#111;color:#eee;padding:20px;max-width:400px;margin:auto}h2{color:#4af}label{display:block;margin-top:12px}input{width:100%;padding:8px;margin-top:4px;background:#222;color:#fff;border:1px solid #4af;border-radius:4px}button{margin-top:20px;width:100%;padding:12px;background:#4af;color:#000;border:none;border-radius:6px;font-size:16px;cursor:pointer}</style></head><body><h2>&#9201; Watch WiFi Setup</h2><form method='POST' action='/save'><label>WiFi Network (SSID)<input name='ssid' type='text' maxlength='32' required placeholder='MyNetwork'></label><label>Password<input name='pass' type='password' maxlength='63' placeholder='(leave blank for open)'></label><button type='submit'>Save &amp; Connect</button></form></body></html>";

const HTML_OK: &[u8] = b"HTTP/1.1 200 OK
Content-Type: text/html
Connection: close

<!DOCTYPE html><html><body style='font-family:sans-serif;background:#111;color:#eee;padding:30px;text-align:center'><h2 style='color:#4af'>&#10003; Saved!</h2><p>Watch is connecting to your WiFi.<br>You can close this page.</p></body></html>";

const HTML_REDIRECT: &[u8] = b"HTTP/1.1 302 Found
Location: http://192.168.4.1/
Connection: close

";

const HTML_ERR: &[u8] = b"HTTP/1.1 400 Bad Request
Content-Type: text/html
Connection: close

<!DOCTYPE html><html><body style='background:#111;color:#f44;padding:30px'><h2>Error: bad form data</h2></body></html>";

pub enum ServeResult {
    Pending,
    CredentialsSaved(StoredCredentials),
    Idle,
}

/// Run all AP services concurrently (DHCP, DNS, HTTP) until valid credentials are saved.
pub async fn run_ap_services(stack: Stack<'static>) -> StoredCredentials {
    use embassy_futures::select::select3;
    let dhcp_fut = run_dhcp_server(stack);
    let dns_fut = run_dns_server(stack);
    let http_fut = async {
        loop {
            match serve_one(stack).await {
                ServeResult::CredentialsSaved(creds) => return creds,
                _ => {}
            }
        }
    };

    match select3(dhcp_fut, dns_fut, http_fut).await {
        embassy_futures::select::Either3::Third(creds) => creds,
        _ => unreachable!(),
    }
}

/// Minimal DHCP server assigning 192.168.4.2 to the client
pub async fn run_dhcp_server(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 512];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(67).is_err() {
        println!("[DHCP] Failed to bind port 67");
        return;
    }
    println!("[DHCP] Server ready on port 67");

    loop {
        let mut buf = [0u8; 512];
        let (n, _from) = match sock.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(_) => continue,
        };
        if n < 240 || buf[0] != 1 || buf[236..240] != [99, 130, 83, 99] {
            continue;
        }

        // Parse options for message type
        let mut msg_type: Option<u8> = None;
        let mut idx = 240;
        while idx < n {
            let opt = buf[idx];
            if opt == 255 { break; }
            if opt == 0 { idx += 1; continue; }
            if idx + 1 >= n { break; }
            let len = buf[idx + 1] as usize;
            if idx + 2 + len > n { break; }
            if opt == 53 && len == 1 {
                msg_type = Some(buf[idx + 2]);
            }
            idx += 2 + len;
        }

        let resp_type = match msg_type {
            Some(1) => 2, // DISCOVER -> OFFER
            Some(3) => 5, // REQUEST -> ACK
            _ => continue,
        };

        let mut reply = [0u8; 300];
        reply[0] = 2; // BOOTREPLY
        reply[1] = buf[1];
        reply[2] = buf[2];
        reply[3] = 0;
        reply[4..8].copy_from_slice(&buf[4..8]); // xid
        reply[8..10].copy_from_slice(&[0, 0]);
        reply[10..12].copy_from_slice(&buf[10..12]);
        reply[12..16].copy_from_slice(&[0, 0, 0, 0]);
        reply[16..20].copy_from_slice(&[192, 168, 4, 2]); // Client IP
        reply[20..24].copy_from_slice(&[192, 168, 4, 1]); // Server IP
        reply[24..28].copy_from_slice(&[0, 0, 0, 0]);
        reply[28..44].copy_from_slice(&buf[28..44]); // Client MAC
        reply[236..240].copy_from_slice(&[99, 130, 83, 99]); // Cookie

        let mut opt_idx = 240;
        // Option 53: Message Type
        reply[opt_idx..opt_idx+3].copy_from_slice(&[53, 1, resp_type]); opt_idx += 3;
        // Option 54: Server ID
        reply[opt_idx..opt_idx+6].copy_from_slice(&[54, 4, 192, 168, 4, 1]); opt_idx += 6;
        // Option 51: Lease Time (7200s)
        reply[opt_idx..opt_idx+6].copy_from_slice(&[51, 4, 0x00, 0x00, 0x1C, 0x20]); opt_idx += 6;
        // Option 1: Subnet Mask (255.255.255.0)
        reply[opt_idx..opt_idx+6].copy_from_slice(&[1, 4, 255, 255, 255, 0]); opt_idx += 6;
        // Option 3: Router (192.168.4.1)
        reply[opt_idx..opt_idx+6].copy_from_slice(&[3, 4, 192, 168, 4, 1]); opt_idx += 6;
        // Option 6: DNS (192.168.4.1)
        reply[opt_idx..opt_idx+6].copy_from_slice(&[6, 4, 192, 168, 4, 1]); opt_idx += 6;
        // Option 255: End
        reply[opt_idx] = 255; opt_idx += 1;

        println!("[DHCP] Handshake -> offering 192.168.4.2");
        let dest = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
        let _ = sock.send_to(&reply[..opt_idx], dest).await;
    }
}

/// Captive DNS server redirecting all domain queries to 192.168.4.1
pub async fn run_dns_server(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 256];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(53).is_err() {
        println!("[DNS] Failed to bind port 53");
        return;
    }
    println!("[DNS] Captive DNS ready on port 53");

    loop {
        let mut buf = [0u8; 256];
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(_) => continue,
        };
        if n < 12 {
            continue;
        }

        let mut resp = [0u8; 256];
        resp[0..2].copy_from_slice(&buf[0..2]); // ID
        resp[2..4].copy_from_slice(&[0x81, 0x80]); // Standard query response, No error
        resp[4..6].copy_from_slice(&[0x00, 0x01]); // 1 Question
        resp[6..8].copy_from_slice(&[0x00, 0x01]); // 1 Answer
        resp[8..10].copy_from_slice(&[0x00, 0x00]);
        resp[10..12].copy_from_slice(&[0x00, 0x00]);

        // Find query name end
        let mut q_end = 12;
        while q_end < n && buf[q_end] != 0 {
            q_end += 1 + (buf[q_end] as usize);
        }
        q_end += 1; // null byte
        q_end += 4; // QTYPE (2) + QCLASS (2)
        if q_end > n || q_end + 16 > resp.len() {
            continue;
        }

        resp[12..q_end].copy_from_slice(&buf[12..q_end]);

        let mut a_idx = q_end;
        resp[a_idx..a_idx + 2].copy_from_slice(&[0xc0, 0x0c]); a_idx += 2; // Pointer to name
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x01]); a_idx += 2; // Type A
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x01]); a_idx += 2; // Class IN
        resp[a_idx..a_idx + 4].copy_from_slice(&[0x00, 0x00, 0x00, 0x3c]); a_idx += 4; // TTL 60
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x04]); a_idx += 2; // Data length 4
        resp[a_idx..a_idx + 4].copy_from_slice(&[192, 168, 4, 1]); a_idx += 4; // IP

        let _ = sock.send_to(&resp[..a_idx], from).await;
    }
}

/// Serve one HTTP connection.
pub async fn serve_one(stack: Stack<'static>) -> ServeResult {
    let mut rx_buf = [0u8; 512];
    let mut tx_buf = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(10)));

    let endpoint = IpListenEndpoint { addr: None, port: 80 };
    if socket.accept(endpoint).await.is_err() {
        return ServeResult::Idle;
    }

    let mut req = [0u8; 512];
    let mut total = 0usize;
    loop {
        match socket.read(&mut req[total..]).await {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total >= 4 {
                    for i in 0..total.saturating_sub(3) {
                        if &req[i..i + 4] == b"

" {
                            let header_end = i + 4;
                            let content_length = extract_content_length(&req[..i]);
                            if total >= header_end + content_length {
                                break;
                            }
                        }
                    }
                }
                if total >= req.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    if total < 4 {
        return ServeResult::Idle;
    }

    let req_slice = &req[..total];

    if req_slice.starts_with(b"POST /save") {
        let body = extract_body(req_slice);
        match parse_form_creds(body) {
            Some(creds) => {
                let _ = write_all(&mut socket, HTML_OK).await;
                socket.close();
                let _ = write_credentials(&creds);
                ServeResult::CredentialsSaved(creds)
            }
            None => {
                let _ = write_all(&mut socket, HTML_ERR).await;
                socket.close();
                ServeResult::Pending
            }
        }
    } else if req_slice.starts_with(b"GET /") {
        let _ = write_all(&mut socket, HTML_FORM).await;
        socket.close();
        ServeResult::Pending
    } else {
        // Captive probe redirect
        let _ = write_all(&mut socket, HTML_REDIRECT).await;
        socket.close();
        ServeResult::Pending
    }
}

async fn write_all(socket: &mut TcpSocket<'_>, buf: &[u8]) -> Result<(), ()> {
    let mut written = 0;
    while written < buf.len() {
        match socket.write(&buf[written..]).await {
            Ok(0) => return Err(()),
            Ok(n) => written += n,
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

fn extract_content_length(headers: &[u8]) -> usize {
    let s = core::str::from_utf8(headers).unwrap_or("");
    for line in s.lines() {
        let l = line.trim();
        if l.to_ascii_lowercase().starts_with("content-length:") {
            if let Some(v) = l.splitn(2, ':').nth(1) {
                return v.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }
    0
}

fn extract_body(request: &[u8]) -> &[u8] {
    for i in 0..request.len().saturating_sub(3) {
        if &request[i..i + 4] == b"

" {
            return &request[i + 4..];
        }
    }
    b""
}

fn parse_form_creds(body: &[u8]) -> Option<StoredCredentials> {
    let s = core::str::from_utf8(body).ok()?;
    let mut ssid_str: &str = "";
    let mut pass_str: &str = "";

    for part in s.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k.trim() {
                "ssid" => ssid_str = v.trim(),
                "pass" => pass_str = v.trim(),
                _ => {}
            }
        }
    }

    if ssid_str.is_empty() {
        return None;
    }

    let mut creds = StoredCredentials::empty();
    let ssid_bytes = url_decode(ssid_str);
    let pass_bytes = url_decode(pass_str);

    let sl = ssid_bytes.len().min(32);
    creds.ssid[..sl].copy_from_slice(&ssid_bytes[..sl]);
    creds.ssid_len = sl;

    let pl = pass_bytes.len().min(64);
    creds.password[..pl].copy_from_slice(&pass_bytes[..pl]);
    creds.pass_len = pl;

    Some(creds)
}

fn url_decode(s: &str) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_digit(bytes[i + 1]);
                let lo = hex_digit(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                } else {
                    out.push(b'%');
                }
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
