//! Readiness probe: `GET <url>` must return 200.
//!
//! Plain HTTP uses a minimal HTTP/1.1 client over std's TcpStream (no dependency);
//! HTTPS is delegated to `curl` when it is on PATH.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTarget {
    pub host: String,
    pub port: u16,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheme {
    Http(HttpTarget),
    Https,
}

pub fn parse_url(url: &str) -> Result<Scheme, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("not an http(s) URL: {url}"))?;
    match scheme {
        "https" => return Ok(Scheme::Https),
        "http" => {}
        other => return Err(format!("unsupported scheme {other}")),
    }
    let split = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (host, after) = stripped.split_once(']').ok_or("unterminated IPv6 host")?;
        (host.to_string(), after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => p.parse().map_err(|_| format!("invalid port in {url}"))?,
        None => 80,
    };
    if host.is_empty() {
        return Err(format!("missing host in {url}"));
    }
    Ok(Scheme::Http(HttpTarget { host, port, path }))
}

/// One GET; returns the status code or a short reason ("connection refused").
pub fn get_status(url: &str, timeout: Duration) -> Result<u16, String> {
    match parse_url(url)? {
        Scheme::Http(target) => http_get(&target, timeout),
        Scheme::Https => curl_get(url, timeout),
    }
}

pub fn is_ready(url: &str, timeout: Duration) -> Result<(), String> {
    match get_status(url, timeout)? {
        200 => Ok(()),
        code => Err(format!("HTTP {code}")),
    }
}

/// Polls every 500 ms until ready or `deadline`; `stop` can abort early.
pub fn wait_ready(
    url: &str,
    deadline: Instant,
    mut stop: impl FnMut() -> Option<String>,
) -> Result<(), String> {
    loop {
        let last = match is_ready(url, Duration::from_secs(2)) {
            Ok(()) => return Ok(()),
            Err(reason) => reason,
        };
        if let Some(reason) = stop() {
            return Err(reason);
        }
        if Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn describe(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::ConnectionRefused => "connection refused".into(),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "timed out".into(),
        _ => error.to_string(),
    }
}

/// Loopback connects succeed in microseconds; past this a refused port is
/// only being retried (Windows retries a refused SYN for about 2 s).
const LOOPBACK_CONNECT: Duration = Duration::from_millis(500);

/// Connects to every address at once and keeps the first that answers, so a
/// refused `::1` never delays the `127.0.0.1` the app actually binds (or the
/// other way round). Errors with the last failure when none connects.
fn connect_any(addrs: &[SocketAddr], timeout: Duration) -> Result<TcpStream, String> {
    let timeout = if !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback()) {
        timeout.min(LOOPBACK_CONNECT)
    } else {
        timeout
    };
    match addrs {
        [] => Err("no address".into()),
        [addr] => TcpStream::connect_timeout(addr, timeout).map_err(|e| describe(&e)),
        _ => {
            let (tx, rx) = std::sync::mpsc::channel();
            for &addr in addrs {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(TcpStream::connect_timeout(&addr, timeout));
                });
            }
            drop(tx);
            let mut last = "no address".to_string();
            for result in rx {
                match result {
                    Ok(stream) => return Ok(stream),
                    Err(e) => last = describe(&e),
                }
            }
            Err(last)
        }
    }
}

fn http_get(target: &HttpTarget, timeout: Duration) -> Result<u16, String> {
    let addrs: Vec<SocketAddr> = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}: {e}", target.host))?
        .collect();
    let mut stream = connect_any(&addrs, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| describe(&e))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| describe(&e))?;
    let host = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}:{}\r\nUser-Agent: guard\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        target.path, target.port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| describe(&e))?;
    read_status(&mut stream)
}

fn read_status(stream: &mut impl Read) -> Result<u16, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    while !buf.windows(2).any(|w| w == b"\r\n") {
        let n = stream.read(&mut chunk).map_err(|e| describe(&e))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    parse_status_line(&String::from_utf8_lossy(&buf))
}

pub fn parse_status_line(head: &str) -> Result<u16, String> {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/") => code
            .parse()
            .map_err(|_| format!("invalid HTTP status line: {line}")),
        _ => Err("invalid HTTP response".into()),
    }
}

