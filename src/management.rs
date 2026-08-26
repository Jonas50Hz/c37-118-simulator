//! Bounded HTTP parsing and response encoding for the management surface.

use std::{
    error::Error,
    fmt,
    io::{self, Write},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 8 * 1024;
pub const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;

const MAX_REQUEST_HEADERS: usize = 32;
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const EMPTY_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
type StaticHeader = (&'static str, &'static str);
const ALLOW_GET_HEADERS: [StaticHeader; 1] = [("Allow", "GET")];
const ALLOW_POST_HEADERS: [StaticHeader; 1] = [("Allow", "POST")];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioTarget {
    pub stream_id: u16,
    #[serde(default)]
    pub connection_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareScenarioRequest {
    pub target: ScenarioTarget,
    pub scenario_name: String,
    #[serde(default)]
    pub actor_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmScenarioRequest {
    pub token: u64,
    pub actor_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearScenarioRequest {
    pub target: ScenarioTarget,
    #[serde(default)]
    pub actor_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelScenarioRequest {
    pub token: u64,
    pub actor_label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioTokenRequestBody {
    token: serde_json::Value,
    #[serde(default)]
    actor_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleStateCursor {
    pub process_identity: String,
    pub controller_revision: u64,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagementRequest {
    Healthz,
    Readyz,
    Metrics,
    Catalog,
    State,
    ConsoleState { cursor: Option<ConsoleStateCursor> },
    Prepare(PrepareScenarioRequest),
    Confirm(ConfirmScenarioRequest),
    Clear(ClearScenarioRequest),
    Cancel(CancelScenarioRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Complete(ManagementRequest),
    Incomplete,
    Error(ParseError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    RequestTooLarge,
    IncompleteAtLimit,
    MalformedRequest,
    TooManyHeaders,
    UnsupportedHttpVersion,
    MissingHost,
    DuplicateHost,
    InvalidHost,
    UnsupportedTransferEncoding,
    DuplicateContentLength,
    InvalidContentLength,
    DuplicateContentType,
    UnsupportedMediaType,
    ContentLengthRequired,
    BodyTooLarge,
    BodyNotAllowed,
    TrailingData,
    MethodNotAllowed(AllowedMethod),
    UnknownPath,
    InvalidJsonBody,
    InvalidTarget,
    InvalidToken,
    InvalidQuery,
}

impl ParseError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "request_too_large",
            Self::IncompleteAtLimit => "incomplete_at_limit",
            Self::MalformedRequest => "malformed_request",
            Self::TooManyHeaders => "too_many_headers",
            Self::UnsupportedHttpVersion => "unsupported_http_version",
            Self::MissingHost => "missing_host",
            Self::DuplicateHost => "duplicate_host",
            Self::InvalidHost => "invalid_host",
            Self::UnsupportedTransferEncoding => "unsupported_transfer_encoding",
            Self::DuplicateContentLength => "duplicate_content_length",
            Self::InvalidContentLength => "invalid_content_length",
            Self::DuplicateContentType => "duplicate_content_type",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::ContentLengthRequired => "content_length_required",
            Self::BodyTooLarge => "body_too_large",
            Self::BodyNotAllowed => "body_not_allowed",
            Self::TrailingData => "trailing_data",
            Self::MethodNotAllowed(_) => "method_not_allowed",
            Self::UnknownPath => "unknown_path",
            Self::InvalidJsonBody => "invalid_json_body",
            Self::InvalidTarget => "invalid_target",
            Self::InvalidToken => "invalid_token",
            Self::InvalidQuery => "invalid_query",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "request exceeds the 8192-byte limit",
            Self::IncompleteAtLimit => "request is incomplete at the 8192-byte limit",
            Self::MalformedRequest => "request is not valid HTTP/1.1",
            Self::TooManyHeaders => "request has too many headers",
            Self::UnsupportedHttpVersion => "only HTTP/1.1 is supported",
            Self::MissingHost => "HTTP/1.1 requests require a host header",
            Self::DuplicateHost => "host must appear exactly once",
            Self::InvalidHost => {
                "host must be nonempty and contain no ASCII whitespace or control characters"
            }
            Self::UnsupportedTransferEncoding => "transfer encoding is not supported",
            Self::DuplicateContentLength => "content-length must appear at most once",
            Self::InvalidContentLength => "content-length must be a single unsigned decimal integer",
            Self::DuplicateContentType => "content-type must appear at most once",
            Self::UnsupportedMediaType => {
                "POST requests must use application/json when content-type is provided"
            }
            Self::ContentLengthRequired => "POST requests require content-length",
            Self::BodyTooLarge => "request body exceeds the 8192-byte limit",
            Self::BodyNotAllowed => "GET requests must not include a body",
            Self::TrailingData => "request contains trailing bytes",
            Self::MethodNotAllowed(_) => "method is not allowed for this path",
            Self::UnknownPath => "path is not supported",
            Self::InvalidJsonBody => "request body must be a valid JSON object",
            Self::InvalidTarget => "target stream_id must be greater than zero",
            Self::InvalidToken => "token must be greater than zero",
            Self::InvalidQuery => "query parameters are not valid for this endpoint",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Self::RequestTooLarge | Self::IncompleteAtLimit | Self::BodyTooLarge => {
                StatusCode::PayloadTooLarge
            }
            Self::UnsupportedMediaType => StatusCode::UnsupportedMediaType,
            Self::MethodNotAllowed(_) => StatusCode::MethodNotAllowed,
            Self::UnknownPath => StatusCode::NotFound,
            Self::MalformedRequest
            | Self::TooManyHeaders
            | Self::UnsupportedHttpVersion
            | Self::MissingHost
            | Self::DuplicateHost
            | Self::InvalidHost
            | Self::UnsupportedTransferEncoding
            | Self::DuplicateContentLength
            | Self::InvalidContentLength
            | Self::DuplicateContentType
            | Self::ContentLengthRequired
            | Self::BodyNotAllowed
            | Self::TrailingData
            | Self::InvalidJsonBody
            | Self::InvalidTarget
            | Self::InvalidToken
            | Self::InvalidQuery => StatusCode::BadRequest,
        }
    }

    const fn allowed_method(self) -> Option<AllowedMethod> {
        match self {
            Self::MethodNotAllowed(method) => Some(method),
            _ => None,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    Accepted,
    NoContent,
    BadRequest,
    NotFound,
    MethodNotAllowed,
    NotAcceptable,
    PayloadTooLarge,
    UnsupportedMediaType,
    InternalServerError,
    ServiceUnavailable,
}

impl StatusCode {
    const fn code_and_reason(self) -> (u16, &'static str) {
        match self {
            Self::Ok => (200, "OK"),
            Self::Accepted => (202, "Accepted"),
            Self::NoContent => (204, "No Content"),
            Self::BadRequest => (400, "Bad Request"),
            Self::NotFound => (404, "Not Found"),
            Self::MethodNotAllowed => (405, "Method Not Allowed"),
            Self::NotAcceptable => (406, "Not Acceptable"),
            Self::PayloadTooLarge => (413, "Payload Too Large"),
            Self::UnsupportedMediaType => (415, "Unsupported Media Type"),
            Self::InternalServerError => (500, "Internal Server Error"),
            Self::ServiceUnavailable => (503, "Service Unavailable"),
        }
    }
}

#[derive(Debug)]
pub enum ResponseEncodeError {
    Json(serde_json::Error),
    BodyTooLarge { actual: usize, maximum: usize },
    NoContentBody { actual: usize },
}

impl fmt::Display for ResponseEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "cannot encode JSON response: {error}"),
            Self::BodyTooLarge { actual, maximum } => {
                write!(formatter, "response body is {actual} bytes; maximum is {maximum}")
            }
            Self::NoContentBody { actual } => {
                write!(formatter, "204 No Content responses cannot include a {actual}-byte body")
            }
        }
    }
}

impl Error for ResponseEncodeError {}

pub fn parse(input: &[u8]) -> ParseOutcome {
    if input.len() > MAX_REQUEST_BYTES {
        return ParseOutcome::Error(ParseError::RequestTooLarge);
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_REQUEST_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let header_end = match request.parse(input) {
        Ok(httparse::Status::Complete(length)) => length,
        Ok(httparse::Status::Partial) => return incomplete_or_at_limit(input.len()),
        Err(httparse::Error::TooManyHeaders) => {
            return ParseOutcome::Error(ParseError::TooManyHeaders)
        }
        Err(_) => return ParseOutcome::Error(ParseError::MalformedRequest),
    };

    if request.version != Some(1) {
        return ParseOutcome::Error(ParseError::UnsupportedHttpVersion);
    }
    let framing = match inspect_headers(request.headers) {
        Ok(framing) => framing,
        Err(error) => return ParseOutcome::Error(error),
    };
    let route = match route_for(request.method, request.path) {
        Ok(route) => route,
        Err(error) => return ParseOutcome::Error(error),
    };

    if route.requires_json_body()
        && framing
            .content_type
            .is_some_and(|content_type| !is_json_content_type(content_type))
    {
        return ParseOutcome::Error(ParseError::UnsupportedMediaType);
    }

    let body_length = match framing.content_length {
        Some(length) => length,
        None if route.requires_json_body() => {
            return ParseOutcome::Error(ParseError::ContentLengthRequired)
        }
        None => 0,
    };
    if body_length > MAX_REQUEST_BYTES {
        return ParseOutcome::Error(ParseError::BodyTooLarge);
    }
    if !route.requires_json_body() && body_length != 0 {
        return ParseOutcome::Error(ParseError::BodyNotAllowed);
    }

    let request_end = match header_end.checked_add(body_length) {
        Some(length) if length <= MAX_REQUEST_BYTES => length,
        _ => return ParseOutcome::Error(ParseError::RequestTooLarge),
    };
    if input.len() < request_end {
        return incomplete_or_at_limit(input.len());
    }
    if input.len() > request_end {
        return ParseOutcome::Error(ParseError::TrailingData);
    }

    let body = &input[header_end..request_end];
    match route {
        Route::Healthz => ParseOutcome::Complete(ManagementRequest::Healthz),
        Route::Readyz => ParseOutcome::Complete(ManagementRequest::Readyz),
        Route::Metrics => ParseOutcome::Complete(ManagementRequest::Metrics),
        Route::Catalog => ParseOutcome::Complete(ManagementRequest::Catalog),
        Route::State => ParseOutcome::Complete(ManagementRequest::State),
        Route::ConsoleState(cursor) => ParseOutcome::Complete(ManagementRequest::ConsoleState {
            cursor,
        }),
        Route::Prepare => parse_prepare(body),
        Route::Confirm => parse_confirm(body),
        Route::Clear => parse_clear(body),
        Route::Cancel => parse_cancel(body),
    }
}

pub fn json_success<T: Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<Vec<u8>, ResponseEncodeError> {
    json_response(status, value, &[])
}

pub fn json_error(
    status: StatusCode,
    code: &str,
    message: &str,
) -> Result<Vec<u8>, ResponseEncodeError> {
    json_response(
        status,
        &ErrorEnvelope {
            error: ErrorDetails { code, message },
        },
        &[],
    )
}

pub fn parse_error_response(error: ParseError) -> Result<Vec<u8>, ResponseEncodeError> {
    let extra_headers: &[StaticHeader] = match error.allowed_method() {
        Some(AllowedMethod::Get) => &ALLOW_GET_HEADERS,
        Some(AllowedMethod::Post) => &ALLOW_POST_HEADERS,
        None => &[],
    };
    json_response(
        error.status(),
        &ErrorEnvelope {
            error: ErrorDetails {
                code: error.code(),
                message: error.message(),
            },
        },
        extra_headers,
    )
}

pub fn prometheus_text(
    status: StatusCode,
    body: impl AsRef<[u8]>,
) -> Result<Vec<u8>, ResponseEncodeError> {
    encode_response(status, PROMETHEUS_CONTENT_TYPE, body.as_ref(), &[])
}

pub fn empty_response(status: StatusCode) -> Vec<u8> {
    encode_response(status, EMPTY_CONTENT_TYPE, b"", &[])
        .expect("an empty response cannot exceed the response body limit")
}

#[derive(Debug, Clone)]
enum Route {
    Healthz,
    Readyz,
    Metrics,
    Catalog,
    State,
    ConsoleState(Option<ConsoleStateCursor>),
    Prepare,
    Confirm,
    Clear,
    Cancel,
}

impl Route {
    const fn requires_json_body(&self) -> bool {
        matches!(self, Self::Prepare | Self::Confirm | Self::Clear | Self::Cancel)
    }
}

struct RequestFraming<'a> {
    content_length: Option<usize>,
    content_type: Option<&'a [u8]>,
}

struct BoundedJsonWriter {
    body: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl BoundedJsonWriter {
    fn new(maximum: usize) -> Self {
        Self {
            body: Vec::new(),
            maximum,
            exceeded: false,
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.maximum.saturating_sub(self.body.len()) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "JSON response body exceeds the configured limit",
            ));
        }
        self.body.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorDetails<'a>,
}

#[derive(Serialize)]
struct ErrorDetails<'a> {
    code: &'a str,
    message: &'a str,
}

fn incomplete_or_at_limit(length: usize) -> ParseOutcome {
    if length == MAX_REQUEST_BYTES {
        ParseOutcome::Error(ParseError::IncompleteAtLimit)
    } else {
        ParseOutcome::Incomplete
    }
}

fn inspect_headers<'a>(
    headers: &[httparse::Header<'a>],
) -> Result<RequestFraming<'a>, ParseError> {
    let mut content_length = None;
    let mut content_type = None;
    let mut host_count = 0;

    for header in headers {
        if header.name.eq_ignore_ascii_case("host") {
            host_count += 1;
            if !is_valid_host(trim_leading_http_whitespace(header.value)) {
                return Err(ParseError::InvalidHost);
            }
        }
        if header.name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ParseError::UnsupportedTransferEncoding);
        }
        if header.name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ParseError::DuplicateContentLength);
            }
            content_length = Some(parse_content_length(header.value)?);
        }
        if header.name.eq_ignore_ascii_case("content-type") {
            if content_type.is_some() {
                return Err(ParseError::DuplicateContentType);
            }
            content_type = Some(header.value);
        }
    }

    if host_count == 0 {
        return Err(ParseError::MissingHost);
    }
    if host_count > 1 {
        return Err(ParseError::DuplicateHost);
    }
    Ok(RequestFraming {
        content_length,
        content_type,
    })
}

