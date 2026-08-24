//! IEEE C37.118.2-2024 version-3 frame encoding and bounded parsing.
//!
//! The supported V3 subset contains command, capability, stream-configuration,
//! periodic-data, and error-response frames. Remote configuration, discrete
//! events, extended commands, and V2 compatibility are intentionally excluded.

use std::fmt;

use crate::config::{
    EndpointDescriptor, FREQUENCY_COUNT, PHASOR_COUNT, ROCOF_COUNT, V3_PROTOCOL_VERSION,
};

pub const SYNC_BYTE: u8 = 0xaa;
pub const FRAME_HEADER_BYTES: usize = 14;
pub const FRAME_CHECKSUM_BYTES: usize = 2;
pub const MIN_FRAME_BYTES: usize = FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
pub const MAX_COMMAND_FRAME_BYTES: usize = 4 * 1024;
pub const PERIODIC_DATA_FRAME_BYTES: usize = 48;
pub const ERROR_RESPONSE_FRAME_BYTES: usize = 20;

pub const FRAME_TYPE_PERIODIC_DATA: u8 = 0x8;
pub const FRAME_TYPE_CAPABILITY: u8 = 0xa;
pub const FRAME_TYPE_STREAM_CONFIGURATION: u8 = 0xb;
pub const FRAME_TYPE_COMMAND: u8 = 0xc;
pub const FRAME_TYPE_ERROR_RESPONSE: u8 = 0xf;
pub const PROTOCOL_VERSION: u8 = V3_PROTOCOL_VERSION;

pub const COMMAND_STOP: u16 = 0x0010;
pub const COMMAND_START: u16 = 0x0020;
pub const COMMAND_CAPABILITY: u16 = 0x0040;
pub const COMMAND_STREAM_CONFIGURATION: u16 = 0x0060;

pub const STAT_FLAG_SYNC_UNCERTAIN: u16 = 0x2000;
pub const TIME_QUALITY_UNKNOWN: u16 = 0x7fff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
    BadSync,
    DeclaredSizeTooSmall,
    DeclaredSizeTooLarge,
    Incomplete,
    TrailingBytes,
    BadChecksum,
    UnsupportedVersion,
    UnexpectedFrameType,
    InvalidCommandSize,
    UnsupportedCommand,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TooShort => "frame is shorter than the C37.118 V3 envelope",
            Self::BadSync => "frame does not begin with the C37.118 sync byte",
            Self::DeclaredSizeTooSmall => "frame declares a size below the V3 envelope minimum",
            Self::DeclaredSizeTooLarge => "frame declares a size above the accepted limit",
            Self::Incomplete => "frame bytes do not satisfy the declared size",
            Self::TrailingBytes => "frame contains bytes beyond the declared size",
            Self::BadChecksum => "frame checksum does not match its payload",
            Self::UnsupportedVersion => "frame does not use C37.118.2-2024 version 3",
            Self::UnexpectedFrameType => "frame type is not accepted in this context",
            Self::InvalidCommandSize => "command frame does not have the V3 fixed command size",
            Self::UnsupportedCommand => "command is not supported by the V3 simulator subset",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FrameError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub soc: u32,
    pub fracsec: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Stop,
    Start,
    Capability,
    StreamConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRequest {
    pub stream_id: u16,
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorResponseCode {
    RejectedCommand = 1,
    WrongStreamOrPmu = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameView<'a> {
    bytes: &'a [u8],
}

impl<'a> FrameView<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FrameError> {
        Self::parse_with_limit(bytes, u16::MAX as usize)
    }

    pub fn parse_with_limit(bytes: &'a [u8], maximum_size: usize) -> Result<Self, FrameError> {
        if bytes.len() < MIN_FRAME_BYTES {
            return Err(FrameError::TooShort);
        }
        if bytes[0] != SYNC_BYTE {
            return Err(FrameError::BadSync);
        }
        let declared_size = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
        if declared_size < MIN_FRAME_BYTES {
            return Err(FrameError::DeclaredSizeTooSmall);
        }
        if declared_size > maximum_size {
            return Err(FrameError::DeclaredSizeTooLarge);
        }
        if bytes.len() < declared_size {
            return Err(FrameError::Incomplete);
        }
        if bytes.len() > declared_size {
            return Err(FrameError::TrailingBytes);
        }
        if !checksum_matches(bytes) {
            return Err(FrameError::BadChecksum);
        }
        Ok(Self { bytes })
    }

    pub fn frame_type(self) -> u8 {
        self.bytes[1] >> 4
    }

    pub fn version(self) -> u8 {
        self.bytes[1] & 0x0f
    }

    pub fn stream_id(self) -> u16 {
        u16::from_be_bytes([self.bytes[4], self.bytes[5]])
    }

    pub fn timestamp(self) -> Timestamp {
        Timestamp {
            soc: u32::from_be_bytes([self.bytes[6], self.bytes[7], self.bytes[8], self.bytes[9]]),
            fracsec: u32::from_be_bytes([0, self.bytes[11], self.bytes[12], self.bytes[13]]),
        }
    }

    pub fn body(self) -> &'a [u8] {
        &self.bytes[FRAME_HEADER_BYTES..self.bytes.len() - FRAME_CHECKSUM_BYTES]
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

