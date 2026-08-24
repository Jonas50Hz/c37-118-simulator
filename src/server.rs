//! Single-threaded, bounded TCP server for the C37.118 V1 simulator subset.

use std::{
    fmt,
    io::{self, Read, Write},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mio::{
    net::{TcpListener, TcpStream},
    Events, Interest, Poll, Registry, Token,
};
use socket2::SockRef;

use crate::wire_v3::Timestamp;
use crate::{
    config::{CompiledProfile, EndpointDescriptor, Limits, WireVersion, MAX_COMMAND_FRAME_BYTES},
    wire_v2, wire_v3,
};

const EVENTS_CAPACITY: usize = 256;

#[derive(Debug)]
pub struct ServerError(String);

impl ServerError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ServerError {}

impl From<io::Error> for ServerError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerStats {
    pub accepted_clients: u64,
    pub closed_clients: u64,
    pub rejected_clients: u64,
    pub malformed_commands: u64,
    pub unsupported_commands: u64,
    pub slow_clients: u64,
    pub sent_data_frames: u64,
    pub skipped_ticks: u64,
}

pub struct Server {
    poll: Poll,
    events: Events,
    endpoints: Vec<Endpoint>,
    scheduler: Scheduler,
    wall_clock: WallClock,
    seed: u64,
    stats: ServerStats,
}

struct Endpoint {
    descriptor: EndpointDescriptor,
    limits: Limits,
    listener: TcpListener,
    frames: EndpointFrames,
    data_frame: DataFrame,
    connection: Option<Connection>,
}

enum EndpointFrames {
    V2 {
        header: Vec<u8>,
        configuration_1: Vec<u8>,
        configuration_2: Vec<u8>,
    },
    V3 {
        capability: Vec<u8>,
        stream_configuration: Vec<u8>,
    },
}

enum DataFrame {
    V2([u8; wire_v2::PERIODIC_DATA_FRAME_BYTES]),
    V3([u8; wire_v3::PERIODIC_DATA_FRAME_BYTES]),
}

impl DataFrame {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::V2(bytes) => bytes,
            Self::V3(bytes) => bytes,
        }
    }
}

struct Connection {
    stream: TcpStream,
    receive: ReceiveBuffer,
    pending: PendingFrame,
    error_frame: Option<[u8; wire_v3::ERROR_RESPONSE_FRAME_BYTES]>,
    streaming: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingFrame {
    None,
    V2Header { offset: usize },
    V2Configuration1 { offset: usize },
    V2Configuration2 { offset: usize },
    V3Capability { offset: usize },
    V3StreamConfiguration { offset: usize },
    Error { offset: usize },
    Data { offset: usize },
}

impl PendingFrame {
    fn is_pending(self) -> bool {
        !matches!(self, Self::None)
    }

    fn with_offset(self, offset: usize) -> Self {
        match self {
            Self::None => Self::None,
            Self::V2Header { .. } => Self::V2Header { offset },
            Self::V2Configuration1 { .. } => Self::V2Configuration1 { offset },
            Self::V2Configuration2 { .. } => Self::V2Configuration2 { offset },
            Self::V3Capability { .. } => Self::V3Capability { offset },
            Self::V3StreamConfiguration { .. } => Self::V3StreamConfiguration { offset },
            Self::Error { .. } => Self::Error { offset },
            Self::Data { .. } => Self::Data { offset },
        }
    }

