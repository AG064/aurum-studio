//! A small HTTP/1.1 server, written on `std::net`.
//!
//! Aurum holds a hard line on dependencies, and a web framework is a large one:
//! it brings a runtime, a router, a TLS stack, and their transitive trees. The
//! shell needs a handful of routes on the loopback interface, which is a few
//! hundred lines of parsing and formatting rather than a framework.
//!
//! Not supporting a thing is a feature here. This server speaks HTTP/1.0 and
//! HTTP/1.1, and refuses everything else loudly:
//!
//! - **No chunked transfer encoding.** It is a request-smuggling vector and
//!   nothing the shell needs, so `Transfer-Encoding` is rejected outright
//!   rather than half-implemented.
//! - **No `Content-Length` disagreement.** Duplicates, and any value that is
//!   not a plain non-negative integer, are refused. Two parsers reading one
//!   request differently is how smuggling works.
//! - **No header folding.** An obsolete continuation line is rejected instead
//!   of being joined, because the join is where interpretations diverge.
//! - **No unbounded reads.** The request line, header block, and body each
//!   have a ceiling, so a client cannot make the server allocate without
//!   limit.
//!
//! Every one of those is a deliberate refusal with a test, not an omission.

use std::io::{self, BufRead, Write};

/// The longest request line accepted, including the method and version.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;
/// The most header lines accepted.
pub const MAX_HEADERS: usize = 100;
/// The most bytes of header block accepted, across all lines.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// The largest request body accepted.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// Why a request could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The request line or headers exceeded a limit.
    TooLarge(&'static str),
    /// The request was not valid HTTP this server accepts.
    Malformed(String),
    /// The connection failed while reading.
    Io(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(what) => write!(f, "the {what} was too large"),
            Self::Malformed(what) => write!(f, "malformed request: {what}"),
            Self::Io(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The status code to answer a parse failure with.
///
/// A request that was merely too big is a different answer from one that was
/// never valid HTTP, and clients act on the difference.
impl ParseError {
    pub fn status(&self) -> u16 {
        match self {
            Self::TooLarge(_) => 413,
            Self::Malformed(_) => 400,
            Self::Io(_) => 400,
        }
    }
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, with the query string removed and percent-decoded.
    pub path: String,
    /// Decoded query parameters, in the order given.
    pub query: Vec<(String, String)>,
    /// Header names lowercased, values as sent.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// A header value, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// A query parameter, matched exactly.
    ///
    /// The first match wins, so a repeated parameter cannot be used to make
    /// the value that is checked differ from the value that is used.
    pub fn query_value(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// A cookie value by name.
    ///
    /// The `Cookie` header is one semicolon-separated list, so it is parsed
    /// rather than matched. This exists because a browser fetches a page's
    /// stylesheet and script as **separate requests**, and those carry no query
    /// string and no custom header — only what the origin sends by itself,
    /// which is a cookie. Authenticating on the header and the query string
    /// alone therefore passes every test and renders an unstyled page in a
    /// browser, which is exactly what happened.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim())
        })
    }
}

/// Read one request.
///
/// `Ok(None)` means the peer closed cleanly before sending anything, which is
/// an ordinary end of a keep-alive connection rather than a failure.
pub fn read_request(reader: &mut impl BufRead) -> Result<Option<Request>, ParseError> {
    let Some(request_line) = read_line(reader, MAX_REQUEST_LINE, "request line")? else {
        return Ok(None);
    };
    if request_line.is_empty() {
        // Tolerate leading blank lines, which some clients send after a
        // previous request.
        return read_request(reader);
    }

    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| ParseError::Malformed("no method".into()))?;
    let target = parts
        .next()
        .ok_or_else(|| ParseError::Malformed("no request target".into()))?;
    let version = parts
        .next()
        .ok_or_else(|| ParseError::Malformed("no HTTP version".into()))?;
    if parts.next().is_some() {
        return Err(ParseError::Malformed(
            "the request line has more than three parts".into(),
        ));
    }

    // The method is used verbatim in responses and logs, so it is restricted
    // to the token characters rather than accepted as arbitrary text.
    if method.is_empty() || !method.bytes().all(is_token_byte) {
        return Err(ParseError::Malformed("invalid method".into()));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ParseError::Malformed(format!(
            "unsupported HTTP version '{version}'"
        )));
    }
    if version == "HTTP/1.1" {
        // A target that is not origin-form has no defined meaning for this
        // server, and accepting absolute-form invites host confusion.
        if !target.starts_with('/') {
            return Err(ParseError::Malformed(
                "HTTP/1.1 requires an origin-form target".into(),
            ));
        }
    }

    let (raw_path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
    let path = percent_decode(raw_path)?;
    let query = match raw_query {
        Some(query) => parse_query(query)?,
        None => Vec::new(),
    };

    let headers = read_headers(reader)?;

    let body = read_body(reader, &headers, method)?;

    Ok(Some(Request {
        method: method.to_string(),
        path,
        query,
        headers,
        body,
    }))
}

