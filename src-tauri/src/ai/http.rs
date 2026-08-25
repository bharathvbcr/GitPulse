//! A minimal HTTP/1.1 client for talking to a local model server.
//!
//! It is deliberately small and deliberately restricted to loopback. GitPulse's
//! AI features are *local* features: the diff of a user's unpublished work is
//! about as sensitive as a payload gets, so the transport itself refuses to
//! address anything but `127.0.0.1`, `::1` or `localhost`, and speaks no TLS
//! because there is no remote host to authenticate. A misconfigured base URL
//! therefore fails as a refusal, never as a silent upload.
//!
//! It also means no new dependency: `std::net` is enough for JSON over
//! loopback, including the chunked responses llama.cpp-family servers send.

use std::io;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Hard ceiling on a response body. A model server answering a completion in
/// gigabytes is a malfunction, and reading it into memory would make the
/// malfunction ours.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Ceiling for one framing line (status, header, chunk-size, trailer).
/// `read_line` has no bound of its own; without this cap a peer streaming
/// newline-less bytes allocates without limit.
const MAX_LINE_BYTES: usize = 16 * 1024;

/// Reads one CRLF-terminated framing line without ever buffering more than
/// [`MAX_LINE_BYTES`]. A short final line without a terminator is accepted
/// (EOF is a legitimate end for sloppy peers); anything longer is a fault.
fn read_line_capped<R: Read>(reader: &mut BufReader<R>) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_LINE_BYTES as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() >= MAX_LINE_BYTES && !bytes.ends_with(b"\n") {
        return Err(format!(
            "framing line exceeds the {MAX_LINE_BYTES} byte cap"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "non-UTF-8 byte in response framing".to_string())
}

/// Enforces one wall-clock deadline across every socket read and write. The
/// per-operation timeouts it installs shrink as the deadline approaches, so a
/// peer dribbling single bytes can no longer keep a transfer alive forever.
struct DeadlineStream {
    inner: TcpStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "overall deadline exceeded"))
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.set_read_timeout(Some(self.remaining()?))?;
        self.inner.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.set_write_timeout(Some(self.remaining()?))?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// Path prefix from the base URL, without a trailing slash ("" or "/v1").
    pub prefix: String,
}

impl Endpoint {
    /// The base URL this endpoint was parsed from, normalised.
    pub fn base_url(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.prefix)
    }
}

/// Parses an OpenAI-compatible base URL, refusing anything not on loopback.
pub fn parse_base_url(base_url: &str) -> Result<Endpoint, String> {
    let trimmed = base_url.trim();
    let rest = match trimmed.split_once("://") {
        Some(("http", rest)) => rest,
        Some(("https", _)) => {
            return Err(
                "https is refused: GitPulse only talks to a model server on this machine, \
                 where there is no remote identity to verify"
                    .into(),
            )
        }
        Some((scheme, _)) => return Err(format!("unsupported URL scheme '{}'", scheme)),
        None => trimmed,
    };

    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err("credentials in the base URL are refused".into());
    }

    let (host, port) = split_host_port(authority)?;
    let host_key = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let is_loopback = matches!(host_key.as_str(), "localhost" | "127.0.0.1" | "::1")
        || host_key.starts_with("127.");
    if !is_loopback {
        return Err(format!(
            "'{}' is not a loopback address; GitPulse's AI features only ever address a model \
             server running on this machine",
            host
        ));
    }

    if path.bytes().any(|b| b <= 0x20 || b == 0x7f || b >= 0x80) {
        return Err(
            "base URL path contains spaces or control characters and would corrupt the request"
                .into(),
        );
    }

    let prefix = path.trim_end_matches('/').to_string();
    Ok(Endpoint {
        host: host_key,
        port,
        prefix,
    })
}

