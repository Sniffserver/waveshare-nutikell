// Minimal HTTP GET/POST client using embassy-net TCP
// No external crate needed - just raw TCP + HTTP/1.1

use embassy_net::{tcp::TcpSocket, Stack};
use embassy_time::Duration;

/// Write the entire buffer to a TCP socket, handling partial writes.
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

/// Simple HTTP response (status + truncated body)
pub struct HttpResponse {
    pub status: u16,
    pub body: [u8; 128],
    pub body_len: usize,
}

/// Errors returned by a consumer that receives a streamed response body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpSinkError {
    Rejected,
}

/// Receives a successful HTTP response body without requiring the transport to
/// retain the whole body. Implementations must apply their own bounded limits.
pub trait HttpResponseSink {
    fn on_chunk(&mut self, chunk: &[u8]) -> Result<(), HttpSinkError>;
    fn on_complete(&mut self, total_len: usize) -> Result<(), HttpSinkError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpStreamError {
    Transport,
    InvalidResponse,
    Sink(HttpSinkError),
}

/// Send an HTTP GET and stream only its body to `sink`.
///
/// The response headers are capped at 512 bytes, but the body is never
/// accumulated by this function. This is the required path for companion
/// snapshots: a 12 KiB JSON document must not be placed on an Embassy task
/// stack merely to get from Wi-Fi to a bounded parser.
pub async fn http_get_stream<'a, S: HttpResponseSink>(
    stack: Stack<'a>,
    url: &str,
    sink: &mut S,
) -> Result<u16, HttpStreamError> {
    let (host, port, path) = parse_url(url).map_err(|_| HttpStreamError::Transport)?;
    let ip = parse_ip(host).map_err(|_| HttpStreamError::Transport)?;

    let mut socket_rx = [0u8; 1024];
    let mut socket_tx = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut socket_rx, &mut socket_tx);
    socket.set_timeout(Some(Duration::from_secs(5)));
    socket
        .connect((ip, port))
        .await
        .map_err(|_| HttpStreamError::Transport)?;

    let mut request = [0u8; 256];
    let request_len = format_request(&mut request, "GET", host, path);
    write_all(&mut socket, &request[..request_len])
        .await
        .map_err(|_| HttpStreamError::Transport)?;

    let mut chunk = [0u8; 512];
    let mut headers = [0u8; 512];
    let mut header_len = 0usize;
    let mut body_len = 0usize;
    let mut status = None;

    loop {
        let read = socket
            .read(&mut chunk)
            .await
            .map_err(|_| HttpStreamError::Transport)?;
        if read == 0 {
            break;
        }

        if let Some(_) = status {
            sink.on_chunk(&chunk[..read])
                .map_err(HttpStreamError::Sink)?;
            body_len += read;
            continue;
        }

        let remaining = headers.len().saturating_sub(header_len);
        if read > remaining {
            return Err(HttpStreamError::InvalidResponse);
        }
        headers[header_len..header_len + read].copy_from_slice(&chunk[..read]);
        header_len += read;

        if let Some(body_start) = header_end(&headers[..header_len]) {
            let response_status =
                response_status(&headers[..body_start]).ok_or(HttpStreamError::InvalidResponse)?;
            status = Some(response_status);

            if response_status != 200 {
                return Ok(response_status);
            }

            let body = &headers[body_start..header_len];
            if !body.is_empty() {
                sink.on_chunk(body).map_err(HttpStreamError::Sink)?;
                body_len += body.len();
            }
        }
    }

    let response_status = status.ok_or(HttpStreamError::InvalidResponse)?;
    if response_status == 200 {
        sink.on_complete(body_len).map_err(HttpStreamError::Sink)?;
    }
    Ok(response_status)
}