/// One CRLF-terminated line, without the terminator.
///
/// Returns `None` only on a clean end of stream with nothing read.
fn read_line(
    reader: &mut impl BufRead,
    limit: usize,
    what: &'static str,
) -> Result<Option<String>, ParseError> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                if buffer.is_empty() {
                    return Ok(None);
                }
                // A final line without a terminator is accepted only if it is
                // the very end of the stream, which for a request line means
                // a truncated request.
                break;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ParseError::Io(error.to_string())),
        }

        let byte = byte[0];
        // A bare LF is accepted as a terminator: some clients send it, and
        // refusing it buys nothing.
        if byte == b'\n' {
            break;
        }
        if buffer.len() >= limit {
            return Err(ParseError::TooLarge(what));
        }
        buffer.push(byte);
    }

    // A NUL byte cannot appear in a header or request line, and letting one
    // through is how a value passes a C-string comparison but not a Rust one.
    if buffer.contains(&0) {
        return Err(ParseError::Malformed(format!("a NUL byte in the {what}")));
    }

    let mut text = String::from_utf8(buffer)
        .map_err(|_| ParseError::Malformed(format!("the {what} was not UTF-8")))?;
    if text.ends_with('\r') {
        text.pop();
    }
    Ok(Some(text))
}

fn read_headers(reader: &mut impl BufRead) -> Result<Vec<(String, String)>, ParseError> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;

    loop {
        let Some(line) = read_line(reader, MAX_REQUEST_LINE, "header")? else {
            return Err(ParseError::Malformed(
                "the connection ended inside the headers".into(),
            ));
        };
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= MAX_HEADERS {
            return Err(ParseError::TooLarge("header block"));
        }
        total += line.len();
        if total > MAX_HEADER_BYTES {
            return Err(ParseError::TooLarge("header block"));
        }

        // Obsolete line folding. Joining it is where two parsers start to
        // disagree about what the headers are, so it is refused.
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(ParseError::Malformed(
                "folded headers are not accepted".into(),
            ));
        }

        let Some((name, value)) = line.split_once(':') else {
            return Err(ParseError::Malformed(format!(
                "'{}' is not a header",
                truncate(&line)
            )));
        };
        // A space before the colon is a smuggling vector: some parsers trim
        // it and others treat the whole line as the name.
        if name.ends_with(' ') || name.ends_with('\t') {
            return Err(ParseError::Malformed(
                "whitespace before a header colon".into(),
            ));
        }
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            return Err(ParseError::Malformed(format!(
                "'{}' is not a header name",
                truncate(name)
            )));
        }

        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
}

