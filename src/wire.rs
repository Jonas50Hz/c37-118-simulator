//! Bounded C37.118 frame-envelope handling.
//!
//! V1 accepts only command frames up to four KiB. It uses the CRC-16-CCITT
//! checksum convention required by the protocol frame envelope. Detailed
//! frame-body interoperability remains separately validated against approved
//! C37.118.2-2024 evidence.

use std::fmt;

use crate::config::{EndpointDescriptor, PHASOR_COUNT};

pub const SYNC_BYTE: u8 = 0xaa;
pub const FRAME_HEADER_BYTES: usize = 14;
pub const FRAME_CHECKSUM_BYTES: usize = 2;
pub const MIN_FRAME_BYTES: usize = FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
pub const MAX_COMMAND_FRAME_BYTES: usize = 4 * 1024;
pub const DATA_FRAME_BYTES: usize = 74;

pub const FRAME_TYPE_DATA: u8 = 0;
pub const FRAME_TYPE_HEADER: u8 = 1;
pub const FRAME_TYPE_CONFIG_2: u8 = 3;
pub const FRAME_TYPE_COMMAND: u8 = 4;

pub const COMMAND_STOP: u16 = 1;
pub const COMMAND_START: u16 = 2;
pub const COMMAND_HEADER: u16 = 3;
pub const COMMAND_CONFIG_2: u16 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
    BadSync,
    DeclaredSizeTooSmall,
    DeclaredSizeTooLarge,
    Incomplete,
    TrailingBytes,
    BadChecksum,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TooShort => "frame is shorter than the C37.118 envelope",
            Self::BadSync => "frame does not begin with the C37.118 sync byte",
            Self::DeclaredSizeTooSmall => "frame declares a size below the envelope minimum",
            Self::DeclaredSizeTooLarge => "frame declares a size above the V1 command limit",
            Self::Incomplete => "frame bytes do not satisfy the declared size",
            Self::TrailingBytes => "frame contains bytes beyond the declared size",
            Self::BadChecksum => "frame checksum does not match its payload",
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
    Config2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRequest {
    pub idcode: u16,
    pub version: u8,
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

    pub fn idcode(self) -> u16 {
        u16::from_be_bytes([self.bytes[4], self.bytes[5]])
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

pub fn parse_command(frame: FrameView<'_>) -> Result<CommandRequest, FrameError> {
    if frame.frame_type() != FRAME_TYPE_COMMAND || frame.bytes().len() != 18 {
        return Err(FrameError::BadSync);
    }

    let command = match u16::from_be_bytes([frame.bytes()[14], frame.bytes()[15]]) {
        COMMAND_STOP => Command::Stop,
        COMMAND_START => Command::Start,
        COMMAND_HEADER => Command::Header,
        COMMAND_CONFIG_2 => Command::Config2,
        _ => return Err(FrameError::BadSync),
    };

    Ok(CommandRequest {
        idcode: frame.idcode(),
        version: frame.version(),
        command,
    })
}

pub fn encode_command(
    idcode: u16,
    version: u8,
    command: Command,
    timestamp: Timestamp,
) -> Vec<u8> {
    let command = match command {
        Command::Stop => COMMAND_STOP,
        Command::Start => COMMAND_START,
        Command::Header => COMMAND_HEADER,
        Command::Config2 => COMMAND_CONFIG_2,
    };
    encode_frame(FRAME_TYPE_COMMAND, version, idcode, timestamp, &command.to_be_bytes())
}

pub fn encode_header(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    encode_frame(
        FRAME_TYPE_HEADER,
        endpoint.protocol_version,
        endpoint.idcode,
        timestamp,
        &endpoint.header,
    )
}

pub fn encode_config_2(endpoint: &EndpointDescriptor, timestamp: Timestamp) -> Vec<u8> {
    let mut body = Vec::with_capacity(158);
    body.extend_from_slice(&endpoint.time_base.to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&endpoint.station_name);
    body.extend_from_slice(&endpoint.idcode.to_be_bytes());
    body.extend_from_slice(&0x000b_u16.to_be_bytes());
    body.extend_from_slice(&(PHASOR_COUNT as u16).to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    body.extend_from_slice(&0_u16.to_be_bytes());
    for name in endpoint.channel_names {
        body.extend_from_slice(&name);
    }
    for index in 0..PHASOR_COUNT {
        let phasor_type = if index < 3 { 0_u32 } else { 1_u32 << 24 };
        body.extend_from_slice(&(phasor_type | 1).to_be_bytes());
    }
    body.extend_from_slice(&nominal_frequency_code(endpoint.nominal_frequency_hz).to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&(endpoint.data_rate_hz as i16).to_be_bytes());

    encode_frame(
        FRAME_TYPE_CONFIG_2,
        endpoint.protocol_version,
        endpoint.idcode,
        timestamp,
        &body,
    )
}

pub fn encode_data_into(
    endpoint: &EndpointDescriptor,
    seed: u64,
    sample_index: u64,
    timestamp: Timestamp,
    output: &mut [u8; DATA_FRAME_BYTES],
) {
    output[0] = SYNC_BYTE;
    output[1] = frame_type_and_version(FRAME_TYPE_DATA, endpoint.protocol_version);
    output[2..4].copy_from_slice(&(DATA_FRAME_BYTES as u16).to_be_bytes());
    output[4..6].copy_from_slice(&endpoint.idcode.to_be_bytes());
    output[6..10].copy_from_slice(&timestamp.soc.to_be_bytes());
    output[10..14].copy_from_slice(&timestamp.fracsec.to_be_bytes());

    let mut offset = FRAME_HEADER_BYTES;
    write_u16(output, &mut offset, 0);
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
        write_f32(output, &mut offset, magnitude);
        write_f32(output, &mut offset, phase);
    }
    write_f32(
        output,
        &mut offset,
        endpoint.frequency_deviation.nominal
            + endpoint.frequency_deviation.variation * waveform(seed, endpoint.index, 6, sample_index),
    );
    write_f32(
        output,
        &mut offset,
        endpoint.rocof.nominal + endpoint.rocof.variation * waveform(seed, endpoint.index, 7, sample_index),
    );

    debug_assert_eq!(offset + FRAME_CHECKSUM_BYTES, DATA_FRAME_BYTES);
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

pub fn append_checksum(frame: &mut Vec<u8>) {
    let checksum = crc16_ccitt(frame);
    frame.extend_from_slice(&checksum.to_be_bytes());
}

fn encode_frame(
    frame_type: u8,
    version: u8,
    idcode: u16,
    timestamp: Timestamp,
    body: &[u8],
) -> Vec<u8> {
    let size = MIN_FRAME_BYTES + body.len();
    assert!(size <= u16::MAX as usize, "C37.118 frame exceeds FRAMESIZE");

    let mut frame = Vec::with_capacity(size);
    frame.push(SYNC_BYTE);
    frame.push(frame_type_and_version(frame_type, version));
    frame.extend_from_slice(&(size as u16).to_be_bytes());
    frame.extend_from_slice(&idcode.to_be_bytes());
    frame.extend_from_slice(&timestamp.soc.to_be_bytes());
    frame.extend_from_slice(&timestamp.fracsec.to_be_bytes());
    frame.extend_from_slice(body);
    append_checksum(&mut frame);
    frame
}

fn frame_type_and_version(frame_type: u8, version: u8) -> u8 {
    debug_assert!(frame_type <= 5);
    debug_assert!((1..=15).contains(&version));
    (frame_type << 4) | version
}

fn nominal_frequency_code(frequency_hz: u16) -> u16 {
    match frequency_hz {
        50 => 1,
        60 => 0,
        _ => unreachable!("configuration validation limits nominal frequency"),
    }
}

fn waveform(seed: u64, endpoint_index: usize, channel: usize, sample_index: u64) -> f32 {
    let phase_seed = seed
        .wrapping_add((endpoint_index as u64).wrapping_mul(37))
        .wrapping_add((channel as u64).wrapping_mul(101));
    let phase = (phase_seed % 6_283) as f32 / 1_000.0;
    let step = (sample_index % 10_000) as f32 * 0.031_415_928;
    (phase + step).sin()
}

fn write_u16(buffer: &mut [u8], offset: &mut usize, value: u16) {
    buffer[*offset..*offset + 2].copy_from_slice(&value.to_be_bytes());
    *offset += 2;
}

fn write_f32(buffer: &mut [u8], offset: &mut usize, value: f32) {
    buffer[*offset..*offset + 4].copy_from_slice(&value.to_bits().to_be_bytes());
    *offset += 4;
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

#[cfg(test)]
mod tests {
    use crate::config::parse_profile;

    use super::{
        append_checksum, checksum_matches, crc16_ccitt, encode_config_2, encode_data_into,
        encode_header, encode_command, parse_command, Command, CommandRequest, FrameError,
        FrameView, Timestamp, DATA_FRAME_BYTES, FRAME_TYPE_CONFIG_2, FRAME_TYPE_DATA,
    };

    fn endpoint() -> crate::config::EndpointDescriptor {
        parse_profile(
            "seed: 7\nlimits:\n  max_logical_pmus: 1\n  max_clients_per_endpoint: 1\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: 1\n  bind_address: 127.0.0.1\n  first_listen_port: 4712\n  first_idcode: 1001\n  station_name_prefix: WAMA-PMU-\n  header: WAMA C37.118 simulator\n  protocol_version: 1\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n",
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
    fn parses_a_bounded_checksumming_frame() {
        let mut frame = vec![0xaa, 0x41, 0x00, 0x12, 0x03, 0xe9];
        frame.extend_from_slice(&[0; 8]);
        frame.extend_from_slice(&[0x00, 0x01]);
        append_checksum(&mut frame);

        let view = FrameView::parse(&frame).expect("frame must parse");
        assert_eq!(view.frame_type(), 4);
        assert_eq!(view.version(), 1);
        assert_eq!(view.idcode(), 1001);
        assert!(checksum_matches(view.bytes()));
    }

    #[test]
    fn rejects_an_unbounded_declared_size_without_allocating() {
        let mut frame = vec![0xaa, 0x41, 0x20, 0x00];
        frame.resize(16, 0);

        assert_eq!(
            FrameView::parse_with_limit(&frame, super::MAX_COMMAND_FRAME_BYTES),
            Err(FrameError::DeclaredSizeTooLarge)
        );
    }

    #[test]
    fn encodes_the_supported_command_set() {
        let frame = encode_command(
            7,
            1,
            Command::Config2,
            Timestamp {
                soc: 1,
                fracsec: 2,
            },
        );
        let parsed = FrameView::parse(&frame).expect("command must parse");

        assert_eq!(
            parse_command(parsed),
            Ok(CommandRequest {
                idcode: 7,
                version: 1,
                command: Command::Config2,
            })
        );
    }

    #[test]
    fn builds_static_header_and_configuration_frames() {
        let endpoint = endpoint();
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 0,
        };
        let header = encode_header(&endpoint, timestamp);
        let config = encode_config_2(&endpoint, timestamp);

        assert!(checksum_matches(&header));
        assert!(checksum_matches(&config));
        assert_eq!(FrameView::parse(&config).expect("config must parse").frame_type(), FRAME_TYPE_CONFIG_2);
        assert_eq!(u16::from_be_bytes([config[18], config[19]]), 1);
        assert_eq!(u16::from_be_bytes([config[40], config[41]]), 6);
    }

    #[test]
    fn writes_a_fixed_size_deterministic_data_frame() {
        let endpoint = endpoint();
        let timestamp = Timestamp {
            soc: 1_700_000_000,
            fracsec: 20_000,
        };
        let mut first = [0_u8; DATA_FRAME_BYTES];
        let mut second = [0_u8; DATA_FRAME_BYTES];
        encode_data_into(&endpoint, 7, 2, timestamp, &mut first);
        encode_data_into(&endpoint, 7, 2, timestamp, &mut second);

        assert_eq!(first, second);
        assert!(checksum_matches(&first));
        assert_eq!(FrameView::parse(&first).expect("data must parse").frame_type(), FRAME_TYPE_DATA);
    }
}