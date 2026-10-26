// Minimal HTTP GET/POST client using embassy-net TCP
// No external crate needed - just raw TCP + HTTP/1.1

use embassy_net::{tcp::TcpSocket, Stack};
use embassy_time::Duration;

/// Write the entire buffer to a TCP socket, handling partial writes.
async fn write_all(socket: &mut TcpSocket<'_>, buf: &[u8]) -> Result<(), HttpError> {
    let mut written = 0;
    while written < buf.len() {
        match socket.write(&buf[written..]).await {
            Ok(0) => return Err(HttpError::Transport),
            Ok(n) => written += n,
            Err(_) => return Err(HttpError::Transport),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    Transport,
    InvalidUrl,
    RequestTooLarge,
    HeaderTooLarge,
    InvalidResponse,
    UnsupportedTransferEncoding,
    LengthMismatch,
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
    RequestTooLarge,
    HeaderTooLarge,
    InvalidResponse,
    UnsupportedTransferEncoding,
    LengthMismatch,
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
    let (host, port, path) = parse_url(url).map_err(|_| HttpStreamError::InvalidResponse)?;
    let ip = parse_ip(host).map_err(|_| HttpStreamError::InvalidResponse)?;

    let mut socket_rx = [0u8; 1024];
    let mut socket_tx = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut socket_rx, &mut socket_tx);
    socket.set_timeout(Some(Duration::from_secs(5)));
    socket
        .connect((ip, port))
        .await
        .map_err(|_| HttpStreamError::Transport)?;

    let mut request = [0u8; 256];
    let request_len = format_request(&mut request, "GET", host, path)
        .map_err(|_| HttpStreamError::RequestTooLarge)?;
    write_all(&mut socket, &request[..request_len])
        .await
        .map_err(|_| HttpStreamError::Transport)?;

    let mut chunk = [0u8; 512];
    let mut headers = [0u8; 512];
    let mut header_len = 0usize;
    let mut body_len = 0usize;
    let mut status = None;
    let mut declared_len = None;

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
            body_len = body_len
                .checked_add(read)
                .ok_or(HttpStreamError::LengthMismatch)?;
            if declared_len.is_some_and(|expected| body_len > expected) {
                return Err(HttpStreamError::LengthMismatch);
            }
            continue;
        }

        let take = read.min(headers.len().saturating_sub(header_len));
        headers[header_len..header_len + take].copy_from_slice(&chunk[..take]);
        header_len += take;

        if let Some(body_start) = header_end(&headers[..header_len]) {
            let response_status =
                response_status(&headers[..body_start]).ok_or(HttpStreamError::InvalidResponse)?;
            declared_len = response_content_length(&headers[..body_start])?;
            status = Some(response_status);

            if response_status != 200 {
                return Ok(response_status);
            }

            let body = &headers[body_start..header_len];
            if !body.is_empty() {
                sink.on_chunk(body).map_err(HttpStreamError::Sink)?;
                body_len = body_len
                    .checked_add(body.len())
                    .ok_or(HttpStreamError::LengthMismatch)?;
            }
            let remainder = &chunk[take..read];
            if !remainder.is_empty() {
                sink.on_chunk(remainder).map_err(HttpStreamError::Sink)?;
                body_len = body_len
                    .checked_add(remainder.len())
                    .ok_or(HttpStreamError::LengthMismatch)?;
            }
            if declared_len.is_some_and(|expected| body_len > expected) {
                return Err(HttpStreamError::LengthMismatch);
            }
        } else if take < read || header_len == headers.len() {
            return Err(HttpStreamError::HeaderTooLarge);
        }
    }

    let response_status = status.ok_or(HttpStreamError::InvalidResponse)?;
    if response_status == 200 {
        if declared_len.is_some_and(|expected| expected != body_len) {
            return Err(HttpStreamError::LengthMismatch);
        }
        sink.on_complete(body_len).map_err(HttpStreamError::Sink)?;
    }
    Ok(response_status)
}

/// Send an HTTP GET request and return the response.
/// URL format: "http://host:port/path" or "http://host/path" (port defaults to 80)
pub async fn http_get<'a>(stack: Stack<'a>, url: &str) -> Result<HttpResponse, HttpError> {
    let (host, port, path) = parse_url(url).map_err(|_| HttpError::InvalidUrl)?;

    // Resolve host to IP (simple: try parsing as IP directly)
    let ip = parse_ip(host).map_err(|_| HttpError::InvalidUrl)?;
    let endpoint = (ip, port);

    // TCP connect
    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(5)));

    socket
        .connect(endpoint)
        .await
        .map_err(|_| HttpError::Transport)?;

    // Send HTTP GET request
    let mut req_buf = [0u8; 256];
    let req_len = format_request(&mut req_buf, "GET", host, path)?;
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
pub async fn http_post<'a>(
    stack: Stack<'a>,
    url: &str,
    body: &str,
) -> Result<HttpResponse, HttpError> {
    let (host, port, path) = parse_url(url).map_err(|_| HttpError::InvalidUrl)?;
    let ip = parse_ip(host).map_err(|_| HttpError::InvalidUrl)?;
    let endpoint = (ip, port);

    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 512];
    let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(5)));

    socket
        .connect(endpoint)
        .await
        .map_err(|_| HttpError::Transport)?;

    let mut req_buf = [0u8; 384];
    let req_len = format_post_request(&mut req_buf, host, path, body)?;
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

