//! IEEE C37.118.2-2011 version-2 frame encoding and bounded parsing.
//!
//! The supported V2 subset contains command, header, CFG-1, CFG-2, and
//! periodic-data frames. CFG-3 and V1 compatibility are deliberately excluded.

use std::fmt;

use crate::config::{EndpointDescriptor, PHASOR_COUNT, V2_PROTOCOL_VERSION};

pub const SYNC_BYTE: u8 = 0xaa;
pub const FRAME_HEADER_BYTES: usize = 14;
pub const FRAME_CHECKSUM_BYTES: usize = 2;
pub const MIN_FRAME_BYTES: usize = FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
pub const MAX_COMMAND_FRAME_BYTES: usize = 4 * 1024;
pub const COMMAND_FRAME_BYTES: usize = 18;
pub const PERIODIC_DATA_FRAME_BYTES: usize = 46;

pub const FRAME_TYPE_PERIODIC_DATA: u8 = 0x0;
pub const FRAME_TYPE_HEADER: u8 = 0x1;
pub const FRAME_TYPE_CONFIGURATION_1: u8 = 0x2;
pub const FRAME_TYPE_CONFIGURATION_2: u8 = 0x3;
pub const FRAME_TYPE_COMMAND: u8 = 0x4;
pub const PROTOCOL_VERSION: u8 = V2_PROTOCOL_VERSION;

pub const COMMAND_STOP: u16 = 0x0001;
pub const COMMAND_START: u16 = 0x0002;
pub const COMMAND_HEADER: u16 = 0x0003;
pub const COMMAND_CONFIGURATION_1: u16 = 0x0004;
pub const COMMAND_CONFIGURATION_2: u16 = 0x0005;

