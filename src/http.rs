//! A small HTTP/1.1 client, enough to talk to an engine on this machine
//! (`http://127.0.0.1:…`). One request per connection; the server closes it.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub enum Error {
    /// Could not connect or the connection broke.
    Net(String),
    /// The server answered with this status.
    Status(u16, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Net(e) => write!(f, "{e}"),
            Error::Status(s, body) => write!(f, "HTTP {s}: {}", body.chars().take(200).collect::<String>()),
        }
    }
}

/// POST `body` to `url`. Answers 2xx as Ok, other statuses as `Error::Status`.
pub fn post(url: &str, body: &[u8], headers: &[(&str, &str)], timeout: Duration) -> Result<Response, Error> {
    let rest = url.strip_prefix("http://").ok_or_else(|| Error::Net(format!("only http:// is supported: {url}")))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = hostport
        .to_socket_addrs()
        .map_err(|e| Error::Net(format!("{hostport}: {e}")))?
        .next()
        .ok_or_else(|| Error::Net(format!("{hostport}: no address")))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| Error::Net(e.to_string()))?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(Duration::from_secs(30))).ok();
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Length: {}\r\nConnection: close\r\nUser-Agent: ambition\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let mut req = head.into_bytes();
    req.extend_from_slice(body);
    s.write_all(&req).map_err(|e| Error::Net(e.to_string()))?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| Error::Net(format!("reading the answer: {e}")))?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| Error::Net("no HTTP answer".into()))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| Error::Net("a bad HTTP status line".into()))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let mut data = raw[split + 4..].to_vec();
    let chunked = headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        data = dechunk(&data);
    } else if let Some(n) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()) {
        data.truncate(n);
    }
    let body = String::from_utf8_lossy(&data).to_string();
    let r = Response { status, headers, body };
    if (200..300).contains(&r.status) {
        Ok(r)
    } else {
        Err(Error::Status(r.status, r.body))
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = data.windows(2).position(|w| w == b"\r\n") else { break };
        let size = String::from_utf8_lossy(&data[..eol]);
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        data = &data[eol + 2..];
        if size == 0 || data.len() < size {
            out.extend_from_slice(&data[..size.min(data.len())]);
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve(answer: &'static [u8]) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            // Read the whole request (headers, then Content-Length bytes) before answering.
            loop {
                let n = c.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..i]).to_lowercase();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            c.write_all(answer).unwrap();
        });
        format!("http://127.0.0.1:{port}/x")
    }

    #[test]
    fn plain_and_chunked() {
        let u = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhello");
        let r = post(&u, b"{}", &[], Duration::from_secs(5)).unwrap();
        assert_eq!((r.status, r.body.as_str(), r.header("x-a")), (200, "hello", Some("b")));
        let u = serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n");
        assert_eq!(post(&u, b"", &[], Duration::from_secs(5)).unwrap().body, "hello world");
        let u = serve(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\n\r\nno");
        assert!(matches!(post(&u, b"", &[], Duration::from_secs(5)), Err(Error::Status(401, _))));
    }
}