fn parse_content_length(value: &[u8]) -> Result<usize, ParseError> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(ParseError::InvalidContentLength);
    }
    value.iter().try_fold(0usize, |length, byte| {
        length
            .checked_mul(10)
            .and_then(|length| length.checked_add(usize::from(*byte - b'0')))
            .ok_or(ParseError::InvalidContentLength)
    })
}

fn trim_leading_http_whitespace(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| *byte != b' ' && *byte != b'\t')
        .unwrap_or(value.len());
    &value[start..]
}

fn is_valid_host(value: &[u8]) -> bool {
    !value.is_empty()
        && !value
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
}

fn route_for(method: Option<&str>, request_target: Option<&str>) -> Result<Route, ParseError> {
    let method = method.ok_or(ParseError::MalformedRequest)?;
    let request_target = request_target.ok_or(ParseError::MalformedRequest)?;
    let (path, query) = match request_target.split_once('?') {
        Some((path, query)) if !query.contains('?') => (path, Some(query)),
        Some(_) => return Err(ParseError::InvalidQuery),
        None => (request_target, None),
    };
    let allowed = match path {
        "/healthz" | "/readyz" | "/metrics" | "/v1/catalog" | "/v1/state" => {
            AllowedMethod::Get
        }
        "/v1/scenarios/prepare"
        | "/v1/scenarios/confirm"
        | "/v1/scenarios/clear"
        | "/v1/scenarios/cancel" => {
            AllowedMethod::Post
        }
        _ => return Err(ParseError::UnknownPath),
    };
    if (allowed == AllowedMethod::Get && method != "GET")
        || (allowed == AllowedMethod::Post && method != "POST")
    {
        return Err(ParseError::MethodNotAllowed(allowed));
    }
    match (path, query) {
        ("/healthz", None) => Ok(Route::Healthz),
        ("/readyz", None) => Ok(Route::Readyz),
        ("/metrics", None) => Ok(Route::Metrics),
        ("/v1/catalog", None) => Ok(Route::Catalog),
        ("/v1/state", None) => Ok(Route::State),
        ("/v1/state", Some(query)) => parse_console_state_route(query),
        ("/v1/scenarios/prepare", None) => Ok(Route::Prepare),
        ("/v1/scenarios/confirm", None) => Ok(Route::Confirm),
        ("/v1/scenarios/clear", None) => Ok(Route::Clear),
        ("/v1/scenarios/cancel", None) => Ok(Route::Cancel),
        (_, Some(_)) => Err(ParseError::InvalidQuery),
        _ => unreachable!("known route must map to a route value"),
    }
}