pub fn parse_command(frame: FrameView<'_>) -> Result<CommandRequest, FrameError> {
    if frame.version() != V3_PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    if frame.frame_type() != FRAME_TYPE_COMMAND {
        return Err(FrameError::UnexpectedFrameType);
    }
    if frame.bytes().len() != 18 {
        return Err(FrameError::InvalidCommandSize);
    }
    let command = match u16::from_be_bytes([frame.bytes()[14], frame.bytes()[15]]) {
        COMMAND_STOP => Command::Stop,
        COMMAND_START => Command::Start,
        COMMAND_CAPABILITY => Command::Capability,
        COMMAND_STREAM_CONFIGURATION => Command::StreamConfiguration,
        _ => return Err(FrameError::UnsupportedCommand),
    };
    Ok(CommandRequest {
        stream_id: frame.stream_id(),
        command,
    })
}

pub fn encode_command(stream_id: u16, command: Command, timestamp: Timestamp) -> Vec<u8> {
    let command = match command {
        Command::Stop => COMMAND_STOP,
        Command::Start => COMMAND_START,
        Command::Capability => COMMAND_CAPABILITY,
        Command::StreamConfiguration => COMMAND_STREAM_CONFIGURATION,
    };
    encode_frame(
        FRAME_TYPE_COMMAND,
        stream_id,
        timestamp,
        &command.to_be_bytes(),
    )
}

pub fn encode_capability(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    encode_configuration(FRAME_TYPE_CAPABILITY, endpoint, timestamp)
}

pub fn encode_stream_configuration(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    encode_configuration(FRAME_TYPE_STREAM_CONFIGURATION, endpoint, timestamp)
}

pub fn encode_periodic_data_into(
    endpoint: &EndpointDescriptor,
    seed: u64,
    sample_index: u64,
    timestamp: Timestamp,
    output: &mut [u8; PERIODIC_DATA_FRAME_BYTES],
) {
    write_common(
        output,
        FRAME_TYPE_PERIODIC_DATA,
        endpoint.stream_id,
        timestamp,
    );
    let mut offset = FRAME_HEADER_BYTES;
    write_u16(output, &mut offset, STAT_FLAG_SYNC_UNCERTAIN);
    write_u16(output, &mut offset, TIME_QUALITY_UNKNOWN);
    for channel in 0..PHASOR_COUNT {
        let (magnitude, scale) = if channel < 3 {
            (
                endpoint.voltage_magnitude
                    + endpoint.voltage_variation
                        * waveform(seed, endpoint.index, channel, sample_index),
                endpoint.voltage_scale,
            )
        } else {
            (
                endpoint.current_magnitude
                    + endpoint.current_variation
                        * waveform(seed, endpoint.index, channel, sample_index),
                endpoint.current_scale,
            )
        };
        let phase = match channel % 3 {
            0 => 0.0,
            1 => -2.094_395_2,
            _ => 2.094_395_2,
        };
        write_u16(output, &mut offset, fixed_magnitude(magnitude, scale));
        write_i16(output, &mut offset, fixed_angle(phase));
    }
    write_i16(
        output,
        &mut offset,
        fixed_frequency(
            endpoint.frequency_deviation.nominal
                + endpoint.frequency_deviation.variation
                    * waveform(seed, endpoint.index, PHASOR_COUNT, sample_index),
        ),
    );
    write_i16(
        output,
        &mut offset,
        fixed_rocof(
            endpoint.rocof.nominal
                + endpoint.rocof.variation
                    * waveform(seed, endpoint.index, PHASOR_COUNT + 1, sample_index),
        ),
    );
    debug_assert_eq!(offset + FRAME_CHECKSUM_BYTES, PERIODIC_DATA_FRAME_BYTES);
    let checksum = crc16_ccitt(&output[..offset]);
    output[offset..offset + FRAME_CHECKSUM_BYTES].copy_from_slice(&checksum.to_be_bytes());
}

