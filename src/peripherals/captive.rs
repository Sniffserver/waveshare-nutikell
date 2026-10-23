// Minimal captive-portal services (DHCP + DNS + HTTP) for WiFi provisioning

use edge_dhcp::{
    server::{Action, ServerOptions},
    Ipv4Addr, Options, Packet,
};
use embassy_net::{
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
    IpAddress, IpEndpoint, IpListenEndpoint, Stack,
};
use embassy_time::Duration;
use esp_println::println;

use crate::peripherals::nvs::{write_credentials, StoredCredentials};

const FORM_BODY: &[u8] = b"<!DOCTYPE html><html><head><meta name='viewport' content='width=device-width,initial-scale=1'><title>Watch WiFi Setup</title><style>body{font-family:sans-serif;background:#111;color:#eee;padding:20px;max-width:380px;margin:auto}h2{color:#4af;margin-top:0}label{display:block;margin-top:14px;font-size:14px;color:#aaa}input{width:100%;padding:10px;margin-top:5px;background:#222;color:#fff;border:1px solid #4af;border-radius:6px;box-sizing:border-box;font-size:16px}button{margin-top:24px;width:100%;padding:14px;background:#4af;color:#000;border:none;border-radius:6px;font-size:16px;font-weight:bold;cursor:pointer}</style></head><body><h2>&#9201; Watch WiFi Setup</h2><form method='POST' action='/save'><label>WiFi Network (SSID)<input name='ssid' type='text' maxlength='32' required placeholder='MyNetwork'></label><label>Password<input name='pass' type='password' maxlength='63' placeholder='(leave blank for open)'></label><button type='submit'>Save &amp; Connect</button></form></body></html>";

const OK_BODY: &[u8] = b"<!DOCTYPE html><html><body style='font-family:sans-serif;background:#111;color:#eee;padding:30px;text-align:center'><h2 style='color:#4af'>&#10003; Saved!</h2><p>Watch is connecting to your WiFi.<br>You can close this page.</p></body></html>";

const ERR_BODY: &[u8] = b"<!DOCTYPE html><html><body style='background:#111;color:#f44;padding:30px'><h2>Error: bad form data</h2></body></html>";

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

/// RFC 2131 compliant DHCP server powered by edge-dhcp
pub async fn run_dhcp_server(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 1024];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 1024];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(67).is_err() {
        println!("[DHCP] Failed to bind port 67");
        return;
    }
    println!("[DHCP] Server listening on port 67");

    let ip = Ipv4Addr::new(192, 168, 4, 1);
    let mut gw = [ip];
    let dns = [ip];
    let mut server_options = ServerOptions::new(ip, Some(&mut gw));
    server_options.dns = &dns;
    server_options.captive_url = Some("http://192.168.4.1");

    let client_ip = Ipv4Addr::new(192, 168, 4, 2);

    let mut in_buf = [0u8; 1024];
    let mut out_buf = [0u8; 1024];

    loop {
        let mut opt_buf = Options::buf();
        let (n, _from) = match sock.recv_from(&mut in_buf).await {
            Ok(res) => res,
            Err(_) => continue,
        };

        let packet = match Packet::decode(&in_buf[..n]) {
            Ok(p) => p,
            Err(e) => {
                println!("[DHCP] Decode error: {:?}", e);
                continue;
            }
        };

        let action = match server_options.process(&packet) {
            Some(a) => a,
            None => continue,
        };

        let reply = match action {
            Action::Discover(_req_ip, _mac) => {
                println!("[DHCP] Discover -> sending Offer 192.168.4.2");
                server_options.offer(&packet, client_ip, &mut opt_buf)
            }
            Action::Request(req_ip, _mac) => {
                println!("[DHCP] Request ({:?}) -> sending Ack", req_ip);
                server_options.ack_nak(&packet, Some(client_ip), &mut opt_buf)
            }
            Action::Release(_, _) | Action::Decline(_, _) => continue,
        };

        match reply.encode(&mut out_buf) {
            Ok(bytes) => {
                let dest = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
                if let Err(e) = sock.send_to(bytes, dest).await {
                    println!("[DHCP] Send error: {:?}", e);
                } else {
                    println!("[DHCP] Response sent ({} bytes)", bytes.len());
                }
            }
            Err(e) => println!("[DHCP] Encode error: {:?}", e),
        }
    }
}

