//! A small, general-purpose HTTP and HTTPS client on top of ESP-IDF's `esp_http_client`.
//!
//! ```ignore
//! let client = http::Client::new();
//!
//! let text = client.get("https://example.com/api")?.text()?;
//!
//! let response = client.send(
//!     &http::Request::post("https://example.com/items")
//!         .header("Content-Type", "application/json")
//!         .header("Authorization", "Bearer ...")
//!         .body(r#"{"name":"pulsar"}"#),
//! )?;
//! if response.is_success() {
//!     log::info!("created, {:?} bytes", response.content_length());
//! }
//! ```
//!
//! # What is supported
//!
//! - **HTTP/1.1 only.** ESP-IDF has no HTTP/2 framing and no QUIC, so HTTP/2 and HTTP/3 are not
//!   available. The types here do not expose the protocol, so another backend can replace the
//!   one underneath later.
//! - **TLS 1.3**, with TLS 1.2 as a fallback for servers that lack 1.3 (mbedTLS picks the highest
//!   version both sides know). Enabled in `sdkconfig.defaults`.
//! - **Certificate checks** against the Mozilla root certificate bundle built into the firmware:
//!   the chain, the host name and the validity dates. The dates need the right time, so an HTTPS
//!   request first waits for [`clock::wait_synced`] and fails with [`Error::ClockNotSynced`]
//!   if the clock is not set in time. Call [`clock::start`] once for that.
//! - Redirects are followed for `GET` and `HEAD` only (never for requests with a body), as long as
//!   they keep the scheme. A redirect from `http://` to `https://` is **not** handled: the server
//!   answered 400 in testing. Ask for the `https://` URL directly.
//!
//! # Things to know
//!
//! - Every [`Client::send`] opens a new connection (a new TLS handshake for HTTPS), so there is no
//!   keep-alive between requests. `esp_http_client` would keep the headers of the previous request
//!   on a reused connection, which could send an `Authorization` header to the wrong host, and
//!   `esp-idf-svc` panics when a reused connection is used again after an error.
//! - The thread that makes a request needs at least 10 KiB of stack for the TLS handshake. Build
//!   it with `thread::Builder::stack_size`.
//! - A header that appears more than once in a response is reported once, with the last value.
//! - Request and response bodies are never logged, nor are headers or the query part of a URL: they
//!   often hold tokens. Only the method, the host and the status are.

use std::fmt;
use std::io;

use esp_idf_svc::http::Method as EspMethod;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection, FollowRedirectsPolicy};
use esp_idf_svc::sys::{self, EspError};

use crate::clock;
use crate::config::{
    HTTP_CLOCK_WAIT, HTTP_MAX_BODY_BYTES, HTTP_RX_BUFFER_BYTES, HTTP_TIMEOUT, HTTP_TX_BUFFER_BYTES,
};

/// Bytes read from the connection at a time when reading a whole body.
const READ_CHUNK_BYTES: usize = 1024;

/// What can go wrong with a request.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The URL is not `http://` or `https://`, or it contains spaces or control characters.
    #[error("invalid URL: it must start with http:// or https:// and hold no whitespace")]
    InvalidUrl,
    /// A header name is empty or has a character that is not allowed in one, or a header value
    /// has a line break or a NUL byte (which would let it forge more headers).
    #[error("invalid header name or value")]
    InvalidHeader,
    /// The clock was not set in time, so the certificate cannot be checked.
    #[error("the clock is not set, cannot check the certificate (was clock::start called?)")]
    ClockNotSynced,
    /// The body is longer than the limit (in bytes) for reading it into memory.
    #[error("the response body is longer than {0} bytes; read it with io::Read instead")]
    BodyTooLarge(usize),
    /// The body is not valid UTF-8, so it cannot be returned as text.
    #[error("the response body is not valid UTF-8")]
    NotUtf8,
    /// The connection or the TLS handshake failed, or the server sent something invalid.
    #[error("HTTP client: {0}")]
    Driver(#[from] EspError),
}