fn split_host_port(authority: &str) -> Result<(String, u16), String> {
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: [::1]:11434
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("malformed IPv6 authority '{}'", authority))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| format!("bad port in '{}'", authority))?,
            None => 80,
        };
        return Ok((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse()
                .map_err(|_| format!("bad port in '{}'", authority))?;
            Ok((host.to_string(), port))
        }
        None => Ok((authority.to_string(), 80)),
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// One request/response round trip. `body` is JSON for a POST, `None` for GET.
pub fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let addr = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}:{}: {}", endpoint.host, endpoint.port, e))?
        .next()
        .ok_or_else(|| format!("no address for {}:{}", endpoint.host, endpoint.port))?;

    let started = Instant::now();
    let stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("cannot reach {}: {}", endpoint.base_url(), e))?;
    let _ = stream.set_nodelay(true);
    let mut bounded = DeadlineStream {
        inner: stream,
        deadline: started + timeout,
    };

    let full_path = format!("{}{}", endpoint.prefix, path);
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nAccept: application/json\r\n\
         User-Agent: GitPulse/0.1 (local)\r\n",
        method, full_path, endpoint.host, endpoint.port
    );
    if let Some(payload) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    head.push_str("\r\n");

    bounded
        .write_all(head.as_bytes())
        .and_then(|_| {
            if let Some(payload) = body {
                bounded.write_all(payload.as_bytes())?;
            }
            bounded.flush()
        })
        .map_err(|e| format!("write to {} failed: {}", endpoint.base_url(), e))?;

    read_response(BufReader::new(bounded))
}