/// Captive DNS server redirecting all domain queries to 192.168.4.1
pub async fn run_dns_server(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 512];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(53).is_err() {
        println!("[DNS] Failed to bind port 53");
        return;
    }
    println!("[DNS] Captive DNS ready on port 53");

    loop {
        let mut buf = [0u8; 512];
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(res) => res,
            Err(_) => continue,
        };
        if n < 12 {
            continue;
        }

        let mut resp = [0u8; 512];
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
        resp[a_idx..a_idx + 2].copy_from_slice(&[0xc0, 0x0c]);
        a_idx += 2; // Pointer to name
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x01]);
        a_idx += 2; // Type A
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x01]);
        a_idx += 2; // Class IN
        resp[a_idx..a_idx + 4].copy_from_slice(&[0x00, 0x00, 0x00, 0x3c]);
        a_idx += 4; // TTL 60
        resp[a_idx..a_idx + 2].copy_from_slice(&[0x00, 0x04]);
        a_idx += 2; // Data length 4
        resp[a_idx..a_idx + 4].copy_from_slice(&[192, 168, 4, 1]);
        a_idx += 4; // IP

        let _ = sock.send_to(&resp[..a_idx], from).await;
    }
}

/// Serve one HTTP connection.
/// FIX: properly break out of read loop when headers are fully received.
pub async fn serve_one(stack: Stack<'static>) -> ServeResult {
    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 1536];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(8)));

    let endpoint = IpListenEndpoint {
        addr: None,
        port: 80,
    };
    if socket.accept(endpoint).await.is_err() {
        return ServeResult::Idle;
    }

    println!(
        "[HTTP] Client connected from {:?}",
        socket.remote_endpoint()
    );

    let mut req = [0u8; 1024];
    let mut total = 0usize;

    // Read until we have the full HTTP headers (and body for POST).
    // CRITICAL: use a flag to break the OUTER loop, not just the inner for-loop.
    'read: loop {
        match socket.read(&mut req[total..]).await {
            Ok(0) => break 'read,
            Ok(n) => {
                total += n;
                // Check if we've received the end of headers
                if total >= 4 {
                    let mut header_end = None;
                    for i in 0..total.saturating_sub(3) {
                        if &req[i..i + 4] == b"\r\n\r\n" {
                            header_end = Some(i + 4);
                            break;
                        }
                    }
                    if let Some(hend) = header_end {
                        let content_length = extract_content_length(&req[..hend.saturating_sub(4)]);
                        if total >= hend + content_length {
                            break 'read; // Got full request
                        }
                    }
                }
                if total >= req.len() {
                    break 'read; // Buffer full
                }
            }
            Err(_) => break 'read,
        }
    }

    if total < 4 {
        return ServeResult::Idle;
    }

    let req_slice = &req[..total];
    let first_line = core::str::from_utf8(req_slice)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    println!("[HTTP] Request: {}", first_line);

    if req_slice.starts_with(b"POST /save") {
        let body = extract_body(req_slice);
        match parse_form_creds(body) {
            Some(creds) => {
                println!("[HTTP] Saving credentials for SSID: {}", creds.ssid_str());
                send_response(&mut socket, "200 OK", OK_BODY).await;
                socket.close();
                let _ = write_credentials(&creds);
                ServeResult::CredentialsSaved(creds)
            }
            None => {
                println!("[HTTP] Error: invalid form data");
                send_response(&mut socket, "400 Bad Request", ERR_BODY).await;
                socket.close();
                ServeResult::Pending
            }
        }
    } else if req_slice.starts_with(b"GET /favicon.ico") {
        send_empty(&mut socket, "404 Not Found").await;
        socket.close();
        ServeResult::Pending
    } else if req_slice.starts_with(b"GET / ")
        || req_slice.starts_with(b"GET / HTTP/")
        || req_slice.starts_with(b"GET /index.html")
        || req_slice.starts_with(b"GET /hotspot-detect")
        || req_slice.starts_with(b"GET /generate_204")
        || req_slice.starts_with(b"GET /connecttest")
        || req_slice.starts_with(b"GET /ncsi")
        || req_slice.starts_with(b"GET /redirect")
    {
        println!("[HTTP] Serving setup HTML form ({} bytes)", FORM_BODY.len());
        send_response(&mut socket, "200 OK", FORM_BODY).await;
        socket.close();
        ServeResult::Pending
    } else {
        println!("[HTTP] Captive redirect for {}", first_line);
        send_redirect(&mut socket).await;
        socket.close();
        ServeResult::Pending
    }
}

async fn send_response(socket: &mut TcpSocket<'_>, status: &str, body: &[u8]) {
    let header = alloc::format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        body.len()
    );
    let _ = write_all(socket, header.as_bytes()).await;
    let _ = write_all(socket, body).await;
    let _ = socket.flush().await;
}

async fn send_redirect(socket: &mut TcpSocket<'_>) {
    let resp = b"HTTP/1.1 302 Found\r\nLocation: http://192.168.4.1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let _ = write_all(socket, resp).await;
    let _ = socket.flush().await;
}

async fn send_empty(socket: &mut TcpSocket<'_>, status: &str) {
    let header = alloc::format!(
        "HTTP/1.1 {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        status
    );
    let _ = write_all(socket, header.as_bytes()).await;
    let _ = socket.flush().await;
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
        if &request[i..i + 4] == b"\r\n\r\n" {
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