fn parse_console_state_route(query: &str) -> Result<Route, ParseError> {
    if query.is_empty() {
        return Err(ParseError::InvalidQuery);
    }

    let mut format = None;
    let mut cursor = None;
    for parameter in query.split('&') {
        let Some((name, value)) = parameter.split_once('=') else {
            return Err(ParseError::InvalidQuery);
        };
        match name {
            "format" if format.replace(value).is_none() => {}
            "cursor" if cursor.replace(parse_console_cursor(value)?).is_none() => {}
            _ => return Err(ParseError::InvalidQuery),
        }
    }

    if format == Some("console-v1") {
        Ok(Route::ConsoleState(cursor))
    } else {
        Err(ParseError::InvalidQuery)
    }
}

fn parse_console_cursor(value: &str) -> Result<ConsoleStateCursor, ParseError> {
    let mut parts = value.split(':');
    let process_identity = parts.next().ok_or(ParseError::InvalidQuery)?;
    let controller_revision = parse_decimal_u64(parts.next().ok_or(ParseError::InvalidQuery)?)?;
    let offset = usize::try_from(parse_decimal_u64(parts.next().ok_or(ParseError::InvalidQuery)?)?)
        .map_err(|_| ParseError::InvalidQuery)?;
    if parts.next().is_some()
        || process_identity.len() != 64
        || !process_identity.bytes().all(|byte| {
            byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
        })
    {
        return Err(ParseError::InvalidQuery);
    }
    Ok(ConsoleStateCursor {
        process_identity: process_identity.to_owned(),
        controller_revision,
        offset,
    })
}