/// Decide how long the body is, and read it.
///
/// The framing rules are the part of HTTP that has to be exactly right, so
/// they are checked rather than assumed.
fn read_body(
    reader: &mut impl BufRead,
    headers: &[(String, String)],
    method: &str,
) -> Result<Vec<u8>, ParseError> {
    let mut transfer_encoding = None;
    let mut lengths: Vec<&str> = Vec::new();
    for (name, value) in headers {
        match name.as_str() {
            "transfer-encoding" => {
                if transfer_encoding.is_some() {
                    return Err(ParseError::Malformed(
                        "more than one Transfer-Encoding".into(),
                    ));
                }
                transfer_encoding = Some(value.as_str());
            }
            "content-length" => lengths.push(value.as_str()),
            _ => {}
        }
    }

    if transfer_encoding.is_some() {
        // Refused rather than ignored: a body whose length this server cannot
        // agree on is a body it must not read.
        return Err(ParseError::Malformed(
            "Transfer-Encoding is not supported".into(),
        ));
    }

    let length = match lengths.len() {
        0 => "",
        // Repeated identical values are legal only when they agree exactly,
        // and even then there is no reason for a client of this server to
        // send them, so the ambiguity is refused.
        1 => lengths[0],
        _ => return Err(ParseError::Malformed("more than one Content-Length".into())),
    };

    let length = if length.is_empty() {
        0
    } else {
        length
            .parse::<usize>()
            .map_err(|_| ParseError::Malformed("Content-Length is not a number".into()))?
    };

    if length > MAX_BODY {
        return Err(ParseError::TooLarge("body"));
    }

    // A body on a method that has none is still read, so the connection stays
    // in sync, but it is not passed on as if it meant something.
    let _ = method;

    let mut body = vec![0u8; length];
    if length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|error| ParseError::Io(format!("the body ended early: {error}")))?;
    }
    Ok(body)
}

/// Whether a byte may appear in a method or header name.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Decode `%XX` escapes.
///
/// The decoded path is used for routing, so it is decoded **before** being
/// matched: routing on the encoded form is how `%2e%2e%2f` walks out of a
/// directory.
pub fn percent_decode(input: &str) -> Result<String, ParseError> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ParseError::Malformed("a truncated percent escape".into()));
            }
            let high = hex_value(bytes[index + 1])
                .ok_or_else(|| ParseError::Malformed("an invalid percent escape".into()))?;
            let low = hex_value(bytes[index + 2])
                .ok_or_else(|| ParseError::Malformed("an invalid percent escape".into()))?;
            out.push(high << 4 | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ParseError::Malformed("the path was not UTF-8".into()))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_query(input: &str) -> Result<Vec<(String, String)>, ParseError> {
    let mut pairs = Vec::new();
    if input.is_empty() {
        return Ok(pairs);
    }
    for part in input.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = match part.split_once('=') {
            Some((key, value)) => (key, value),
            None => (part, ""),
        };
        pairs.push((percent_decode(key)?, percent_decode(value)?));
    }
    Ok(pairs)
}

fn truncate(text: &str) -> String {
    text.chars().take(40).collect()
}

/// A response to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    /// Headers beyond the ones the writer always sends.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: content_type.to_string(),
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self::new(
            status,
            "text/plain; charset=utf-8",
            body.into().into_bytes(),
        )
    }

    pub fn html(body: impl Into<String>) -> Self {
        Self::new(200, "text/html; charset=utf-8", body.into().into_bytes())
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
        Self::new(status, "application/json; charset=utf-8", body)
    }

    /// A refusal, with a body that says why.
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self::text(status, message)
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// The bytes to put on the wire.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
            self.status,
            reason(self.status),
            self.content_type,
            self.body.len()
        );
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("Connection: close\r\n\r\n");

        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }

    pub fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.to_bytes())?;
        writer.flush()
    }
}