/// Send an HTTP GET request and return the response.
/// URL format: "http://host:port/path" or "http://host/path" (port defaults to 80)
pub async fn http_get<'a>(stack: Stack<'a>, url: &str) -> Result<HttpResponse, ()> {
    let (host, port, path) = parse_url(url)?;

    // Resolve host to IP (simple: try parsing as IP directly)
    let ip = parse_ip(host)?;
    let endpoint = (ip, port);

    // TCP connect
    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(5)));

    socket.connect(endpoint).await.map_err(|_| ())?;

    // Send HTTP GET request
    let mut req_buf = [0u8; 256];
    let req_len = format_request(&mut req_buf, "GET", host, path);
    write_all(&mut socket, &req_buf[..req_len]).await?;

    // Read response
    let mut resp_buf = [0u8; 512];
    let mut total = 0;
    loop {
        match socket.read(&mut resp_buf[total..]).await {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total >= resp_buf.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // Parse status code + body
    parse_response(&resp_buf[..total])
}

/// Send an HTTP POST request
pub async fn http_post<'a>(stack: Stack<'a>, url: &str, body: &str) -> Result<HttpResponse, ()> {
    let (host, port, path) = parse_url(url)?;
    let ip = parse_ip(host)?;
    let endpoint = (ip, port);

    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(5)));

    socket.connect(endpoint).await.map_err(|_| ())?;

    let mut req_buf = [0u8; 384];
    let req_len = format_post_request(&mut req_buf, host, path, body);
    write_all(&mut socket, &req_buf[..req_len]).await?;

    let mut resp_buf = [0u8; 512];
    let mut total = 0;
    loop {
        match socket.read(&mut resp_buf[total..]).await {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total >= resp_buf.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    parse_response(&resp_buf[..total])
}

// === Internal helpers ===

fn parse_url(url: &str) -> Result<(&str, u16, &str), ()> {
    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = match url.find('/') {
        Some(i) => (&url[..i], &url[i..]),
        None => (url, "/"),
    };
    let (host, port) = match host_port.find(':') {
        Some(i) => (
            &host_port[..i],
            host_port[i + 1..].parse::<u16>().unwrap_or(80),
        ),
        None => (host_port, 80),
    };
    Ok((host, port, path))
}

fn parse_ip(host: &str) -> Result<embassy_net::Ipv4Address, ()> {
    let parts: [u8; 4] = {
        let mut parts = [0u8; 4];
        let mut idx = 0;
        for segment in host.split('.') {
            if idx >= 4 {
                return Err(());
            }
            parts[idx] = segment.parse::<u8>().map_err(|_| ())?;
            idx += 1;
        }
        if idx != 4 {
            return Err(());
        }
        parts
    };
    Ok(embassy_net::Ipv4Address::new(
        parts[0], parts[1], parts[2], parts[3],
    ))
}

fn format_request(buf: &mut [u8], method: &str, host: &str, path: &str) -> usize {
    let mut pos = 0;
    for &b in method.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    if pos < buf.len() {
        buf[pos] = b' ';
        pos += 1;
    }
    for &b in path.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in b" HTTP/1.1\r\nHost: " {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in host.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in b"\r\nConnection: close\r\n\r\n" {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    pos
}

fn format_post_request(buf: &mut [u8], host: &str, path: &str, body: &str) -> usize {
    let mut pos = 0;
    for &b in b"POST " {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in path.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in b" HTTP/1.1\r\nHost: " {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in host.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in b"\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: " {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    // Content length
    let len = body.len();
    if len >= 100 {
        if pos < buf.len() {
            buf[pos] = b'0' + (len / 100) as u8;
            pos += 1;
        }
    }
    if len >= 10 {
        if pos < buf.len() {
            buf[pos] = b'0' + ((len / 10) % 10) as u8;
            pos += 1;
        }
    }
    if pos < buf.len() {
        buf[pos] = b'0' + (len % 10) as u8;
        pos += 1;
    }
    for &b in b"\r\n\r\n" {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in body.as_bytes() {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    pos
}

fn parse_response(data: &[u8]) -> Result<HttpResponse, ()> {
    if data.len() < 12 {
        return Err(());
    }

    // Parse "HTTP/1.x STATUS"
    let status_str = core::str::from_utf8(&data[9..12]).map_err(|_| ())?;
    let status = status_str.parse::<u16>().unwrap_or(0);

    // Find body (after \r\n\r\n)
    let mut body_start = data.len();
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            body_start = i + 4;
            break;
        }
    }

    let mut resp = HttpResponse {
        status,
        body: [0u8; 128],
        body_len: 0,
    };

    let body_data = &data[body_start..];
    let copy_len = body_data.len().min(128);
    resp.body[..copy_len].copy_from_slice(&body_data[..copy_len]);
    resp.body_len = copy_len;

    Ok(resp)
}

fn header_end(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn response_status(headers: &[u8]) -> Option<u16> {
    if headers.len() < 12 || &headers[..5] != b"HTTP/" {
        return None;
    }
    core::str::from_utf8(&headers[9..12]).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parser_preserves_body_boundary() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        let body_start = header_end(response).expect("header terminator");
        assert_eq!(response_status(&response[..body_start]), Some(200));
        assert_eq!(&response[body_start..], b"{}");
    }

    #[test]
    fn malformed_or_incomplete_headers_are_rejected() {
        assert_eq!(header_end(b"HTTP/1.1 200 OK\r\n"), None);
        assert_eq!(response_status(b"NOT HTTP\r\n\r\n"), None);
        assert_eq!(response_status(b"HTTP/1.1 XX0\r\n\r\n"), None);
    }
}
