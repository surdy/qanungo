//! A minimal synchronous HTTP/1.1 client for the Patwari read surfaces.
//!
//! Deliberately the same blocking, one-fresh-connection-per-request `std::net` design Munshi's
//! archive client uses (munshi ADR 0006): a coaching report is a handful of requests against a
//! LAN server, which does not justify pulling an async runtime into the dependency tree.
//! `https://` endpoints wrap that same stream in rustls, verifying against the system trust
//! store with no TLS policy knobs (munshi ADR 0013); plain `http://` stays fully supported
//! because the production archive is reached over the trusted LAN.
//!
//! Only what a read client needs is implemented: `GET`, response headers, chunked bodies (the
//! artifact-content route streams, so its body always arrives chunked), and a per-request byte
//! ceiling. There is no connection reuse and no retry — Patwari serves ~8 concurrent requests
//! with a 30s timeout, and a client that retries into a busy LAN server makes things worse.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use thiserror::Error;

/// Default ceiling on one response — status line, headers, and body together, since the bound is
/// applied to the socket read rather than to the parsed document. Listings are small JSON
/// documents; artifact downloads raise it deliberately to the stored size the listing already
/// declared, plus framing headroom.
const MAX_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("endpoint {0} is not a supported http(s) URL")]
    UnsupportedEndpoint(String),
    #[error("http transport failed: {0}")]
    Transport(String),
    #[error("http protocol error: {0}")]
    Protocol(String),
    #[error("tls setup failed: {0}")]
    Tls(String),
}

/// A parsed endpoint authority: scheme (as the `tls` flag), host, and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// The scheme's default port; the `Host` header omits it, matching what proxies and virtual
    /// hosts conventionally expect.
    const fn default_port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }
}

/// Splits an `http://host[:port]` or `https://host[:port]` endpoint into its parsed authority,
/// defaulting the port per scheme; other schemes are rejected.
pub fn parse_endpoint(endpoint: &str) -> Result<Endpoint, HttpError> {
    let unsupported = || HttpError::UnsupportedEndpoint(endpoint.to_owned());
    let (tls, rest) = if let Some(rest) = endpoint.strip_prefix("http://") {
        (false, rest)
    } else if let Some(rest) = endpoint.strip_prefix("https://") {
        (true, rest)
    } else {
        return Err(unsupported());
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>().map_err(|_| unsupported())?,
        ),
        None => (authority.to_owned(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(unsupported());
    }
    Ok(Endpoint { tls, host, port })
}

/// Percent-encodes a value for safe inclusion in a request target. Unlike a path encoder this
/// escapes `/` too, because every value it is used on here — a cursor, a UUID, a timestamp — is
/// a single query-parameter or path segment value rather than a path.
pub fn encode_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// One response: the status, the body, and the header names lowercased. The artifact-content
/// route conveys an artifact's declared sizes, digests, and compression in `x-patwari-*`
/// headers, so headers are not optional detail here.
///
/// There is deliberately no "body as text" accessor. The only thing this client ever lifts out
/// of a response body it did not ask to parse is Patwari's stable machine-readable `error.code`;
/// rendering a server's free text would put an unbounded upstream string on a path that ends in
/// a report sworn to carry none.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Response {
    /// The first value of a response header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The process-wide TLS client configuration: rustls on the ring provider, verifying against the
/// system trust store. Built once so TLS 1.3 session resumption spans a run's requests.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, HttpError> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let loaded = rustls_native_certs::load_native_certs();
            let mut roots = rustls::RootCertStore::empty();
            let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
            if added == 0 {
                let detail = loaded
                    .errors
                    .first()
                    .map_or_else(|| "no certificates found".to_owned(), ToString::to_string);
                return Err(format!(
                    "no usable roots in the system trust store: {detail}"
                ));
            }
            Ok(Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            ))
        })
        .clone()
        .map_err(HttpError::Tls)
}

/// One request's transport: the plain socket, or that same socket wrapped in rustls.
enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

/// Sends one `GET` over a fresh `Connection: close` socket, bounding the body at
/// [`MAX_RESPONSE_BYTES`].
pub fn get(endpoint: &Endpoint, timeout: Duration, target: &str) -> Result<Response, HttpError> {
    get_with_limit(endpoint, timeout, target, MAX_RESPONSE_BYTES)
}