pub const MESSAGE_TIME_QUALITY_UNKNOWN: u8 = 0x0f;
pub const STAT_FLAG_SYNC_UNCERTAIN: u16 = 0x2000;
pub const STAT_PMU_TIME_QUALITY_UNKNOWN: u16 = 0x01c0;

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
            Self::TooShort => "frame is shorter than the C37.118 V2 envelope",
            Self::BadSync => "frame does not begin with the C37.118 V2 sync word",
            Self::DeclaredSizeTooSmall => "frame declares a size below the V2 envelope minimum",
            Self::DeclaredSizeTooLarge => "frame declares a size above the accepted limit",
            Self::Incomplete => "frame bytes do not satisfy the declared size",
            Self::TrailingBytes => "frame contains bytes beyond the declared size",
            Self::BadChecksum => "frame checksum does not match its payload",
            Self::UnsupportedVersion => "frame does not use C37.118.2-2011 version 2",
            Self::UnexpectedFrameType => "frame type is not accepted in this context",
            Self::InvalidCommandSize => "command frame does not have the V2 fixed command size",
            Self::UnsupportedCommand => "command is not supported by the V2 simulator subset",
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
    Header,
    Configuration1,
    Configuration2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRequest {
    pub idcode: u16,
    pub command: Command,
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
        if bytes[0] != SYNC_BYTE || bytes[1] & 0x80 != 0 {
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

    pub fn idcode(self) -> u16 {
        u16::from_be_bytes([self.bytes[4], self.bytes[5]])
    }

    pub fn timestamp(self) -> Timestamp {
        Timestamp {
            soc: u32::from_be_bytes([self.bytes[6], self.bytes[7], self.bytes[8], self.bytes[9]]),
            fracsec: u32::from_be_bytes([0, self.bytes[11], self.bytes[12], self.bytes[13]]),
        }
    }

    pub fn message_time_quality(self) -> u8 {
        self.bytes[10]
    }

    pub fn body(self) -> &'a [u8] {
        &self.bytes[FRAME_HEADER_BYTES..self.bytes.len() - FRAME_CHECKSUM_BYTES]
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

pub fn parse_command(frame: FrameView<'_>) -> Result<CommandRequest, FrameError> {
    if frame.version() != V2_PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    if frame.frame_type() != FRAME_TYPE_COMMAND {
        return Err(FrameError::UnexpectedFrameType);
    }
    if frame.bytes().len() != COMMAND_FRAME_BYTES {
        return Err(FrameError::InvalidCommandSize);
    }
    let command = match u16::from_be_bytes([frame.bytes()[14], frame.bytes()[15]]) {
        COMMAND_STOP => Command::Stop,
        COMMAND_START => Command::Start,
        COMMAND_HEADER => Command::Header,
        COMMAND_CONFIGURATION_1 => Command::Configuration1,
        COMMAND_CONFIGURATION_2 => Command::Configuration2,
        _ => return Err(FrameError::UnsupportedCommand),
    };
    Ok(CommandRequest {
        idcode: frame.idcode(),
        command,
    })
}

pub fn encode_command(idcode: u16, command: Command, timestamp: Timestamp) -> Vec<u8> {
    let command = match command {
        Command::Stop => COMMAND_STOP,
        Command::Start => COMMAND_START,
        Command::Header => COMMAND_HEADER,
        Command::Configuration1 => COMMAND_CONFIGURATION_1,
        Command::Configuration2 => COMMAND_CONFIGURATION_2,
    };
    encode_frame(
        FRAME_TYPE_COMMAND,
        idcode,
        timestamp,
        &command.to_be_bytes(),
    )
}

pub fn encode_header(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    let metadata = endpoint
        .v2
        .as_ref()
        .expect("V2 header encoding requires V2 endpoint metadata");
    encode_frame(
        FRAME_TYPE_HEADER,
        endpoint.stream_id,
        timestamp,
        &metadata.station_name,
    )
}

pub fn encode_configuration_1(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    encode_configuration(FRAME_TYPE_CONFIGURATION_1, endpoint, timestamp)
}

pub fn encode_configuration_2(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    encode_configuration(FRAME_TYPE_CONFIGURATION_2, endpoint, timestamp)
}

pub fn encode_periodic_data_into(
    endpoint: &EndpointDescriptor,
    seed: u64,
    sample_index: u64,
    timestamp: Timestamp,
    output: &mut [u8; PERIODIC_DATA_FRAME_BYTES],
) {
    let metadata = endpoint
        .v2
        .as_ref()
        .expect("V2 periodic-data encoding requires V2 endpoint metadata");
    write_common(
        output,
        FRAME_TYPE_PERIODIC_DATA,
        endpoint.stream_id,
        timestamp,
    );
    let mut offset = FRAME_HEADER_BYTES;
    let stat = if metadata.good_stat {
        0
    } else {
        STAT_FLAG_SYNC_UNCERTAIN | STAT_PMU_TIME_QUALITY_UNKNOWN
    };
    write_u16(
        output,
        &mut offset,
        stat,
    );
    for channel in 0..PHASOR_COUNT {
        let magnitude = if channel < 3 {
            endpoint.voltage_magnitude
                + endpoint.voltage_variation * waveform(seed, endpoint.index, channel, sample_index)
        } else {
            endpoint.current_magnitude
                + endpoint.current_variation * waveform(seed, endpoint.index, channel, sample_index)
        };
        let phase = match channel % 3 {
            0 => 0.0,
            1 => -2.094_395_2,
            _ => 2.094_395_2,
        };
        write_u16(
            output,
            &mut offset,
            fixed_magnitude(magnitude, phunit_scale(metadata.phunits[channel])),
        );
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
    let metadata = endpoint
        .v2
        .as_ref()
        .expect("V2 configuration encoding requires V2 endpoint metadata");
    let mut body = Vec::with_capacity(156);
    body.extend_from_slice(&endpoint.time_base.to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&metadata.station_name);
    body.extend_from_slice(&endpoint.pmu_id.to_be_bytes());
    body.extend_from_slice(&0x0001_u16.to_be_bytes());
    body.extend_from_slice(&(PHASOR_COUNT as u16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    for name in &metadata.channel_names {
        body.extend_from_slice(name);
    }
    for phunit in metadata.phunits {
        body.extend_from_slice(&phunit.to_be_bytes());
    }
    body.extend_from_slice(
        &if endpoint.nominal_frequency_hz == 50 {
            1_u16
        } else {
            0_u16
        }
        .to_be_bytes(),
    );
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&(endpoint.data_rate_hz as i16).to_be_bytes());
    encode_frame(frame_type, endpoint.stream_id, timestamp, &body)
}

fn encode_frame(frame_type: u8, idcode: u16, timestamp: Timestamp, body: &[u8]) -> Vec<u8> {
    let size = MIN_FRAME_BYTES + body.len();
    assert!(size <= u16::MAX as usize, "C37.118 frame exceeds FRAMESIZE");
    let mut frame = vec![0_u8; FRAME_HEADER_BYTES];
    frame.extend_from_slice(body);
    frame.resize(size, 0);
    write_common(&mut frame, frame_type, idcode, timestamp);
    let checksum = crc16_ccitt(&frame[..size - FRAME_CHECKSUM_BYTES]);
    frame[size - FRAME_CHECKSUM_BYTES..].copy_from_slice(&checksum.to_be_bytes());
    frame
}

fn write_common(output: &mut [u8], frame_type: u8, idcode: u16, timestamp: Timestamp) {
    debug_assert!(output.len() >= MIN_FRAME_BYTES);
    debug_assert!(timestamp.fracsec <= 0x00ff_ffff);
    let frame_size = output.len() as u16;
    output[0] = SYNC_BYTE;
    output[1] = (frame_type << 4) | V2_PROTOCOL_VERSION;
    output[2..4].copy_from_slice(&frame_size.to_be_bytes());
    output[4..6].copy_from_slice(&idcode.to_be_bytes());
    output[6..10].copy_from_slice(&timestamp.soc.to_be_bytes());
    output[10] = MESSAGE_TIME_QUALITY_UNKNOWN;
    let fraction = timestamp.fracsec.to_be_bytes();
    output[11..14].copy_from_slice(&fraction[1..]);
}

fn phunit_scale(phunit: u32) -> f32 {
    (phunit & 0x00ff_ffff) as f32 / 100_000.0
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
    use crate::config::{load_profile, parse_profile};

    use super::{
        checksum_matches, crc16_ccitt, encode_command, encode_configuration_1,
        encode_configuration_2, encode_header, encode_periodic_data_into, parse_command, Command,
        CommandRequest, FrameError, FrameView, Timestamp, FRAME_TYPE_COMMAND,
        FRAME_TYPE_CONFIGURATION_1, FRAME_TYPE_CONFIGURATION_2, FRAME_TYPE_HEADER,
        FRAME_TYPE_PERIODIC_DATA, PERIODIC_DATA_FRAME_BYTES, STAT_FLAG_SYNC_UNCERTAIN,
        STAT_PMU_TIME_QUALITY_UNKNOWN,
    };

    fn endpoint() -> crate::config::EndpointDescriptor {
        parse_profile(
            "seed: 7\nlimits:\n  max_logical_pmus: 1\n  max_clients_per_endpoint: 1\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: 1\n  bind_address: 127.0.0.1\n  first_listen_port: 4712\n  first_stream_id: 1001\n  first_pmu_id: 1001\n  pdc_name: WAMA\n  pmu_name_prefix: WAMA-PMU-\n  protocol_version: 2\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    voltage_class: 400000.0\n    voltage_scale: 10.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n    current_scale: 1.0\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n",
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
    fn encodes_the_v2_start_command() {
        let frame = encode_command(1001, Command::Start, Timestamp { soc: 1, fracsec: 2 });
        let parsed = FrameView::parse(&frame).expect("frame must parse");

        assert_eq!(parsed.bytes()[1], 0x42);
        assert_eq!(parsed.frame_type(), FRAME_TYPE_COMMAND);
        assert_eq!(parsed.idcode(), 1001);
        assert_eq!(parsed.message_time_quality(), 0x0f);
        assert_eq!(parsed.timestamp().fracsec, 2);
        assert_eq!(parsed.bytes()[14..16], [0x00, 0x02]);
        assert_eq!(
            parse_command(parsed),
            Ok(CommandRequest {
                idcode: 1001,
                command: Command::Start,
            })
        );
    }

    #[test]
    fn rejects_a_v1_command_version() {
        let mut frame = encode_command(1001, Command::Start, Timestamp { soc: 1, fracsec: 2 });
        frame[1] = 0x41;
        let checksum = crc16_ccitt(&frame[..frame.len() - 2]);
        let end = frame.len();
        frame[end - 2..].copy_from_slice(&checksum.to_be_bytes());
        let parsed = FrameView::parse(&frame).expect("frame envelope must parse");

        assert_eq!(parse_command(parsed), Err(FrameError::UnsupportedVersion));
    }

    #[test]
    fn builds_v2_header_and_configuration_frames() {
        let endpoint = endpoint();
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 20_000,
        };
        let header = encode_header(&endpoint, timestamp);
        let configuration_1 = encode_configuration_1(&endpoint, timestamp);
        let configuration_2 = encode_configuration_2(&endpoint, timestamp);

        assert!(checksum_matches(&header));
        assert!(checksum_matches(&configuration_1));
        assert!(checksum_matches(&configuration_2));
        assert_eq!(
            FrameView::parse(&header)
                .expect("header must parse")
                .frame_type(),
            FRAME_TYPE_HEADER
        );
        assert_eq!(
            FrameView::parse(&configuration_1)
                .expect("configuration 1 must parse")
                .frame_type(),
            FRAME_TYPE_CONFIGURATION_1
        );
        assert_eq!(
            FrameView::parse(&configuration_2)
                .expect("configuration 2 must parse")
                .frame_type(),
            FRAME_TYPE_CONFIGURATION_2
        );
        assert_eq!(configuration_1.len(), 174);
        assert_eq!(configuration_2.len(), 174);
        assert_eq!(&configuration_1[14..18], &1_000_000_u32.to_be_bytes());
        assert_eq!(&configuration_1[18..20], &1_u16.to_be_bytes());
    }

    #[test]
    fn writes_a_fixed_size_v2_periodic_data_frame() {
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
        assert_eq!(view.bytes()[1], 0x02);
        assert_eq!(view.frame_type(), FRAME_TYPE_PERIODIC_DATA);
        assert_eq!(view.message_time_quality(), 0x0f);
        assert_eq!(view.timestamp().fracsec, 20_000);
        assert_eq!(
            u16::from_be_bytes([view.body()[0], view.body()[1]]),
            STAT_FLAG_SYNC_UNCERTAIN | STAT_PMU_TIME_QUALITY_UNKNOWN,
        );
    }

    #[test]
    fn writes_good_stat_only_for_profile_selected_v2_pmus() {
        let profile = load_profile(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/profiles/five-pmu-v2.yaml"
        ))
        .expect("five-PMU V2 profile must compile");
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 20_000,
        };

        for (endpoint, expected_stat) in profile.endpoints.iter().zip([
            0,
            0,
            STAT_FLAG_SYNC_UNCERTAIN | STAT_PMU_TIME_QUALITY_UNKNOWN,
            STAT_FLAG_SYNC_UNCERTAIN | STAT_PMU_TIME_QUALITY_UNKNOWN,
            STAT_FLAG_SYNC_UNCERTAIN | STAT_PMU_TIME_QUALITY_UNKNOWN,
        ]) {
            let mut frame = [0_u8; PERIODIC_DATA_FRAME_BYTES];
            encode_periodic_data_into(endpoint, profile.seed, 2, timestamp, &mut frame);
            let view = FrameView::parse(&frame).expect("data must parse");
            assert_eq!(u16::from_be_bytes([view.body()[0], view.body()[1]]), expected_stat);
        }
    }
}