/// Reads a response off any reader, so the framing is testable without a socket.
pub fn read_response<R: Read>(mut reader: BufReader<R>) -> Result<HttpResponse, String> {
    let mut header_bytes = 0usize;
    let status_line = read_line_capped(&mut reader).map_err(|e| format!("no status line: {e}"))?;
    header_bytes += status_line.len();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("malformed status line: {}", status_line.trim()))?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let line = read_line_capped(&mut reader).map_err(|e| format!("malformed headers: {e}"))?;
        let n = line.len();
        if n == 0 {
            break;
        }
        header_bytes += n;
        if header_bytes > MAX_HEADER_BYTES {
            return Err("response headers past the 64 KiB cap".into());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => content_length = value.parse().ok(),
                "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                    chunked = true
                }
                _ => {}
            }
        }
    }

    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(len) = content_length {
        if len > MAX_BODY_BYTES {
            return Err(format!(
                "response body of {} bytes is past the {} byte cap",
                len, MAX_BODY_BYTES
            ));
        }
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("short body: {}", e))?;
        buf
    } else {
        // No framing headers: the server closes the connection to end the body.
        let mut buf = Vec::new();
        reader
            .take(MAX_BODY_BYTES as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("body read failed: {}", e))?;
        buf
    };

    Ok(HttpResponse {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn read_chunked<R: Read>(reader: &mut BufReader<R>) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let size_line =
            read_line_capped(reader).map_err(|e| format!("malformed chunk size: {e}"))?;
        let size_token = size_line
            .trim()
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if size_token.is_empty() {
            break;
        }
        let size = usize::from_str_radix(&size_token, 16)
            .map_err(|_| format!("malformed chunk size '{}'", size_token))?;
        if size == 0 {
            // Consume the trailer section.
            loop {
                let trailer = read_line_capped(reader)?;
                if trailer.trim().is_empty() {
                    break;
                }
            }
            break;
        }
        if body.len() + size > MAX_BODY_BYTES {
            return Err(format!("chunked body past the {} byte cap", MAX_BODY_BYTES));
        }
        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|e| format!("short chunk: {}", e))?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|e| format!("malformed chunk terminator: {e}"))?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::thread;

    #[test]
    fn parses_loopback_base_urls() {
        let e = parse_base_url("http://127.0.0.1:11434/v1").unwrap();
        assert_eq!(e.host, "127.0.0.1");
        assert_eq!(e.port, 11434);
        assert_eq!(e.prefix, "/v1");
        assert_eq!(e.base_url(), "http://127.0.0.1:11434/v1");

        let bare = parse_base_url("localhost:1234").unwrap();
        assert_eq!(bare.port, 1234);
        assert_eq!(bare.prefix, "");

        let v6 = parse_base_url("http://[::1]:8080/v1/").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.prefix, "/v1");
    }

    #[test]
    fn refuses_anything_that_is_not_loopback() {
        for url in [
            "http://api.openai.com/v1",
            "https://127.0.0.1:11434/v1",
            "http://192.168.1.10:11434/v1",
            "http://user:pass@127.0.0.1:11434/v1",
        ] {
            assert!(
                parse_base_url(url).is_err(),
                "{} should have been refused",
                url
            );
        }
    }

    #[test]
    fn reads_content_length_bodies() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"ok\":true}\r\n";
        let res = read_response(BufReader::new(Cursor::new(raw.as_bytes().to_vec()))).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body.trim(), "{\"ok\":true}");
    }

    #[test]
    fn reads_chunked_bodies() {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   7\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\n\r\n";
        let res = read_response(BufReader::new(Cursor::new(raw.as_bytes().to_vec()))).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.body, "{\"a\":1,\"b\":2}");
    }

    #[test]
    fn reads_bodies_framed_only_by_close() {
        let raw = "HTTP/1.1 500 Internal Server Error\r\n\r\nboom";
        let res = read_response(BufReader::new(Cursor::new(raw.as_bytes().to_vec()))).unwrap();
        assert_eq!(res.status, 500);
        assert_eq!(res.body, "boom");
    }

    #[test]
    fn rejects_a_malformed_status_line() {
        let raw = "not-http\r\n\r\n";
        assert!(read_response(BufReader::new(Cursor::new(raw.as_bytes().to_vec()))).is_err());
    }

    /// Regression (audit M1): framing lines were read with unbounded
    /// read_line, so a peer streaming newline-less bytes allocated without
    /// limit. Status, header and chunk-size lines must be capped.
    #[test]
    fn runaway_framing_lines_are_rejected_at_the_cap() {
        let runaway = vec![b'a'; MAX_LINE_BYTES * 4];

        let mut status_only = runaway.clone();
        status_only.truncate(MAX_LINE_BYTES * 2);
        let err = read_response(BufReader::new(Cursor::new(status_only))).unwrap_err();
        assert!(
            err.contains("cap"),
            "status line overrun must name the cap, got: {err}"
        );

        let header_overrun = format!(
            "HTTP/1.1 200 OK\r\nX-Long: {}\n",
            "b".repeat(MAX_LINE_BYTES * 2)
        );
        let err =
            read_response(BufReader::new(Cursor::new(header_overrun.into_bytes()))).unwrap_err();
        assert!(
            err.contains("cap"),
            "header overrun must name the cap, got: {err}"
        );

        let chunked_overrun = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}\n",
            "f".repeat(MAX_LINE_BYTES * 2)
        );
        let err =
            read_response(BufReader::new(Cursor::new(chunked_overrun.into_bytes()))).unwrap_err();
        assert!(
            err.contains("cap"),
            "chunk-size overrun must name the cap, got: {err}"
        );
    }

    /// Regression (audit M3): per-read timeouts reset on every received byte,
    /// so a peer dribbling one byte at a time kept the transfer alive
    /// forever. The whole round trip must respect a single wall-clock
    /// deadline. The watchdog keeps this test from hanging against the old
    /// behavior; it panics instead of blocking the suite.
    #[test]
    fn overall_deadline_bounds_a_dribbling_server() {
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                let mut sock = sock;
                let pattern = b"HTTP/1.1 200 OK\r\n";
                let mut i = 0;
                loop {
                    use std::io::Write;
                    if sock
                        .write_all(&pattern[i % pattern.len()..i % pattern.len() + 1])
                        .is_err()
                    {
                        break;
                    }
                    i += 1;
                    thread::sleep(Duration::from_millis(15));
                }
            }
        });

        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: addr.port(),
            prefix: String::new(),
        };
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        thread::spawn(move || {
            let result = request(&endpoint, "GET", "/", None, Duration::from_millis(400));
            let _ = tx.send((result, started.elapsed()));
        });
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok((_, elapsed)) => assert!(
                elapsed < Duration::from_secs(2),
                "round trip took {:?}; the deadline did not bound it",
                elapsed
            ),
            Err(_) => panic!("request was still running after 3s: no overall deadline"),
        }
        server.join().ok();
    }

    /// The path portion of a base URL lands verbatim in the request head, so
    /// control characters there are request smuggling, not a weird folder.
    #[test]
    fn base_url_path_rejects_control_characters_and_spaces() {
        for url in [
            "http://127.0.0.1:11434/v1 HTTP/1.1",
            "http://127.0.0.1:11434/a\tb",
            "http://127.0.0.1:11434/x\r\nHost: evil",
            "http://127.0.0.1:11434/a\x08b",
            "http://127.0.0.1:11434/pa th",
        ] {
            assert!(
                parse_base_url(url).is_err(),
                "{url:?} smuggles into the request head and must be refused"
            );
        }
    }
}
