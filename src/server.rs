//! Single-threaded, bounded TCP server for the C37.118 V1 simulator subset.


use std::{
    fmt,
    fmt::Write as _,
    io::{self, Read, Write},
    net::SocketAddr,
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
    identity::RuntimeIdentity,
    management,
    scenario::{
        ActivationToken, ScenarioCatalog, ScenarioController, ScenarioControllerSnapshot,
        ScenarioTarget,
    },
    time_health::{TimeHealthMonitor, TimeHealthState, TimeSynchronizationSource},
    wire_v2, wire_v3,
};

const EVENTS_CAPACITY: usize = 512;
const PDC_SLOTS_PER_ENDPOINT: usize = 2;
const MANAGEMENT_CONNECTION_LIMIT: usize = 8;
const MANAGEMENT_WRITE_CHUNK_BYTES: usize = 8 * 1024;
const MANAGEMENT_TOKEN_BASE: usize = usize::MAX / 2 + 1;
const MANAGEMENT_LISTENER_TOKEN: Token = Token(MANAGEMENT_TOKEN_BASE);
const FIRST_VALID_COMMAND_DEADLINE: Duration = Duration::from_secs(15);
const IDLE_NON_STREAMING_DEADLINE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagementConfig {
    pub bind_address: SocketAddr,
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
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
    next_connection_id: u64,
    scheduler: Scheduler,
    wall_clock: WallClock,
    seed: u64,
    stats: ServerStats,
    scenario_controller: Option<ScenarioController>,
    time_health_monitor: TimeHealthMonitor,
    time_synchronization_source: TimeSynchronizationSource,
    time_health_state: TimeHealthState,
    management: Option<ManagementListener>,
    next_management_connection_id: u64,
    ready: bool,
    runtime_metadata: Option<RuntimeMetadata>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RuntimeMetadata {
    deployment_label: String,
    runtime_identity: RuntimeIdentity,
}

struct Endpoint {
    descriptor: EndpointDescriptor,
    limits: Limits,
    listener: TcpListener,
    frames: EndpointFrames,
    data_frame: DataFrame,
    connections: [Option<Connection>; PDC_SLOTS_PER_ENDPOINT],
}

struct ManagementListener {
    listener: TcpListener,
    connections: [Option<ManagementConnection>; MANAGEMENT_CONNECTION_LIMIT],
    rejected_connections: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagementConnectionId(u64);

struct ManagementConnection {
    connection_id: ManagementConnectionId,
    token: Token,
    stream: TcpStream,
    receive: [u8; management::MAX_REQUEST_BYTES],
    received: usize,
    response: Option<Vec<u8>>,
    response_offset: usize,
}

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(serde::Serialize)]
struct ReadinessResponse {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_metadata: Option<RuntimeMetadata>,
}

#[derive(serde::Serialize)]
struct ManagementEndpointState {
    stream_id: u16,
    active_connections: usize,
    connections: Vec<ManagementPdcState>,
}

#[derive(serde::Serialize)]
struct ManagementPdcState {
    connection_id: u64,
    streaming: bool,
}

#[derive(serde::Serialize)]
struct ManagementStateResponse {
    ready: bool,
    time_health: TimeHealthState,
    stats: ServerStats,
    endpoints: Vec<ManagementEndpointState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_metadata: Option<RuntimeMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scenario_controller: Option<ScenarioControllerSnapshot>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConnectionId(u64);

struct Connection {
    connection_id: ConnectionId,
    token: Token,
    stream: TcpStream,
    receive: ReceiveBuffer,
    pending: PendingFrame,
    error_frame: Option<[u8; wire_v3::ERROR_RESPONSE_FRAME_BYTES]>,
    streaming: bool,
    accepted_at: Instant,
    first_valid_command_at: Option<Instant>,
    last_valid_command_at: Instant,
    close_after_flush: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MalformedCommand {
    idcode: Option<u16>,
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
        Self::bind_inner(
            profile,
            None,
            TimeSynchronizationSource::AlwaysVerified,
            None,
        )
    }

    pub fn bind_with_scenarios(
        profile: CompiledProfile,
        catalog: ScenarioCatalog,
    ) -> Result<Self, ServerError> {
        Self::bind_inner(
            profile,
            Some(ScenarioController::new(catalog)),
            TimeSynchronizationSource::AlwaysVerified,
            None,
        )
    }

    pub fn bind_with_runtime_time_health(
        profile: CompiledProfile,
        catalog: ScenarioCatalog,
        time_synchronization_source: TimeSynchronizationSource,
    ) -> Result<Self, ServerError> {
        Self::bind_inner(
            profile,
            Some(ScenarioController::new(catalog)),
            time_synchronization_source,
            None,
        )
    }

    pub fn bind_with_management(
        profile: CompiledProfile,
        catalog: ScenarioCatalog,
        time_synchronization_source: TimeSynchronizationSource,
        management_config: ManagementConfig,
    ) -> Result<Self, ServerError> {
        Self::bind_inner(
            profile,
            Some(ScenarioController::new(catalog)),
            time_synchronization_source,
            Some(management_config),
        )
    }

    pub fn scenario_controller_mut(&mut self) -> Option<&mut ScenarioController> {
        self.scenario_controller.as_mut()
    }

    pub fn time_health_state(&self) -> TimeHealthState {
        self.time_health_state
    }

    pub fn management_address(&self) -> Option<SocketAddr> {
        self.management
            .as_ref()
            .and_then(|management| management.listener.local_addr().ok())
    }

    pub fn configure_runtime_metadata(
        &mut self,
        deployment_label: String,
        runtime_identity: RuntimeIdentity,
    ) {
        self.runtime_metadata = Some(RuntimeMetadata {
            deployment_label,
            runtime_identity,
        });
    }

    fn bind_inner(
        profile: CompiledProfile,
        scenario_controller: Option<ScenarioController>,
        time_synchronization_source: TimeSynchronizationSource,
        management_config: Option<ManagementConfig>,
    ) -> Result<Self, ServerError> {
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
        let reporting_interval = Duration::from_secs(1)
            .checked_div(u32::from(data_rate_hz))
            .ok_or_else(|| ServerError::new("data_rate_hz must be greater than zero"))?;

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
                connections: std::array::from_fn(|_| None),
            });
        }

        let management = match management_config {
            Some(config) => {
                let mut listener = TcpListener::bind(config.bind_address)?;
                poll.registry().register(
                    &mut listener,
                    MANAGEMENT_LISTENER_TOKEN,
                    Interest::READABLE,
                )?;
                Some(ManagementListener {
                    listener,
                    connections: std::array::from_fn(|_| None),
                    rejected_connections: 0,
                })
            }
            None => None,
        };
        let ready = selected_wire_frames_are_valid(&endpoints);

        Ok(Self {
            poll,
            events: Events::with_capacity(EVENTS_CAPACITY),
            endpoints,
            next_connection_id: 1,
            scheduler: Scheduler::new(data_rate_hz, first_sample_at),
            wall_clock,
            seed: profile.seed,
            stats: ServerStats::default(),
            scenario_controller,
            time_health_monitor: TimeHealthMonitor::new(reporting_interval),
            time_synchronization_source,
            time_health_state: TimeHealthState::Unobserved,
            management,
            next_management_connection_id: 1,
            ready,
            runtime_metadata: None,
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
            self.expire_connections(Instant::now())?;

            if let Some(sample_index) = self.scheduler.take_due(Instant::now(), &mut self.stats) {
                let timestamp = self.wall_clock.timestamp_for_sample(sample_index);
                self.emit_data(sample_index, timestamp)?;
            }
        }
    }

    fn expire_connections(&mut self, now: Instant) -> Result<(), ServerError> {
        let registry = self.poll.registry();
        for endpoint in &mut self.endpoints {
            for slot_index in 0..PDC_SLOTS_PER_ENDPOINT {
                let connection_id = endpoint.connections[slot_index]
                    .as_ref()
                    .filter(|connection| connection.deadline_expired(now))
                    .map(|connection| connection.connection_id);
                if let Some(connection_id) = connection_id {
                    close_connection(endpoint, slot_index, connection_id, registry, &mut self.stats)?;
                }
            }
        }
        Ok(())
    }

    fn dispatch_events(&mut self) -> Result<(), ServerError> {
        let mut pending_events = [(Token(0), false, false); EVENTS_CAPACITY];
        let mut pending_event_count = 0;
        for event in &self.events {
            if pending_event_count == pending_events.len() {
                break;
            }
            pending_events[pending_event_count] =
                (event.token(), event.is_readable(), event.is_writable());
            pending_event_count += 1;
        }

        let endpoint_count = self.endpoints.len();
        for &(token, readable, writable) in &pending_events[..pending_event_count] {
            match event_target(token, endpoint_count) {
                Some(EventTarget::Listener { endpoint_index }) => {
                    if readable {
                        let registry = self.poll.registry();
                        accept_connections(
                            &mut self.endpoints[endpoint_index],
                            endpoint_count,
                            endpoint_index,
                            registry,
                            &mut self.next_connection_id,
                            &mut self.stats,
                        )?;
                    }
                }
                Some(EventTarget::Connection {
                    endpoint_index,
                    slot_index,
                    connection_id,
                }) => {
                    if !self.endpoints[endpoint_index].connections[slot_index]
                        .as_ref()
                        .is_some_and(|connection| connection.connection_id == connection_id)
                    {
                        continue;
                    }
                    if readable
                        && !read_connection(
                            &mut self.endpoints[endpoint_index],
                            slot_index,
                            self.poll.registry(),
                            &mut self.stats,
                        )?
                    {
                        close_connection(
                            &mut self.endpoints[endpoint_index],
                            slot_index,
                            connection_id,
                            self.poll.registry(),
                            &mut self.stats,
                        )?;
                        continue;
                    }
                    if writable
                        && !flush_connection(
                            &mut self.endpoints[endpoint_index],
                            slot_index,
                            self.poll.registry(),
                            &mut self.stats,
                        )?
                    {
                        close_connection(
                            &mut self.endpoints[endpoint_index],
                            slot_index,
                            connection_id,
                            self.poll.registry(),
                            &mut self.stats,
                        )?;
                    }
                }
                Some(EventTarget::ManagementListener) if readable => {
                    if let Some(management) = self.management.as_mut() {
                        accept_management_connections(
                            management,
                            self.poll.registry(),
                            &mut self.next_management_connection_id,
                        )?;
                    }
                }
                Some(EventTarget::ManagementConnection {
                    slot_index,
                    connection_id,
                }) => {
                    self.dispatch_management_connection(
                        slot_index,
                        connection_id,
                        readable,
                        writable,
                    )?;
                }
                None => continue,
                _ => continue,
            }
        }
        Ok(())
    }

    fn dispatch_management_connection(
        &mut self,
        slot_index: usize,
        connection_id: ManagementConnectionId,
        readable: bool,
        writable: bool,
    ) -> Result<(), ServerError> {
        let is_current = self
            .management
            .as_ref()
            .and_then(|management| management.connections.get(slot_index))
            .and_then(Option::as_ref)
            .is_some_and(|connection| connection.connection_id == connection_id);
        if !is_current {
            return Ok(());
        }

        if readable {
            let read_result = {
                let management = self
                    .management
                    .as_mut()
                    .expect("management connection requires a management listener");
                let connection = management.connections[slot_index]
                    .as_mut()
                    .expect("current management connection must occupy its slot");
                if connection.response.is_some() {
                    None
                } else {
                    Some(read_management_connection(connection))
                }
            };

            match read_result {
                Some(Ok(ManagementReadResult::Request(request))) => {
                    let response = self.management_request_response(request);
                    self.queue_management_response(slot_index, connection_id, response)?;
                }
                Some(Ok(ManagementReadResult::Response(response))) => {
                    self.queue_management_response(slot_index, connection_id, response)?;
                }
                Some(Ok(ManagementReadResult::Closed)) | Some(Err(_)) => {
                    self.close_management_connection(slot_index, connection_id)?;
                    return Ok(());
                }
                Some(Ok(ManagementReadResult::Incomplete)) | None => {}
            }
        }

        if writable {
            let keep_open = {
                let management = self
                    .management
                    .as_mut()
                    .expect("management connection requires a management listener");
                let connection = management.connections[slot_index]
                    .as_mut()
                    .expect("current management connection must occupy its slot");
                flush_management_connection(connection, self.poll.registry())
            };
            if !matches!(keep_open, Ok(true)) {
                self.close_management_connection(slot_index, connection_id)?;
            }
        }
        Ok(())
    }

    fn queue_management_response(
        &mut self,
        slot_index: usize,
        connection_id: ManagementConnectionId,
        response: Vec<u8>,
    ) -> Result<(), ServerError> {
        let management = self
            .management
            .as_mut()
            .expect("management response requires a management listener");
        let Some(connection) = management.connections[slot_index].as_mut() else {
            return Ok(());
        };
        if connection.connection_id != connection_id {
            return Ok(());
        }
        queue_management_response(connection, response, self.poll.registry())?;
        Ok(())
    }

    fn close_management_connection(
        &mut self,
        slot_index: usize,
        connection_id: ManagementConnectionId,
    ) -> Result<(), ServerError> {
        if let Some(management) = self.management.as_mut() {
            close_management_connection(
                management,
                slot_index,
                connection_id,
                self.poll.registry(),
            )?;
        }
        Ok(())
    }

    fn management_request_response(&mut self, request: management::ManagementRequest) -> Vec<u8> {
        match request {
            management::ManagementRequest::Healthz => management_response_or_internal(
                management::json_success(
                    management::StatusCode::Ok,
                    &HealthResponse { status: "ok" },
                ),
            ),
            management::ManagementRequest::Readyz => {
                let status = if self.ready {
                    management::StatusCode::Ok
                } else {
                    management::StatusCode::ServiceUnavailable
                };
                management_response_or_internal(management::json_success(
                    status,
                    &ReadinessResponse {
                        ready: self.ready,
                        runtime_metadata: self.runtime_metadata.clone(),
                    },
                ))
            }
            management::ManagementRequest::Metrics => self.management_metrics_response(),
            management::ManagementRequest::State => self.management_state_response(),
            management::ManagementRequest::Prepare(request) => {
                let target = scenario_target_from_management(request.target);
                let actor_label = request.actor_label.clone();
                let Some(controller) = self.scenario_controller.as_mut() else {
                    return scenario_controller_unavailable_response();
                };
                match controller.prepare_activation(
                    target,
                    request.scenario_name,
                    request.actor_label,
                    Instant::now(),
                ) {
                    Ok(snapshot) => {
                        self.log_scenario_action("prepare", actor_label.as_deref(), &snapshot);
                        management_response_or_internal(management::json_success(
                            management::StatusCode::Accepted,
                            &snapshot,
                        ))
                    }
                    Err(_) => scenario_request_rejected_response(),
                }
            }
            management::ManagementRequest::Confirm(request) => {
                let Some(token) = ActivationToken::from_value(request.token) else {
                    return management_error_response(
                        management::StatusCode::BadRequest,
                        "invalid_token",
                        "token must be greater than zero",
                    );
                };
                let Some(controller) = self.scenario_controller.as_mut() else {
                    return scenario_controller_unavailable_response();
                };
                match controller.confirm(token, Instant::now()) {
                    Ok(snapshot) => {
                        self.log_scenario_action("confirm", request.actor_label.as_deref(), &snapshot);
                        management_response_or_internal(management::json_success(
                            management::StatusCode::Accepted,
                            &snapshot,
                        ))
                    }
                    Err(_) => scenario_request_rejected_response(),
                }
            }
            management::ManagementRequest::Clear(request) => {
                let target = scenario_target_from_management(request.target);
                let actor_label = request.actor_label.clone();
                let Some(controller) = self.scenario_controller.as_mut() else {
                    return scenario_controller_unavailable_response();
                };
                match controller.prepare_clear(target, request.actor_label, Instant::now()) {
                    Ok(snapshot) => {
                        self.log_scenario_action("clear", actor_label.as_deref(), &snapshot);
                        management_response_or_internal(management::json_success(
                            management::StatusCode::Accepted,
                            &snapshot,
                        ))
                    }
                    Err(_) => scenario_request_rejected_response(),
                }
            }
        }
    }

    fn management_metrics_response(&self) -> Vec<u8> {
        let active_connections: usize = self
            .endpoints
            .iter()
            .map(|endpoint| {
                endpoint
                    .connections
                    .iter()
                    .filter(|connection| connection.is_some())
                    .count()
            })
            .sum();
        let mut metrics = String::new();
        writeln!(
            metrics,
            "c37_118_simulator_ready {}",
            u8::from(self.ready)
        )
        .expect("writing to a String cannot fail");
        write_time_health_metrics(&mut metrics, self.time_health_state);
        writeln!(
            metrics,
            "c37_118_simulator_pdc_active {}",
            active_connections
        )
        .expect("writing to a String cannot fail");
        writeln!(
            metrics,
            "c37_118_simulator_pdc_rejected_total {}",
            self.stats.rejected_clients
        )
        .expect("writing to a String cannot fail");
        writeln!(
            metrics,
            "c37_118_simulator_pdc_slow_total {}",
            self.stats.slow_clients
        )
        .expect("writing to a String cannot fail");
        writeln!(
            metrics,
            "c37_118_simulator_pdc_disconnected_total {}",
            self.stats.closed_clients
        )
        .expect("writing to a String cannot fail");
        writeln!(
            metrics,
            "c37_118_simulator_sent_data_frames_total {}",
            self.stats.sent_data_frames
        )
        .expect("writing to a String cannot fail");
        writeln!(
            metrics,
            "c37_118_simulator_skipped_ticks_total {}",
            self.stats.skipped_ticks
        )
        .expect("writing to a String cannot fail");
        if let Some(management) = self.management.as_ref() {
            writeln!(
                metrics,
                "c37_118_simulator_management_rejected_connections_total {}",
                management.rejected_connections
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(metadata) = self.runtime_metadata.as_ref() {
            writeln!(
                metrics,
                "c37_118_simulator_runtime_info{{deployment_label=\"{}\",image_ref=\"{}\",profile_sha256=\"{}\",scenario_catalog_sha256=\"{}\"}} 1",
                prometheus_label(&metadata.deployment_label),
                prometheus_label(&metadata.runtime_identity.image_ref),
                metadata.runtime_identity.profile_sha256,
                metadata.runtime_identity.scenario_catalog_sha256,
            )
            .expect("writing to a String cannot fail");
        }
        for endpoint in &self.endpoints {
            let active_connections = endpoint
                .connections
                .iter()
                .filter(|connection| connection.is_some())
                .count();
            writeln!(
                metrics,
                "c37_118_simulator_pdc_connections{{stream_id=\"{}\"}} {}",
                endpoint.descriptor.stream_id, active_connections
            )
            .expect("writing to a String cannot fail");
        }
        management_response_or_internal(management::prometheus_text(
            management::StatusCode::Ok,
            metrics,
        ))
    }

    fn management_state_response(&mut self) -> Vec<u8> {
        let endpoints = self
            .endpoints
            .iter()
            .map(|endpoint| ManagementEndpointState {
                stream_id: endpoint.descriptor.stream_id,
                active_connections: endpoint
                    .connections
                    .iter()
                    .filter(|connection| connection.is_some())
                    .count(),
                connections: endpoint
                    .connections
                    .iter()
                    .filter_map(|connection| connection.as_ref())
                    .map(|connection| ManagementPdcState {
                        connection_id: connection.connection_id.0,
                        streaming: connection.streaming,
                    })
                    .collect(),
            })
            .collect();
        let scenario_controller = self
            .scenario_controller
            .as_mut()
            .map(|controller| controller.snapshot(Instant::now()));
        management_response_or_internal(management::json_success(
            management::StatusCode::Ok,
            &ManagementStateResponse {
                ready: self.ready,
                time_health: self.time_health_state,
                stats: self.stats,
                endpoints,
                runtime_metadata: self.runtime_metadata.clone(),
                scenario_controller,
            },
        ))
    }

    fn log_scenario_action<T: serde::Serialize>(
        &self,
        action: &str,
        actor_label: Option<&str>,
        result: &T,
    ) {
        let Some(metadata) = self.runtime_metadata.as_ref() else {
            return;
        };
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let record = serde_json::json!({
            "event": "scenario_control",
            "timestamp_unix_ms": timestamp_unix_ms,
            "deployment_label": metadata.deployment_label,
            "actor_label": actor_label,
            "action": action,
            "result": result,
        });
        if let Ok(record) = serde_json::to_string(&record) {
            eprintln!("{record}");
        }
    }
}

fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

enum ManagementReadResult {
        Incomplete,
        Request(management::ManagementRequest),
        Response(Vec<u8>),
        Closed,
    }

    fn accept_management_connections(
        management: &mut ManagementListener,
        registry: &Registry,
        next_connection_id: &mut u64,
    ) -> Result<(), ServerError> {
        for _ in 0..MANAGEMENT_CONNECTION_LIMIT {
            match management.listener.accept() {
                Ok((mut stream, _)) => {
                    let Some(slot_index) = management
                        .connections
                        .iter()
                        .position(|connection| connection.is_none())
                    else {
                        management.rejected_connections += 1;
                        continue;
                    };
                    let connection_id = ManagementConnectionId(*next_connection_id);
                    let Some(next_id) = (*next_connection_id).checked_add(1) else {
                        management.rejected_connections += 1;
                        continue;
                    };
                    let token = match management_connection_token(slot_index, connection_id) {
                        Ok(token) => token,
                        Err(_) => {
                            management.rejected_connections += 1;
                            continue;
                        }
                    };
                    if registry
                        .register(&mut stream, token, Interest::READABLE)
                        .is_err()
                    {
                        management.rejected_connections += 1;
                        continue;
                    }
                    *next_connection_id = next_id;
                    management.connections[slot_index] = Some(ManagementConnection {
                        connection_id,
                        token,
                        stream,
                        receive: [0; management::MAX_REQUEST_BYTES],
                        received: 0,
                        response: None,
                        response_offset: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn read_management_connection(
        connection: &mut ManagementConnection,
    ) -> Result<ManagementReadResult, io::Error> {
        let mut closed = false;
        loop {
            if connection.received == connection.receive.len() {
                break;
            }
            match connection
                .stream
                .read(&mut connection.receive[connection.received..])
            {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(read) => connection.received += read,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }

        if connection.received == 0 && closed {
            return Ok(ManagementReadResult::Closed);
        }

        match management::parse(&connection.receive[..connection.received]) {
            management::ParseOutcome::Complete(request) => Ok(ManagementReadResult::Request(request)),
            management::ParseOutcome::Incomplete if closed => Ok(ManagementReadResult::Response(
                management_response_or_internal(management::parse_error_response(
                    management::ParseError::MalformedRequest,
                )),
            )),
            management::ParseOutcome::Incomplete => Ok(ManagementReadResult::Incomplete),
            management::ParseOutcome::Error(error) => Ok(ManagementReadResult::Response(
                management_response_or_internal(management::parse_error_response(error)),
            )),
        }
    }

    fn queue_management_response(
        connection: &mut ManagementConnection,
        response: Vec<u8>,
        registry: &Registry,
    ) -> Result<(), io::Error> {
        connection.response = Some(response);
        connection.response_offset = 0;
        registry.reregister(&mut connection.stream, connection.token, Interest::WRITABLE)
    }

    fn flush_management_connection(
        connection: &mut ManagementConnection,
        registry: &Registry,
    ) -> Result<bool, io::Error> {
        let Some(response) = connection.response.as_ref() else {
            return Ok(false);
        };
        let response_length = response.len();
        let result = connection
            .stream
            .write(&response[connection.response_offset..response_length.min(
                connection
                    .response_offset
                    .saturating_add(MANAGEMENT_WRITE_CHUNK_BYTES),
            )]);
        match result {
            Ok(0) => Ok(false),
            Ok(written) => {
                connection.response_offset += written;
                if connection.response_offset == response_length {
                    return Ok(false);
                }
                registry.reregister(&mut connection.stream, connection.token, Interest::WRITABLE)?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                registry.reregister(&mut connection.stream, connection.token, Interest::WRITABLE)?;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    fn close_management_connection(
        management: &mut ManagementListener,
        slot_index: usize,
        connection_id: ManagementConnectionId,
        registry: &Registry,
    ) -> Result<(), ServerError> {
        if !management.connections[slot_index]
            .as_ref()
            .is_some_and(|connection| connection.connection_id == connection_id)
        {
            return Ok(());
        }
        if let Some(mut connection) = management.connections[slot_index].take() {
            registry.deregister(&mut connection.stream)?;
        }
        Ok(())
    }

    fn scenario_target_from_management(target: management::ScenarioTarget) -> ScenarioTarget {
        match target.connection_id {
            Some(connection_id) => ScenarioTarget::Pdc {
                stream_id: target.stream_id,
                connection_id,
            },
            None => ScenarioTarget::Endpoint {
                stream_id: target.stream_id,
            },
        }
    }

    fn scenario_controller_unavailable_response() -> Vec<u8> {
        management_error_response(
            management::StatusCode::ServiceUnavailable,
            "scenario_controller_unavailable",
            "scenario controller is not enabled",
        )
    }

    fn scenario_request_rejected_response() -> Vec<u8> {
        management_error_response(
            management::StatusCode::BadRequest,
            "scenario_request_rejected",
            "scenario request could not be applied",
        )
    }

    fn management_error_response(
        status: management::StatusCode,
        code: &str,
        message: &str,
    ) -> Vec<u8> {
        management_response_or_internal(management::json_error(status, code, message))
    }

    fn management_response_or_internal(
        response: Result<Vec<u8>, management::ResponseEncodeError>,
    ) -> Vec<u8> {
        response.unwrap_or_else(|_| {
            management::json_error(
                management::StatusCode::InternalServerError,
                "response_encoding_failed",
                "management response encoding failed",
            )
            .unwrap_or_else(|_| management::empty_response(management::StatusCode::InternalServerError))
        })
    }

    fn write_time_health_metrics(metrics: &mut String, current: TimeHealthState) {
        for state in [
            TimeHealthState::Unobserved,
            TimeHealthState::Verified,
            TimeHealthState::SynchronizationUnverified,
            TimeHealthState::MaterialClockRegression,
        ] {
            writeln!(
                metrics,
                "c37_118_simulator_time_health_{} {}",
                state.as_str(),
                u8::from(state == current)
            )
            .expect("writing to a String cannot fail");
        }
    }

fn selected_wire_frames_are_valid(endpoints: &[Endpoint]) -> bool {
        endpoints.iter().all(|endpoint| match (&endpoint.descriptor.wire_version, &endpoint.frames) {
            (WireVersion::V2, EndpointFrames::V2 { header, .. }) => wire_v2::FrameView::parse(header)
                .is_ok_and(|frame| {
                    frame.version() == wire_v2::PROTOCOL_VERSION
                        && frame.frame_type() == wire_v2::FRAME_TYPE_HEADER
                        && frame.idcode() == endpoint.descriptor.stream_id
                }),
            (WireVersion::V3, EndpointFrames::V3 { capability, .. }) => {
                wire_v3::FrameView::parse(capability).is_ok_and(|frame| {
                    frame.version() == wire_v3::PROTOCOL_VERSION
                        && frame.frame_type() == wire_v3::FRAME_TYPE_CAPABILITY
                        && frame.stream_id() == endpoint.descriptor.stream_id
                })
            }
            _ => false,
        })
}

impl Server {
    fn emit_data(&mut self, sample_index: u64, timestamp: Timestamp) -> Result<(), ServerError> {
        self.time_health_state = self.time_health_monitor.observe_boundary(
            SystemTime::now(),
            self.time_synchronization_source.is_verified(),
        );
        let force_degraded_time = self.time_health_state.is_degraded();

        if let Some(controller) = self.scenario_controller.as_mut() {
            controller.advance_boundary(sample_index).map_err(|error| {
                ServerError::new(format!(
                    "scenario controller failed at reporting boundary {sample_index}: {error}"
                ))
            })?;
        }

        let registry = self.poll.registry();
        let scenario_controller = self.scenario_controller.as_ref();
        let frame_plans: Vec<_> = self
            .endpoints
            .iter()
            .map(|endpoint| {
                scenario_controller.map(|controller| controller.frame_plan(endpoint.descriptor.stream_id))
            })
            .collect();
        let endpoints = &mut self.endpoints;
        let stats = &mut self.stats;
        let seed = self.seed;
        for (endpoint, frame_plan) in endpoints.iter_mut().zip(&frame_plans) {
            if let Some(frame_plan) = &frame_plan {
                for connection_id in &frame_plan.disconnect_pdc_connection_ids {
                    for slot_index in 0..PDC_SLOTS_PER_ENDPOINT {
                        if endpoint.connections[slot_index]
                            .as_ref()
                            .is_some_and(|connection| connection.connection_id.0 == *connection_id)
                        {
                            close_connection(
                                endpoint,
                                slot_index,
                                ConnectionId(*connection_id),
                                registry,
                                stats,
                            )?;
                            break;
                        }
                    }
                }
            }
        }

        for (endpoint, frame_plan) in endpoints.iter_mut().zip(&frame_plans) {
            for slot_index in 0..PDC_SLOTS_PER_ENDPOINT {
                let Some((connection_id, streaming, pending)) = endpoint.connections[slot_index]
                    .as_ref()
                    .map(|connection| {
                        (
                            connection.connection_id,
                            connection.streaming,
                            connection.pending.is_pending(),
                        )
                    })
                else {
                    continue;
                };
                if !streaming {
                    continue;
                }
                if pending {
                    stats.slow_clients += 1;
                    close_connection(
                        endpoint,
                        slot_index,
                        connection_id,
                        registry,
                        stats,
                    )?;
                    continue;
                }
            }

            if !endpoint.connections.iter().any(|connection| {
                connection
                    .as_ref()
                    .is_some_and(|connection| connection.streaming)
            }) {
                continue;
            }

            if frame_plan.as_ref().is_some_and(|frame_plan| frame_plan.omit_data) {
                continue;
            }

            let (descriptor, data_frame) = (&endpoint.descriptor, &mut endpoint.data_frame);
            match (descriptor.wire_version, data_frame, frame_plan.as_ref()) {
                (WireVersion::V2, DataFrame::V2(frame), Some(frame_plan)) => {
                    wire_v2::encode_periodic_data_with_scenario_into(
                        descriptor,
                        seed,
                        sample_index,
                        wire_v2::Timestamp {
                            soc: timestamp.soc,
                            fracsec: timestamp.fracsec,
                        },
                        force_degraded_time || frame_plan.degraded_time,
                        frame_plan.signal,
                        frame,
                    )
                }
                (WireVersion::V2, DataFrame::V2(frame), None) => {
                    wire_v2::encode_periodic_data_with_scenario_into(
                        descriptor,
                        seed,
                        sample_index,
                        wire_v2::Timestamp {
                            soc: timestamp.soc,
                            fracsec: timestamp.fracsec,
                        },
                        force_degraded_time,
                        None,
                        frame,
                    )
                }
                (WireVersion::V3, DataFrame::V3(frame), Some(frame_plan)) => {
                    wire_v3::encode_periodic_data_with_scenario_into(
                        descriptor,
                        seed,
                        sample_index,
                        timestamp,
                        force_degraded_time || frame_plan.degraded_time,
                        frame_plan.signal,
                        frame,
                    )
                }
                (WireVersion::V3, DataFrame::V3(frame), None) => {
                    wire_v3::encode_periodic_data_with_scenario_into(
                        descriptor,
                        seed,
                        sample_index,
                        timestamp,
                        force_degraded_time,
                        None,
                        frame,
                    )
                }
                _ => {
                    return Err(ServerError::new(
                        "endpoint frame storage does not match its wire version",
                    ))
                }
            }

            for slot_index in 0..PDC_SLOTS_PER_ENDPOINT {
                let Some((connection_id, streaming)) = endpoint.connections[slot_index]
                    .as_ref()
                    .map(|connection| (connection.connection_id, connection.streaming))
                else {
                    continue;
                };
                if !streaming {
                    continue;
                }
                let connection = endpoint.connections[slot_index]
                    .as_mut()
                    .expect("connection was checked before data generation");
                connection.pending = PendingFrame::Data { offset: 0 };
                reregister_connection(connection, registry, true)?;
                if !flush_connection(endpoint, slot_index, registry, stats)? {
                    close_connection(
                        endpoint,
                        slot_index,
                        connection_id,
                        registry,
                        stats,
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl Connection {
    fn new(
        connection_id: ConnectionId,
        token: Token,
        stream: TcpStream,
        command_limit: usize,
        wire_version: WireVersion,
    ) -> Self {
        let accepted_at = Instant::now();
        Self {
            connection_id,
            token,
            stream,
            receive: ReceiveBuffer::new(command_limit),
            pending: PendingFrame::None,
            error_frame: (wire_version == WireVersion::V3)
                .then_some([0; wire_v3::ERROR_RESPONSE_FRAME_BYTES]),
            streaming: false,
            accepted_at,
            first_valid_command_at: None,
            last_valid_command_at: accepted_at,
            close_after_flush: false,
        }
    }

    fn record_valid_command(&mut self, now: Instant) {
        self.first_valid_command_at.get_or_insert(now);
        self.last_valid_command_at = now;
    }

    fn deadline_expired(&self, now: Instant) -> bool {
        if self.first_valid_command_at.is_none() {
            return now.saturating_duration_since(self.accepted_at)
                >= FIRST_VALID_COMMAND_DEADLINE;
        }
        !self.streaming
            && now.saturating_duration_since(self.last_valid_command_at)
                >= IDLE_NON_STREAMING_DEADLINE
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

    fn next_command(
        &mut self,
        wire_version: WireVersion,
    ) -> Result<Option<ReceivedCommand>, MalformedCommand> {
        if self.length < 4 {
            return Ok(None);
        }
        let size = usize::from(u16::from_be_bytes([self.bytes[2], self.bytes[3]]));
        if size > self.limit {
            return Err(self.malformed_command());
        }
        if self.length < size {
            return Ok(None);
        }
        let malformed = self.malformed_command();

        let command = match wire_version {
            WireVersion::V2 => {
                let frame = wire_v2::FrameView::parse(&self.bytes[..size])
                    .map_err(|_| malformed)?;
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
                let frame = wire_v3::FrameView::parse(&self.bytes[..size])
                    .map_err(|_| malformed)?;
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

    fn malformed_command(&self) -> MalformedCommand {
        MalformedCommand {
            idcode: (self.length >= 6)
                .then(|| u16::from_be_bytes([self.bytes[4], self.bytes[5]])),
        }
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
    endpoint_count: usize,
    endpoint_index: usize,
    registry: &Registry,
    next_connection_id: &mut u64,
    stats: &mut ServerStats,
) -> Result<(), ServerError> {
    loop {
        match endpoint.listener.accept() {
            Ok((stream, _)) => {
                let Some(slot_index) = endpoint
                    .connections
                    .iter()
                    .position(|connection| connection.is_none())
                else {
                    drop(stream);
                    stats.rejected_clients += 1;
                    continue;
                };
                let connection_id = ConnectionId(*next_connection_id);
                let Some(next_id) = (*next_connection_id).checked_add(1) else {
                    drop(stream);
                    stats.rejected_clients += 1;
                    continue;
                };
                let token = match connection_token(
                    endpoint_count,
                    endpoint_index,
                    slot_index,
                    connection_id,
                ) {
                    Ok(token) => token,
                    Err(_) => {
                        drop(stream);
                        stats.rejected_clients += 1;
                        continue;
                    }
                };
                match configure_stream(stream, endpoint.limits, token, registry) {
                    Ok(stream) => {
                        *next_connection_id = next_id;
                        endpoint.connections[slot_index] = Some(Connection::new(
                            connection_id,
                            token,
                            stream,
                            endpoint.limits.command_frame_bytes,
                            endpoint.descriptor.wire_version,
                        ));
                        stats.accepted_clients += 1;
                    }
                    Err(_) => stats.rejected_clients += 1,
                }
            }
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
    slot_index: usize,
    registry: &Registry,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let Some(connection) = endpoint.connections[slot_index].as_mut() else {
        return Ok(true);
    };
    if connection.close_after_flush {
        return Ok(true);
    }
    let closed_after_read = match connection.receive.read_from(&mut connection.stream) {
        Ok(ReadResult::Closed) => return Ok(false),
        Ok(ReadResult::ClosedWithBufferedData) => true,
        Ok(ReadResult::Open) => false,
        Err(_) => {
            stats.malformed_commands += 1;
            return Ok(false);
        }
    };

    if !process_buffered_commands(endpoint, slot_index, registry, stats)? {
        return Ok(false);
    }
    if !closed_after_read {
        return Ok(true);
    }
    Ok(endpoint.connections[slot_index]
        .as_ref()
        .is_some_and(|connection| connection.close_after_flush && connection.pending.is_pending()))
}

fn process_buffered_commands(
    endpoint: &mut Endpoint,
    slot_index: usize,
    registry: &Registry,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let wire_version = endpoint.descriptor.wire_version;
    let stream_id = endpoint.descriptor.stream_id;
    loop {
        if endpoint.connections[slot_index]
            .as_ref()
            .is_some_and(|connection| connection.pending.is_pending() || connection.close_after_flush)
        {
            return Ok(true);
        }
        let request = {
            let connection = endpoint.connections[slot_index]
                .as_mut()
                .expect("connection remains while handling buffered commands");
            match connection.receive.next_command(wire_version) {
                Ok(request) => request,
                Err(malformed) => {
                    stats.malformed_commands += 1;
                    if wire_version == WireVersion::V3 {
                        queue_error_response(
                            endpoint,
                            slot_index,
                            registry,
                            malformed.idcode.unwrap_or(stream_id),
                            wire_v3::ErrorResponseCode::RejectedCommand,
                        )?;
                        return Ok(true);
                    }
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
                        slot_index,
                        registry,
                        idcode,
                        wire_v3::ErrorResponseCode::RejectedCommand,
                    )?;
                    return Ok(true);
                }
                return Ok(false);
            }
        };
        if request.idcode != stream_id {
            stats.malformed_commands += 1;
            if wire_version == WireVersion::V3 {
                queue_error_response(
                    endpoint,
                    slot_index,
                    registry,
                    request.idcode,
                    wire_v3::ErrorResponseCode::WrongStreamOrPmu,
                )?;
                return Ok(true);
            }
            return Ok(false);
        }
        endpoint.connections[slot_index]
            .as_mut()
            .expect("connection remains while recording a valid command")
            .record_valid_command(Instant::now());
        if !handle_command(endpoint, slot_index, registry, request.command, stats)? {
            return Ok(false);
        }
    }
}

fn queue_error_response(
    endpoint: &mut Endpoint,
    slot_index: usize,
    registry: &Registry,
    stream_id: u16,
    code: wire_v3::ErrorResponseCode,
) -> Result<(), ServerError> {
    let timestamp = response_timestamp(endpoint.descriptor.time_base)?;
    let connection = endpoint.connections[slot_index]
        .as_mut()
        .expect("connection remains while queuing an error response");
    let error_frame = connection
        .error_frame
        .as_mut()
        .expect("V3 error responses require a V3 connection");
    wire_v3::encode_error_response_into(stream_id, code, timestamp, error_frame);
    connection.pending = PendingFrame::Error { offset: 0 };
    connection.streaming = false;
    connection.close_after_flush = true;
    reregister_connection(connection, registry, true)?;
    Ok(())
}

fn handle_command(
    endpoint: &mut Endpoint,
    slot_index: usize,
    registry: &Registry,
    command: ServerCommand,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let connection = endpoint.connections[slot_index]
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

    reregister_connection(connection, registry, connection.pending.is_pending())?;
    Ok(true)
}

fn flush_connection(
    endpoint: &mut Endpoint,
    slot_index: usize,
    registry: &Registry,
    stats: &mut ServerStats,
) -> Result<bool, ServerError> {
    let pending = endpoint.connections[slot_index]
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
                .connections[slot_index]
                .as_ref()
                .expect("pending error response requires a connection")
                .error_frame
                .expect("pending error response requires a V3 connection");
            &error_frame[offset..]
        }
        PendingFrame::Data { .. } => &endpoint.data_frame.bytes()[offset..],
    };
    let expected_length = offset + bytes.len();
    let result = endpoint.connections[slot_index]
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
                let close_after_flush = {
                    let connection = endpoint.connections[slot_index]
                    .as_mut()
                    .expect("connection remains after frame write");
                    connection.pending = PendingFrame::None;
                    connection.close_after_flush
                };
                if close_after_flush {
                    return Ok(false);
                }
                if !process_buffered_commands(endpoint, slot_index, registry, stats)? {
                    return Ok(false);
                }
                let connection = endpoint.connections[slot_index]
                    .as_mut()
                    .expect("connection remains after buffered command processing");
                if !connection.pending.is_pending() {
                    reregister_connection(connection, registry, false)?;
                }
            } else {
                let connection = endpoint.connections[slot_index]
                    .as_mut()
                    .expect("connection remains after partial frame write");
                connection.pending = pending.with_offset(offset + written);
                reregister_connection(connection, registry, true)?;
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let connection = endpoint.connections[slot_index]
                .as_mut()
                .expect("connection remains after would-block write");
            reregister_connection(connection, registry, true)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn reregister_connection(
    connection: &mut Connection,
    registry: &Registry,
    writable: bool,
) -> Result<(), io::Error> {
    let interest = if writable {
        Interest::READABLE.add(Interest::WRITABLE)
    } else {
        Interest::READABLE
    };
    registry.reregister(&mut connection.stream, connection.token, interest)
}

fn close_connection(
    endpoint: &mut Endpoint,
    slot_index: usize,
    connection_id: ConnectionId,
    registry: &Registry,
    stats: &mut ServerStats,
) -> Result<(), ServerError> {
    if !endpoint.connections[slot_index]
        .as_ref()
        .is_some_and(|connection| connection.connection_id == connection_id)
    {
        return Ok(());
    }
    if let Some(mut connection) = endpoint.connections[slot_index].take() {
        registry.deregister(&mut connection.stream)?;
        stats.closed_clients += 1;
    }
    Ok(())
}

enum EventTarget {
    Listener {
        endpoint_index: usize,
    },
    Connection {
        endpoint_index: usize,
        slot_index: usize,
        connection_id: ConnectionId,
    },
    ManagementListener,
    ManagementConnection {
        slot_index: usize,
        connection_id: ManagementConnectionId,
    },
}

fn listener_token(index: usize) -> Token {
    Token(index)
}

fn connection_token(
    endpoint_count: usize,
    endpoint_index: usize,
    slot_index: usize,
    connection_id: ConnectionId,
) -> Result<Token, ServerError> {
    let connection_id = usize::try_from(connection_id.0)
        .map_err(|_| ServerError::new("connection ID exceeds the platform token range"))?;
    let token = connection_id
        .checked_mul(endpoint_count)
        .and_then(|value| value.checked_add(endpoint_index))
        .and_then(|value| value.checked_mul(PDC_SLOTS_PER_ENDPOINT))
        .and_then(|value| value.checked_add(slot_index))
        .and_then(|value| value.checked_add(endpoint_count))
        .ok_or_else(|| ServerError::new("connection token exceeds the platform token range"))?;
    if token >= MANAGEMENT_TOKEN_BASE {
        return Err(ServerError::new(
            "connection token exceeds the reserved PDC token range",
        ));
    }
    Ok(Token(token))
}

fn management_connection_token(
    slot_index: usize,
    connection_id: ManagementConnectionId,
) -> Result<Token, ServerError> {
    let connection_id = usize::try_from(connection_id.0)
        .map_err(|_| ServerError::new("management connection ID exceeds the platform token range"))?;
    let encoded = connection_id
        .checked_sub(1)
        .and_then(|value| value.checked_mul(MANAGEMENT_CONNECTION_LIMIT))
        .and_then(|value| value.checked_add(slot_index))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| ServerError::new("management connection token exceeds the platform token range"))?;
    let token = MANAGEMENT_TOKEN_BASE
        .checked_add(encoded)
        .ok_or_else(|| ServerError::new("management connection token exceeds the platform token range"))?;
    Ok(Token(token))
}

fn event_target(token: Token, endpoint_count: usize) -> Option<EventTarget> {
    if token == MANAGEMENT_LISTENER_TOKEN {
        return Some(EventTarget::ManagementListener);
    }
    if token.0 > MANAGEMENT_TOKEN_BASE {
        let encoded = token.0.checked_sub(MANAGEMENT_TOKEN_BASE + 1)?;
        let slot_index = encoded % MANAGEMENT_CONNECTION_LIMIT;
        let connection_id = encoded / MANAGEMENT_CONNECTION_LIMIT + 1;
        let connection_id = u64::try_from(connection_id).ok()?;
        return Some(EventTarget::ManagementConnection {
            slot_index,
            connection_id: ManagementConnectionId(connection_id),
        });
    }
    if token.0 < endpoint_count {
        return Some(EventTarget::Listener {
            endpoint_index: token.0,
        });
    }
    if endpoint_count == 0 {
        return None;
    }
    let encoded = token.0.checked_sub(endpoint_count)?;
    let slot_index = encoded % PDC_SLOTS_PER_ENDPOINT;
    let endpoint_and_connection = encoded / PDC_SLOTS_PER_ENDPOINT;
    let endpoint_index = endpoint_and_connection % endpoint_count;
    let connection_id = endpoint_and_connection / endpoint_count;
    let connection_id = u64::try_from(connection_id).ok()?;
    (connection_id != 0).then(|| EventTarget::Connection {
        endpoint_index,
        slot_index,
        connection_id: ConnectionId(connection_id),
    })
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
        fs,
        io::{Read, Write},
        net::{
            Shutdown, SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream,
        },
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    use crate::{
        config::{parse_profile, WireVersion},
        scenario::{parse_catalog, ScenarioCatalog, ScenarioTarget},
        time_health::{TimeHealthState, TimeSynchronizationSource},
        wire_v2,
        wire_v3::{
            encode_command, Command, FrameView, Timestamp, FRAME_TYPE_CAPABILITY,
            FRAME_TYPE_ERROR_RESPONSE, FRAME_TYPE_PERIODIC_DATA, FRAME_TYPE_STREAM_CONFIGURATION,
        },
    };

    use super::{
        aligned_timestamp, BufferedCommandRequest, ManagementConfig, PendingFrame, ReceiveBuffer,
        ReceivedCommand, Server, ServerCommand, WallClock, FIRST_VALID_COMMAND_DEADLINE,
        IDLE_NON_STREAMING_DEADLINE,
    };

    fn profile(port: u16) -> String {
        format!(
            "seed: 7\nlimits:\n  max_logical_pmus: 1\n  max_clients_per_endpoint: 2\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: 1\n  bind_address: 127.0.0.1\n  first_listen_port: {port}\n  first_stream_id: 1001\n  first_pmu_id: 1001\n  pdc_name: WAMA\n  pmu_name_prefix: WAMA-PMU-\n  protocol_version: 3\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    voltage_class: 400000.0\n    voltage_scale: 10.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n    current_scale: 1.0\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n"
        )
    }

    fn v2_profile(port: u16) -> String {
        profile(port).replace("protocol_version: 3", "protocol_version: 2")
    }

    fn two_endpoint_profile(port: u16) -> String {
        profile(port)
            .replace("max_logical_pmus: 1", "max_logical_pmus: 2")
            .replace("count: 1", "count: 2")
    }

    fn good_stat_v2_profile(port: u16) -> String {
        v2_profile(port).replace(
            "protocol_version: 2\n",
            "protocol_version: 2\n  v2_good_stat_pmu_ids:\n    - 1001\n",
        )
    }

    fn baseline_catalog() -> ScenarioCatalog {
        parse_catalog(include_str!("../scenarios/baseline.yaml"))
            .expect("baseline scenario catalog must compile")
    }

    #[test]
    fn management_loopback_serves_health_readiness_state_and_metrics() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let server = Server::bind_with_management(
            profile,
            baseline_catalog(),
            TimeSynchronizationSource::AlwaysVerified,
            ManagementConfig {
                bind_address: "127.0.0.1:0".parse().expect("parse management address"),
            },
        )
        .expect("management-enabled server must bind");
        let management_address = server
            .management_address()
            .expect("management listener must expose its bound address");
        let handle = thread::spawn(move || {
            let mut server = server;
            server
                .run_for(Duration::from_secs(3))
                .expect("server must run")
        });

        let health = management_get(management_address, "/healthz");
        assert_http_status(&health, "HTTP/1.1 200 OK");
        assert_eq!(response_body(&health), br#"{"status":"ok"}"#);

        let readiness = management_get(management_address, "/readyz");
        assert_http_status(&readiness, "HTTP/1.1 200 OK");
        assert_eq!(response_body(&readiness), br#"{"ready":true}"#);

        let state = management_get(management_address, "/v1/state");
        assert_http_status(&state, "HTTP/1.1 200 OK");
        let state = std::str::from_utf8(response_body(&state)).expect("state JSON must be UTF-8");
        assert!(state.contains("\"ready\":true"));
        assert!(state.contains("\"time_health\":"));
        assert!(state.contains("\"stats\":"));
        assert!(state.contains("\"stream_id\":1001"));
        assert!(state.contains("\"active_connections\":0"));
        assert!(state.contains("\"scenario_controller\":"));

        let metrics = management_get(management_address, "/metrics");
        assert_http_status(&metrics, "HTTP/1.1 200 OK");
        let metrics = std::str::from_utf8(response_body(&metrics))
            .expect("metrics response must be UTF-8");
        for metric in [
            "c37_118_simulator_ready 1",
            "c37_118_simulator_time_health_",
            "c37_118_simulator_pdc_active",
            "c37_118_simulator_pdc_rejected_total",
            "c37_118_simulator_pdc_slow_total",
            "c37_118_simulator_pdc_disconnected_total",
            "c37_118_simulator_sent_data_frames_total",
            "c37_118_simulator_skipped_ticks_total",
            "c37_118_simulator_pdc_connections{stream_id=\"1001\"}",
        ] {
            assert!(metrics.contains(metric), "missing metric {metric}");
        }

        let _ = handle.join().expect("server thread must finish");
    }

    #[test]
    fn management_prepare_confirm_activates_at_a_reporting_boundary() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&good_stat_v2_profile(port)).expect("profile must compile");
        let server = Server::bind_with_management(
            profile,
            baseline_catalog(),
            TimeSynchronizationSource::AlwaysVerified,
            ManagementConfig {
                bind_address: "127.0.0.1:0".parse().expect("parse management address"),
            },
        )
        .expect("management-enabled server must bind");
        let management_address = server
            .management_address()
            .expect("management listener must expose its bound address");
        let handle = thread::spawn(move || {
            let mut server = server;
            server
                .run_for(Duration::from_secs(3))
                .expect("server must run")
        });

        let invalid = management_post(
            management_address,
            "/v1/scenarios/prepare",
            r#"{"target":{"stream_id":1001},"scenario_name":"not-in-catalog"}"#,
        );
        assert_http_status(&invalid, "HTTP/1.1 400 Bad Request");
        assert!(std::str::from_utf8(response_body(&invalid))
            .expect("error JSON must be UTF-8")
            .contains("\"code\":\"scenario_request_rejected\""));

        let mut stream = connect(port);
        stream
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start V2 stream");
        let baseline = read_frame(&mut stream);
        assert_eq!(
            u16::from_be_bytes([baseline[14], baseline[15]]),
            0,
            "the configured baseline must be visible before activation"
        );

        let prepared = management_post(
            management_address,
            "/v1/scenarios/prepare",
            r#"{"target":{"stream_id":1001},"scenario_name":"degraded-time","actor_label":"test"}"#,
        );
        assert_http_status(&prepared, "HTTP/1.1 202 Accepted");
        let prepared: serde_json::Value = serde_json::from_slice(response_body(&prepared))
            .expect("prepare response must be JSON");
        let token = prepared["token"].as_u64().expect("prepare response token");

        let confirmed = management_post(
            management_address,
            "/v1/scenarios/confirm",
            &format!(r#"{{"token":{token}}}"#),
        );
        assert_http_status(&confirmed, "HTTP/1.1 202 Accepted");
        assert!(std::str::from_utf8(response_body(&confirmed))
            .expect("confirm response must be UTF-8")
            .contains("\"action\":{\"activate\":{\"scenario_name\":\"degraded-time\"}}"));

        let degraded_stat = wire_v2::STAT_FLAG_SYNC_UNCERTAIN | wire_v2::STAT_PMU_TIME_QUALITY_UNKNOWN;
        let mut saw_degraded_boundary = false;
        for _ in 0..55 {
            let frame = read_frame(&mut stream);
            if u16::from_be_bytes([frame[14], frame[15]]) == degraded_stat {
                saw_degraded_boundary = true;
                break;
            }
        }
        assert!(
            saw_degraded_boundary,
            "confirmed scenario must affect periodic data at a reporting boundary"
        );

        drop(stream);
        let _ = handle.join().expect("server thread must finish");
    }

    #[test]
    fn malformed_management_request_does_not_interrupt_a_pdc_stream() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let server = Server::bind_with_management(
            profile,
            baseline_catalog(),
            TimeSynchronizationSource::AlwaysVerified,
            ManagementConfig {
                bind_address: "127.0.0.1:0".parse().expect("parse management address"),
            },
        )
        .expect("management-enabled server must bind");
        let management_address = server
            .management_address()
            .expect("management listener must expose its bound address");
        let handle = thread::spawn(move || {
            let mut server = server;
            server
                .run_for(Duration::from_secs(3))
                .expect("server must run")
        });

        let mut stream = connect(port);
        stream
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start PDC stream");
        assert_eq!(frame_type(&mut stream), FRAME_TYPE_PERIODIC_DATA);

        let malformed = management_request(
            management_address,
            b"GET /healthz HTTP/1.1\r\nHost: simulator\r\nContent-Length: 1\r\n\r\nx",
        );
        assert_http_status(&malformed, "HTTP/1.1 400 Bad Request");
        let headers = response_headers(&malformed);
        assert!(headers.contains("Connection: close"));
        assert!(std::str::from_utf8(response_body(&malformed))
            .expect("error JSON must be UTF-8")
            .contains("\"code\":\"body_not_allowed\""));

        assert_eq!(frame_type(&mut stream), FRAME_TYPE_PERIODIC_DATA);
        drop(stream);
        let _ = handle.join().expect("server thread must finish");
    }

    fn activate_scenario(server: &mut Server, target: ScenarioTarget, scenario_name: &str) {
        let now = Instant::now();
        let controller = server
            .scenario_controller_mut()
            .expect("scenario-enabled server must own a controller");
        let prepared = controller
            .prepare_activation(target, scenario_name, None, now)
            .expect("scenario activation must prepare");
        controller
            .confirm(prepared.token, now)
            .expect("scenario activation must confirm");
    }

    fn emit_boundary(server: &mut Server, sample_index: u64) {
        let timestamp = server.wall_clock.timestamp_for_sample(sample_index);
        server
            .emit_data(sample_index, timestamp)
            .expect("reporting boundary must run");
    }

    struct TemporarySynchronizationStatusFile {
        path: PathBuf,
    }

    impl TemporarySynchronizationStatusFile {
        fn new(contents: &str) -> Self {
            static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);

            let file_id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "c37-118-simulator-time-health-{}-{file_id}",
                std::process::id()
            ));
            fs::write(&path, contents).expect("write synchronization status file");
            Self { path }
        }

        fn source(&self) -> TimeSynchronizationSource {
            TimeSynchronizationSource::File {
                path: self.path.clone(),
            }
        }

        fn set(&self, contents: &str) {
            fs::write(&self.path, contents).expect("update synchronization status file");
        }
    }

    impl Drop for TemporarySynchronizationStatusFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn reserve_consecutive_ports(count: u16) -> u16 {
        loop {
            let first = StdTcpListener::bind("127.0.0.1:0").expect("reserve first local port");
            let first_port = first.local_addr().expect("discover first local port").port();
            let mut listeners = vec![first];
            let mut available = true;
            for offset in 1..count {
                let Some(port) = first_port.checked_add(offset) else {
                    available = false;
                    break;
                };
                match StdTcpListener::bind(("127.0.0.1", port)) {
                    Ok(listener) => listeners.push(listener),
                    Err(_) => {
                        available = false;
                        break;
                    }
                }
            }
            drop(listeners);
            if available {
                return first_port;
            }
        }
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
    fn serves_v3_capability_configuration_and_data_to_two_pdcs_without_a_gateway() {
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
        let mut first = connect(port);
        let mut second = connect(port);
        let mut requests =
            encode_command(1001, Command::Capability, Timestamp { soc: 1, fracsec: 0 });
        requests.extend_from_slice(&encode_command(
            1001,
            Command::StreamConfiguration,
            Timestamp { soc: 1, fracsec: 0 },
        ));
        first
            .write_all(&requests)
            .expect("request capability and configuration together");
        assert_eq!(frame_type(&mut first), FRAME_TYPE_CAPABILITY);
        assert_eq!(frame_type(&mut first), FRAME_TYPE_STREAM_CONFIGURATION);

        first
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start first stream");
        second
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start second stream");
        assert_eq!(frame_type(&mut first), FRAME_TYPE_PERIODIC_DATA);
        assert_eq!(frame_type(&mut second), FRAME_TYPE_PERIODIC_DATA);

        drop(first);
        assert_eq!(frame_type(&mut second), FRAME_TYPE_PERIODIC_DATA);
        drop(second);
        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.accepted_clients, 2);
        assert!(stats.sent_data_frames >= 3);
    }

    #[test]
    fn serves_v2_header_configurations_and_data_to_two_pdcs_without_a_gateway() {
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
        let mut first = connect(port);
        let mut second = connect(port);
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
        first
            .write_all(&requests)
            .expect("request header and configurations together");
        assert_eq!(v2_frame_type(&mut first), wire_v2::FRAME_TYPE_HEADER);
        assert_eq!(
            v2_frame_type(&mut first),
            wire_v2::FRAME_TYPE_CONFIGURATION_1
        );
        assert_eq!(
            v2_frame_type(&mut first),
            wire_v2::FRAME_TYPE_CONFIGURATION_2
        );

        first
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start first V2 stream");
        second
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start second V2 stream");
        assert_eq!(
            v2_frame_type(&mut first),
            wire_v2::FRAME_TYPE_PERIODIC_DATA
        );
        assert_eq!(
            v2_frame_type(&mut second),
            wire_v2::FRAME_TYPE_PERIODIC_DATA
        );

        drop(first);
        assert_eq!(
            v2_frame_type(&mut second),
            wire_v2::FRAME_TYPE_PERIODIC_DATA
        );
        drop(second);
        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.accepted_clients, 2);
        assert!(stats.sent_data_frames >= 3);
    }

    #[test]
    fn v3_missing_frames_suppresses_only_the_target_endpoint() {
        let port = reserve_consecutive_ports(2);
        let profile = parse_profile(&two_endpoint_profile(port)).expect("profile must compile");
        let mut server = Server::bind_with_scenarios(profile, baseline_catalog())
            .expect("scenario-enabled server must bind");
        let mut target = connect(port);
        let mut peer = connect(port + 1);
        pump_until(&mut server, |server| {
            server.endpoints[0].connections[0].is_some()
                && server.endpoints[1].connections[0].is_some()
        });

        target
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start target stream");
        peer.write_all(&encode_command(
            1002,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer stream");
        pump_until(&mut server, |server| {
            server.endpoints[0].connections[0]
                .as_ref()
                .is_some_and(|connection| connection.streaming)
                && server.endpoints[1].connections[0]
                    .as_ref()
                    .is_some_and(|connection| connection.streaming)
        });

        activate_scenario(
            &mut server,
            ScenarioTarget::Endpoint { stream_id: 1001 },
            "missing-frames",
        );
        emit_boundary(&mut server, 0);

        target
            .set_nonblocking(true)
            .expect("make target read nonblocking");
        let mut byte = [0_u8; 1];
        assert!(matches!(
            target.read(&mut byte),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(server.endpoints[0].connections[0]
            .as_ref()
            .is_some_and(|connection| connection.streaming));
        assert_eq!(frame_type(&mut peer), FRAME_TYPE_PERIODIC_DATA);
        assert!(server.endpoints[1].connections[0]
            .as_ref()
            .is_some_and(|connection| connection.streaming));
    }

    #[test]
    fn v3_disconnect_pdc_closes_only_the_target_peer_before_data_framing() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let mut server = Server::bind_with_scenarios(profile, baseline_catalog())
            .expect("scenario-enabled server must bind");
        let mut target = connect(port);
        let mut peer = connect(port);
        pump_until(&mut server, |server| {
            server.endpoints[0]
                .connections
                .iter()
                .all(|connection| connection.is_some())
        });

        target
            .write_all(&encode_command(
                1001,
                Command::Start,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start target stream");
        peer.write_all(&encode_command(
            1001,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer stream");
        pump_until(&mut server, |server| {
            server.endpoints[0].connections.iter().all(|connection| {
                connection
                    .as_ref()
                    .is_some_and(|connection| connection.streaming)
            })
        });

        let target_slot = slot_for_client(&server, &target);
        let peer_slot = slot_for_client(&server, &peer);
        let target_connection_id = server.endpoints[0].connections[target_slot]
            .as_ref()
            .expect("target PDC must occupy a slot")
            .connection_id
            .0;
        activate_scenario(
            &mut server,
            ScenarioTarget::Pdc {
                stream_id: 1001,
                connection_id: target_connection_id,
            },
            "disconnect-pdc",
        );
        emit_boundary(&mut server, 0);

        assert_connection_closed(&mut target);
        assert!(server.endpoints[0].connections[target_slot].is_none());
        assert_eq!(frame_type(&mut peer), FRAME_TYPE_PERIODIC_DATA);
        assert!(server.endpoints[0].connections[peer_slot]
            .as_ref()
            .is_some_and(|connection| connection.streaming));
    }

    #[test]
    fn v2_degraded_time_changes_a_good_stat_periodic_frame() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&good_stat_v2_profile(port)).expect("profile must compile");
        let mut server = Server::bind_with_scenarios(profile, baseline_catalog())
            .expect("scenario-enabled server must bind");
        let mut stream = connect(port);
        pump_until(&mut server, |server| server.endpoints[0].connections[0].is_some());

        stream
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start V2 stream");
        pump_until(&mut server, |server| {
            server.endpoints[0].connections[0]
                .as_ref()
                .is_some_and(|connection| connection.streaming)
        });

        emit_boundary(&mut server, 0);
        let baseline = read_frame(&mut stream);
        assert_eq!(
            u16::from_be_bytes([baseline[14], baseline[15]]),
            0,
            "configured V2 good-stat baseline must be observable"
        );

        activate_scenario(
            &mut server,
            ScenarioTarget::Endpoint { stream_id: 1001 },
            "degraded-time",
        );
        emit_boundary(&mut server, 1);
        let degraded = read_frame(&mut stream);
        assert_eq!(
            u16::from_be_bytes([degraded[14], degraded[15]]),
            wire_v2::STAT_FLAG_SYNC_UNCERTAIN | wire_v2::STAT_PMU_TIME_QUALITY_UNKNOWN,
        );
        wire_v2::FrameView::parse(&degraded).expect("degraded V2 frame must validate");
    }

    #[test]
    fn runtime_time_health_degrades_and_recovers_a_good_stat_v2_stream() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let status_file = TemporarySynchronizationStatusFile::new("unverified\n");
        let profile = parse_profile(&good_stat_v2_profile(port)).expect("profile must compile");
        let mut server = Server::bind_with_runtime_time_health(
            profile,
            baseline_catalog(),
            status_file.source(),
        )
        .expect("time-health-enabled server must bind");
        assert_eq!(server.time_health_state(), TimeHealthState::Unobserved);
        assert!(server.time_health_state().is_degraded());
        let mut stream = connect(port);
        pump_until(&mut server, |server| server.endpoints[0].connections[0].is_some());

        stream
            .write_all(&wire_v2::encode_command(
                1001,
                wire_v2::Command::Start,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("start V2 stream");
        pump_until(&mut server, |server| {
            server.endpoints[0].connections[0]
                .as_ref()
                .is_some_and(|connection| connection.streaming)
        });

        emit_boundary(&mut server, 0);
        let degraded = read_frame(&mut stream);
        assert_eq!(
            u16::from_be_bytes([degraded[14], degraded[15]]),
            wire_v2::STAT_FLAG_SYNC_UNCERTAIN | wire_v2::STAT_PMU_TIME_QUALITY_UNKNOWN,
        );
        assert_eq!(
            server.time_health_state(),
            TimeHealthState::SynchronizationUnverified
        );
        assert!(server.endpoints[0].connections[0]
            .as_ref()
            .is_some_and(|connection| connection.streaming));

        status_file.set("verified\n");
        emit_boundary(&mut server, 1);
        let recovered = read_frame(&mut stream);
        assert_eq!(
            u16::from_be_bytes([recovered[14], recovered[15]]),
            0,
            "a verified boundary must restore the configured V2 good STAT"
        );
        assert_eq!(server.time_health_state(), TimeHealthState::Verified);
        assert!(server.endpoints[0].connections[0]
            .as_ref()
            .is_some_and(|connection| connection.streaming));
    }

    #[test]
    fn v3_wrong_stream_error_closes_only_the_malformed_pdc() {
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
            let mut malformed = connect(port);
            let mut peer = connect(port);
            malformed
            .write_all(&encode_command(
                7,
                Command::Capability,
                Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("send mismatched command");
        peer.write_all(&encode_command(
            1001,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer stream");
        let response = read_frame(&mut malformed);
        let view = FrameView::parse(&response).expect("error response must parse");
        assert_eq!(view.frame_type(), FRAME_TYPE_ERROR_RESPONSE);
        assert_eq!(view.body(), [0x00, 0x02, 0x00, 0x00]);
        assert_connection_closed(&mut malformed);
        assert_eq!(frame_type(&mut peer), FRAME_TYPE_PERIODIC_DATA);
        drop(peer);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.malformed_commands, 1);
        assert!(stats.closed_clients >= 1);
    }

    #[test]
    fn v3_malformed_frame_error_closes_only_the_malformed_pdc() {
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
        let mut malformed = connect(port);
        let mut peer = connect(port);
        let mut command = encode_command(1001, Command::Capability, Timestamp { soc: 1, fracsec: 0 });
        *command
            .last_mut()
            .expect("V3 command must contain a checksum byte") ^= 0xff;
        malformed
            .write_all(&command)
            .expect("send malformed V3 command");
        peer.write_all(&encode_command(
            1001,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer stream");
        let response = read_frame(&mut malformed);
        let view = FrameView::parse(&response).expect("error response must parse");
        assert_eq!(view.frame_type(), FRAME_TYPE_ERROR_RESPONSE);
        assert_eq!(view.body(), [0x00, 0x01, 0x00, 0x00]);
        assert_connection_closed(&mut malformed);
        assert_eq!(frame_type(&mut peer), FRAME_TYPE_PERIODIC_DATA);
        drop(peer);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.malformed_commands, 1);
        assert!(stats.closed_clients >= 1);
    }

    #[test]
    fn v2_malformed_pdc_does_not_interrupt_its_peer() {
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
        let mut malformed = connect(port);
        let mut peer = connect(port);
        malformed
            .write_all(&wire_v2::encode_command(
                7,
                wire_v2::Command::Header,
                wire_v2::Timestamp { soc: 1, fracsec: 0 },
            ))
            .expect("send mismatched V2 command");
        peer.write_all(&wire_v2::encode_command(
            1001,
            wire_v2::Command::Start,
            wire_v2::Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer V2 stream");
        assert_connection_closed(&mut malformed);
        assert_eq!(
            v2_frame_type(&mut peer),
            wire_v2::FRAME_TYPE_PERIODIC_DATA
        );
        drop(peer);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.malformed_commands, 1);
        assert!(stats.closed_clients >= 1);
    }

    #[test]
    fn rejects_a_third_pdc_connection_per_endpoint() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let handle = thread::spawn(move || {
            let mut server = Server::bind(profile).expect("server must bind");
            server
                .run_for(Duration::from_secs(1))
                .expect("server must run")
        });
        let first = connect(port);
        let second = connect(port);
        let mut rejected = connect(port);
        assert_connection_closed(&mut rejected);
        drop(first);
        drop(second);

        let stats = handle.join().expect("server thread must finish");
        assert_eq!(stats.accepted_clients, 2);
        assert_eq!(stats.rejected_clients, 1);
    }

    #[test]
    fn disconnecting_a_pending_slow_pdc_does_not_interrupt_its_peer() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let mut server = Server::bind(profile).expect("server must bind");
        let mut slow = connect(port);
        let mut peer = connect(port);
        pump_until(&mut server, |server| {
            server.endpoints[0]
                .connections
                .iter()
                .all(|connection| connection.is_some())
        });

        slow.write_all(&encode_command(
            1001,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start slow stream");
        peer.write_all(&encode_command(
            1001,
            Command::Start,
            Timestamp { soc: 1, fracsec: 0 },
        ))
        .expect("start peer stream");
        pump_until(&mut server, |server| {
            server.endpoints[0].connections.iter().all(|connection| {
                connection
                    .as_ref()
                    .is_some_and(|connection| connection.streaming)
            })
        });

        let slow_slot = slot_for_client(&server, &slow);
        let peer_slot = slot_for_client(&server, &peer);
        server.endpoints[0].connections[slow_slot]
            .as_mut()
            .expect("slow PDC must occupy a slot")
            .pending = PendingFrame::Data { offset: 0 };
        let timestamp = server.wall_clock.timestamp_for_sample(0);
        server
            .emit_data(0, timestamp)
            .expect("reporting boundary must run");

        assert!(server.endpoints[0].connections[slow_slot].is_none());
        assert!(server.endpoints[0].connections[peer_slot]
            .as_ref()
            .is_some_and(|connection| connection.streaming));
        assert_eq!(frame_type(&mut peer), FRAME_TYPE_PERIODIC_DATA);
        assert_eq!(server.stats.slow_clients, 1);
        drop(slow);
        drop(peer);
    }

    #[test]
    fn deadline_expiration_is_slot_local_and_exempts_active_streaming() {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve a local port");
        let port = listener.local_addr().expect("discover local port").port();
        drop(listener);

        let profile = parse_profile(&profile(port)).expect("profile must compile");
        let mut server = Server::bind(profile).expect("server must bind");
        let first = connect(port);
        let second = connect(port);
        pump_until(&mut server, |server| {
            server.endpoints[0]
                .connections
                .iter()
                .all(|connection| connection.is_some())
        });

        let now = Instant::now();
        let expired_slot = 0;
        let active_slot = 1;
        let expired = server.endpoints[0].connections[expired_slot]
            .as_mut()
            .expect("first PDC must occupy a slot");
        expired.accepted_at = now - FIRST_VALID_COMMAND_DEADLINE;
        let active = server.endpoints[0].connections[active_slot]
            .as_mut()
            .expect("second PDC must occupy a slot");
        active.first_valid_command_at = Some(now - IDLE_NON_STREAMING_DEADLINE);
        active.last_valid_command_at = now - IDLE_NON_STREAMING_DEADLINE;
        active.streaming = true;

        server
            .expire_connections(now)
            .expect("deadline expiration must run");
        assert!(server.endpoints[0].connections[expired_slot].is_none());
        assert!(server.endpoints[0].connections[active_slot]
            .as_ref()
            .is_some_and(|connection| connection.streaming));

        let active = server.endpoints[0].connections[active_slot]
            .as_mut()
            .expect("active PDC must remain connected");
        active.streaming = false;
        server
            .expire_connections(now)
            .expect("idle expiration must run");
        assert!(server.endpoints[0].connections[active_slot].is_none());
        drop(first);
        drop(second);
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

    fn management_get(address: SocketAddr, path: &str) -> Vec<u8> {
        management_request(
            address,
            format!("GET {path} HTTP/1.1\r\nHost: simulator\r\n\r\n").as_bytes(),
        )
    }

    fn management_post(address: SocketAddr, path: &str, body: &str) -> Vec<u8> {
        management_request(
            address,
            format!(
                "POST {path} HTTP/1.1\r\nHost: simulator\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
    }

    fn management_request(address: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut stream = StdTcpStream::connect(address).expect("connect to management listener");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set management read timeout");
        stream
            .write_all(request)
            .expect("write management request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("read management response until close");
        response
    }

    fn assert_http_status(response: &[u8], expected: &str) {
        assert!(
            response.starts_with(expected.as_bytes()),
            "expected {expected}, received {}",
            String::from_utf8_lossy(response)
        );
    }

    fn response_headers(response: &[u8]) -> &str {
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response must have a header separator");
        std::str::from_utf8(&response[..separator]).expect("HTTP headers must be UTF-8")
    }

    fn response_body(response: &[u8]) -> &[u8] {
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response must have a header separator");
        &response[separator + 4..]
    }

    fn assert_connection_closed(stream: &mut StdTcpStream) {
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => {}
            Ok(count) => panic!("expected connection closure, received {count} bytes"),
            Err(error) => panic!("expected connection closure: {error}"),
        }
    }

    fn pump_until(server: &mut Server, predicate: impl Fn(&Server) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !predicate(server) {
            assert!(Instant::now() < deadline, "server did not reach the expected state");
            server
                .poll
                .poll(&mut server.events, Some(Duration::from_millis(20)))
                .expect("poll server events");
            server.dispatch_events().expect("dispatch server events");
        }
    }

    fn slot_for_client(server: &Server, client: &StdTcpStream) -> usize {
        let client_address = client.local_addr().expect("discover client address");
        server.endpoints[0]
            .connections
            .iter()
            .position(|connection| {
                connection.as_ref().is_some_and(|connection| {
                    connection.stream.peer_addr().ok() == Some(client_address)
                })
            })
            .expect("client must occupy a server slot")
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
