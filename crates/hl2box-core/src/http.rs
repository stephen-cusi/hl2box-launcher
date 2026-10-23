//! Minimal HTTP/1.1 client over the platform TLS stack.
//!
//! Why hand-rolled: the usual Rust TLS stacks for HTTPS (rustls via ring,
//! aws-lc) build C code, which drags a clang requirement onto hosts that
//! only have MSVC. `native-tls` binds the platform provider instead -
//! SChannel on Windows (pure bindings, no compiler), Security.framework on
//! macOS, OpenSSL on Linux/Android - and HTTP/1.1 with Content-Length and
//! chunked framing is a few hundred lines, which is all the workshop
//! client needs: one POST form and GET downloads with redirects.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use native_tls::TlsStream;

use crate::{err, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path plus query, always starts with '/'.
    pub target: String,
}

pub fn parse_url(input: &str) -> Result<Url> {
    let (scheme, rest) = match input.split_once("://") {
        Some(x) => x,
        None => return err(format!("url: missing scheme: {input}")),
    };
    let tls = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        other => return err(format!("url: unsupported scheme: {other}")),
    };
    let (authority, target) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // Drop userinfo if present.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| crate::Error(format!("url: bad port: {p}")))?,
            )
        }
        _ => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return err("url: empty host");
    }
    Ok(Url {
        tls,
        host,
        port,
        target: target.to_string(),
    })
}

enum Wire {
    Plain(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(s) => s.flush(),
        }
    }
}

fn connect(url: &Url, timeout: Duration) -> Result<Wire> {
    let addr = format!("{}:{}", url.host, url.port);
    let tcp = TcpStream::connect(&addr).map_err(|e| crate::Error(format!("connect: {e}")))?;
    tcp.set_read_timeout(Some(timeout))
        .and_then(|_| tcp.set_write_timeout(Some(timeout)))
        .map_err(|e| crate::Error(format!("socket options: {e}")))?;
    if url.tls {
        let conn = native_tls::TlsConnector::new()
            .map_err(|e| crate::Error(format!("tls: {e}")))?;
        let stream = conn
            .connect(&url.host, tcp)
            .map_err(|e| crate::Error(format!("tls handshake: {e}")))?;
        Ok(Wire::Tls(stream))
    } else {
        Ok(Wire::Plain(tcp))
    }
}

struct ResponseHead {
    status: u16,
    location: Option<String>,
    content_length: Option<u64>,
    chunked: bool,
}

fn read_head(reader: &mut BufReader<Wire>) -> Result<ResponseHead> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| crate::Error(format!("http: {e}")))?;
    // "HTTP/1.1 200 OK"
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| crate::Error(format!("http: bad status line: {line:?}")))?;

    let mut head = ResponseHead {
        status,
        location: None,
        content_length: None,
        chunked: false,
    };
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| crate::Error(format!("http: {e}")))?;
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "location" => head.location = Some(v.to_string()),
                "content-length" => head.content_length = v.parse::<u64>().ok(),
                "transfer-encoding" if v.eq_ignore_ascii_case("chunked") => {
                    head.chunked = true
                }
                _ => {}
            }
        }
        if line.len() > 65536 {
            return err("http: response head too large");
        }
    }
    Ok(head)
}

fn request_bytes(
    method: &str,
    url: &Url,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout: Duration,
    redirects: u32,
) -> Result<(u16, Vec<u8>)> {
    if redirects == 0 {
        return err("http: too many redirects");
    }
    let mut wire = connect(url, timeout)?;
    let mut req = format!("{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: hl2box-launcher\r\nAccept: */*\r\n", url.target, url.host);
    if let Some(ct) = content_type {
        req.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    wire.write_all(req.as_bytes())?;
    if let Some(b) = body {
        wire.write_all(b)?;
    }
    wire.flush()?;

    let mut reader = BufReader::new(wire);
    let head = read_head(&mut reader)?;
    if (300..400).contains(&head.status) {
        if let Some(loc) = head.location {
            let next = resolve(&loc, url)?;
            return request_bytes(method, &next, content_type, body, timeout, redirects - 1);
        }
    }

    let mut out = Vec::new();
    if head.chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line)?;
            let size = usize::from_str_radix(
                size_line.trim().split(';').next().unwrap_or("").trim(),
                16,
            )
            .map_err(|_| crate::Error("http: bad chunk size".into()))?;
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            reader.read_exact(&mut chunk)?;
            out.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf)?;
            if out.len() > 64 * 1024 * 1024 {
                return err("http: response too large for a json call");
            }
        }
    } else {
        reader
            .read_to_end(&mut out)
            .map_err(|e| crate::Error(format!("http: {e}")))?;
    }
    Ok((head.status, out))
}