/// Like [`get`], but bounds the response at `max_response_bytes`. An artifact download raises the
/// ceiling to the stored size the listing already declared, so a body longer than declared is
/// truncated into a digest-verification failure rather than read into memory unbounded.
pub fn get_with_limit(
    endpoint: &Endpoint,
    timeout: Duration,
    target: &str,
    max_response_bytes: usize,
) -> Result<Response, HttpError> {
    let Endpoint { host, port, .. } = endpoint;
    let address = (host.as_str(), *port)
        .to_socket_addrs()
        .map_err(|error| HttpError::Transport(error.to_string()))?
        .next()
        .ok_or_else(|| HttpError::Transport(format!("could not resolve {host}:{port}")))?;
    let tcp = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    tcp.set_read_timeout(Some(timeout))
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    tcp.set_write_timeout(Some(timeout))
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    let mut stream = if endpoint.tls {
        let config = tls_config()?;
        let server_name = ServerName::try_from(host.clone())
            .map_err(|_| HttpError::Tls(format!("{host} is not a valid TLS server name")))?;
        let connection = rustls::ClientConnection::new(config, server_name)
            .map_err(|error| HttpError::Tls(error.to_string()))?;
        Transport::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Transport::Plain(tcp)
    };

    let host_header = if *port == endpoint.default_port() {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let head = format!(
        "GET {target} HTTP/1.1\r\nHost: {host_header}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    stream
        .flush()
        .map_err(|error| HttpError::Transport(error.to_string()))?;

    let mut raw = Vec::new();
    stream
        .take(max_response_bytes as u64)
        .read_to_end(&mut raw)
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    parse_response(&raw)
}

/// Splits a raw HTTP/1.1 response into its status, headers, and de-chunked body.
pub fn parse_response(raw: &[u8]) -> Result<Response, HttpError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| HttpError::Protocol("response has no header terminator".to_owned()))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1).map(ToOwned::to_owned))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| HttpError::Protocol("unparseable status line".to_owned()))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let chunked = headers.iter().any(|(name, value)| {
        name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok(Response {
        status,
        body,
        headers,
    })
}

/// Reassembles a `Transfer-Encoding: chunked` body. Patwari streams artifact content, so this is
/// the normal path for a download rather than an exotic one.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, HttpError> {
    let mut output = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| HttpError::Protocol("malformed chunk header".to_owned()))?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size =
            usize::from_str_radix(size_text.trim().split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| HttpError::Protocol("malformed chunk size".to_owned()))?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size {
            return Err(HttpError::Protocol("truncated chunk body".to_owned()));
        }
        output.extend_from_slice(&body[..size]);
        body = &body[size..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_and_https_endpoints_and_rejects_others() {
        assert_eq!(
            parse_endpoint("http://192.168.16.169:8787").unwrap(),
            Endpoint {
                tls: false,
                host: "192.168.16.169".to_owned(),
                port: 8787,
            }
        );
        assert_eq!(
            parse_endpoint("https://patwari.example.com").unwrap().port,
            443
        );
        assert_eq!(parse_endpoint("http://localhost").unwrap().port, 80);
        assert!(parse_endpoint("ftp://host").is_err());
        assert!(parse_endpoint("192.168.16.169:8787").is_err());
        assert!(parse_endpoint("https://").is_err());
    }

    #[test]
    fn encodes_query_values_including_slashes_and_colons() {
        assert_eq!(
            encode_value("2026-08-17T00:00:00Z"),
            "2026-08-17T00%3A00%3A00Z"
        );
        assert_eq!(encode_value("a/b"), "a%2Fb");
        assert_eq!(encode_value("plain-value_1.0~x"), "plain-value_1.0~x");
    }

    #[test]
    fn parses_a_content_length_response_with_headers() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Patwari-Compression: zstd\r\n\r\nhi";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hi");
        assert_eq!(response.header("x-patwari-compression"), Some("zstd"));
        assert_eq!(response.header("X-PATWARI-COMPRESSION"), Some("zstd"));
        assert_eq!(response.header("absent"), None);
    }

    #[test]
    fn reassembles_a_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.body, b"Wikipedia");
    }

    #[test]
    fn refuses_a_response_without_a_header_terminator() {
        assert!(matches!(
            parse_response(b"HTTP/1.1 200 OK\r\n"),
            Err(HttpError::Protocol(_))
        ));
    }
}