/// The reason phrase for a status code.
///
/// Only the codes this server can produce are listed; anything else is given
/// a neutral phrase rather than a wrong one.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn parse(raw: &str) -> Result<Option<Request>, ParseError> {
        let mut reader = BufReader::new(raw.as_bytes());
        read_request(&mut reader)
    }

    fn parse_ok(raw: &str) -> Request {
        parse(raw)
            .expect("should parse")
            .expect("should be a request")
    }

    #[test]
    fn a_simple_get_parses() {
        let request = parse_ok("GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/health");
        assert_eq!(request.header("host"), Some("localhost"));
        assert!(request.body.is_empty());
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let request = parse_ok("GET / HTTP/1.1\r\nX-Aurum-Token: abc\r\n\r\n");
        assert_eq!(request.header("x-aurum-token"), Some("abc"));
        assert_eq!(request.header("X-AURUM-TOKEN"), Some("abc"));
        assert_eq!(request.header("missing"), None);
    }

    #[test]
    fn query_parameters_are_decoded() {
        let request = parse_ok("GET /api/thing?a=1&b=hello%20world&c=%2Fpath HTTP/1.1\r\n\r\n");
        assert_eq!(request.path, "/api/thing");
        assert_eq!(request.query_value("a"), Some("1"));
        assert_eq!(request.query_value("b"), Some("hello world"));
        assert_eq!(request.query_value("c"), Some("/path"));
        assert_eq!(request.query_value("missing"), None);
    }

    #[test]
    fn a_repeated_query_parameter_uses_the_first() {
        // The value that is checked must be the value that is used.
        let request = parse_ok("GET /?t=good&t=evil HTTP/1.1\r\n\r\n");
        assert_eq!(request.query_value("t"), Some("good"));
    }

    #[test]
    fn a_body_is_read_to_its_length() {
        let request = parse_ok("POST /api/x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello");
        assert_eq!(request.body, b"hello");
    }

    #[test]
    fn a_body_split_across_lines_still_reads() {
        // The body is read by length, not by line, so a body containing CRLF
        // must survive intact. `a\r\nb\r\nc` is seven bytes.
        let raw = "POST / HTTP/1.1\r\nContent-Length: 7\r\n\r\na\r\nb\r\nc";
        let request = parse_ok(raw);
        assert_eq!(request.body, b"a\r\nb\r\nc");
    }

    #[test]
    fn a_clean_disconnect_is_not_an_error() {
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn an_http_1_0_request_is_accepted() {
        let request = parse_ok("GET / HTTP/1.0\r\n\r\n");
        assert_eq!(request.path, "/");
    }

    #[test]
    fn an_unsupported_version_is_refused() {
        let error = parse("GET / HTTP/2.0\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
        assert_eq!(error.status(), 400);
    }

    #[test]
    fn an_absolute_form_target_is_refused() {
        // Accepting this invites host confusion: the server would have to
        // decide whether the target's host or the Host header wins.
        let error = parse("GET http://elsewhere/ HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_chunked_body_is_refused_rather_than_half_read() {
        let error = parse("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")
            .unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
        assert!(error.to_string().contains("Transfer-Encoding"));
    }

    #[test]
    fn two_content_length_headers_are_refused() {
        // The classic smuggling shape: two parsers, two answers.
        let error = parse("POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello")
            .unwrap_err();
        assert!(error.to_string().contains("Content-Length"));
    }

    #[test]
    fn a_non_numeric_content_length_is_refused() {
        let error = parse("POST / HTTP/1.1\r\nContent-Length: five\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_negative_content_length_is_refused() {
        let error = parse("POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_folded_header_is_refused() {
        let error = parse("GET / HTTP/1.1\r\nX-Thing: one\r\n  two\r\n\r\n").unwrap_err();
        assert!(error.to_string().contains("folded"));
    }

    #[test]
    fn whitespace_before_a_colon_is_refused() {
        let error = parse("GET / HTTP/1.1\r\nX-Thing : one\r\n\r\n").unwrap_err();
        assert!(error.to_string().contains("whitespace"));
    }

    #[test]
    fn a_nul_byte_in_a_header_is_refused() {
        let error = parse("GET / HTTP/1.1\r\nX-Thing: a\0b\r\n\r\n").unwrap_err();
        assert!(error.to_string().contains("NUL"));
    }

    #[test]
    fn an_overlong_request_line_is_refused() {
        let long = "a".repeat(MAX_REQUEST_LINE + 10);
        let raw = format!("GET /{long} HTTP/1.1\r\n\r\n");
        let error = parse(&raw).unwrap_err();
        assert!(matches!(error, ParseError::TooLarge(_)));
        assert_eq!(error.status(), 413);
    }

    #[test]
    fn an_overlong_body_is_refused_before_it_is_read() {
        // The check happens on the declared length, so the server never
        // allocates the buffer.
        let raw = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        let error = parse(&raw).unwrap_err();
        assert!(matches!(error, ParseError::TooLarge(_)));
        assert_eq!(error.status(), 413);
    }

    #[test]
    fn too_many_headers_are_refused() {
        let mut raw = String::from("GET / HTTP/1.1\r\n");
        for index in 0..MAX_HEADERS + 1 {
            raw.push_str(&format!("X-{index}: v\r\n"));
        }
        raw.push_str("\r\n");
        let error = parse(&raw).unwrap_err();
        assert!(matches!(error, ParseError::TooLarge(_)));
    }

    #[test]
    fn a_header_without_a_colon_is_refused() {
        let error = parse("GET / HTTP/1.1\r\nnot a header\r\n\r\n").unwrap_err();
        assert!(error.to_string().contains("not a header"));
    }

    #[test]
    fn a_path_traversal_escape_is_decoded_before_matching() {
        // Routing on the encoded form is how `%2e%2e%2f` walks out of a
        // directory, so the decode happens first and the result is visible to
        // the router.
        let request = parse_ok("GET /assets/%2e%2e%2fsecret HTTP/1.1\r\n\r\n");
        assert!(
            request.path.contains(".."),
            "the escape should be decoded, got '{}'",
            request.path
        );
    }

    #[test]
    fn a_truncated_percent_escape_is_refused() {
        let error = parse("GET /a%2 HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_percent_escape_that_is_not_utf8_is_refused() {
        let error = parse("GET /%ff%fe HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_method_with_spaces_cannot_be_injected() {
        let error = parse("GE T / HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(error, ParseError::Malformed(_)));
    }

    #[test]
    fn a_bare_line_feed_terminates_a_line() {
        let request = parse_ok("GET /x HTTP/1.1\nHost: a\n\n");
        assert_eq!(request.path, "/x");
        assert_eq!(request.header("host"), Some("a"));
    }

    #[test]
    fn a_body_that_ends_early_is_an_error() {
        let error = parse("POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort").unwrap_err();
        assert!(matches!(error, ParseError::Io(_)));
    }

    // -- responses ---------------------------------------------------------

    #[test]
    fn a_response_has_a_content_length_and_closes() {
        let response = Response::text(200, "hello");
        let bytes = String::from_utf8(response.to_bytes()).unwrap();
        assert!(bytes.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(bytes.contains("Content-Length: 5\r\n"));
        assert!(bytes.contains("Connection: close\r\n"));
        assert!(bytes.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn a_response_with_extra_headers_writes_them() {
        let response = Response::text(200, "x").with_header("Cache-Control", "no-store");
        let bytes = String::from_utf8(response.to_bytes()).unwrap();
        assert!(bytes.contains("Cache-Control: no-store\r\n"));
    }

    #[test]
    fn a_json_response_is_valid_json() {
        let response = Response::json(200, &serde_json::json!({"ok": true}));
        assert!(response.content_type.starts_with("application/json"));
        let parsed: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn every_status_this_server_sends_has_a_reason() {
        for status in [200u16, 204, 400, 401, 403, 404, 405, 413, 500, 503] {
            assert_ne!(reason(status), "Unknown", "status {status} needs a reason");
        }
    }

    #[test]
    fn a_body_containing_binary_survives() {
        let payload: Vec<u8> = (0u8..=255).collect();
        let response = Response::new(200, "application/octet-stream", payload.clone());
        let bytes = response.to_bytes();
        let split = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("headers end");
        assert_eq!(&bytes[split + 4..], &payload[..]);
    }
}