fn parse_decimal_u64(value: &str) -> Result<u64, ParseError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ParseError::InvalidQuery);
    }
    value.bytes().try_fold(0_u64, |number, byte| {
        number
            .checked_mul(10)
            .and_then(|number| number.checked_add(u64::from(byte - b'0')))
            .ok_or(ParseError::InvalidQuery)
    })
}

fn is_json_content_type(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
}

fn parse_json_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ParseError> {
    serde_json::from_slice(body).map_err(|_| ParseError::InvalidJsonBody)
}

fn parse_prepare(body: &[u8]) -> ParseOutcome {
    let request = match parse_json_body::<PrepareScenarioRequest>(body) {
        Ok(request) => request,
        Err(error) => return ParseOutcome::Error(error),
    };
    if request.target.stream_id == 0 {
        return ParseOutcome::Error(ParseError::InvalidTarget);
    }
    ParseOutcome::Complete(ManagementRequest::Prepare(request))
}

fn parse_confirm(body: &[u8]) -> ParseOutcome {
    let (token, actor_label) = match parse_scenario_token_request(
        body,
        parse_legacy_compatible_scenario_token,
    ) {
        Ok(request) => request,
        Err(error) => return ParseOutcome::Error(error),
    };
    ParseOutcome::Complete(ManagementRequest::Confirm(ConfirmScenarioRequest {
        token,
        actor_label,
    }))
}