/// Resolve a Location header value (absolute or relative) against the
/// request url.
fn resolve(loc: &str, base: &Url) -> Result<Url> {
    if loc.contains("://") {
        return parse_url(loc);
    }
    Ok(Url {
        target: if loc.starts_with('/') {
            loc.to_string()
        } else {
            format!("/{}", loc)
        },
        ..base.clone()
    })
}

/// POST an `application/x-www-form-urlencoded` body, return (status, body).
pub fn post_form(url: &str, form: &str, timeout: Duration) -> Result<(u16, Vec<u8>)> {
    let u = parse_url(url)?;
    request_bytes(
        "POST",
        &u,
        Some("application/x-www-form-urlencoded"),
        Some(form.as_bytes()),
        timeout,
        5,
    )
}

/// Streaming GET into a callback-driven reader. Returns the status and a
/// reader positioned at the body. Redirects are followed before the
/// stream is handed out.
pub fn get_stream(url: &str, timeout: Duration) -> Result<(u16, BodyReader)> {
    let mut u = parse_url(url)?;
    for _ in 0..5 {
        let mut wire = connect(&u, timeout)?;
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: hl2box-launcher\r\nAccept: */*\r\n\r\n",
            u.target, u.host
        );
        wire.write_all(req.as_bytes())?;
        wire.flush()?;

        let mut reader = BufReader::new(wire);
        let mut head = read_head(&mut reader)?;
        if (300..400).contains(&head.status) {
            let Some(loc) = head.location.take() else {
                break;
            };
            u = resolve(&loc, &u)?;
            continue;
        }
        return Ok((
            head.status,
            BodyReader {
                inner: reader,
                remaining: head.content_length,
                chunked: head.chunked,
                chunk_left: 0,
                done: false,
            },
        ));
    }
    err("http: redirect loop")
}

/// Body reader that understands both Content-Length and chunked framing.
pub struct BodyReader {
    inner: BufReader<Wire>,
    remaining: Option<u64>,
    chunked: bool,
    chunk_left: u64,
    done: bool,
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.chunked {
            if self.chunk_left == 0 {
                let mut line = String::new();
                self.inner.read_line(&mut line)?;
                if line.trim().is_empty() {
                    // trailing CRLF between chunks
                    line.clear();
                    self.inner.read_line(&mut line)?;
                }
                let t = line.trim();
                if t.is_empty() || t == "0" {
                    self.done = true;
                    return Ok(0);
                }
                self.chunk_left = u64::from_str_radix(
                    t.split(';').next().unwrap_or("").trim(),
                    16,
                )
                .map_err(|_| std::io::Error::other("http: bad chunk size"))?;
                if self.chunk_left == 0 {
                    self.done = true;
                    return Ok(0);
                }
            }
            let want = buf.len().min(self.chunk_left as usize);
            let n = self.inner.read(&mut buf[..want])?;
            self.chunk_left -= n as u64;
            if n == 0 {
                self.done = true;
            }
            Ok(n)
        } else {
            let cap = match self.remaining {
                Some(0) => {
                    self.done = true;
                    return Ok(0);
                }
                Some(r) => buf.len().min(r as usize),
                None => buf.len(),
            };
            let n = self.inner.read(&mut buf[..cap])?;
            if let Some(r) = &mut self.remaining {
                *r -= n as u64;
                if *r == 0 {
                    self.done = true;
                }
            }
            if n == 0 {
                self.done = true;
            }
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let u = parse_url("https://api.steampowered.com/ISteamRemoteStorage/x/v1/").unwrap();
        assert!(u.tls);
        assert_eq!(u.host, "api.steampowered.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.target, "/ISteamRemoteStorage/x/v1/");

        let u = parse_url("http://host:8080").unwrap();
        assert!(!u.tls);
        assert_eq!(u.port, 8080);
        assert_eq!(u.target, "/");
    }
}
