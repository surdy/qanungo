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
//!
//! # Two ways to read a response
//!
//! [`get`] buffers: it reads the whole response into memory under [`MAX_RESPONSE_BYTES`], which
//! is right for the JSON API surfaces, where a document that does not fit in a megabyte is a
//! protocol failure rather than a large answer.
//!
//! [`get_streaming`] does not: it parses the head, then hands back a [`Body`] that reads the
//! remainder off the socket as it arrives, de-chunking on the way. Transcript artifacts run to
//! hundreds of megabytes, so the download path reads them a buffer at a time rather than
//! materializing them; nothing in this module ever holds a whole artifact.
//!
//! # What the timeout means
//!
//! `timeout` is applied to the socket, not to the request: it bounds `connect`, and it bounds
//! *each* read and write syscall. A buffered `GET` therefore effectively has a per-request
//! deadline, because its whole body arrives in one `read_to_end`. A streamed body instead gets
//! *read-progress* semantics — every socket read must complete within the timeout, and the total
//! transfer is unbounded while bytes keep flowing, which is the only sane rule for a body that
//! may legitimately take minutes.
//!
//! That is deliberately more permissive than the archive. `patwari-server` arms a whole-body
//! deadline of `PATWARI_REQUEST_TIMEOUT` (30s in the production deployment) when it constructs a
//! download response, and kills the stream when it expires, so the *effective* ceiling on a
//! download is the server's, not this client's. A slow-but-alive server is the case the
//! read-progress timeout is for; a server that has given up mid-body closes the connection, the
//! transfer comes up short of its declared size, and the download path refuses it as a
//! verification failure rather than caching a prefix.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use thiserror::Error;

/// Default ceiling on one response — status line, headers, and body together, since the bound is
/// applied to the socket read rather than to the parsed document. Listings are small JSON
/// documents. This bounds the *buffered* reader only; [`get_streaming`] deliberately has no
/// whole-response ceiling, because its caller bounds the transfer against the sizes the archive
/// declared for the artifact instead.
const MAX_RESPONSE_BYTES: usize = 1_048_576;

/// Ceiling on a streamed response's head — the status line and every header. Generous for the
/// dozen `x-patwari-*` headers a download carries, and small enough that a peer which never sends
/// the terminator cannot grow this buffer without bound.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Ceiling on one chunked-framing line: a hex size, an optional extension, and CRLF.
const MAX_FRAMING_LINE_BYTES: u64 = 4096;

/// Socket read buffer behind a streamed body.
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

/// How much of a *failed* streamed response is read back, so the archive's stable machine-readable
/// `error.code` can be lifted out of an error document without reading an error page of unbounded
/// size.
const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024;

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
        header(&self.headers, name)
    }
}

/// One response whose head has been parsed but whose body is still on the socket.
///
/// The head is small and bounded; the body is not, and is deliberately never held whole. Reading
/// it is the caller's job, through [`StreamingResponse::body`].
pub struct StreamingResponse {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

impl StreamingResponse {
    /// The first value of a response header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The unread remainder of the response, de-chunked.
    pub fn body(&mut self) -> &mut Body {
        &mut self.body
    }

    /// Reads back at most [`MAX_ERROR_BODY_BYTES`] of the body, for the one thing this client
    /// lifts out of a body it did not ask to parse: Patwari's stable `error.code`. A read failure
    /// yields no bytes rather than an error — the status is already the finding.
    pub fn error_body(&mut self) -> Vec<u8> {
        let mut document = Vec::new();
        let _ = (&mut self.body)
            .take(MAX_ERROR_BODY_BYTES)
            .read_to_end(&mut document);
        document
    }
}

/// The first value of a header, matched case-insensitively.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// How the peer said the body ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// `Transfer-Encoding: chunked`. Takes precedence over `Content-Length`, per RFC 9112.
    Chunked,
    /// `Content-Length`: exactly this many bytes.
    Length,
    /// Neither header. `Connection: close` makes end-of-stream the end of the body.
    UntilClose,
}