fn parse_clear(body: &[u8]) -> ParseOutcome {
    let request = match parse_json_body::<ClearScenarioRequest>(body) {
        Ok(request) => request,
        Err(error) => return ParseOutcome::Error(error),
    };
    if request.target.stream_id == 0 {
        return ParseOutcome::Error(ParseError::InvalidTarget);
    }
    ParseOutcome::Complete(ManagementRequest::Clear(request))
}

fn parse_cancel(body: &[u8]) -> ParseOutcome {
    let (token, actor_label) = match parse_scenario_token_request(body, parse_canonical_scenario_token)
    {
        Ok(request) => request,
        Err(error) => return ParseOutcome::Error(error),
    };
    ParseOutcome::Complete(ManagementRequest::Cancel(CancelScenarioRequest {
        token,
        actor_label,
    }))
}

fn parse_scenario_token_request(
    body: &[u8],
    parse_token: fn(serde_json::Value) -> Option<u64>,
) -> Result<(u64, Option<String>), ParseError> {
    let request = parse_json_body::<ScenarioTokenRequestBody>(body)?;
    let token = parse_token(request.token).ok_or(ParseError::InvalidToken)?;
    Ok((token, request.actor_label))
}

fn parse_legacy_compatible_scenario_token(token: serde_json::Value) -> Option<u64> {
    match token {
        serde_json::Value::String(token) => parse_canonical_decimal_token(&token),
        serde_json::Value::Number(token) => parse_canonical_decimal_token(&token.to_string()),
        _ => None,
    }
}

fn parse_canonical_scenario_token(token: serde_json::Value) -> Option<u64> {
    let serde_json::Value::String(token) = token else {
        return None;
    };
    parse_canonical_decimal_token(&token)
}

