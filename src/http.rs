//! One HTTP/1.x request per connection behind Caddy. No keepalive or chunked bodies.
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
pub(crate) struct Request {
    pub timestamp: String,
    pub signature: String,
    pub body: Vec<u8>,
}
fn read_more(stream: &mut TcpStream, buffer: &mut [u8], deadline: Instant) -> Result<usize, u16> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(408u16)?;
    if remaining.is_zero() {
        return Err(408);
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| 500u16)?;
    match stream.read(buffer) {
        Ok(0) => Err(400),
        Ok(n) => Ok(n),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            Err(408)
        }
        Err(_) => Err(400),
    }
}
pub(crate) fn read(stream: &mut TcpStream, deadline: Instant) -> Result<Request, u16> {
    const MAX_HEADERS: usize = 8192;
    const MAX_BODY: usize = 65536;
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(pos) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
            break pos + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(431);
        }
        let mut chunk = [0u8; 1024];
        let limit = chunk.len().min(MAX_HEADERS - bytes.len());
        let n = read_more(stream, &mut chunk[..limit], deadline)?;
        bytes.extend_from_slice(&chunk[..n]);
    };
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Request::new(&mut headers);
    if parsed.parse(&bytes[..end]).map_err(|_| 400u16)? != httparse::Status::Complete(end) {
        return Err(400);
    }
    if parsed.method != Some("POST") || parsed.path != Some("/interactions") {
        return Err(404);
    }
    let mut length = None;
    let mut timestamp = None;
    let mut signature = None;
    for header in parsed.headers.iter() {
        let name = header.name;
        let value = std::str::from_utf8(header.value).map_err(|_| 400u16)?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(400);
            }
            length = Some(value.parse::<usize>().map_err(|_| 413u16)?);
        } else if name.eq_ignore_ascii_case("Transfer-Encoding")
            || name.eq_ignore_ascii_case("Expect")
        {
            return Err(400);
        } else if name.eq_ignore_ascii_case("X-Signature-Timestamp") {
            if timestamp.replace(value.to_owned()).is_some() {
                return Err(400);
            }
        } else if name.eq_ignore_ascii_case("X-Signature-Ed25519")
            && signature.replace(value.to_owned()).is_some()
        {
            return Err(400);
        }
    }
    let length = length.ok_or(411u16)?;
    if length > MAX_BODY {
        return Err(413);
    }
    // Reject missing/malformed signature headers before waiting for any body.
    let timestamp = timestamp
        .filter(|x| !x.is_empty() && x.len() <= 20 && x.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(401u16)?;
    let signature = signature
        .filter(|x| x.len() == 128 && x.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(401u16)?;
    while bytes.len() < end + length {
        let mut chunk = [0u8; 8192];
        let limit = chunk.len().min(end + length - bytes.len());
        let n = read_more(stream, &mut chunk[..limit], deadline)?;
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(Request {
        timestamp,
        signature,
        body: bytes[end..end + length].to_vec(),
    })
}
pub(crate) fn respond(stream: &mut TcpStream, code: u16, body: &[u8]) -> std::io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_millis(100)))?;
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut response = header.into_bytes();
    response.extend_from_slice(body);
    stream.write_all(&response)?;
    stream.shutdown(std::net::Shutdown::Write)
}
pub(crate) fn reject(stream: &mut TcpStream, code: u16) {
    let _ = respond(stream, code, b"");
    // Closing with unread request bytes can reset TCP and discard the response.
    // After sending FIN, drain only a bounded amount and never wait indefinitely.
    let deadline = Instant::now() + Duration::from_millis(100);
    let mut remaining = 8192 + 65536;
    while remaining > 0 {
        let mut buffer = [0u8; 8192];
        let limit = buffer.len().min(remaining);
        match read_more(stream, &mut buffer[..limit], deadline) {
            Ok(n) => remaining -= n,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (client, listener.accept().unwrap().0)
    }
    fn wire(extra: &str) -> String {
        format!(
            "POST /interactions HTTP/1.1\r\nHost: localhost\r\nX-Signature-Timestamp: 1000\r\nX-Signature-Ed25519: {}\r\n{extra}\r\n",
            "0".repeat(128)
        )
    }
    fn parse(text: &str) -> Result<Request, u16> {
        let (mut client, mut server) = pair();
        client.write_all(text.as_bytes()).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        read(&mut server, Instant::now() + Duration::from_secs(1))
    }
    #[test]
    fn bounded_http_request_preserves_exact_signed_body() {
        let body = "{\"type\":1}";
        let request =
            parse(&(wire(&format!("Content-Length: {}\r\n", body.len())) + body)).unwrap();
        assert_eq!(request.body, body.as_bytes());
        assert_eq!(request.timestamp, "1000");
    }
    #[test]
    fn ambiguous_framing_and_oversized_requests_are_rejected() {
        for headers in [
            "Content-Length: 2\r\nContent-Length: 2\r\n",
            "Content-Length: 2\r\nTransfer-Encoding: chunked\r\n",
            "Content-Length: -1\r\n",
            "Expect: 100-continue\r\nContent-Length: 2\r\n",
        ] {
            assert_eq!(parse(&wire(headers)).err(), Some(400));
        }
        assert_eq!(parse(&wire("Content-Length: 65537\r\n")).err(), Some(413));
        assert_eq!(parse(&wire("")).err(), Some(411));
        assert_eq!(
            parse(&format!(
                "POST /interactions HTTP/1.1\r\nX: {}",
                "x".repeat(8192)
            ))
            .err(),
            Some(431)
        );
    }
    #[test]
    fn missing_signature_does_not_wait_for_body() {
        let (mut client, mut server) = pair();
        client
            .write_all(b"POST /interactions HTTP/1.1\r\nContent-Length: 65536\r\n\r\n{")
            .unwrap();
        assert_eq!(
            read(&mut server, Instant::now() + Duration::from_millis(200)).err(),
            Some(401)
        );
    }
    #[test]
    fn rejection_delivers_complete_response_before_bounded_close() {
        let (mut client, mut server) = pair();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .write_all((wire("Content-Length: 65536\r\n") + "{").as_bytes())
            .unwrap();
        let started = Instant::now();
        let worker = thread::spawn(move || reject(&mut server, 503));
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn partial_headers_and_body_have_absolute_deadlines() {
        for initial in [
            "POST /interactions HTTP/1.1\r\n".to_string(),
            wire("Content-Length: 65536\r\n") + "{",
        ] {
            let (mut client, mut server) = pair();
            client.write_all(initial.as_bytes()).unwrap();
            let worker = thread::spawn(move || {
                read(&mut server, Instant::now() + Duration::from_millis(200)).err()
            });
            // Keep sending bytes: an inactivity timeout alone would not finish.
            let started = Instant::now();
            while !worker.is_finished() && started.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(40));
                let _ = client.write_all(b" ");
            }
            assert_eq!(worker.join().unwrap(), Some(408));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}