/// A response body still arriving on the socket, de-chunked on the way through.
///
/// Reading short of the declared length is not reported as an error here: the download path
/// compares what it received against the size and digest the archive declared, and a body cut
/// short by a server-side deadline or a dropped connection has to fail *there*, as a verification
/// failure, so it is refused for the right reason and never cached.
pub struct Body {
    reader: BufReader<Transport>,
    framing: Framing,
    /// Bytes still owed by the current frame: the whole body under [`Framing::Length`], the
    /// current chunk under [`Framing::Chunked`].
    remaining: u64,
    /// Whether a chunk's data has been consumed and its trailing CRLF is still unread.
    chunk_terminator_pending: bool,
    finished: bool,
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.finished || out.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::UntilClose => self.read_frame(out),
            Framing::Length => {
                if self.remaining == 0 {
                    self.finished = true;
                    return Ok(0);
                }
                self.read_frame(out)
            }
            Framing::Chunked => {
                if self.remaining == 0 && !self.open_next_chunk()? {
                    return Ok(0);
                }
                self.read_frame(out)
            }
        }
    }
}

impl Body {
    /// Reads within the current frame, never past what it still owes.
    fn read_frame(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let want = if self.framing == Framing::UntilClose {
            out.len()
        } else {
            out.len().min(clamp_to_usize(self.remaining))
        };
        let read = self.reader.read(&mut out[..want])?;
        if read == 0 {
            self.finished = true;
            return Ok(0);
        }
        if self.framing != Framing::UntilClose {
            self.remaining -= read as u64;
        }
        Ok(read)
    }

    /// Consumes the previous chunk's trailing CRLF and this chunk's size line. Returns whether a
    /// chunk with data was opened; a zero-size chunk or an end of stream finishes the body.
    fn open_next_chunk(&mut self) -> std::io::Result<bool> {
        if self.chunk_terminator_pending {
            if self.framing_line()?.is_none() {
                self.finished = true;
                return Ok(false);
            }
            self.chunk_terminator_pending = false;
        }
        let Some(line) = self.framing_line()? else {
            self.finished = true;
            return Ok(false);
        };
        let size = parse_chunk_size(&line)?;
        if size == 0 {
            self.finished = true;
            return Ok(false);
        }
        self.remaining = size;
        self.chunk_terminator_pending = true;
        Ok(true)
    }

    /// One CRLF-terminated framing line, bounded by [`MAX_FRAMING_LINE_BYTES`], or `None` at end
    /// of stream.
    fn framing_line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        let read = (&mut self.reader)
            .take(MAX_FRAMING_LINE_BYTES)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        if line.last() != Some(&b'\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunk framing line is unterminated or too long",
            ));
        }
        Ok(Some(line))
    }
}

/// Parses a chunk size line: hexadecimal, with any chunk extension after `;` ignored.
fn parse_chunk_size(line: &[u8]) -> std::io::Result<u64> {
    let text = String::from_utf8_lossy(line);
    let size = text.trim_end_matches(['\r', '\n']);
    let size = size.split(';').next().unwrap_or("").trim();
    u64::from_str_radix(size, 16)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed chunk size"))
}

/// Saturating `u64` → `usize`, for bounding a read against a frame that may be larger than this
/// platform can address in one buffer.
fn clamp_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
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
    let stream = send_get(endpoint, timeout, target)?;
    let mut raw = Vec::new();
    stream
        .take(max_response_bytes as u64)
        .read_to_end(&mut raw)
        .map_err(|error| HttpError::Transport(error.to_string()))?;
    parse_response(&raw)
}

/// Sends one `GET` and returns as soon as the response head has been parsed, leaving the body on
/// the socket for the caller to stream. There is no ceiling on the body: the artifact download
/// bounds itself against the sizes the archive declared for that artifact, which is a tighter and
/// more meaningful bound than any constant here could be.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be reached or the response head is unparseable,
/// oversized, or never terminated.
pub fn get_streaming(
    endpoint: &Endpoint,
    timeout: Duration,
    target: &str,
) -> Result<StreamingResponse, HttpError> {
    let stream = send_get(endpoint, timeout, target)?;
    let mut reader = BufReader::with_capacity(STREAM_BUFFER_BYTES, stream);
    let head = read_head(&mut reader)?;
    let (status, headers) = parse_head(&head)?;

    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let length =
        header(&headers, "content-length").and_then(|value| value.trim().parse::<u64>().ok());
    let (framing, remaining) = match (chunked, length) {
        (true, _) => (Framing::Chunked, 0),
        (false, Some(length)) => (Framing::Length, length),
        (false, None) => (Framing::UntilClose, 0),
    };

    Ok(StreamingResponse {
        status,
        headers,
        body: Body {
            reader,
            framing,
            remaining,
            chunk_terminator_pending: false,
            finished: false,
        },
    })
}