fn parse_canonical_decimal_token(token: &str) -> Option<u64> {
    let bytes = token.as_bytes();
    if !matches!(bytes.first(), Some(b'1'..=b'9'))
        || !bytes.iter().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*byte - b'0')))
    })
}

fn json_response<T: Serialize>(
    status: StatusCode,
    value: &T,
    extra_headers: &[StaticHeader],
) -> Result<Vec<u8>, ResponseEncodeError> {
    let maximum = if status == StatusCode::NoContent {
        0
    } else {
        MAX_RESPONSE_BODY_BYTES
    };
    let mut writer = BoundedJsonWriter::new(maximum);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.exceeded {
            return if status == StatusCode::NoContent {
                Err(ResponseEncodeError::NoContentBody { actual: 1 })
            } else {
                Err(ResponseEncodeError::BodyTooLarge {
                    actual: MAX_RESPONSE_BODY_BYTES + 1,
                    maximum: MAX_RESPONSE_BODY_BYTES,
                })
            };
        }
        return Err(ResponseEncodeError::Json(error));
    }
    encode_response(status, JSON_CONTENT_TYPE, &writer.body, extra_headers)
}

fn encode_response(
    status: StatusCode,
    content_type: &'static str,
    body: &[u8],
    extra_headers: &[StaticHeader],
) -> Result<Vec<u8>, ResponseEncodeError> {
    if status == StatusCode::NoContent {
        if !body.is_empty() {
            return Err(ResponseEncodeError::NoContentBody { actual: body.len() });
        }
        let (code, reason) = status.code_and_reason();
        let mut response = format!("HTTP/1.1 {code} {reason}\r\n").into_bytes();
        append_static_headers(&mut response, extra_headers);
        response.extend_from_slice(b"Connection: close\r\n\r\n");
        return Ok(response);
    }
    if body.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(ResponseEncodeError::BodyTooLarge {
            actual: body.len(),
            maximum: MAX_RESPONSE_BODY_BYTES,
        });
    }
    let (code, reason) = status.code_and_reason();
    let mut response = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
        body.len()
    )
    .into_bytes();
    append_static_headers(&mut response, extra_headers);
    response.extend_from_slice(b"Connection: close\r\n\r\n");
    response.extend_from_slice(body);
    Ok(response)
}