/// An HTTP method. Only the ones that `esp_http_client` supports without surprises are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Read a resource.
    Get,
    /// Like `GET`, but the response has no body.
    Head,
    /// Create a resource or run an action.
    Post,
    /// Replace a resource.
    Put,
    /// Change part of a resource.
    Patch,
    /// Remove a resource.
    Delete,
    /// Ask what the server supports.
    Options,
}

impl Method {
    /// The name as it appears in the request line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Method> for EspMethod {
    fn from(method: Method) -> Self {
        match method {
            Method::Get => Self::Get,
            Method::Head => Self::Head,
            Method::Post => Self::Post,
            Method::Put => Self::Put,
            Method::Patch => Self::Patch,
            Method::Delete => Self::Delete,
            Method::Options => Self::Options,
        }
    }
}

/// A request, built with the methods below. Nothing is sent until [`Client::send`].
///
/// `Debug` shows only the method, the host and the sizes: URLs, headers and bodies can hold
/// secrets.
#[derive(Clone)]
pub struct Request {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    /// A request without headers and without a body.
    #[must_use]
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A `GET` request.
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    /// A `HEAD` request.
    #[must_use]
    pub fn head(url: impl Into<String>) -> Self {
        Self::new(Method::Head, url)
    }

    /// A `POST` request.
    #[must_use]
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::Post, url)
    }

    /// A `PUT` request.
    #[must_use]
    pub fn put(url: impl Into<String>) -> Self {
        Self::new(Method::Put, url)
    }

    /// A `PATCH` request.
    #[must_use]
    pub fn patch(url: impl Into<String>) -> Self {
        Self::new(Method::Patch, url)
    }

    /// A `DELETE` request.
    #[must_use]
    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(Method::Delete, url)
    }

    /// Adds a header. A `Content-Length` header is ignored: the length of the body is used. The
    /// name and value are checked when the request is sent.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets the body, replacing an earlier one. Set a `Content-Type` header to match.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Rejects what `esp_http_client` would pass on unchecked.
    fn validate(&self) -> Result<(), Error> {
        let has_scheme = ["http://", "https://"].iter().any(|scheme| {
            self.url
                .get(..scheme.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
        });
        if !has_scheme
            || self
                .url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::InvalidUrl);
        }
        for (name, value) in &self.headers {
            if !is_token(name) || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
                return Err(Error::InvalidHeader);
            }
        }
        Ok(())
    }

    fn is_https(&self) -> bool {
        self.url
            .get(.."https://".len())
            .is_some_and(|start| start.eq_ignore_ascii_case("https://"))
    }

    /// The host part of the URL, for logging. It leaves out any user name, the path and the query.
    fn host(&self) -> &str {
        let rest = self
            .url
            .split_once("://")
            .map_or(self.url.as_str(), |(_, rest)| rest);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        authority.rsplit('@').next().unwrap_or_default()
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("host", &self.host())
            .field("headers", &self.headers.len())
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// The punctuation allowed in a header name next to letters and digits (RFC 9110 "token").
const TOKEN_SYMBOLS: &[u8] = b"!#$%&'*+-.^_`|~";

/// Whether `name` is a valid header name (an HTTP "token": printable ASCII without separators).
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || TOKEN_SYMBOLS.contains(&b))
}