fn append(buf: &mut [u8], pos: &mut usize, bytes: &[u8]) -> Result<(), HttpError> {
    let end = pos
        .checked_add(bytes.len())
        .ok_or(HttpError::RequestTooLarge)?;
    if end > buf.len() {
        return Err(HttpError::RequestTooLarge);
    }
    buf[*pos..end].copy_from_slice(bytes);
    *pos = end;
    Ok(())
}

fn append_decimal(buf: &mut [u8], pos: &mut usize, mut value: usize) -> Result<(), HttpError> {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    append(buf, pos, &digits[start..])
}

fn format_request(
    buf: &mut [u8],
    method: &str,
    host: &str,
    path: &str,
) -> Result<usize, HttpError> {
    let mut pos = 0;
    append(buf, &mut pos, method.as_bytes())?;
    append(buf, &mut pos, b" ")?;
    append(buf, &mut pos, path.as_bytes())?;
    append(buf, &mut pos, b" HTTP/1.1\r\nHost: ")?;
    append(buf, &mut pos, host.as_bytes())?;
    append(buf, &mut pos, b"\r\nConnection: close\r\n\r\n")?;
    Ok(pos)
}

fn format_post_request(
    buf: &mut [u8],
    host: &str,
    path: &str,
    body: &str,
) -> Result<usize, HttpError> {
    let mut pos = 0;
    append(buf, &mut pos, b"POST ")?;
    append(buf, &mut pos, path.as_bytes())?;
    append(buf, &mut pos, b" HTTP/1.1\r\nHost: ")?;
    append(buf, &mut pos, host.as_bytes())?;
    append(
        buf,
        &mut pos,
        b"\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: ",
    )?;
    append_decimal(buf, &mut pos, body.len())?;
    append(buf, &mut pos, b"\r\n\r\n")?;
    append(buf, &mut pos, body.as_bytes())?;
    Ok(pos)
}

fn parse_response(data: &[u8]) -> Result<HttpResponse, HttpError> {
    if data.len() < 12 {
        return Err(HttpError::InvalidResponse);
    }

    // Parse "HTTP/1.x STATUS"
    let status_str = core::str::from_utf8(&data[9..12]).map_err(|_| HttpError::InvalidResponse)?;
    let status = status_str
        .parse::<u16>()
        .map_err(|_| HttpError::InvalidResponse)?;

    // Find body (after \r\n\r\n)
    let body_start = header_end(data).ok_or(HttpError::InvalidResponse)?;

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

/// Validate framing that this deliberately small HTTP/1.1 client supports.
/// Chunked responses are rejected rather than accidentally presenting chunk
/// markers as JSON to a companion parser.
fn response_content_length(headers: &[u8]) -> Result<Option<usize>, HttpStreamError> {
    let text = core::str::from_utf8(headers).map_err(|_| HttpStreamError::InvalidResponse)?;
    let mut length = None;
    for line in text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.trim().eq_ignore_ascii_case("transfer-encoding")
            && !value.is_empty()
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err(HttpStreamError::UnsupportedTransferEncoding);
        }
        if name.trim().eq_ignore_ascii_case("content-length") {
            let parsed = value
                .parse::<usize>()
                .map_err(|_| HttpStreamError::InvalidResponse)?;
            if length.replace(parsed).is_some() {
                return Err(HttpStreamError::InvalidResponse);
            }
        }
    }
    Ok(length)
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

    #[test]
    fn requests_do_not_truncate_and_post_length_is_exact() {
        let mut tiny = [0u8; 8];
        assert_eq!(
            format_request(&mut tiny, "GET", "127.0.0.1", "/"),
            Err(HttpError::RequestTooLarge)
        );

        let body = "x".repeat(1000);
        let mut request = [0u8; 1200];
        let length = format_post_request(&mut request, "127.0.0.1", "/sync", &body).unwrap();
        let request = core::str::from_utf8(&request[..length]).unwrap();
        assert!(request.contains("Content-Length: 1000\r\n"));
        assert!(request.ends_with(&body));
    }

    #[test]
    fn response_framing_rejects_chunked_and_invalid_lengths() {
        assert_eq!(
            response_content_length(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HttpStreamError::UnsupportedTransferEncoding),
        );
        assert_eq!(
            response_content_length(b"HTTP/1.1 200 OK\r\nContent-Length: nope\r\n\r\n"),
            Err(HttpStreamError::InvalidResponse),
        );
    }
}