fn append_static_headers(response: &mut Vec<u8>, headers: &[StaticHeader]) {
    for (name, value) in headers {
        response.extend_from_slice(name.as_bytes());
        response.extend_from_slice(b": ");
        response.extend_from_slice(value.as_bytes());
        response.extend_from_slice(b"\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Success<'a> {
        message: &'a str,
    }

    fn json_post(path: &str, body: &str) -> Vec<u8> {
        format!(
            "POST {path} HTTP/1.1\r\nHost: simulator\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn complete(input: &[u8]) -> ManagementRequest {
        match parse(input) {
            ParseOutcome::Complete(request) => request,
            outcome => panic!("expected complete request, got {outcome:?}"),
        }
    }

    fn assert_error(input: &[u8], expected: ParseError) {
        assert_eq!(parse(input), ParseOutcome::Error(expected));
    }

    fn response_parts(response: &[u8]) -> (&str, &[u8]) {
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("response separator");
        (
            std::str::from_utf8(&response[..separator]).expect("headers UTF-8"),
            &response[separator + 4..],
        )
    }

    #[test]
    fn parses_all_routes_and_typed_bodies() {
        for (path, expected) in [
            ("/healthz", ManagementRequest::Healthz),
            ("/readyz", ManagementRequest::Readyz),
            ("/metrics", ManagementRequest::Metrics),
            ("/v1/catalog", ManagementRequest::Catalog),
            ("/v1/state", ManagementRequest::State),
        ] {
            assert_eq!(
                complete(format!("GET {path} HTTP/1.1\r\nHost: simulator\r\n\r\n").as_bytes()),
                expected
            );
        }
        assert_eq!(
            complete(
                b"GET /v1/state?format=console-v1 HTTP/1.1\r\nHost: simulator\r\n\r\n",
            ),
            ManagementRequest::ConsoleState { cursor: None }
        );
        assert_eq!(
            complete(
                b"GET /v1/state?cursor=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:7:48&format=console-v1 HTTP/1.1\r\nHost: simulator\r\n\r\n",
            ),
            ManagementRequest::ConsoleState {
                cursor: Some(ConsoleStateCursor {
                    process_identity: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_owned(),
                    controller_revision: 7,
                    offset: 48,
                }),
            }
        );
        assert_eq!(
            complete(&json_post(
                "/v1/scenarios/prepare",
                r#"{"target":{"stream_id":1001,"connection_id":44},"scenario_name":"missing-frames","actor_label":"operator"}"#,
            )),
            ManagementRequest::Prepare(PrepareScenarioRequest {
                target: ScenarioTarget {
                    stream_id: 1001,
                    connection_id: Some(44),
                },
                scenario_name: "missing-frames".to_owned(),
                actor_label: Some("operator".to_owned()),
            })
        );
        assert_eq!(
            complete(&json_post("/v1/scenarios/confirm", r#"{"token":"19"}"#)),
            ManagementRequest::Confirm(ConfirmScenarioRequest {
                token: 19,
                actor_label: None,
            })
        );
        assert_eq!(
            complete(&json_post("/v1/scenarios/confirm", r#"{"token":19}"#)),
            ManagementRequest::Confirm(ConfirmScenarioRequest {
                token: 19,
                actor_label: None,
            })
        );
        assert_eq!(
            complete(&json_post("/v1/scenarios/clear", r#"{"target":{"stream_id":1001}}"#)),
            ManagementRequest::Clear(ClearScenarioRequest {
                target: ScenarioTarget {
                    stream_id: 1001,
                    connection_id: None,
                },
                actor_label: None,
            })
        );
        assert_eq!(
            complete(&json_post(
                "/v1/scenarios/cancel",
                r#"{"token":"19","actor_label":"canceller"}"#,
            )),
            ManagementRequest::Cancel(CancelScenarioRequest {
                token: 19,
                actor_label: Some("canceller".to_owned()),
            })
        );
    }

    #[test]
    fn scenario_token_routes_allow_legacy_numbers_only_for_confirmation() {
        assert_eq!(
            complete(&json_post("/v1/scenarios/confirm", r#"{"token":19}"#)),
            ManagementRequest::Confirm(ConfirmScenarioRequest {
                token: 19,
                actor_label: None,
            })
        );
        assert_error(
            &json_post("/v1/scenarios/cancel", r#"{"token":19}"#),
            ParseError::InvalidToken,
        );
        assert_eq!(
            complete(&json_post("/v1/scenarios/cancel", r#"{"token":"19"}"#)),
            ManagementRequest::Cancel(CancelScenarioRequest {
                token: 19,
                actor_label: None,
            })
        );
    }

    #[test]
    fn rejects_noncanonical_or_invalid_scenario_tokens() {
        for token in [
            "0",
            "-1",
            "1.0",
            "1e0",
            "true",
            "null",
            "\"0\"",
            "\"01\"",
            "\"+1\"",
            "\" 1\"",
            "\"1 \"",
            "\"18446744073709551616\"",
        ] {
            let body = format!(r#"{{"token":{token}}}"#);
            assert_error(
                &json_post("/v1/scenarios/confirm", &body),
                ParseError::InvalidToken,
            );
            assert_error(
                &json_post("/v1/scenarios/cancel", &body),
                ParseError::InvalidToken,
            );
        }
        assert_eq!(
            complete(&json_post(
                "/v1/scenarios/confirm",
                r#"{"token":"18446744073709551615"}"#,
            )),
            ManagementRequest::Confirm(ConfirmScenarioRequest {
                token: u64::MAX,
                actor_label: None,
            })
        );
    }

    #[test]
    fn rejects_ambiguous_or_invalid_framing() {
        assert_error(b"GET /healthz HTTP/1.1\r\n\r\n", ParseError::MissingHost);
        assert_error(
            b"GET /healthz HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
            ParseError::DuplicateHost,
        );
        assert_error(
            b"GET /healthz HTTP/1.1\r\nHost: a b\r\n\r\n",
            ParseError::InvalidHost,
        );
        assert_error(
            b"POST /v1/scenarios/confirm HTTP/1.1\r\nHost: simulator\r\nTransfer-Encoding: chunked\r\nContent-Length: 11\r\n\r\n{\"token\":1}",
            ParseError::UnsupportedTransferEncoding,
        );
        assert_error(
            b"POST /v1/scenarios/confirm HTTP/1.1\r\nHost: simulator\r\nContent-Length: 11, 11\r\n\r\n{\"token\":1}",
            ParseError::InvalidContentLength,
        );
        assert_error(
            b"GET /healthz HTTP/1.1\r\nHost: simulator\r\nContent-Length: 1\r\n\r\nx",
            ParseError::BodyNotAllowed,
        );
        assert_error(
            b"GET /healthz HTTP/1.1\r\nHost: simulator\r\n\r\nGET /readyz HTTP/1.1\r\nHost: simulator\r\n\r\n",
            ParseError::TrailingData,
        );
    }

    #[test]
    fn enforces_bounds_and_json_policy() {
        assert_error(&vec![b'x'; MAX_REQUEST_BYTES + 1], ParseError::RequestTooLarge);
        assert_eq!(parse(b"GET /healthz HTTP/1.1\r\n"), ParseOutcome::Incomplete);
        let mut exact = b"GET /healthz HTTP/1.1\r\nHost: simulator\r\nX-Pad: ".to_vec();
        exact.resize(MAX_REQUEST_BYTES - 4, b'a');
        exact.extend_from_slice(b"\r\n\r\n");
        assert_eq!(complete(&exact), ManagementRequest::Healthz);
        let missing_type = b"POST /v1/scenarios/confirm HTTP/1.1\r\nHost: simulator\r\nContent-Length: 11\r\n\r\n{\"token\":1}";
        assert!(matches!(complete(missing_type), ManagementRequest::Confirm(_)));
        assert_error(
            &json_post("/v1/scenarios/confirm", r#"{"token":1}{"token":2}"#),
            ParseError::InvalidJsonBody,
        );
        assert_error(
            &json_post("/v1/scenarios/prepare", r#"{"target":{"stream_id":0},"scenario_name":"normal"}"#),
            ParseError::InvalidTarget,
        );
    }

    #[test]
    fn distinguishes_unknown_paths_from_method_mismatches() {
        assert_error(
            b"PUT /v1/unknown HTTP/1.1\r\nHost: simulator\r\nContent-Length: 0\r\n\r\n",
            ParseError::UnknownPath,
        );
        assert_error(
            b"POST /healthz HTTP/1.1\r\nHost: simulator\r\nContent-Length: 0\r\n\r\n",
            ParseError::MethodNotAllowed(AllowedMethod::Get),
        );
        let known = parse_error_response(ParseError::MethodNotAllowed(AllowedMethod::Get))
            .expect("known route error response");
        let (headers, _) = response_parts(&known);
        assert!(headers.contains("\r\nAllow: GET\r\n"));
        let unknown = parse_error_response(ParseError::UnknownPath).expect("unknown response");
        let (headers, _) = response_parts(&unknown);
        assert!(headers.starts_with("HTTP/1.1 404 Not Found"));
        assert!(!headers.contains("\r\nAllow:"));
    }

    #[test]
    fn rejects_unknown_or_malformed_console_state_queries() {
        for request in [
            b"GET /v1/state?format=console-v2 HTTP/1.1\r\nHost: simulator\r\n\r\n".as_slice(),
            b"GET /v1/state?format=console-v1&unknown=value HTTP/1.1\r\nHost: simulator\r\n\r\n",
            b"GET /v1/state?format=console-v1&format=console-v1 HTTP/1.1\r\nHost: simulator\r\n\r\n",
            b"GET /v1/state?format=console-v1&cursor=not-a-cursor HTTP/1.1\r\nHost: simulator\r\n\r\n",
            b"GET /v1/catalog?format=console-v1 HTTP/1.1\r\nHost: simulator\r\n\r\n",
        ] {
            assert_error(request, ParseError::InvalidQuery);
        }
    }

    #[test]
    fn encodes_bounded_and_valid_responses() {
        let metrics = prometheus_text(StatusCode::Ok, b"simulator_up 1\n")
            .expect("metrics response");
        let (headers, body) = response_parts(&metrics);
        assert!(headers.contains("Content-Length: 15"));
        assert_eq!(body, b"simulator_up 1\n");
        assert_eq!(
            empty_response(StatusCode::NoContent),
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
        );
        assert!(matches!(
            json_success(StatusCode::NoContent, &Success { message: "ok" }),
            Err(ResponseEncodeError::NoContentBody { .. })
        ));
        assert!(matches!(
            json_success(StatusCode::Ok, &"x".repeat(MAX_RESPONSE_BODY_BYTES + 1)),
            Err(ResponseEncodeError::BodyTooLarge { .. })
        ));
    }
}
