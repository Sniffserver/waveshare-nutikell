// Minimal captive-portal HTTP server for WiFi provisioning
//
// The watch runs in AP mode with SSID "Watch-Setup" and an open password.
// It listens on TCP port 80. Any GET / returns an HTML form.
// A POST to /save with form body `ssid=...&pass=...` saves to NVS and returns 200.
//
// The caller should poll `serve_one()` in a loop while in provisioning mode.

use embassy_net::{tcp::TcpSocket, IpListenEndpoint, Stack};
use embassy_time::Duration;

use crate::peripherals::nvs::{write_credentials, StoredCredentials};

const HTML_FORM: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
<!DOCTYPE html><html><head>\
<meta name='viewport' content='width=device-width,initial-scale=1'>\
<style>body{font-family:sans-serif;background:#111;color:#eee;padding:20px;max-width:400px;margin:auto}\
h2{color:#4af}label{display:block;margin-top:12px}input{width:100%;padding:8px;margin-top:4px;\
background:#222;color:#fff;border:1px solid #4af;border-radius:4px}\
button{margin-top:20px;width:100%;padding:12px;background:#4af;color:#000;\
border:none;border-radius:6px;font-size:16px;cursor:pointer}</style>\
</head><body>\
<h2>&#9201; Watch WiFi Setup</h2>\
<form method='POST' action='/save'>\
<label>WiFi Network (SSID)<input name='ssid' type='text' maxlength='32' required placeholder='MyNetwork'></label>\
<label>Password<input name='pass' type='password' maxlength='63' placeholder='(leave blank for open)'></label>\
<button type='submit'>Save &amp; Connect</button>\
</form></body></html>";

const HTML_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
<!DOCTYPE html><html><body style='font-family:sans-serif;background:#111;color:#eee;padding:30px;text-align:center'>\
<h2 style='color:#4af'>&#10003; Saved!</h2>\
<p>Watch is connecting to your WiFi.<br>You can close this page.</p></body></html>";

const HTML_ERR: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
<!DOCTYPE html><html><body style='background:#111;color:#f44;padding:30px'><h2>Error: bad form data</h2></body></html>";

/// Result of one `serve_one()` call.
pub enum ServeResult {
    /// Still waiting — client connected but no credentials received yet.
    Pending,
    /// Client submitted valid credentials — saved to NVS.
    CredentialsSaved(StoredCredentials),
    /// TCP error or empty request — caller should just loop again.
    Idle,
}

/// Serve one HTTP connection. Runs until the connection closes.
/// Call in a loop while in provisioning mode.
pub async fn serve_one(stack: Stack<'_>) -> ServeResult {
    let mut rx_buf = [0u8; 2048];
    let mut tx_buf = [0u8; 2048];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(10)));

    let endpoint = IpListenEndpoint {
        addr: None,
        port: 80,
    };
    if socket.accept(endpoint).await.is_err() {
        return ServeResult::Idle;
    }

    // Read the full request (up to 1800 bytes)
    let mut req = [0u8; 1800];
    let mut total = 0usize;
    loop {
        match socket.read(&mut req[total..]).await {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                // Stop when we see end-of-headers (we only care about the body)
                if total >= 4 {
                    for i in 0..total.saturating_sub(3) {
                        if &req[i..i + 4] == b"\r\n\r\n" {
                            // For POST we still need body; for GET this is enough
                            // Heuristic: wait until we have all body bytes
                            let header_end = i + 4;
                            let content_length = extract_content_length(&req[..i]);
                            if total >= header_end + content_length {
                                total = total; // break outer
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

    // Route on method + path
    if req_slice.starts_with(b"POST /save") {
        // Extract form body
        let body = extract_body(req_slice);
        match parse_form_creds(body) {
            Some(creds) => {
                // Send OK page
                let _ = write_all(&mut socket, HTML_OK).await;
                socket.close();
                // Persist to NVS
                let _ = write_credentials(&creds);
                ServeResult::CredentialsSaved(creds)
            }
            None => {
                let _ = write_all(&mut socket, HTML_ERR).await;
                socket.close();
                ServeResult::Pending
            }
        }
    } else {
        // Serve the form for any other request (GET / or captive portal probes)
        let _ = write_all(&mut socket, HTML_FORM).await;
        socket.close();
        ServeResult::Pending
    }
}

// === Internal helpers ===

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

/// Parse `ssid=...&pass=...` URL-encoded form body.
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

/// Minimal URL percent-decoding and `+` → space.
/// Returns a stack buffer (up to 96 bytes).
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