/// Sends requests. It is cheap to create and holds no connection, so one value can serve every
/// request of a program, from any thread.
#[derive(Clone, Copy, Debug)]
pub struct Client {
    configuration: Configuration,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client with the settings from [`config`](crate::config): timeout, buffer sizes and the
    /// built-in root certificates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            configuration: Configuration {
                buffer_size: Some(HTTP_RX_BUFFER_BYTES),
                buffer_size_tx: Some(HTTP_TX_BUFFER_BYTES),
                timeout: Some(HTTP_TIMEOUT),
                follow_redirects_policy: FollowRedirectsPolicy::FollowGetHead,
                crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
                ..Configuration::default()
            },
        }
    }

    /// Changes how long a request may wait for network data before it fails.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.configuration.timeout = Some(timeout);
        self
    }

    /// Sends `request` and returns once the status and the headers of the response have arrived.
    /// The body is read afterwards from the [`Response`]. An error status such as 404 is **not** an
    /// error here: check [`Response::status`].
    ///
    /// # Errors
    ///
    /// Fails if the request is invalid, the clock is not set (HTTPS only), or the connection, the
    /// TLS handshake or the transfer fails.
    pub fn send(&self, request: &Request) -> Result<Response, Error> {
        request.validate()?;
        if request.is_https() && !clock::wait_synced(HTTP_CLOCK_WAIT) {
            return Err(Error::ClockNotSynced);
        }

        // A new connection for every request, see the module documentation.
        let mut connection = EspHttpConnection::new(&self.configuration)?;

        // The length is always given: without it `esp-idf-svc` would send a POST as chunked
        // and not send the body of a PUT or PATCH at all.
        let length = request.body.len().to_string();
        let mut headers: Vec<(&str, &str)> = request
            .headers
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("content-length"))
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        headers.push(("Content-Length", length.as_str()));

        connection.initiate_request(request.method.into(), &request.url, &headers)?;
        if !request.body.is_empty() {
            connection.write_all(&request.body)?;
        }
        connection.initiate_response()?;

        let status = connection.status();
        log::info!("{} {} -> {status}", request.method, request.host());
        Ok(Response { connection, status })
    }

    /// Sends a `GET` request. A shortcut for [`Client::send`] with [`Request::get`].
    ///
    /// # Errors
    ///
    /// The same as [`Client::send`].
    pub fn get(&self, url: &str) -> Result<Response, Error> {
        self.send(&Request::get(url))
    }
}

/// The response to a request: the status and headers are known, the body is still on the
/// connection. Read it with [`bytes`](Self::bytes), [`text`](Self::text) or [`io::Read`]. Dropping
/// the response closes the connection.
pub struct Response {
    connection: EspHttpConnection,
    status: u16,
}

impl Response {
    /// The status code, for example 200 or 404.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.status, 200..=299)
    }

    /// The value of a response header (the name is not case-sensitive). If the header appears more
    /// than once, this is the last value.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.connection.header(name)
    }

    /// The length of the body, if the server said so (`Content-Length`).
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.header("Content-Length")?.trim().parse().ok()
    }

    /// Reads the whole body into memory.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::BodyTooLarge`] if the body is longer than `HTTP_MAX_BODY_BYTES` (see
    /// [`config`](crate::config)), or if the transfer fails.
    pub fn bytes(mut self) -> Result<Vec<u8>, Error> {
        let limit = HTTP_MAX_BODY_BYTES;
        let announced = self
            .content_length()
            .map_or(0, |length| usize::try_from(length).unwrap_or(usize::MAX));
        if announced > limit {
            return Err(Error::BodyTooLarge(limit));
        }

        let mut body = Vec::with_capacity(announced);
        let mut chunk = [0_u8; READ_CHUNK_BYTES];
        loop {
            let read = self.connection.read(&mut chunk)?;
            if read == 0 {
                return Ok(body);
            }
            if body.len() + read > limit {
                return Err(Error::BodyTooLarge(limit));
            }
            body.extend_from_slice(&chunk[..read]);
        }
    }

    /// Reads the whole body into memory as UTF-8 text.
    ///
    /// # Errors
    ///
    /// The same as [`Response::bytes`], and [`Error::NotUtf8`] if the body is not valid UTF-8.
    pub fn text(self) -> Result<String, Error> {
        String::from_utf8(self.bytes()?).map_err(|_| Error::NotUtf8)
    }
}

/// Streams the body, for responses too large for [`Response::bytes`].
impl io::Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.connection.read(buf).map_err(io::Error::other)
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
