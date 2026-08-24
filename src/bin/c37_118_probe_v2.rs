//! Independent C37.118.2-2011 version-2 probe implementation.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    time::{Duration, Instant},
};

use c37_118_simulator::wire_v2::{
    self, Command, FRAME_TYPE_CONFIGURATION_1, FRAME_TYPE_CONFIGURATION_2, FRAME_TYPE_HEADER,
    FRAME_TYPE_PERIODIC_DATA,
};
use mio::{net::TcpStream, Events, Interest, Poll, Token};

use super::{ProbeArguments, ProbeError, ProbeSummary};

const MAX_FRAME_BYTES: usize = 4 * 1024;
const EVENTS_CAPACITY: usize = 256;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_HEADER_BYTES: usize = 14;
const FRAME_CHECKSUM_BYTES: usize = 2;

pub(super) fn run(arguments: ProbeArguments) -> Result<ProbeSummary, ProbeError> {
    let mut probe = Probe::connect(arguments)?;
    probe.run()
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
    idcode: u16,
    data_rate_hz: u16,
    input: [u8; MAX_FRAME_BYTES],
    input_length: usize,
    output: [u8; wire_v2::COMMAND_FRAME_BYTES],
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
    AwaitingHeader,
    AwaitingConfiguration1,
    AwaitingConfiguration2,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigurationSummary {
    time_base: u32,
    pmu_id: u16,
    data_rate_hz: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timestamp {
    soc: u32,
    fracsec: u32,
}

#[derive(Debug, Clone, Copy)]
struct DecodedFrame<'a> {
    frame_type: u8,
    idcode: u16,
    timestamp: Timestamp,
    message_time_quality: u8,
    body: &'a [u8],
}

impl Probe {
    fn connect(arguments: ProbeArguments) -> Result<Self, ProbeError> {
        let poll = Poll::new()?;
        let mut clients = Vec::with_capacity(arguments.count);
        for index in 0..arguments.count {
            let port = arguments.first_port + index as u16;
            let idcode = arguments.first_stream_id + index as u16;
            let address = resolve(&arguments.host, port)?;
            let mut stream = TcpStream::connect(address)?;
            poll.registry().register(
                &mut stream,
                Token(index),
                Interest::READABLE.add(Interest::WRITABLE),
            )?;
            clients.push(Client::new(stream, idcode, arguments.data_rate_hz));
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
                return Err(ProbeError::new("C37.118 V2 handshake timed out"));
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
                "at least one V2 endpoint emitted fewer than {minimum_per_endpoint} data frames"
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
    fn new(stream: TcpStream, idcode: u16, data_rate_hz: u16) -> Self {
        Self {
            stream,
            idcode,
            data_rate_hz,
            input: [0; MAX_FRAME_BYTES],
            input_length: 0,
            output: [0; wire_v2::COMMAND_FRAME_BYTES],
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
            self.queue_command(Command::Header)?;
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
                        Command::Header => ClientState::AwaitingHeader,
                        Command::Configuration1 => ClientState::AwaitingConfiguration1,
                        Command::Configuration2 => ClientState::AwaitingConfiguration2,
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
                return Err(ProbeError::new("C37.118 V2 probe receive buffer is full"));
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
            if !(FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES..=self.input.len()).contains(&frame_size)
            {
                return Err(ProbeError::new(
                    "received C37.118 V2 frame has an invalid declared size",
                ));
            }
            if self.input_length < frame_size {
                break;
            }
            let mut decoded_bytes = [0_u8; MAX_FRAME_BYTES];
            decoded_bytes[..frame_size].copy_from_slice(&self.input[..frame_size]);
            self.input.copy_within(frame_size..self.input_length, 0);
            self.input_length -= frame_size;
            let frame = decode_v2_frame(&decoded_bytes[..frame_size])?;
            validate_frame_identity(frame, self.idcode)?;
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
            (ClientState::AwaitingHeader, FRAME_TYPE_HEADER) => {
                decode_header(frame.body)?;
                self.queue_command(Command::Configuration1)?
            }
            (ClientState::AwaitingConfiguration1, FRAME_TYPE_CONFIGURATION_1) => {
                let configuration = decode_configuration(frame.body, self.data_rate_hz)?;
                self.configuration = Some(configuration);
                self.queue_command(Command::Configuration2)?
            }
            (ClientState::AwaitingConfiguration2, FRAME_TYPE_CONFIGURATION_2) => {
                let configuration = decode_configuration(frame.body, self.data_rate_hz)?;
                if self.configuration != Some(configuration) {
                    return Err(ProbeError::new("CFG-1 and CFG-2 frames disagree"));
                }
                self.queue_command(Command::Start)?
            }
            (ClientState::Streaming, FRAME_TYPE_PERIODIC_DATA) => {
                let configuration = self
                    .configuration
                    .ok_or_else(|| ProbeError::new("periodic data arrived before CFG-2"))?;
                decode_periodic_data(frame, configuration, self.last_timestamp)?;
                self.last_timestamp = Some(frame.timestamp);
                self.data_frames += 1;
            }
            _ => {
                return Err(ProbeError::new(
                    "unexpected C37.118 V2 frame for protocol state",
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
        let frame = wire_v2::encode_command(
            self.idcode,
            command,
            wire_v2::Timestamp { soc: 0, fracsec: 0 },
        );
        if frame.len() != self.output.len() {
            return Err(ProbeError::new(
                "C37.118 V2 command frame has an unexpected size",
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

fn decode_v2_frame(bytes: &[u8]) -> Result<DecodedFrame<'_>, ProbeError> {
    if bytes.len() < FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES {
        return Err(ProbeError::new(
            "C37.118 V2 frame is shorter than the V2 envelope",
        ));
    }
    if bytes[0] != 0xaa || bytes[1] & 0x80 != 0 {
        return Err(ProbeError::new("C37.118 V2 frame has an invalid sync word"));
    }
    if bytes[1] & 0x0f != wire_v2::PROTOCOL_VERSION {
        return Err(ProbeError::new(format!(
            "received C37.118 version {} but expected 2",
            bytes[1] & 0x0f
        )));
    }
    let frame_size = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    if frame_size != bytes.len() || frame_size < FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES {
        return Err(ProbeError::new(
            "C37.118 V2 frame size does not match its envelope",
        ));
    }
    let expected_checksum = u16::from_be_bytes([bytes[frame_size - 2], bytes[frame_size - 1]]);
    if probe_crc16_ccitt(&bytes[..frame_size - 2]) != expected_checksum {
        return Err(ProbeError::new("C37.118 V2 frame checksum is invalid"));
    }
    Ok(DecodedFrame {
        frame_type: bytes[1] >> 4,
        idcode: u16::from_be_bytes([bytes[4], bytes[5]]),
        timestamp: Timestamp {
            soc: u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            fracsec: u32::from_be_bytes([0, bytes[11], bytes[12], bytes[13]]),
        },
        message_time_quality: bytes[10],
        body: &bytes[FRAME_HEADER_BYTES..frame_size - FRAME_CHECKSUM_BYTES],
    })
}

fn validate_frame_identity(
    frame: DecodedFrame<'_>,
    expected_idcode: u16,
) -> Result<(), ProbeError> {
    if frame.idcode != expected_idcode {
        return Err(ProbeError::new(format!(
            "received C37.118 V2 IDCODE {} but expected {expected_idcode}",
            frame.idcode
        )));
    }
    Ok(())
}

fn decode_header(body: &[u8]) -> Result<(), ProbeError> {
    if body.is_empty() || !body.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return Err(ProbeError::new(
            "C37.118 V2 HDR payload must contain printable ASCII text",
        ));
    }
    Ok(())
}

fn decode_configuration(
    body: &[u8],
    expected_rate_hz: u16,
) -> Result<ConfigurationSummary, ProbeError> {
    let mut offset = 0;
    let time_base = take_u32(body, &mut offset)?;
    if time_base == 0 || time_base >> 24 != 0 || time_base % u32::from(expected_rate_hz) != 0 {
        return Err(ProbeError::new(
            "V2 configuration TIME_BASE is invalid for the requested data rate",
        ));
    }
    if take_u16(body, &mut offset)? != 1 {
        return Err(ProbeError::new("probe requires one PMU per V2 stream"));
    }
    let station_name = take_bytes(body, &mut offset, 16)?;
    if !station_name.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return Err(ProbeError::new("V2 station name is not fixed-width ASCII"));
    }
    let pmu_id = take_u16(body, &mut offset)?;
    if pmu_id == 0 || pmu_id == u16::MAX {
        return Err(ProbeError::new("V2 configuration PMU IDCODE is invalid"));
    }
    if take_u16(body, &mut offset)? != 0x0001 {
        return Err(ProbeError::new(
            "probe requires fixed-point polar V2 FORMAT",
        ));
    }
    let phasor_count = take_u16(body, &mut offset)?;
    let analog_count = take_u16(body, &mut offset)?;
    let digital_count = take_u16(body, &mut offset)?;
    if (phasor_count, analog_count, digital_count) != (6, 0, 0) {
        return Err(ProbeError::new(
            "V2 configuration signal counts do not match the simulator profile",
        ));
    }
    for _ in 0..usize::from(phasor_count) {
        let name = take_bytes(body, &mut offset, 16)?;
        if !name.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
            return Err(ProbeError::new("V2 channel name is not fixed-width ASCII"));
        }
    }
    for _ in 0..usize::from(phasor_count) {
        let phunit = take_u32(body, &mut offset)?;
        if phunit >> 24 > 1 || phunit & 0x00ff_ffff == 0 {
            return Err(ProbeError::new("V2 PHUNIT is invalid"));
        }
    }
    if take_u16(body, &mut offset)? > 1 {
        return Err(ProbeError::new("V2 FNOM is invalid"));
    }
    take_u16(body, &mut offset)?;
    let data_rate = take_i16(body, &mut offset)?;
    if offset != body.len() || data_rate != expected_rate_hz as i16 {
        return Err(ProbeError::new(
            "V2 configuration data rate or frame length is invalid",
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
    if frame.message_time_quality != wire_v2::MESSAGE_TIME_QUALITY_UNKNOWN {
        return Err(ProbeError::new(
            "V2 periodic data message time quality is not the simulator's conservative unknown value",
        ));
    }
    if frame.body.len() != 30 {
        return Err(ProbeError::new(
            "periodic data frame has an unexpected V2 body length",
        ));
    }
    let stat = u16::from_be_bytes([frame.body[0], frame.body[1]]);
    let conservative_stat = wire_v2::STAT_FLAG_SYNC_UNCERTAIN
        | wire_v2::STAT_PMU_TIME_QUALITY_UNKNOWN;
    if stat != 0 && stat != conservative_stat {
        return Err(ProbeError::new(
            "V2 periodic data STAT is neither good nor the simulator's conservative status",
        ));
    }
    if frame.timestamp.fracsec >= configuration.time_base
        || frame.timestamp.fracsec
            % (configuration.time_base / u32::from(configuration.data_rate_hz))
            != 0
    {
        return Err(ProbeError::new(
            "V2 periodic data timestamp is not aligned to the configured rate",
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
                "V2 periodic data timestamps skipped a configured reporting interval",
            ));
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use c37_118_simulator::wire_v2::{self, Command, Timestamp};

    use super::decode_v2_frame;

    #[test]
    fn rejects_a_v1_command_frame() {
        let mut frame =
            wire_v2::encode_command(1001, Command::Start, Timestamp { soc: 0, fracsec: 0 });
        frame[1] = 0x41;
        let checksum = wire_v2::crc16_ccitt(&frame[..frame.len() - 2]);
        let end = frame.len();
        frame[end - 2..].copy_from_slice(&checksum.to_be_bytes());

        let error = decode_v2_frame(&frame).expect_err("V1 frame must be rejected");

        assert!(error.to_string().contains("expected 2"));
    }
}
