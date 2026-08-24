//! Standalone bounded C37.118 protocol probe for simulator verification.

mod c37_118_probe_v2;

use std::{
    env, fmt,
    io::{self, Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    str,
    time::{Duration, Instant},
};

use c37_118_simulator::{
    config::WireVersion,
    wire_v3::{
        encode_command, Command, Timestamp, FRAME_TYPE_CAPABILITY, FRAME_TYPE_ERROR_RESPONSE,
        FRAME_TYPE_PERIODIC_DATA, FRAME_TYPE_STREAM_CONFIGURATION, PROTOCOL_VERSION,
    },
};
use mio::{net::TcpStream, Events, Interest, Poll, Token};

const MAX_ENDPOINTS: usize = 100;
const MAX_FRAME_BYTES: usize = 4 * 1024;
const EVENTS_CAPACITY: usize = 256;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_HEADER_BYTES: usize = 14;
const FRAME_CHECKSUM_BYTES: usize = 2;

fn main() {
    match run() {
        Ok(summary) => println!(
            "probe_result endpoints={} data_frames={} elapsed_ms={}",
            summary.endpoints,
            summary.data_frames,
            summary.elapsed.as_millis()
        ),
        Err(error) => {
            eprintln!("c37-118-probe: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<ProbeSummary, ProbeError> {
    let (wire_version, raw_arguments) = parse_wire_version(env::args().skip(1))?;
    let arguments = ProbeArguments::parse(raw_arguments.into_iter())?;
    match wire_version {
        WireVersion::V2 => c37_118_probe_v2::run(arguments),
        WireVersion::V3 => {
            let mut probe = Probe::connect(arguments)?;
            probe.run()
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProbeError(String);

impl ProbeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

impl From<io::Error> for ProbeError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug)]
pub(crate) struct ProbeArguments {
    pub(crate) host: String,
    pub(crate) first_port: u16,
    pub(crate) first_stream_id: u16,
    pub(crate) count: usize,
    pub(crate) duration: Duration,
    pub(crate) data_rate_hz: u16,
}

impl ProbeArguments {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, ProbeError> {
        let mut host = None;
        let mut first_port = None;
        let mut first_stream_id = None;
        let mut count = None;
        let mut duration_seconds = None;
        let mut data_rate_hz = None;
        let mut arguments = arguments;

        while let Some(flag) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| ProbeError::new(format!("{flag} requires a value")))?;
            match flag.as_str() {
                "--host" => host = Some(value),
                "--first-port" => first_port = Some(parse_u16(&flag, &value)?),
                "--first-stream-id" => first_stream_id = Some(parse_u16(&flag, &value)?),
                "--count" => count = Some(parse_usize(&flag, &value)?),
                "--duration-seconds" => duration_seconds = Some(parse_u64(&flag, &value)?),
                "--data-rate-hz" => data_rate_hz = Some(parse_u16(&flag, &value)?),
                _ => return Err(ProbeError::new(format!("unknown argument {flag}"))),
            }
        }

        let host = host.ok_or_else(usage)?;
        let first_port = first_port.ok_or_else(usage)?;
        let first_stream_id = first_stream_id.ok_or_else(usage)?;
        let count = count.ok_or_else(usage)?;
        let duration_seconds = duration_seconds.ok_or_else(usage)?;
        let data_rate_hz = data_rate_hz.ok_or_else(usage)?;
        if !(1..=MAX_ENDPOINTS).contains(&count) {
            return Err(ProbeError::new(format!(
                "--count must be in 1..={MAX_ENDPOINTS}"
            )));
        }
        if first_port == 0 || first_port.checked_add((count - 1) as u16).is_none() {
            return Err(ProbeError::new("--first-port range is invalid"));
        }
        if first_stream_id == 0
            || first_stream_id
                .checked_add((count - 1) as u16)
                .is_none_or(|last_stream_id| last_stream_id == u16::MAX)
        {
            return Err(ProbeError::new("--first-stream-id range is invalid"));
        }
        if duration_seconds == 0 || data_rate_hz == 0 {
            return Err(ProbeError::new("duration and data rate must be positive"));
        }

        Ok(Self {
            host,
            first_port,
            first_stream_id,
            count,
            duration: Duration::from_secs(duration_seconds),
            data_rate_hz,
        })
    }
}

struct Probe {
    poll: Poll,
    events: Events,
    clients: Vec<Client>,
    duration: Duration,
    data_rate_hz: u16,
}

struct Client {
    stream: TcpStream,
    stream_id: u16,
    data_rate_hz: u16,
    input: [u8; MAX_FRAME_BYTES],
    input_length: usize,
    output: [u8; 18],
    output_length: usize,
    output_offset: usize,
    state: ClientState,
    configuration: Option<ConfigurationSummary>,
    last_timestamp: Option<Timestamp>,
    data_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientState {
    Connecting,
    Sending(Command),
    AwaitingCapability,
    AwaitingStreamConfiguration,
    Streaming,
}

pub(crate) struct ProbeSummary {
    pub(crate) endpoints: usize,
    pub(crate) data_frames: u64,
    pub(crate) elapsed: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigurationSummary {
    time_base: u32,
    pmu_id: u16,
    data_rate_hz: u16,
}

#[derive(Debug, Clone, Copy)]
struct DecodedFrame<'a> {
    frame_type: u8,
    stream_id: u16,
    timestamp: Timestamp,
    body: &'a [u8],
}

impl Probe {
    fn connect(arguments: ProbeArguments) -> Result<Self, ProbeError> {
        let poll = Poll::new()?;
        let mut clients = Vec::with_capacity(arguments.count);
        for index in 0..arguments.count {
            let port = arguments.first_port + index as u16;
            let stream_id = arguments.first_stream_id + index as u16;
            let address = resolve(&arguments.host, port)?;
            let mut stream = TcpStream::connect(address)?;
            poll.registry().register(
                &mut stream,
                Token(index),
                Interest::READABLE.add(Interest::WRITABLE),
            )?;
            clients.push(Client::new(stream, stream_id, arguments.data_rate_hz));
        }

        Ok(Self {
            poll,
            events: Events::with_capacity(EVENTS_CAPACITY),
            clients,
            duration: arguments.duration,
            data_rate_hz: arguments.data_rate_hz,
        })
    }

    fn run(&mut self) -> Result<ProbeSummary, ProbeError> {
        let handshake_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut streaming_started = None;

        loop {
            let now = Instant::now();
            if streaming_started.is_none() && now >= handshake_deadline {
                return Err(ProbeError::new("C37.118 handshake timed out"));
            }
            if let Some(started) = streaming_started {
                if now.duration_since(started) >= self.duration {
                    return self.finish(started);
                }
            }

            self.poll
                .poll(&mut self.events, Some(Duration::from_millis(100)))?;
            let registry = self.poll.registry();
            for event in &self.events {
                let index = event.token().0;
                if index >= self.clients.len() {
                    continue;
                }
                let client = &mut self.clients[index];
                if event.is_writable() {
                    client.write_ready(registry, Token(index))?;
                }
                if event.is_readable() {
                    client.read_ready(registry, Token(index))?;
                }
            }
            if streaming_started.is_none() && self.clients.iter().all(Client::is_streaming) {
                streaming_started = Some(Instant::now());
            }
        }
    }

    fn finish(&self, started: Instant) -> Result<ProbeSummary, ProbeError> {
        let minimum_per_endpoint = (u64::from(self.data_rate_hz) * self.duration.as_secs() * 4) / 5;
        if self
            .clients
            .iter()
            .any(|client| client.data_frames < minimum_per_endpoint)
        {
            return Err(ProbeError::new(format!(
                "at least one endpoint emitted fewer than {minimum_per_endpoint} data frames"
            )));
        }
        Ok(ProbeSummary {
            endpoints: self.clients.len(),
            data_frames: self.clients.iter().map(|client| client.data_frames).sum(),
            elapsed: started.elapsed(),
        })
    }
}

impl Client {
    fn new(stream: TcpStream, stream_id: u16, data_rate_hz: u16) -> Self {
        Self {
            stream,
            stream_id,
            data_rate_hz,
            input: [0; MAX_FRAME_BYTES],
            input_length: 0,
            output: [0; 18],
            output_length: 0,
            output_offset: 0,
            state: ClientState::Connecting,
            configuration: None,
            last_timestamp: None,
            data_frames: 0,
        }
    }

    fn is_streaming(&self) -> bool {
        self.state == ClientState::Streaming && self.data_frames > 0
    }

    fn write_ready(&mut self, registry: &mio::Registry, token: Token) -> Result<(), ProbeError> {
        if self.state == ClientState::Connecting {
            if let Some(error) = self.stream.take_error()? {
                return Err(ProbeError::new(format!("connection failed: {error}")));
            }
            self.queue_command(Command::Capability)?;
        }

        if !matches!(self.state, ClientState::Sending(_)) {
            return Ok(());
        }
        match self
            .stream
            .write(&self.output[self.output_offset..self.output_length])
        {
            Ok(0) => Err(ProbeError::new("peer closed during command write")),
            Ok(written) => {
                self.output_offset += written;
                if self.output_offset == self.output_length {
                    let command = match self.state {
                        ClientState::Sending(command) => command,
                        _ => unreachable!("write state was checked"),
                    };
                    self.state = match command {
                        Command::Capability => ClientState::AwaitingCapability,
                        Command::StreamConfiguration => ClientState::AwaitingStreamConfiguration,
                        Command::Start => ClientState::Streaming,
                        Command::Stop => return Err(ProbeError::new("probe does not send stop")),
                    };
                    registry.reregister(&mut self.stream, token, Interest::READABLE)?;
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_ready(&mut self, registry: &mio::Registry, token: Token) -> Result<(), ProbeError> {
        loop {
            if self.input_length == self.input.len() {
                return Err(ProbeError::new("C37.118 probe receive buffer is full"));
            }
            match self.stream.read(&mut self.input[self.input_length..]) {
                Ok(0) => return Err(ProbeError::new("peer closed during frame read")),
                Ok(count) => self.input_length += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }

        while self.input_length >= 4 {
            let frame_size = usize::from(u16::from_be_bytes([self.input[2], self.input[3]]));
            if frame_size > self.input.len() {
                return Err(ProbeError::new(
                    "received C37.118 frame exceeds probe buffer",
                ));
            }
            if self.input_length < frame_size {
                break;
            }
            let mut decoded_bytes = [0_u8; MAX_FRAME_BYTES];
            decoded_bytes[..frame_size].copy_from_slice(&self.input[..frame_size]);
            self.input.copy_within(frame_size..self.input_length, 0);
            self.input_length -= frame_size;
            let frame = decode_v3_frame(&decoded_bytes[..frame_size])?;
            validate_frame_identity(frame, self.stream_id)?;
            self.handle_frame(frame, registry, token)?;
        }
        Ok(())
    }

    fn handle_frame(
        &mut self,
        frame: DecodedFrame<'_>,
        registry: &mio::Registry,
        token: Token,
    ) -> Result<(), ProbeError> {
        match (self.state, frame.frame_type) {
            (ClientState::AwaitingCapability, FRAME_TYPE_CAPABILITY) => {
                let configuration = decode_configuration(frame.body, self.data_rate_hz)?;
                self.configuration = Some(configuration);
                self.queue_command(Command::StreamConfiguration)?
            }
            (ClientState::AwaitingStreamConfiguration, FRAME_TYPE_STREAM_CONFIGURATION) => {
                let configuration = decode_configuration(frame.body, self.data_rate_hz)?;
                if self.configuration != Some(configuration) {
                    return Err(ProbeError::new(
                        "capability and stream-configuration frames disagree",
                    ));
                }
                self.queue_command(Command::Start)?
            }
            (ClientState::Streaming, FRAME_TYPE_PERIODIC_DATA) => {
                let configuration = self.configuration.ok_or_else(|| {
                    ProbeError::new("periodic data arrived before stream configuration")
                })?;
                decode_periodic_data(frame, configuration, self.last_timestamp)?;
                self.last_timestamp = Some(frame.timestamp);
                self.data_frames += 1;
            }
            (_, FRAME_TYPE_ERROR_RESPONSE) => {
                return Err(ProbeError::new(format!(
                    "source returned C37.118 error response {}",
                    decode_error_response(frame.body)?
                )));
            }
            _ => {
                return Err(ProbeError::new(
                    "unexpected C37.118 frame for protocol state",
                ))
            }
        }
        if matches!(self.state, ClientState::Sending(_)) {
            registry.reregister(
                &mut self.stream,
                token,
                Interest::READABLE.add(Interest::WRITABLE),
            )?;
        }
        Ok(())
    }

    fn queue_command(&mut self, command: Command) -> Result<(), ProbeError> {
        let frame = encode_command(self.stream_id, command, Timestamp { soc: 0, fracsec: 0 });
        if frame.len() != self.output.len() {
            return Err(ProbeError::new(
                "C37.118 command frame has an unexpected size",
            ));
        }
        self.output.copy_from_slice(&frame);
        self.output_length = frame.len();
        self.output_offset = 0;
        self.state = ClientState::Sending(command);
        Ok(())
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, ProbeError> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| ProbeError::new(format!("cannot resolve {host}:{port}")))
}

fn parse_u16(name: &str, value: &str) -> Result<u16, ProbeError> {
    value
        .parse()
        .map_err(|_| ProbeError::new(format!("{name} must be a u16")))
}

fn parse_usize(name: &str, value: &str) -> Result<usize, ProbeError> {
    value
        .parse()
        .map_err(|_| ProbeError::new(format!("{name} must be an integer")))
}

fn parse_u64(name: &str, value: &str) -> Result<u64, ProbeError> {
    value
        .parse()
        .map_err(|_| ProbeError::new(format!("{name} must be an integer")))
}

fn parse_wire_version(
    arguments: impl Iterator<Item = String>,
) -> Result<(WireVersion, Vec<String>), ProbeError> {
    let mut wire_version = WireVersion::V3;
    let mut found_wire_version = false;
    let mut retained = Vec::new();
    let mut arguments = arguments;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| ProbeError::new(format!("{flag} requires a value")))?;
        if flag != "--wire-version" {
            retained.push(flag);
            retained.push(value);
            continue;
        }
        if found_wire_version {
            return Err(ProbeError::new("--wire-version may only be provided once"));
        }
        let parsed = value
            .parse::<u8>()
            .map_err(|_| ProbeError::new("--wire-version must be 2 or 3"))?;
        wire_version = WireVersion::try_from(parsed)
            .map_err(|_| ProbeError::new("--wire-version must be 2 or 3"))?;
        found_wire_version = true;
    }

    Ok((wire_version, retained))
}

fn decode_v3_frame(bytes: &[u8]) -> Result<DecodedFrame<'_>, ProbeError> {
    if bytes.len() < FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES {
        return Err(ProbeError::new(
            "C37.118 frame is shorter than the V3 envelope",
        ));
    }
    if bytes[0] != 0xaa {
        return Err(ProbeError::new("C37.118 frame has an invalid sync byte"));
    }
    if bytes[1] & 0x0f != PROTOCOL_VERSION {
        return Err(ProbeError::new(format!(
            "received C37.118 version {} but expected {PROTOCOL_VERSION}",
            bytes[1] & 0x0f
        )));
    }
    let frame_size = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    if frame_size != bytes.len() || frame_size < FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES {
        return Err(ProbeError::new(
            "C37.118 frame size does not match its envelope",
        ));
    }
    let expected_checksum = u16::from_be_bytes([bytes[frame_size - 2], bytes[frame_size - 1]]);
    if probe_crc16_ccitt(&bytes[..frame_size - 2]) != expected_checksum {
        return Err(ProbeError::new("C37.118 frame checksum is invalid"));
    }
    Ok(DecodedFrame {
        frame_type: bytes[1] >> 4,
        stream_id: u16::from_be_bytes([bytes[4], bytes[5]]),
        timestamp: Timestamp {
            soc: u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            fracsec: u32::from_be_bytes([0, bytes[11], bytes[12], bytes[13]]),
        },
        body: &bytes[FRAME_HEADER_BYTES..frame_size - FRAME_CHECKSUM_BYTES],
    })
}

fn validate_frame_identity(
    frame: DecodedFrame<'_>,
    expected_stream_id: u16,
) -> Result<(), ProbeError> {
    if frame.stream_id != expected_stream_id {
        return Err(ProbeError::new(format!(
            "received C37.118 STREAM_ID {} but expected {expected_stream_id}",
            frame.stream_id
        )));
    }
    Ok(())
}

fn decode_configuration(
    body: &[u8],
    expected_rate_hz: u16,
) -> Result<ConfigurationSummary, ProbeError> {
    let mut offset = 0;
    let continuation_index = take_u16(body, &mut offset)?;
    if continuation_index != 0 {
        return Err(ProbeError::new(
            "fragmented C37.118 configuration is not supported by the probe",
        ));
    }
    let time_base = take_u32(body, &mut offset)?;
    if time_base == 0 || time_base >> 24 != 0 || time_base % u32::from(expected_rate_hz) != 0 {
        return Err(ProbeError::new(
            "configuration TIME_BASE is invalid for the requested data rate",
        ));
    }
    take_name(body, &mut offset)?;
    if take_u16(body, &mut offset)? != 1 {
        return Err(ProbeError::new("probe requires one PMU per V3 stream"));
    }
    take_name(body, &mut offset)?;
    let pmu_id = take_u16(body, &mut offset)?;
    if pmu_id == 0 || pmu_id == u16::MAX {
        return Err(ProbeError::new("configuration PMU_ID is invalid"));
    }
    if take_u16(body, &mut offset)? != u16::from(PROTOCOL_VERSION) {
        return Err(ProbeError::new("configuration PMU_VERSION is not V3"));
    }
    take_bytes(body, &mut offset, 16)?;
    if take_u16(body, &mut offset)? != 0x0001 {
        return Err(ProbeError::new("probe requires fixed-point polar FORMAT"));
    }
    let phasor_count = take_u16(body, &mut offset)?;
    let analog_count = take_u16(body, &mut offset)?;
    let frequency_count = take_u16(body, &mut offset)?;
    let rocof_count = take_u16(body, &mut offset)?;
    let digital_count = take_u16(body, &mut offset)?;
    if (
        phasor_count,
        analog_count,
        frequency_count,
        rocof_count,
        digital_count,
    ) != (6, 0, 1, 1, 0)
    {
        return Err(ProbeError::new(
            "configuration signal counts do not match the V3 simulator profile",
        ));
    }
    for _ in 0..usize::from(phasor_count + frequency_count + rocof_count) {
        take_name(body, &mut offset)?;
    }
    take_bytes(body, &mut offset, 16 * usize::from(phasor_count))?;
    take_bytes(
        body,
        &mut offset,
        8 * usize::from(frequency_count + rocof_count),
    )?;
    take_bytes(body, &mut offset, 12)?;
    let pmu_flag = take_u16(body, &mut offset)?;
    if pmu_flag & (1 << 12) != 0 {
        return Err(ProbeError::new(
            "probe does not support periodic data attributes",
        ));
    }
    take_bytes(body, &mut offset, 8)?;
    let pmu_rate = take_i16(body, &mut offset)?;
    take_u16(body, &mut offset)?;
    let stream_rate = take_i16(body, &mut offset)?;
    take_u16(body, &mut offset)?;
    if offset != body.len()
        || pmu_rate != expected_rate_hz as i16
        || stream_rate != expected_rate_hz as i16
    {
        return Err(ProbeError::new(
            "configuration data rates or frame length are invalid",
        ));
    }
    Ok(ConfigurationSummary {
        time_base,
        pmu_id,
        data_rate_hz: expected_rate_hz,
    })
}

fn decode_periodic_data(
    frame: DecodedFrame<'_>,
    configuration: ConfigurationSummary,
    previous: Option<Timestamp>,
) -> Result<(), ProbeError> {
    if frame.body.len() != 32 {
        return Err(ProbeError::new(
            "periodic data frame has an unexpected V3 body length",
        ));
    }
    if frame.timestamp.fracsec >= configuration.time_base
        || frame.timestamp.fracsec
            % (configuration.time_base / u32::from(configuration.data_rate_hz))
            != 0
    {
        return Err(ProbeError::new(
            "periodic data timestamp is not aligned to the configured rate",
        ));
    }
    if let Some(previous) = previous {
        let previous_ticks = u64::from(previous.soc) * u64::from(configuration.time_base)
            + u64::from(previous.fracsec);
        let current_ticks = u64::from(frame.timestamp.soc) * u64::from(configuration.time_base)
            + u64::from(frame.timestamp.fracsec);
        let expected_step =
            u64::from(configuration.time_base / u32::from(configuration.data_rate_hz));
        if current_ticks.saturating_sub(previous_ticks) != expected_step {
            return Err(ProbeError::new(
                "periodic data timestamps skipped a configured reporting interval",
            ));
        }
    }
    Ok(())
}

fn decode_error_response(body: &[u8]) -> Result<u16, ProbeError> {
    if body.len() != 4 {
        return Err(ProbeError::new(
            "C37.118 error response has an invalid length",
        ));
    }
    Ok(u16::from_be_bytes([body[0], body[1]]))
}

fn take_name<'a>(body: &'a [u8], offset: &mut usize) -> Result<&'a str, ProbeError> {
    let length = usize::from(
        *take_bytes(body, offset, 1)?
            .first()
            .expect("one byte requested"),
    );
    let name = take_bytes(body, offset, length)?;
    str::from_utf8(name).map_err(|_| ProbeError::new("configuration name is not UTF-8"))
}

fn take_u16(body: &[u8], offset: &mut usize) -> Result<u16, ProbeError> {
    let bytes = take_bytes(body, offset, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn take_i16(body: &[u8], offset: &mut usize) -> Result<i16, ProbeError> {
    let bytes = take_bytes(body, offset, 2)?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn take_u32(body: &[u8], offset: &mut usize) -> Result<u32, ProbeError> {
    let bytes = take_bytes(body, offset, 4)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn take_bytes<'a>(
    body: &'a [u8],
    offset: &mut usize,
    length: usize,
) -> Result<&'a [u8], ProbeError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| ProbeError::new("configuration field length overflow"))?;
    let bytes = body
        .get(*offset..end)
        .ok_or_else(|| ProbeError::new("configuration frame is truncated"))?;
    *offset = end;
    Ok(bytes)
}

fn probe_crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut checksum = 0xffff_u16;
    for byte in bytes {
        checksum ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            checksum = if checksum & 0x8000 != 0 {
                (checksum << 1) ^ 0x1021
            } else {
                checksum << 1
            };
        }
    }
    checksum
}

fn usage() -> ProbeError {
    ProbeError::new(
        "usage: c37-118-probe [--wire-version <2|3>] --host <host> --first-port <port> --first-stream-id <id> --count <1-100> --duration-seconds <seconds> --data-rate-hz <rate>",
    )
}

#[cfg(test)]
mod tests {
    use c37_118_simulator::wire_v3::{encode_command, Command, Timestamp};

    use super::{decode_v3_frame, validate_frame_identity};

    #[test]
    fn rejects_a_frame_for_another_endpoint() {
        let frame = encode_command(1002, Command::Capability, Timestamp { soc: 0, fracsec: 0 });
        let view = decode_v3_frame(&frame).expect("frame must parse");

        let error = validate_frame_identity(view, 1001).expect_err("must reject STREAM_ID");

        assert!(error.to_string().contains("1002"));
    }
}