    fn offset(self) -> usize {
        match self {
            Self::None => 0,
            Self::V2Header { offset }
            | Self::V2Configuration1 { offset }
            | Self::V2Configuration2 { offset }
            | Self::V3Capability { offset }
            | Self::V3StreamConfiguration { offset }
            | Self::Error { offset }
            | Self::Data { offset } => offset,
        }
    }
}

struct ReceiveBuffer {
    bytes: [u8; MAX_COMMAND_FRAME_BYTES],
    length: usize,
    limit: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum ReceivedCommand {
    Accepted(BufferedCommandRequest),
    Rejected {
        idcode: u16,
        wire_version: WireVersion,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferedCommandRequest {
    idcode: u16,
    command: ServerCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerCommand {
    Stop,
    Start,
    V2Header,
    V2Configuration1,
    V2Configuration2,
    V3Capability,
    V3StreamConfiguration,
}

enum ReadResult {
    Open,
    Closed,
    ClosedWithBufferedData,
}

struct Scheduler {
    started: Instant,
    data_rate_hz: u16,
    next_sample_index: u64,
}

struct WallClock {
    first_timestamp: Timestamp,
    ticks_per_frame: u32,
    time_base: u32,
}

impl Server {
    pub fn bind(profile: CompiledProfile) -> Result<Self, ServerError> {
        let data_rate_hz = profile
            .endpoints
            .first()
            .ok_or_else(|| ServerError::new("compiled profile has no endpoints"))?
            .data_rate_hz;
        let time_base = profile
            .endpoints
            .first()
            .ok_or_else(|| ServerError::new("compiled profile has no endpoints"))?
            .time_base;
        if profile.endpoints.iter().any(|endpoint| {
            endpoint.data_rate_hz != data_rate_hz || endpoint.time_base != time_base
        }) {
            return Err(ServerError::new(
                "the simulator requires one shared data_rate_hz and TIME_BASE across all endpoints",
            ));
        }

        let poll = Poll::new()?;
        let limits = profile.limits;
        let (wall_clock, first_sample_at) = WallClock::aligned_start(data_rate_hz, time_base)?;
        let timestamp = wall_clock.timestamp_for_sample(0);
        let mut endpoints = Vec::with_capacity(profile.endpoints.len());
        for (index, descriptor) in profile.endpoints.into_iter().enumerate() {
            let mut listener = TcpListener::bind(descriptor.address)?;
            poll.registry()
                .register(&mut listener, listener_token(index), Interest::READABLE)?;
            let (frames, data_frame) = match descriptor.wire_version {
                WireVersion::V2 => {
                    let timestamp = wire_v2::Timestamp {
                        soc: timestamp.soc,
                        fracsec: timestamp.fracsec,
                    };
                    (
                        EndpointFrames::V2 {
                            header: wire_v2::encode_header(&descriptor, timestamp),
                            configuration_1: wire_v2::encode_configuration_1(
                                &descriptor,
                                timestamp,
                            ),
                            configuration_2: wire_v2::encode_configuration_2(
                                &descriptor,
                                timestamp,
                            ),
                        },
                        DataFrame::V2([0; wire_v2::PERIODIC_DATA_FRAME_BYTES]),
                    )
                }
                WireVersion::V3 => (
                    EndpointFrames::V3 {
                        capability: wire_v3::encode_capability(&descriptor, timestamp),
                        stream_configuration: wire_v3::encode_stream_configuration(
                            &descriptor,
                            timestamp,
                        ),
                    },
                    DataFrame::V3([0; wire_v3::PERIODIC_DATA_FRAME_BYTES]),
                ),
            };
            endpoints.push(Endpoint {
                descriptor,
                limits,
                listener,
                frames,
                data_frame,
                connection: None,
            });
        }

        Ok(Self {
            poll,
            events: Events::with_capacity(EVENTS_CAPACITY),
            endpoints,
            scheduler: Scheduler::new(data_rate_hz, first_sample_at),
            wall_clock,
            seed: profile.seed,
            stats: ServerStats::default(),
        })
    }

    pub fn run(&mut self) -> Result<(), ServerError> {
        self.run_until(None).map(|_| ())
    }

    pub fn run_for(&mut self, duration: Duration) -> Result<ServerStats, ServerError> {
        self.run_until(Some(Instant::now() + duration))
    }

    pub fn stats(&self) -> ServerStats {
        self.stats
    }

    fn run_until(&mut self, deadline: Option<Instant>) -> Result<ServerStats, ServerError> {
        loop {
            let now = Instant::now();
            if deadline.is_some_and(|end| now >= end) {
                return Ok(self.stats);
            }

            let timeout = deadline.map_or_else(
                || self.scheduler.timeout(now),
                |end| {
                    self.scheduler
                        .timeout(now)
                        .min(end.saturating_duration_since(now))
                },
            );
            self.poll.poll(&mut self.events, Some(timeout))?;
            self.dispatch_events()?;

            if let Some(sample_index) = self.scheduler.take_due(Instant::now(), &mut self.stats) {
                let timestamp = self.wall_clock.timestamp_for_sample(sample_index);
                self.emit_data(sample_index, timestamp)?;
            }
        }
    }

    fn dispatch_events(&mut self) -> Result<(), ServerError> {
        let endpoint_count = self.endpoints.len();
        let registry = self.poll.registry();
        let endpoints = &mut self.endpoints;
        let stats = &mut self.stats;

        for event in &self.events {
            let token = event.token().0;
            if token < endpoint_count {
                if event.is_readable() {
                    accept_connections(
                        &mut endpoints[token],
                        registry,
                        connection_token(endpoint_count, token),
                        stats,
                    )?;
                }
                continue;
            }

            let endpoint_index = token - endpoint_count;
            if endpoint_index >= endpoint_count {
                continue;
            }
            if event.is_readable()
                && !read_connection(
                    &mut endpoints[endpoint_index],
                    registry,
                    connection_token(endpoint_count, endpoint_index),
                    stats,
                )?
            {
                close_connection(&mut endpoints[endpoint_index], registry, stats)?;
                continue;
            }
            if event.is_writable()
                && !flush_connection(
                    &mut endpoints[endpoint_index],
                    registry,
                    connection_token(endpoint_count, endpoint_index),
                    stats,
                )?
            {
                close_connection(&mut endpoints[endpoint_index], registry, stats)?;
            }
        }
        Ok(())
    }

    fn emit_data(&mut self, sample_index: u64, timestamp: Timestamp) -> Result<(), ServerError> {
        let endpoint_count = self.endpoints.len();
        let registry = self.poll.registry();
        for (index, endpoint) in self.endpoints.iter_mut().enumerate() {
            let Some(connection) = endpoint.connection.as_ref() else {
                continue;
            };
            if !connection.streaming {
                continue;
            }
            if connection.pending.is_pending() {
                self.stats.slow_clients += 1;
                close_connection(endpoint, registry, &mut self.stats)?;
                continue;
            }

            let (descriptor, data_frame) = (&endpoint.descriptor, &mut endpoint.data_frame);
            match (descriptor.wire_version, data_frame) {
                (WireVersion::V2, DataFrame::V2(frame)) => wire_v2::encode_periodic_data_into(
                    descriptor,
                    self.seed,
                    sample_index,
                    wire_v2::Timestamp {
                        soc: timestamp.soc,
                        fracsec: timestamp.fracsec,
                    },
                    frame,
                ),
                (WireVersion::V3, DataFrame::V3(frame)) => wire_v3::encode_periodic_data_into(
                    descriptor,
                    self.seed,
                    sample_index,
                    timestamp,
                    frame,
                ),
                _ => {
                    return Err(ServerError::new(
                        "endpoint frame storage does not match its wire version",
                    ))
                }
            }
            let connection = endpoint
                .connection
                .as_mut()
                .expect("connection was checked before data generation");
            connection.pending = PendingFrame::Data { offset: 0 };
            reregister_connection(
                connection,
                registry,
                connection_token(endpoint_count, index),
                true,
            )?;
            if !flush_connection(
                endpoint,
                registry,
                connection_token(endpoint_count, index),
                &mut self.stats,
            )? {
                close_connection(endpoint, registry, &mut self.stats)?;
            }
        }
        Ok(())
    }
}

impl Connection {
    fn new(stream: TcpStream, command_limit: usize, wire_version: WireVersion) -> Self {
        Self {
            stream,
            receive: ReceiveBuffer::new(command_limit),
            pending: PendingFrame::None,
            error_frame: (wire_version == WireVersion::V3)
                .then_some([0; wire_v3::ERROR_RESPONSE_FRAME_BYTES]),
            streaming: false,
        }
    }
}

impl ReceiveBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: [0; MAX_COMMAND_FRAME_BYTES],
            length: 0,
            limit,
        }
    }

    fn read_from(&mut self, stream: &mut TcpStream) -> Result<ReadResult, io::Error> {
        loop {
            if self.length == self.limit {
                return Ok(ReadResult::Open);
            }
            match stream.read(&mut self.bytes[self.length..self.limit]) {
                Ok(0) => {
                    return if self.length == 0 {
                        Ok(ReadResult::Closed)
                    } else {
                        Ok(ReadResult::ClosedWithBufferedData)
                    };
                }
                Ok(count) => self.length += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(ReadResult::Open)
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn next_command(&mut self, wire_version: WireVersion) -> Result<Option<ReceivedCommand>, ()> {
        if self.length < 4 {
            return Ok(None);
        }
        let size = usize::from(u16::from_be_bytes([self.bytes[2], self.bytes[3]]));
        if size > self.limit {
            return Err(());
        }
        if self.length < size {
            return Ok(None);
        }

        let command = match wire_version {
            WireVersion::V2 => {
                let frame = wire_v2::FrameView::parse(&self.bytes[..size]).map_err(|_| ())?;
                match wire_v2::parse_command(frame) {
                    Ok(request) => ReceivedCommand::Accepted(BufferedCommandRequest {
                        idcode: request.idcode,
                        command: match request.command {
                            wire_v2::Command::Stop => ServerCommand::Stop,
                            wire_v2::Command::Start => ServerCommand::Start,
                            wire_v2::Command::Header => ServerCommand::V2Header,
                            wire_v2::Command::Configuration1 => ServerCommand::V2Configuration1,
                            wire_v2::Command::Configuration2 => ServerCommand::V2Configuration2,
                        },
                    }),
                    Err(_) => ReceivedCommand::Rejected {
                        idcode: frame.idcode(),
                        wire_version,
                    },
                }
            }
            WireVersion::V3 => {
                let frame = wire_v3::FrameView::parse(&self.bytes[..size]).map_err(|_| ())?;
                match wire_v3::parse_command(frame) {
                    Ok(request) => ReceivedCommand::Accepted(BufferedCommandRequest {
                        idcode: request.stream_id,
                        command: match request.command {
                            wire_v3::Command::Stop => ServerCommand::Stop,
                            wire_v3::Command::Start => ServerCommand::Start,
                            wire_v3::Command::Capability => ServerCommand::V3Capability,
                            wire_v3::Command::StreamConfiguration => {
                                ServerCommand::V3StreamConfiguration
                            }
                        },
                    }),
                    Err(_) => ReceivedCommand::Rejected {
                        idcode: frame.stream_id(),
                        wire_version,
                    },
                }
            }
        };
        self.bytes.copy_within(size..self.length, 0);
        self.length -= size;
        Ok(Some(command))
    }
}

impl Scheduler {
    fn new(data_rate_hz: u16, started: Instant) -> Self {
        Self {
            started,
            data_rate_hz,
            next_sample_index: 0,
        }
    }

    fn timeout(&self, now: Instant) -> Duration {
        let due = self.started + sample_offset(self.next_sample_index, self.data_rate_hz);
        due.saturating_duration_since(now)
    }

    fn take_due(&mut self, now: Instant, stats: &mut ServerStats) -> Option<u64> {
        let elapsed = now.saturating_duration_since(self.started);
        let current_index = ((elapsed.as_nanos() * u128::from(self.data_rate_hz)) / 1_000_000_000)
            .min(u128::from(u64::MAX)) as u64;
        if current_index < self.next_sample_index {
            return None;
        }
        stats.skipped_ticks += current_index.saturating_sub(self.next_sample_index);
        self.next_sample_index = current_index.saturating_add(1);
        Some(current_index)
    }
}

impl WallClock {
    fn aligned_start(data_rate_hz: u16, time_base: u32) -> Result<(Self, Instant), ServerError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                ServerError::new(format!("system clock is before Unix epoch: {error}"))
            })?;
        let first_timestamp = aligned_timestamp(now, data_rate_hz, time_base)?;
        let delay = delay_until_timestamp(now, first_timestamp, time_base);
        let clock = Self {
            first_timestamp,
            ticks_per_frame: time_base / u32::from(data_rate_hz),
            time_base,
        };
        Ok((clock, Instant::now() + delay))
    }

    fn timestamp_for_sample(&self, sample_index: u64) -> Timestamp {
        let total_ticks = u64::from(self.first_timestamp.fracsec)
            + sample_index.saturating_mul(u64::from(self.ticks_per_frame));
        Timestamp {
            soc: self
                .first_timestamp
                .soc
                .saturating_add((total_ticks / u64::from(self.time_base)) as u32),
            fracsec: (total_ticks % u64::from(self.time_base)) as u32,
        }
    }
}

fn aligned_timestamp(
    now: Duration,
    data_rate_hz: u16,
    time_base: u32,
) -> Result<Timestamp, ServerError> {
    let ticks_per_frame = u64::from(time_base / u32::from(data_rate_hz));
    let total_ticks = now.as_secs().saturating_mul(u64::from(time_base))
        + ((u128::from(now.subsec_nanos()) * u128::from(time_base)) / 1_000_000_000) as u64;
    let next_ticks = (total_ticks / ticks_per_frame + 1).saturating_mul(ticks_per_frame);
    let seconds = next_ticks / u64::from(time_base);
    let soc = u32::try_from(seconds)
        .map_err(|_| ServerError::new("aligned C37.118 timestamp exceeds SOC range"))?;
    Ok(Timestamp {
        soc,
        fracsec: (next_ticks % u64::from(time_base)) as u32,
    })
}

fn delay_until_timestamp(now: Duration, timestamp: Timestamp, time_base: u32) -> Duration {
    let target_ticks =
        u128::from(timestamp.soc) * u128::from(time_base) + u128::from(timestamp.fracsec);
    let target_nanos = (target_ticks * 1_000_000_000).div_ceil(u128::from(time_base));
    let now_nanos = now.as_nanos();
    Duration::from_nanos(
        target_nanos
            .saturating_sub(now_nanos)
            .min(u128::from(u64::MAX)) as u64,
    )
}

fn accept_connections(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    stats: &mut ServerStats,
) -> Result<(), ServerError> {
    loop {
        match endpoint.listener.accept() {
            Ok((stream, _)) if endpoint.connection.is_some() => {
                drop(stream);
                stats.rejected_clients += 1;
            }
            Ok((stream, _)) => match configure_stream(stream, endpoint.limits, token, registry) {
                Ok(stream) => {
                    endpoint.connection = Some(Connection::new(
                        stream,
                        endpoint.limits.command_frame_bytes,
                        endpoint.descriptor.wire_version,
                    ));
                    stats.accepted_clients += 1;
                }
                Err(_) => stats.rejected_clients += 1,
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn configure_stream(
    stream: TcpStream,
    limits: Limits,
    token: Token,
    registry: &Registry,
) -> Result<TcpStream, io::Error> {
    let std_stream: std::net::TcpStream = stream.into();
    std_stream.set_nodelay(true)?;
    let socket = SockRef::from(&std_stream);
    socket.set_recv_buffer_size(limits.receive_buffer_bytes)?;
    socket.set_send_buffer_size(limits.send_buffer_bytes)?;
    let mut stream = TcpStream::from_std(std_stream);
    registry.register(&mut stream, token, Interest::READABLE)?;

    Ok(stream)
}

fn read_connection(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let Some(connection) = endpoint.connection.as_mut() else {
        return Ok(true);
    };
    let closed_after_read = match connection.receive.read_from(&mut connection.stream) {
        Ok(ReadResult::Closed) => return Ok(false),
        Ok(ReadResult::ClosedWithBufferedData) => true,
        Ok(ReadResult::Open) => false,
        Err(_) => {
            stats.malformed_commands += 1;
            return Ok(false);
        }
    };

    let accepted = process_buffered_commands(endpoint, registry, token, stats)?;
    Ok(accepted && !closed_after_read)
}

fn process_buffered_commands(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    loop {
        if endpoint
            .connection
            .as_ref()
            .is_some_and(|connection| connection.pending.is_pending())
        {
            return Ok(true);
        }
        let request = {
            let connection = endpoint
                .connection
                .as_mut()
                .expect("connection remains while handling buffered commands");
            match connection
                .receive
                .next_command(endpoint.descriptor.wire_version)
            {
                Ok(request) => request,
                Err(_) => {
                    stats.malformed_commands += 1;
                    return Ok(false);
                }
            }
        };
        let Some(request) = request else {
            return Ok(true);
        };
        let request = match request {
            ReceivedCommand::Accepted(request) => request,
            ReceivedCommand::Rejected {
                idcode,
                wire_version,
            } => {
                stats.unsupported_commands += 1;
                if wire_version == WireVersion::V3 {
                    queue_error_response(
                        endpoint,
                        registry,
                        token,
                        idcode,
                        wire_v3::ErrorResponseCode::RejectedCommand,
                    )?;
                    return Ok(true);
                }
                return Ok(false);
            }
        };
        if request.idcode != endpoint.descriptor.stream_id {
            stats.malformed_commands += 1;
            if endpoint.descriptor.wire_version == WireVersion::V3 {
                queue_error_response(
                    endpoint,
                    registry,
                    token,
                    request.idcode,
                    wire_v3::ErrorResponseCode::WrongStreamOrPmu,
                )?;
                return Ok(true);
            }
            return Ok(false);
        }
        if !handle_command(endpoint, registry, token, request.command, stats)? {
            return Ok(false);
        }
    }
}

fn queue_error_response(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    stream_id: u16,
    code: wire_v3::ErrorResponseCode,
) -> Result<(), ServerError> {
    let timestamp = response_timestamp(endpoint.descriptor.time_base)?;
    let connection = endpoint
        .connection
        .as_mut()
        .expect("connection remains while queuing an error response");
    let error_frame = connection
        .error_frame
        .as_mut()
        .expect("V3 error responses require a V3 connection");
    wire_v3::encode_error_response_into(stream_id, code, timestamp, error_frame);
    connection.pending = PendingFrame::Error { offset: 0 };
    reregister_connection(connection, registry, token, true)?;
    Ok(())
}

fn handle_command(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    command: ServerCommand,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let connection = endpoint
        .connection
        .as_mut()
        .expect("connection remains while handling command");
    if connection.pending.is_pending() {
        stats.slow_clients += 1;
        return Ok(false);
    }

    match (endpoint.descriptor.wire_version, command) {
        (_, ServerCommand::Stop) => connection.streaming = false,
        (_, ServerCommand::Start) => connection.streaming = true,
        (WireVersion::V2, ServerCommand::V2Header) => {
            connection.pending = PendingFrame::V2Header { offset: 0 }
        }
        (WireVersion::V2, ServerCommand::V2Configuration1) => {
            connection.pending = PendingFrame::V2Configuration1 { offset: 0 }
        }
        (WireVersion::V2, ServerCommand::V2Configuration2) => {
            connection.pending = PendingFrame::V2Configuration2 { offset: 0 }
        }
        (WireVersion::V3, ServerCommand::V3Capability) => {
            connection.pending = PendingFrame::V3Capability { offset: 0 }
        }
        (WireVersion::V3, ServerCommand::V3StreamConfiguration) => {
            connection.pending = PendingFrame::V3StreamConfiguration { offset: 0 }
        }
        _ => {
            stats.unsupported_commands += 1;
            return Ok(false);
        }
    }

    reregister_connection(connection, registry, token, connection.pending.is_pending())?;
    Ok(true)
}

fn flush_connection(
    endpoint: &mut Endpoint,
    registry: &Registry,
    token: Token,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let pending = endpoint
        .connection
        .as_ref()
        .map(|connection| connection.pending)
        .unwrap_or(PendingFrame::None);
    if !pending.is_pending() {
        return Ok(true);
    }

    let offset = pending.offset();
    let error_frame;
    let bytes = match pending {
        PendingFrame::None => return Ok(true),
        PendingFrame::V2Header { .. } => match &endpoint.frames {
            EndpointFrames::V2 { header, .. } => &header[offset..],
            EndpointFrames::V3 { .. } => unreachable!("V2 pending frame requires V2 endpoint"),
        },
        PendingFrame::V2Configuration1 { .. } => match &endpoint.frames {
            EndpointFrames::V2 {
                configuration_1, ..
            } => &configuration_1[offset..],
            EndpointFrames::V3 { .. } => unreachable!("V2 pending frame requires V2 endpoint"),
        },
        PendingFrame::V2Configuration2 { .. } => match &endpoint.frames {
            EndpointFrames::V2 {
                configuration_2, ..
            } => &configuration_2[offset..],
            EndpointFrames::V3 { .. } => unreachable!("V2 pending frame requires V2 endpoint"),
        },
        PendingFrame::V3Capability { .. } => match &endpoint.frames {
            EndpointFrames::V3 { capability, .. } => &capability[offset..],
            EndpointFrames::V2 { .. } => unreachable!("V3 pending frame requires V3 endpoint"),
        },
        PendingFrame::V3StreamConfiguration { .. } => match &endpoint.frames {
            EndpointFrames::V3 {
                stream_configuration,
                ..
            } => &stream_configuration[offset..],
            EndpointFrames::V2 { .. } => unreachable!("V3 pending frame requires V3 endpoint"),
        },
        PendingFrame::Error { .. } => {
            error_frame = endpoint
                .connection
                .as_ref()
                .expect("pending error response requires a connection")
                .error_frame
                .expect("pending error response requires a V3 connection");
            &error_frame[offset..]
        }
        PendingFrame::Data { .. } => &endpoint.data_frame.bytes()[offset..],
    };
    let expected_length = offset + bytes.len();
    let result = endpoint
        .connection
        .as_mut()
        .expect("pending frame requires a connection")
        .stream
        .write(bytes);

    match result {
        Ok(0) => Ok(false),
        Ok(written) => {
            if offset + written == expected_length {
                if matches!(pending, PendingFrame::Data { .. }) {
                    stats.sent_data_frames += 1;
                }
                endpoint
                    .connection
                    .as_mut()
                    .expect("connection remains after frame write")
                    .pending = PendingFrame::None;
                if !process_buffered_commands(endpoint, registry, token, stats)? {
                    return Ok(false);
                }
                let connection = endpoint
                    .connection
                    .as_mut()
                    .expect("connection remains after buffered command processing");
                if !connection.pending.is_pending() {
                    reregister_connection(connection, registry, token, false)?;
                }
            } else {
                let connection = endpoint
                    .connection
                    .as_mut()
                    .expect("connection remains after partial frame write");
                connection.pending = pending.with_offset(offset + written);
                reregister_connection(connection, registry, token, true)?;
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let connection = endpoint
                .connection
                .as_mut()
                .expect("connection remains after would-block write");
            reregister_connection(connection, registry, token, true)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn reregister_connection(
    connection: &mut Connection,
    registry: &Registry,
    token: Token,
    writable: bool,
) -> Result<(), io::Error> {
    let interest = if writable {
        Interest::READABLE.add(Interest::WRITABLE)
    } else {
        Interest::READABLE
    };
    registry.reregister(&mut connection.stream, token, interest)
}

fn close_connection(
    endpoint: &mut Endpoint,
    registry: &Registry,
    stats: &mut ServerStats,
) -> Result<(), ServerError> {
    if let Some(mut connection) = endpoint.connection.take() {
        registry.deregister(&mut connection.stream)?;
        stats.closed_clients += 1;
    }
    Ok(())
}

fn listener_token(index: usize) -> Token {
    Token(index)
}

fn connection_token(endpoint_count: usize, index: usize) -> Token {
    Token(endpoint_count + index)
}

fn sample_offset(sample_index: u64, data_rate_hz: u16) -> Duration {
    let nanoseconds = (u128::from(sample_index) * 1_000_000_000) / u128::from(data_rate_hz);
    Duration::from_nanos(nanoseconds.min(u128::from(u64::MAX)) as u64)
}

fn response_timestamp(time_base: u32) -> Result<Timestamp, ServerError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ServerError::new(format!("system clock is before Unix epoch: {error}")))?;
    let soc = u32::try_from(now.as_secs())
        .map_err(|_| ServerError::new("error-response timestamp exceeds SOC range"))?;
    let fracsec = ((u128::from(now.subsec_nanos()) * u128::from(time_base)) / 1_000_000_000) as u32;
    Ok(Timestamp { soc, fracsec })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpListener as StdTcpListener, TcpStream as StdTcpStream},
        thread,
        time::Duration,
    };

    use crate::{
        config::{parse_profile, WireVersion},
        wire_v2,
        wire_v3::{
            encode_command, Command, FrameView, Timestamp, FRAME_TYPE_CAPABILITY,
            FRAME_TYPE_ERROR_RESPONSE, FRAME_TYPE_PERIODIC_DATA, FRAME_TYPE_STREAM_CONFIGURATION,
        },
    };

    use super::{
        aligned_timestamp, BufferedCommandRequest, ReceiveBuffer, ReceivedCommand, Server,
        ServerCommand, WallClock,
    };

    fn profile(port: u16) -> String {
        format!(
            "seed: 7\nlimits:\n  max_logical_pmus: 1\n  max_clients_per_endpoint: 1\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: 1\n  bind_address: 127.0.0.1\n  first_listen_port: {port}\n  first_stream_id: 1001\n  first_pmu_id: 1001\n  pdc_name: WAMA\n  pmu_name_prefix: WAMA-PMU-\n  protocol_version: 3\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    voltage_class: 400000.0\n    voltage_scale: 10.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n    current_scale: 1.0\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n"
        )
    }

    fn v2_profile(port: u16) -> String {
        profile(port).replace("protocol_version: 3", "protocol_version: 2")
    }

    #[test]
    fn extracts_concatenated_commands_from_a_fixed_buffer() {
        let timestamp = Timestamp { soc: 1, fracsec: 2 };
        let first = encode_command(7, Command::Capability, timestamp);
        let second = encode_command(7, Command::Start, timestamp);
        let mut buffer = ReceiveBuffer::new(4096);
        buffer.bytes[..first.len()].copy_from_slice(&first);
        buffer.bytes[first.len()..first.len() + second.len()].copy_from_slice(&second);
        buffer.length = first.len() + second.len();

        assert_eq!(
            buffer.next_command(WireVersion::V3).expect("first command"),
            Some(ReceivedCommand::Accepted(BufferedCommandRequest {
                idcode: 7,
                command: ServerCommand::V3Capability,
            }))
        );
        assert_eq!(
            buffer
                .next_command(WireVersion::V3)
                .expect("second command"),
            Some(ReceivedCommand::Accepted(BufferedCommandRequest {
                idcode: 7,
                command: ServerCommand::Start,
            }))
        );
        assert_eq!(
            buffer.next_command(WireVersion::V3).expect("empty buffer"),
            None
        );
    }

    #[test]
    fn waits_for_a_fragmented_command_without_growing_the_buffer() {
        let command = encode_command(7, Command::Capability, Timestamp { soc: 1, fracsec: 2 });
        let mut buffer = ReceiveBuffer::new(4096);
        let split_at = 7;
        buffer.bytes[..split_at].copy_from_slice(&command[..split_at]);
        buffer.length = split_at;

        assert_eq!(
            buffer
                .next_command(WireVersion::V3)
                .expect("partial command"),
            None
        );

        buffer.bytes[split_at..command.len()].copy_from_slice(&command[split_at..]);
        buffer.length = command.len();
        assert_eq!(
            buffer
                .next_command(WireVersion::V3)
                .expect("complete command"),
            Some(ReceivedCommand::Accepted(BufferedCommandRequest {
                idcode: 7,
                command: ServerCommand::V3Capability,
            }))
        );
    }

    #[test]
    fn serves_v3_capability_configuration_data_and_stops_without_a_gateway() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let handle = thread::spawn(move || {
            let mut server = Server::bind(profile).expect("server must bind");
            server
                .run_for(Duration::from_secs(2))
                .expect("server must run")
        });
        let mut stream = connect(port);
        let mut requests =
            encode_command(1001, Command::Capability, Timestamp { soc: 1, fracsec: 0 });
        requests.extend_from_slice(&encode_command(
            1001,
            Command::StreamConfiguration,
            Timestamp { soc: 1, fracsec: 0 },
        ));
        stream
            .write_all(&requests)
            .expect("request capability and configuration together");
        assert_eq!(frame_type(&mut stream), FRAME_TYPE_CAPABILITY);
        assert_eq!(frame_type(&mut stream), FRAME_TYPE_STREAM_CONFIGURATION);

        stream
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start stream");
        assert_eq!(frame_type(&mut stream), FRAME_TYPE_PERIODIC_DATA);

        stream
            .write_all(&encode_command(
                1001,
                Command::Stop,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("stop stream");
        drop(stream);
        let stats = handle.join().expect("server thread must finish");
        assert!(stats.sent_data_frames >= 1);
    }

    #[test]
    fn serves_v2_header_configurations_data_and_stops_without_a_gateway() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&v2_profile(port)).expect("profile must compile");
        let handle = thread::spawn(move || {
            let mut server = Server::bind(profile).expect("server must bind");
            server
                .run_for(Duration::from_secs(2))
                .expect("server must run")
        });
        let mut stream = connect(port);
        let mut requests = wire_v2::encode_command(
            1001,
            wire_v2::Command::Header,
            wire_v2::Timestamp { soc: 1, fracsec: 0 },
        );
        requests.extend_from_slice(&wire_v2::encode_command(
            1001,
            wire_v2::Command::Configuration1,
            wire_v2::Timestamp { soc: 1, fracsec: 0 },
        ));
        requests.extend_from_slice(&wire_v2::encode_command(
            1001,
            wire_v2::Command::Configuration2,
            wire_v2::Timestamp { soc: 1, fracsec: 0 },
        ));
        stream
            .write_all(&requests)
            .expect("request header and configurations together");
        assert_eq!(v2_frame_type(&mut stream), wire_v2::FRAME_TYPE_HEADER);
        assert_eq!(
            v2_frame_type(&mut stream),
            wire_v2::FRAME_TYPE_CONFIGURATION_1
        );
        assert_eq!(
            v2_frame_type(&mut stream),
            wire_v2::FRAME_TYPE_CONFIGURATION_2
        );

        stream
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start V2 stream");
        assert_eq!(
            v2_frame_type(&mut stream),
            wire_v2::FRAME_TYPE_PERIODIC_DATA
        );

        stream
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Stop,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("stop V2 stream");
        drop(stream);
        let stats = handle.join().expect("server thread must finish");
        assert!(stats.sent_data_frames >= 1);
    }

    #[test]
    fn returns_a_wrong_stream_error_response() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let handle = thread::spawn(move || {
            let mut server = Server::bind(profile).expect("server must bind");
            server
                .run_for(Duration::from_millis(250))
                .expect("server must run")
        });
        let mut stream = connect(port);
        stream
            .write_all(&encode_command(
                7,
                Command::Capability,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("send mismatched command");
        let response = read_frame(&mut stream);
        let view = FrameView::parse(&response).expect("error response must parse");
        assert_eq!(view.frame_type(), FRAME_TYPE_ERROR_RESPONSE);
        assert_eq!(view.body(), [0x00, 0x02, 0x00, 0x00]);
        drop(stream);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.malformed_commands, 1);
        assert_eq!(stats.closed_clients, 1);
    }

    #[test]
    fn releases_the_connection_slot_after_partial_command_eof() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let handle = thread::spawn(move || {
            let mut server = Server::bind(profile).expect("server must bind");
            server
                .run_for(Duration::from_millis(250))
                .expect("server must run")
        });
        let mut stream = connect(port);
        let command = encode_command(1001, Command::Capability, Timestamp { soc: 1, fracsec: 0 });
        stream
            .write_all(&command[..8])
            .expect("send partial command");
        stream.shutdown(Shutdown::Write).expect("signal EOF");
        drop(stream);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.closed_clients, 1);
        assert_eq!(stats.malformed_commands, 0);
    }

    #[test]
    fn aligns_the_first_timestamp_to_the_next_50_hz_utc_boundary() {
        let timestamp = aligned_timestamp(Duration::new(1_700_000_000, 13_000_000), 50, 1_000_000)
            .expect("timestamp must align");

        assert_eq!(timestamp.soc, 1_700_000_000);
        assert_eq!(timestamp.fracsec, 20_000);
    }

    #[test]
    fn advances_aligned_timestamps_across_a_second_boundary() {
        let clock = WallClock {
            first_timestamp: Timestamp {
                soc: 10,
                fracsec: 980_000,
            },
            ticks_per_frame: 20_000,
            time_base: 1_000_000,
        };

        assert_eq!(
            clock.timestamp_for_sample(0),
            Timestamp {
                soc: 10,
                fracsec: 980_000
            }
        );
        assert_eq!(
            clock.timestamp_for_sample(1),
            Timestamp {
                soc: 11,
                fracsec: 0
            }
        );
    }

    fn connect(port: u16) -> StdTcpStream {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match StdTcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .expect("set timeout");
                    return stream;
                }
                Err(error) if std::time::Instant::now() < deadline => {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionRefused
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::NotConnected
                        ),
                        "unexpected connection error: {error}"
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("cannot connect to simulator: {error}"),
            }
        }
    }

    fn frame_type(stream: &mut StdTcpStream) -> u8 {
        FrameView::parse(&read_frame(stream))
            .expect("frame must validate")
            .frame_type()
    }

    fn v2_frame_type(stream: &mut StdTcpStream) -> u8 {
        wire_v2::FrameView::parse(&read_frame(stream))
            .expect("frame must validate")
            .frame_type()
    }

    fn read_frame(stream: &mut StdTcpStream) -> Vec<u8> {
        let mut prefix = [0_u8; 4];
        stream.read_exact(&mut prefix).expect("read frame prefix");
        let size = usize::from(u16::from_be_bytes([prefix[2], prefix[3]]));
        let mut frame = Vec::with_capacity(size);
        frame.extend_from_slice(&prefix);
        frame.resize(size, 0);
        stream
            .read_exact(&mut frame[4..])
            .expect("read complete frame");
        frame
    }
}