fn curl_get(url: &str, timeout: Duration) -> Result<u16, String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = Command::new("curl")
        .args(["-s", "-o", null, "-w", "%{http_code}", "--max-time"])
        .arg(timeout.as_secs().max(1).to_string())
        .arg(url)
        .output()
        .map_err(|_| "https readiness needs curl on PATH".to_string())?;
    let code: u16 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if code == 0 {
        Err("connection failed".into())
    } else {
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parses_http_urls() {
        assert_eq!(
            parse_url("http://localhost:3000/healthz"),
            Ok(Scheme::Http(HttpTarget {
                host: "localhost".into(),
                port: 3000,
                path: "/healthz".into()
            }))
        );
        assert_eq!(
            parse_url("http://example.com"),
            Ok(Scheme::Http(HttpTarget {
                host: "example.com".into(),
                port: 80,
                path: "/".into()
            }))
        );
        assert_eq!(
            parse_url("http://[::1]:8080/x"),
            Ok(Scheme::Http(HttpTarget {
                host: "::1".into(),
                port: 8080,
                path: "/x".into()
            }))
        );
        assert_eq!(parse_url("https://x.com/a"), Ok(Scheme::Https));
        assert!(parse_url("ftp://x").is_err());
        assert!(parse_url("localhost:3000").is_err());
        assert!(parse_url("http://x:notaport/").is_err());
    }

    #[test]
    fn parses_status_lines() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK\r\n"), Ok(200));
        assert_eq!(
            parse_status_line("HTTP/1.0 503 Service Unavailable"),
            Ok(503)
        );
        assert!(parse_status_line("garbage").is_err());
    }

    #[test]
    fn gets_the_status_of_a_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut b = [0u8; 512];
                let _ = s.read(&mut b);
                let _ = s.write_all(b"HTTP/1.1 503 Nope\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let url = format!("http://127.0.0.1:{port}/healthz");
        assert_eq!(get_status(&url, Duration::from_secs(2)), Ok(503));
    }

    /// A bound socket that never listens: connections to its port are
    /// refused, and no other socket can take the port while it is held.
    fn closed_port() -> (tokio::net::TcpSocket, u16) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (socket, port)
    }

    #[test]
    fn refused_connections_are_described() {
        let (_held, port) = closed_port();
        let error =
            get_status(&format!("http://127.0.0.1:{port}/"), Duration::from_secs(2)).unwrap_err();
        assert!(
            error.contains("refused") || error.contains("timed out"),
            "{error}"
        );
    }

    #[test]
    fn every_resolved_address_is_tried_at_once() {
        // `localhost` is ::1 then 127.0.0.1 on Windows; a server on 127.0.0.1
        // must not wait for the refused ::1 attempt (about 2 s there).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut b = [0u8; 512];
                let _ = s.read(&mut b);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let url = format!("http://localhost:{port}/");
        let begin = Instant::now();
        assert_eq!(get_status(&url, Duration::from_secs(5)), Ok(200));
        assert!(
            begin.elapsed() < Duration::from_millis(900),
            "{:?}",
            begin.elapsed()
        );
    }

    #[test]
    fn the_first_address_that_connects_wins() {
        let (_held, closed) = closed_port();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap();
        let addrs: Vec<SocketAddr> = vec![format!("127.0.0.1:{closed}").parse().unwrap(), open];
        let begin = Instant::now();
        let stream = connect_any(&addrs, Duration::from_secs(5)).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), open);
        assert!(
            begin.elapsed() < Duration::from_millis(900),
            "{:?}",
            begin.elapsed()
        );
        let (_held2, closed2) = closed_port();
        let refused = vec![format!("127.0.0.1:{closed2}").parse().unwrap()];
        let error = connect_any(&refused, Duration::from_secs(5)).unwrap_err();
        assert!(
            error.contains("refused") || error.contains("timed out"),
            "{error}"
        );
        assert_eq!(
            connect_any(&[], Duration::from_secs(1)).unwrap_err(),
            "no address"
        );
    }
}