/// Reads exactly up to and including the `\r\n\r\n` head terminator, leaving the first body byte
/// unconsumed in the reader.
fn read_head(reader: &mut impl BufRead) -> Result<Vec<u8>, HttpError> {
    let mut head: Vec<u8> = Vec::new();
    loop {
        let buffered = reader
            .fill_buf()
            .map_err(|error| HttpError::Transport(error.to_string()))?;
        if buffered.is_empty() {
            return Err(HttpError::Protocol(
                "response ended before its header terminator".to_owned(),
            ));
        }
        // The terminator can straddle the boundary, so the search runs over the last three bytes
        // already held plus what is newly buffered. `head` never contains a complete terminator,
        // so a match always extends into the buffer and consumes at least one byte of it.
        let carried = head.len().min(3);
        let mut window = head[head.len() - carried..].to_vec();
        window.extend_from_slice(buffered);
        let found = window
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .map(|position| position + 4 - carried);
        let take = found.unwrap_or(buffered.len());
        head.extend_from_slice(&buffered[..take]);
        reader.consume(take);
        if found.is_some() {
            return Ok(head);
        }
        if head.len() > MAX_HEAD_BYTES {
            return Err(HttpError::Protocol("response head is too long".to_owned()));
        }
    }
}

/// Splits a raw head into its status code and lowercased header names.
fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>), HttpError> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.trim_end_matches("\r\n\r\n").split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1).map(ToOwned::to_owned))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| HttpError::Protocol("unparseable status line".to_owned()))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok((status, headers))
}

/// Opens a fresh `Connection: close` socket, wraps it in TLS when the endpoint asks for it, and
/// writes the request head. The `timeout` lands on the socket, so it bounds `connect` and each
/// individual read and write rather than the request as a whole.
fn send_get(endpoint: &Endpoint, timeout: Duration, target: &str) -> Result<Transport, HttpError> {
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
    Ok(stream)
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

    #[test]
    fn parses_a_head_into_its_status_and_lowercased_headers() {
        let (status, headers) = parse_head(
            b"HTTP/1.1 404 Not Found\r\nX-Patwari-Compression: zstd\r\nContent-Length: 7\r\n\r\n",
        )
        .unwrap();
        assert_eq!(status, 404);
        assert_eq!(header(&headers, "content-length"), Some("7"));
        assert_eq!(header(&headers, "X-PATWARI-COMPRESSION"), Some("zstd"));
    }

    #[test]
    fn reads_chunk_sizes_in_hex_and_ignores_extensions() {
        assert_eq!(parse_chunk_size(b"1a\r\n").unwrap(), 26);
        assert_eq!(parse_chunk_size(b"ff;name=value\r\n").unwrap(), 255);
        assert_eq!(parse_chunk_size(b"0\r\n").unwrap(), 0);
        assert!(parse_chunk_size(b"zz\r\n").is_err());
    }

    /// A reader that yields one byte per fill, so the head terminator is guaranteed to straddle
    /// buffer boundaries — the case a real socket produces only occasionally and this arithmetic
    /// has to get right every time.
    struct Dribble<'a> {
        bytes: &'a [u8],
        position: usize,
    }

    impl Read for Dribble<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let available = self.fill_buf()?.len();
            let take = available.min(out.len());
            out[..take].copy_from_slice(&self.bytes[self.position..self.position + take]);
            self.consume(take);
            Ok(take)
        }
    }

    impl BufRead for Dribble<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            let end = (self.position + 1).min(self.bytes.len());
            Ok(&self.bytes[self.position..end])
        }

        fn consume(&mut self, amount: usize) {
            self.position += amount;
        }
    }

    #[test]
    fn reads_the_head_without_swallowing_the_first_body_byte() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let mut reader = Dribble {
            bytes: raw,
            position: 0,
        };
        let head = read_head(&mut reader).unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(head.len(), raw.len() - 5);

        let mut body = Vec::new();
        reader.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"hello", "the body must be left entirely unconsumed");
    }

    #[test]
    fn refuses_a_head_that_never_terminates() {
        let mut reader = Dribble {
            bytes: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n",
            position: 0,
        };
        assert!(matches!(
            read_head(&mut reader),
            Err(HttpError::Protocol(_))
        ));
    }
}