pub fn encode_error_response_into(
    stream_id: u16,
    code: ErrorResponseCode,
    timestamp: Timestamp,
    output: &mut [u8; ERROR_RESPONSE_FRAME_BYTES],
) {
    write_common(output, FRAME_TYPE_ERROR_RESPONSE, stream_id, timestamp);
    output[14..16].copy_from_slice(&(code as u16).to_be_bytes());
    output[16..18].copy_from_slice(&0_u16.to_be_bytes());
    let checksum = crc16_ccitt(&output[..18]);
    output[18..20].copy_from_slice(&checksum.to_be_bytes());
}

pub fn checksum_matches(frame: &[u8]) -> bool {
    if frame.len() < FRAME_CHECKSUM_BYTES {
        return false;
    }
    let payload_end = frame.len() - FRAME_CHECKSUM_BYTES;
    let expected = u16::from_be_bytes([frame[payload_end], frame[payload_end + 1]]);
    crc16_ccitt(&frame[..payload_end]) == expected
}

pub fn crc16_ccitt(bytes: &[u8]) -> u16 {
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

fn encode_configuration(
    frame_type: u8,
    endpoint: &EndpointDescriptor,
    timestamp: Timestamp,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&endpoint.time_base.to_be_bytes());
    body.extend_from_slice(&endpoint.pdc_name);
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&endpoint.pmu_name);
    body.extend_from_slice(&endpoint.pmu_id.to_be_bytes());
    body.extend_from_slice(&u16::from(endpoint.protocol_version).to_be_bytes());
    body.extend_from_slice(&endpoint.global_pmu_id);
    body.extend_from_slice(&0x0001_u16.to_be_bytes());
    body.extend_from_slice(&(PHASOR_COUNT as u16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&(FREQUENCY_COUNT as u16).to_be_bytes());
    body.extend_from_slice(&(ROCOF_COUNT as u16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    for name in &endpoint.channel_names {
        body.extend_from_slice(name);
    }
    for channel in 0..PHASOR_COUNT {
        body.extend_from_slice(&[0, 0, phase_type(channel, channel >= 3), 0]);
        append_f32(
            &mut body,
            if channel < 3 {
                endpoint.voltage_scale
            } else {
                endpoint.current_scale
            },
        );
        append_f32(&mut body, 0.0);
        append_f32(&mut body, endpoint.voltage_class);
    }
    append_f32(&mut body, 0.001);
    append_f32(&mut body, 0.0);
    append_f32(&mut body, 0.01);
    append_f32(&mut body, 0.0);
    append_f32(&mut body, f32::INFINITY);
    append_f32(&mut body, f32::INFINITY);
    append_f32(&mut body, f32::INFINITY);
    body.extend_from_slice(&pmu_flag(endpoint.nominal_frequency_hz).to_be_bytes());
    body.extend_from_slice(&(-1_i32).to_be_bytes());
    body.extend_from_slice(&(-1_i32).to_be_bytes());
    body.extend_from_slice(&(endpoint.data_rate_hz as i16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&(endpoint.data_rate_hz as i16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    encode_frame(frame_type, endpoint.stream_id, timestamp, &body)
}

fn encode_frame(frame_type: u8, stream_id: u16, timestamp: Timestamp, body: &[u8]) -> Vec<u8> {
    let size = MIN_FRAME_BYTES + body.len();
    assert!(size <= u16::MAX as usize, "C37.118 frame exceeds FRAMESIZE");
    let mut frame = vec![0_u8; FRAME_HEADER_BYTES];
    frame.extend_from_slice(body);
    frame.resize(size, 0);
    write_common(&mut frame, frame_type, stream_id, timestamp);
    let checksum = crc16_ccitt(&frame[..size - FRAME_CHECKSUM_BYTES]);
    frame[size - FRAME_CHECKSUM_BYTES..].copy_from_slice(&checksum.to_be_bytes());
    frame
}

fn write_common(output: &mut [u8], frame_type: u8, stream_id: u16, timestamp: Timestamp) {
    debug_assert!(output.len() >= MIN_FRAME_BYTES);
    debug_assert!(timestamp.fracsec <= 0x00ff_ffff);
    let frame_size = output.len() as u16;
    output[0] = SYNC_BYTE;
    output[1] = (frame_type << 4) | V3_PROTOCOL_VERSION;
    output[2..4].copy_from_slice(&frame_size.to_be_bytes());
    output[4..6].copy_from_slice(&stream_id.to_be_bytes());
    output[6..10].copy_from_slice(&timestamp.soc.to_be_bytes());
    output[10] = 0;
    let fraction = timestamp.fracsec.to_be_bytes();
    output[11..14].copy_from_slice(&fraction[1..]);
}

fn phase_type(channel: usize, current: bool) -> u8 {
    let component = match channel % 3 {
        0 => 0b100,
        1 => 0b101,
        _ => 0b110,
    };
    if current {
        component | 0b1000
    } else {
        component
    }
}

fn pmu_flag(nominal_frequency_hz: u16) -> u16 {
    (1_u16 << 15)
        | if nominal_frequency_hz == 50 {
            1_u16 << 13
        } else {
            0
        }
}

fn fixed_magnitude(value: f32, scale: f32) -> u16 {
    (value / scale).round() as u16
}

fn fixed_angle(value: f32) -> i16 {
    (value * 10_000.0).round() as i16
}

fn fixed_frequency(value_hz: f32) -> i16 {
    (value_hz * 1_000.0).round() as i16
}

fn fixed_rocof(value_hz_per_second: f32) -> i16 {
    (value_hz_per_second * 100.0).round() as i16
}

fn waveform(seed: u64, endpoint_index: usize, channel: usize, sample_index: u64) -> f32 {
    let phase_seed = seed
        .wrapping_add((endpoint_index as u64).wrapping_mul(37))
        .wrapping_add((channel as u64).wrapping_mul(101));
    let phase = (phase_seed % 6_283) as f32 / 1_000.0;
    let step = (sample_index % 10_000) as f32 * 0.031_415_93;
    (phase + step).sin()
}

fn append_f32(output: &mut Vec<u8>, value: f32) {
    output.extend_from_slice(&value.to_bits().to_be_bytes());
}

fn write_u16(buffer: &mut [u8], offset: &mut usize, value: u16) {
    buffer[*offset..*offset + 2].copy_from_slice(&value.to_be_bytes());
    *offset += 2;
}

fn write_i16(buffer: &mut [u8], offset: &mut usize, value: i16) {
    buffer[*offset..*offset + 2].copy_from_slice(&value.to_be_bytes());
    *offset += 2;
}

#[cfg(test)]
mod tests {
    use crate::config::parse_profile;

    use super::{
        checksum_matches, crc16_ccitt, encode_capability, encode_command,
        encode_error_response_into, encode_periodic_data_into, encode_stream_configuration,
        parse_command, Command, CommandRequest, ErrorResponseCode, FrameError, FrameView,
        Timestamp, ERROR_RESPONSE_FRAME_BYTES, FRAME_TYPE_CAPABILITY, FRAME_TYPE_COMMAND,
        FRAME_TYPE_ERROR_RESPONSE, FRAME_TYPE_PERIODIC_DATA, FRAME_TYPE_STREAM_CONFIGURATION,
        PERIODIC_DATA_FRAME_BYTES,
    };

    fn endpoint() -> crate::config::EndpointDescriptor {
        parse_profile(
            "seed: 7\nlimits:\n  max_logical_pmus: 1\n  max_clients_per_endpoint: 1\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: 1\n  bind_address: 127.0.0.1\n  first_listen_port: 4712\n  first_stream_id: 1001\n  first_pmu_id: 1001\n  pdc_name: WAMA\n  pmu_name_prefix: WAMA-PMU-\n  protocol_version: 3\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    voltage_class: 400000.0\n    voltage_scale: 10.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n    current_scale: 1.0\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n",
        )
        .expect("profile must compile")
        .endpoints
        .remove(0)
    }

    #[test]
    fn calculates_crc16_ccitt_false_reference_vector() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29b1);
    }

    #[test]
    fn encodes_the_v3_start_command() {
        let frame = encode_command(1001, Command::Start, Timestamp { soc: 1, fracsec: 2 });
        let parsed = FrameView::parse(&frame).expect("frame must parse");

        assert_eq!(parsed.bytes()[1], 0xc3);
        assert_eq!(parsed.frame_type(), FRAME_TYPE_COMMAND);
        assert_eq!(parsed.stream_id(), 1001);
        assert_eq!(parsed.timestamp().fracsec, 2);
        assert_eq!(parsed.bytes()[14..16], [0x00, 0x20]);
        assert_eq!(
            parse_command(parsed),
            Ok(CommandRequest {
                stream_id: 1001,
                command: Command::Start,
            })
        );
    }

    #[test]
    fn rejects_a_legacy_command_version() {
        let mut frame = encode_command(1001, Command::Start, Timestamp { soc: 1, fracsec: 2 });
        frame[1] = 0xc1;
        let checksum = crc16_ccitt(&frame[..frame.len() - 2]);
        let end = frame.len();
        frame[end - 2..].copy_from_slice(&checksum.to_be_bytes());
        let parsed = FrameView::parse(&frame).expect("frame envelope must parse");

        assert_eq!(parse_command(parsed), Err(FrameError::UnsupportedVersion));
    }

    #[test]
    fn builds_capability_and_stream_configuration_frames() {
        let endpoint = endpoint();
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 20_000,
        };
        let capability = encode_capability(&endpoint, timestamp);
        let configuration = encode_stream_configuration(&endpoint, timestamp);

        assert!(checksum_matches(&capability));
        assert!(checksum_matches(&configuration));
        assert_eq!(
            FrameView::parse(&capability)
                .expect("capability must parse")
                .frame_type(),
            FRAME_TYPE_CAPABILITY
        );
        assert_eq!(
            FrameView::parse(&configuration)
                .expect("configuration must parse")
                .frame_type(),
            FRAME_TYPE_STREAM_CONFIGURATION
        );
        assert_eq!(u16::from_be_bytes([capability[14], capability[15]]), 0);
    }

    #[test]
    fn writes_a_fixed_size_v3_periodic_data_frame() {
        let endpoint = endpoint();
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 20_000,
        };
        let mut first = [0_u8; PERIODIC_DATA_FRAME_BYTES];
        let mut second = [0_u8; PERIODIC_DATA_FRAME_BYTES];
        encode_periodic_data_into(&endpoint, 7, 2, timestamp, &mut first);
        encode_periodic_data_into(&endpoint, 7, 2, timestamp, &mut second);

        assert_eq!(first, second);
        assert!(checksum_matches(&first));
        let view = FrameView::parse(&first).expect("data must parse");
        assert_eq!(view.bytes()[1], 0x83);
        assert_eq!(view.frame_type(), FRAME_TYPE_PERIODIC_DATA);
        assert_eq!(view.timestamp().fracsec, 20_000);
    }

    #[test]
    fn builds_a_v3_wrong_stream_error_response() {
        let mut frame = [0_u8; ERROR_RESPONSE_FRAME_BYTES];
        encode_error_response_into(
            1002,
            ErrorResponseCode::WrongStreamOrPmu,
            Timestamp { soc: 1, fracsec: 0 },
            &mut frame,
        );

        let view = FrameView::parse(&frame).expect("error response must parse");
        assert_eq!(view.bytes()[1], 0xf3);
        assert_eq!(view.frame_type(), FRAME_TYPE_ERROR_RESPONSE);
        assert_eq!(view.body(), [0x00, 0x02, 0x00, 0x00]);
    }
}
