//! Startup-only profile parsing and bounded fleet compilation.

use std::{
    fmt, fs,
    net::{IpAddr, SocketAddr},
    path::Path,
};

use serde::Deserialize;

pub const MAX_LOGICAL_PMUS: usize = 150;
pub const PHASOR_COUNT: usize = 6;
pub const FREQUENCY_COUNT: usize = 1;
pub const ROCOF_COUNT: usize = 1;
pub const CHANNEL_COUNT: usize = PHASOR_COUNT + FREQUENCY_COUNT + ROCOF_COUNT;
pub const MAX_COMMAND_FRAME_BYTES: usize = 4 * 1024;
pub const V2_PROTOCOL_VERSION: u8 = 2;
pub const V3_PROTOCOL_VERSION: u8 = 3;
pub const V2_NAME_FIELD_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledProfile {
    pub seed: u64,
    pub limits: Limits,
    pub endpoints: Vec<EndpointDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub command_frame_bytes: usize,
    pub receive_buffer_bytes: usize,
    pub send_buffer_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireVersion {
    V2,
    V3,
}

impl WireVersion {
    pub const fn protocol_version(self) -> u8 {
        match self {
            Self::V2 => V2_PROTOCOL_VERSION,
            Self::V3 => V3_PROTOCOL_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EndpointDescriptor {
    pub index: usize,
    pub address: SocketAddr,
    pub stream_id: u16,
    pub pmu_id: u16,
    pub protocol_version: u8,
    pub wire_version: WireVersion,
    pub v2: Option<V2EndpointMetadata>,
    pub pdc_name: Vec<u8>,
    pub pmu_name: Vec<u8>,
    pub global_pmu_id: [u8; 16],
    pub channel_names: Vec<Vec<u8>>,
    pub time_base: u32,
    pub data_rate_hz: u16,
    pub nominal_frequency_hz: u16,
    pub voltage_magnitude: f32,
    pub voltage_variation: f32,
    pub voltage_class: f32,
    pub voltage_scale: f32,
    pub current_magnitude: f32,
    pub current_variation: f32,
    pub current_scale: f32,
    pub frequency_deviation: SignalRecipe,
    pub rocof: SignalRecipe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2EndpointMetadata {
    pub station_name: [u8; V2_NAME_FIELD_BYTES],
    pub channel_names: [[u8; V2_NAME_FIELD_BYTES]; PHASOR_COUNT],
    pub phunits: [u32; PHASOR_COUNT],
    pub good_stat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalRecipe {
    pub nominal: f32,
    pub variation: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl TryFrom<u8> for WireVersion {
    type Error = ConfigError;

    fn try_from(protocol_version: u8) -> Result<Self, Self::Error> {
        match protocol_version {
            V2_PROTOCOL_VERSION => Ok(Self::V2),
            V3_PROTOCOL_VERSION => Ok(Self::V3),
            _ => Err(ConfigError::new(
                "fleet.protocol_version must be 2 for C37.118.2-2011 V2 or 3 for C37.118.2-2024 V3",
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    seed: u64,
    limits: RawLimits,
    fleet: Fleet,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_logical_pmus: usize,
    max_clients_per_endpoint: usize,
    max_command_frame_bytes: usize,
    requested_socket_receive_buffer_bytes: usize,
    requested_socket_send_buffer_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fleet {
    count: usize,
    bind_address: IpAddr,
    first_listen_port: u16,
    first_stream_id: u16,
    first_pmu_id: u16,
    pdc_name: String,
    pmu_name_prefix: String,
    protocol_version: u8,
    #[serde(default)]
    v2_good_stat_pmu_ids: Vec<u16>,
    data_rate_hz: u16,
    time_base: u32,
    nominal_frequency_hz: u16,
    phasors: PhasorRecipe,
    frequency_deviation_hz: SignalRecipe,
    rocof_hz_per_s: SignalRecipe,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhasorRecipe {
    voltage_magnitude: f32,
    voltage_variation: f32,
    voltage_class: f32,
    voltage_scale: f32,
    current_magnitude: f32,
    current_variation: f32,
    current_scale: f32,
}

impl<'de> Deserialize<'de> for SignalRecipe {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSignalRecipe {
            nominal: f32,
            variation: f32,
        }

        let raw = RawSignalRecipe::deserialize(deserializer)?;
        Ok(Self {
            nominal: raw.nominal,
            variation: raw.variation,
        })
    }
}

pub fn load_profile(path: impl AsRef<Path>) -> Result<CompiledProfile, ConfigError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path)
        .map_err(|error| ConfigError::new(format!("cannot read {}: {error}", path.display())))?;
    parse_profile(&contents)
}

pub fn parse_profile(contents: &str) -> Result<CompiledProfile, ConfigError> {
    let profile = serde_yaml::from_str::<Profile>(contents)
        .map_err(|error| ConfigError::new(format!("invalid simulator profile: {error}")))?;
    compile_profile(profile)
}

fn compile_profile(profile: Profile) -> Result<CompiledProfile, ConfigError> {
    validate_limits(&profile.limits)?;
    let wire_version = validate_fleet(&profile.fleet, profile.limits.max_logical_pmus)?;

    let count_minus_one = u16::try_from(profile.fleet.count - 1)
        .map_err(|_| ConfigError::new("fleet.count exceeds the V3 PMU limit"))?;
    let final_port = profile
        .fleet
        .first_listen_port
        .checked_add(count_minus_one)
        .ok_or_else(|| ConfigError::new("fleet listener port range overflows u16"))?;
    let final_stream_id =
        final_identifier(profile.fleet.first_stream_id, count_minus_one, "STREAM_ID")?;
    let final_pmu_id = final_identifier(profile.fleet.first_pmu_id, count_minus_one, "PMU_ID")?;

    if profile.fleet.first_listen_port == 0 || final_port == 0 {
        return Err(ConfigError::new("fleet listener ports must be non-zero"));
    }
    let _ = (final_stream_id, final_pmu_id);

    let (pdc_name, channel_names): (Vec<u8>, Vec<Vec<u8>>) = match wire_version {
        WireVersion::V2 => (Vec::new(), Vec::new()),
        WireVersion::V3 => (
            encoded_name(&profile.fleet.pdc_name, "fleet.pdc_name")?,
            standard_channel_names()?,
        ),
    };
    let v2_channel_names = match wire_version {
        WireVersion::V2 => Some(standard_v2_channel_names()?),
        WireVersion::V3 => None,
    };
    let v2_phunits = match wire_version {
        WireVersion::V2 => Some(v2_phunits(&profile.fleet.phasors)?),
        WireVersion::V3 => None,
    };
    let mut endpoints = Vec::with_capacity(profile.fleet.count);
    for index in 0..profile.fleet.count {
        let offset = u16::try_from(index)
            .map_err(|_| ConfigError::new("fleet index does not fit into u16"))?;
        let port = profile
            .fleet
            .first_listen_port
            .checked_add(offset)
            .ok_or_else(|| ConfigError::new("fleet listener port range overflows u16"))?;
        let stream_id = profile
            .fleet
            .first_stream_id
            .checked_add(offset)
            .ok_or_else(|| ConfigError::new("fleet STREAM_ID range overflows u16"))?;
        let pmu_id = profile
            .fleet
            .first_pmu_id
            .checked_add(offset)
            .ok_or_else(|| ConfigError::new("fleet PMU_ID range overflows u16"))?;
        let pmu_name_source = format!("{}{:03}", profile.fleet.pmu_name_prefix, index + 1);
        let (pmu_name, global_pmu_id) = match wire_version {
            WireVersion::V2 => (Vec::new(), [0; 16]),
            WireVersion::V3 => (
                encoded_name(&pmu_name_source, "fleet.pmu_name_prefix")?,
                deterministic_uuid(profile.seed, index),
            ),
        };
        let v2 = match wire_version {
            WireVersion::V2 => Some(V2EndpointMetadata {
                station_name: v2_fixed_ascii(&pmu_name_source, "fleet.pmu_name_prefix")?,
                channel_names: v2_channel_names
                    .expect("V2 profiles compile fixed-width channel names"),
                phunits: v2_phunits.expect("V2 profiles compile PHUNIT values"),
                good_stat: profile.fleet.v2_good_stat_pmu_ids.contains(&pmu_id),
            }),
            WireVersion::V3 => None,
        };

        endpoints.push(EndpointDescriptor {
            index,
            address: SocketAddr::new(profile.fleet.bind_address, port),
            stream_id,
            pmu_id,
            protocol_version: profile.fleet.protocol_version,
            wire_version,
            v2,
            pdc_name: pdc_name.clone(),
            pmu_name,
            global_pmu_id,
            channel_names: channel_names.clone(),
            time_base: profile.fleet.time_base,
            data_rate_hz: profile.fleet.data_rate_hz,
            nominal_frequency_hz: profile.fleet.nominal_frequency_hz,
            voltage_magnitude: profile.fleet.phasors.voltage_magnitude,
            voltage_variation: profile.fleet.phasors.voltage_variation,
            voltage_class: profile.fleet.phasors.voltage_class,
            voltage_scale: profile.fleet.phasors.voltage_scale,
            current_magnitude: profile.fleet.phasors.current_magnitude,
            current_variation: profile.fleet.phasors.current_variation,
            current_scale: profile.fleet.phasors.current_scale,
            frequency_deviation: profile.fleet.frequency_deviation_hz,
            rocof: profile.fleet.rocof_hz_per_s,
        });
    }

    Ok(CompiledProfile {
        seed: profile.seed,
        limits: Limits {
            command_frame_bytes: profile.limits.max_command_frame_bytes,
            receive_buffer_bytes: profile.limits.requested_socket_receive_buffer_bytes,
            send_buffer_bytes: profile.limits.requested_socket_send_buffer_bytes,
        },
        endpoints,
    })
}

fn validate_limits(limits: &RawLimits) -> Result<(), ConfigError> {
    if !(1..=MAX_LOGICAL_PMUS).contains(&limits.max_logical_pmus) {
        return Err(ConfigError::new(format!(
            "limits.max_logical_pmus must be in 1..={MAX_LOGICAL_PMUS}"
        )));
    }
    if limits.max_clients_per_endpoint != 2 {
        return Err(ConfigError::new(
            "limits.max_clients_per_endpoint must be exactly 2",
        ));
    }
    if !(18..=MAX_COMMAND_FRAME_BYTES).contains(&limits.max_command_frame_bytes) {
        return Err(ConfigError::new(format!(
            "limits.max_command_frame_bytes must be in 18..={MAX_COMMAND_FRAME_BYTES}"
        )));
    }
    for (name, value) in [
        (
            "limits.requested_socket_receive_buffer_bytes",
            limits.requested_socket_receive_buffer_bytes,
        ),
        (
            "limits.requested_socket_send_buffer_bytes",
            limits.requested_socket_send_buffer_bytes,
        ),
    ] {
        if !(1..=MAX_COMMAND_FRAME_BYTES).contains(&value) {
            return Err(ConfigError::new(format!(
                "{name} must be in 1..={MAX_COMMAND_FRAME_BYTES}"
            )));
        }
    }
    Ok(())
}

fn validate_fleet(fleet: &Fleet, max_logical_pmus: usize) -> Result<WireVersion, ConfigError> {
    if !(1..=max_logical_pmus).contains(&fleet.count) {
        return Err(ConfigError::new(format!(
            "fleet.count must be in 1..={max_logical_pmus}"
        )));
    }
    let wire_version = WireVersion::try_from(fleet.protocol_version)?;
    if fleet.data_rate_hz != 50 {
        return Err(ConfigError::new(
            "fleet.data_rate_hz must be 50 for the V3 simulator profile",
        ));
    }
    if !(1..=0x00ff_ffff).contains(&fleet.time_base) {
        return Err(ConfigError::new(
            "fleet.time_base must fit the 24-bit TIME_BASE field",
        ));
    }
    if fleet.time_base % u32::from(fleet.data_rate_hz) != 0 {
        return Err(ConfigError::new(
            "fleet.time_base must be an integer multiple of fleet.data_rate_hz",
        ));
    }
    if !matches!(fleet.nominal_frequency_hz, 50 | 60) {
        return Err(ConfigError::new(
            "fleet.nominal_frequency_hz must be 50 or 60",
        ));
    }
    encoded_name(&fleet.pdc_name, "fleet.pdc_name")?;
    encoded_name(
        &format!("{}{:03}", fleet.pmu_name_prefix, fleet.count),
        "fleet.pmu_name_prefix",
    )?;
    let count_minus_one = u16::try_from(fleet.count - 1)
        .map_err(|_| ConfigError::new("fleet.count exceeds the V3 PMU limit"))?;
    let _ = final_identifier(fleet.first_stream_id, count_minus_one, "STREAM_ID")?;
    let final_pmu_id = final_identifier(fleet.first_pmu_id, count_minus_one, "PMU_ID")?;
    validate_v2_good_stat_pmu_ids(
        &fleet.v2_good_stat_pmu_ids,
        fleet.first_pmu_id,
        final_pmu_id,
        wire_version,
    )?;
    validate_positive_finite(
        "fleet.phasors.voltage_magnitude",
        fleet.phasors.voltage_magnitude,
    )?;
    validate_non_negative(
        "fleet.phasors.voltage_variation",
        fleet.phasors.voltage_variation,
    )?;
    if fleet.phasors.voltage_variation > fleet.phasors.voltage_magnitude {
        return Err(ConfigError::new(
            "fleet.phasors.voltage_variation must not exceed voltage_magnitude",
        ));
    }
    validate_non_negative("fleet.phasors.voltage_class", fleet.phasors.voltage_class)?;
    validate_positive_finite("fleet.phasors.voltage_scale", fleet.phasors.voltage_scale)?;
    validate_fixed_magnitude_range(
        "fleet.phasors.voltage_magnitude",
        fleet.phasors.voltage_magnitude,
        fleet.phasors.voltage_variation,
        fleet.phasors.voltage_scale,
    )?;
    validate_positive_finite(
        "fleet.phasors.current_magnitude",
        fleet.phasors.current_magnitude,
    )?;
    validate_non_negative(
        "fleet.phasors.current_variation",
        fleet.phasors.current_variation,
    )?;
    if fleet.phasors.current_variation > fleet.phasors.current_magnitude {
        return Err(ConfigError::new(
            "fleet.phasors.current_variation must not exceed current_magnitude",
        ));
    }
    validate_positive_finite("fleet.phasors.current_scale", fleet.phasors.current_scale)?;
    validate_fixed_magnitude_range(
        "fleet.phasors.current_magnitude",
        fleet.phasors.current_magnitude,
        fleet.phasors.current_variation,
        fleet.phasors.current_scale,
    )?;
    validate_finite(
        "fleet.frequency_deviation_hz.nominal",
        fleet.frequency_deviation_hz.nominal,
    )?;
    validate_non_negative(
        "fleet.frequency_deviation_hz.variation",
        fleet.frequency_deviation_hz.variation,
    )?;
    validate_finite("fleet.rocof_hz_per_s.nominal", fleet.rocof_hz_per_s.nominal)?;
    validate_non_negative(
        "fleet.rocof_hz_per_s.variation",
        fleet.rocof_hz_per_s.variation,
    )?;
    if fleet.frequency_deviation_hz.nominal.abs() + fleet.frequency_deviation_hz.variation > 32.767
    {
        return Err(ConfigError::new(
            "fleet.frequency_deviation_hz must fit the fixed-point plus-or-minus 32.767 Hz range",
        ));
    }
    if fleet.rocof_hz_per_s.nominal.abs() + fleet.rocof_hz_per_s.variation > 327.67 {
        return Err(ConfigError::new(
            "fleet.rocof_hz_per_s must fit the fixed-point plus-or-minus 327.67 Hz/s range",
        ));
    }

    Ok(wire_version)
}

fn validate_v2_good_stat_pmu_ids(
    pmu_ids: &[u16],
    first_pmu_id: u16,
    final_pmu_id: u16,
    wire_version: WireVersion,
) -> Result<(), ConfigError> {
    if !pmu_ids.is_empty() && wire_version != WireVersion::V2 {
        return Err(ConfigError::new(
            "fleet.v2_good_stat_pmu_ids is supported only by V2 profiles",
        ));
    }
    let mut seen = Vec::with_capacity(pmu_ids.len());
    for pmu_id in pmu_ids {
        if !(*pmu_id >= first_pmu_id && *pmu_id <= final_pmu_id) {
            return Err(ConfigError::new(
                "fleet.v2_good_stat_pmu_ids must reference configured PMU_IDs",
            ));
        }
        if seen.contains(pmu_id) {
            return Err(ConfigError::new(
                "fleet.v2_good_stat_pmu_ids must not contain duplicates",
            ));
        }
        seen.push(*pmu_id);
    }
    Ok(())
}

fn final_identifier(first: u16, offset: u16, name: &str) -> Result<u16, ConfigError> {
    let final_value = first
        .checked_add(offset)
        .ok_or_else(|| ConfigError::new(format!("fleet {name} range overflows u16")))?;
    if first == 0 || final_value == u16::MAX {
        return Err(ConfigError::new(format!(
            "fleet {name}s must be in 1..=65534"
        )));
    }
    Ok(final_value)
}

fn validate_finite(name: &str, value: f32) -> Result<(), ConfigError> {
    if !value.is_finite() {
        return Err(ConfigError::new(format!("{name} must be finite")));
    }
    Ok(())
}

fn validate_positive_finite(name: &str, value: f32) -> Result<(), ConfigError> {
    validate_finite(name, value)?;
    if value <= 0.0 {
        return Err(ConfigError::new(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_non_negative(name: &str, value: f32) -> Result<(), ConfigError> {
    validate_finite(name, value)?;
    if value < 0.0 {
        return Err(ConfigError::new(format!("{name} must be non-negative")));
    }
    Ok(())
}

fn validate_fixed_magnitude_range(
    name: &str,
    nominal: f32,
    variation: f32,
    scale: f32,
) -> Result<(), ConfigError> {
    if (nominal + variation) / scale > f32::from(u16::MAX) {
        return Err(ConfigError::new(format!(
            "{name} and its variation exceed the fixed-point magnitude range for the configured scale"
        )));
    }
    Ok(())
}

fn encoded_name(value: &str, field: &str) -> Result<Vec<u8>, ConfigError> {
    if value.is_empty() || value.len() > u8::MAX as usize {
        return Err(ConfigError::new(format!(
            "{field} must contain 1 to 255 UTF-8 bytes"
        )));
    }
    let mut encoded = Vec::with_capacity(value.len() + 1);
    encoded.push(value.len() as u8);
    encoded.extend_from_slice(value.as_bytes());
    Ok(encoded)
}

fn standard_channel_names() -> Result<Vec<Vec<u8>>, ConfigError> {
    ["VL1", "VL2", "VL3", "IL1", "IL2", "IL3", "FREQ", "ROCOF"]
        .iter()
        .map(|name| encoded_name(name, "V3 channel name"))
        .collect()
}

fn standard_v2_channel_names() -> Result<[[u8; V2_NAME_FIELD_BYTES]; PHASOR_COUNT], ConfigError> {
    let mut names = [[b' '; V2_NAME_FIELD_BYTES]; PHASOR_COUNT];
    for (slot, name) in names
        .iter_mut()
        .zip(["VL1", "VL2", "VL3", "IL1", "IL2", "IL3"])
    {
        *slot = v2_fixed_ascii(name, "V2 channel name")?;
    }
    Ok(names)
}

fn v2_fixed_ascii(value: &str, field: &str) -> Result<[u8; V2_NAME_FIELD_BYTES], ConfigError> {
    if value.is_empty()
        || value.len() > V2_NAME_FIELD_BYTES
        || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err(ConfigError::new(format!(
            "{field} must contain 1 to {V2_NAME_FIELD_BYTES} printable ASCII bytes for C37.118 V2"
        )));
    }
    let mut encoded = [b' '; V2_NAME_FIELD_BYTES];
    encoded[..value.len()].copy_from_slice(value.as_bytes());
    Ok(encoded)
}

fn v2_phunits(phasors: &PhasorRecipe) -> Result<[u32; PHASOR_COUNT], ConfigError> {
    let voltage = v2_phunit(phasors.voltage_scale, false, "fleet.phasors.voltage_scale")?;
    let current = v2_phunit(phasors.current_scale, true, "fleet.phasors.current_scale")?;
    Ok([voltage, voltage, voltage, current, current, current])
}

fn v2_phunit(scale: f32, current: bool, field: &str) -> Result<u32, ConfigError> {
    let scaled = (scale * 100_000.0).round();
    if !scaled.is_finite() || !(1.0..=0x00ff_ffff_u32 as f32).contains(&scaled) {
        return Err(ConfigError::new(format!(
            "{field} cannot be represented by the C37.118 V2 PHUNIT scale"
        )));
    }
    Ok((u32::from(current) << 24) | scaled as u32)
}

fn deterministic_uuid(seed: u64, index: usize) -> [u8; 16] {
    let first = splitmix64(seed ^ index as u64);
    let second = splitmix64(first ^ 0x9e37_79b9_7f4a_7c15);
    let mut identifier = [0_u8; 16];
    identifier[..8].copy_from_slice(&first.to_be_bytes());
    identifier[8..].copy_from_slice(&second.to_be_bytes());
    identifier[6] = (identifier[6] & 0x0f) | 0x40;
    identifier[8] = (identifier[8] & 0x3f) | 0x80;
    identifier
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::{load_profile, parse_profile, WireVersion, MAX_LOGICAL_PMUS};

    fn profile(count: usize) -> String {
        format!(
            "seed: 7\nlimits:\n  max_logical_pmus: {MAX_LOGICAL_PMUS}\n  max_clients_per_endpoint: 2\n  max_command_frame_bytes: 4096\n  requested_socket_receive_buffer_bytes: 4096\n  requested_socket_send_buffer_bytes: 4096\nfleet:\n  count: {count}\n  bind_address: 127.0.0.1\n  first_listen_port: 4712\n  first_stream_id: 1001\n  first_pmu_id: 1001\n  pdc_name: WAMA\n  pmu_name_prefix: WAMA-PMU-\n  protocol_version: 3\n  data_rate_hz: 50\n  time_base: 1000000\n  nominal_frequency_hz: 50\n  phasors:\n    voltage_magnitude: 230000.0\n    voltage_variation: 400.0\n    voltage_class: 400000.0\n    voltage_scale: 10.0\n    current_magnitude: 500.0\n    current_variation: 1.5\n    current_scale: 1.0\n  frequency_deviation_hz:\n    nominal: 0.01\n    variation: 0.002\n  rocof_hz_per_s:\n    nominal: 0.0\n    variation: 0.001\n"
        )
    }

    #[test]
    fn expands_the_maximum_fleet_without_retaining_yaml_nodes() {
        let compiled = parse_profile(&profile(MAX_LOGICAL_PMUS)).expect("profile must compile");
        let last_index = MAX_LOGICAL_PMUS - 1;

        assert_eq!(compiled.endpoints.len(), MAX_LOGICAL_PMUS);
        assert_eq!(compiled.endpoints[0].address.port(), 4712);
        assert_eq!(
            compiled.endpoints[last_index].address.port(),
            4712 + last_index as u16
        );
        assert_eq!(compiled.endpoints[0].stream_id, 1001);
        assert_eq!(
            compiled.endpoints[last_index].stream_id,
            1001 + last_index as u16
        );
        assert_eq!(compiled.endpoints[0].pmu_id, 1001);
        assert_eq!(compiled.endpoints[0].pmu_name, b"\x0cWAMA-PMU-001");
        assert_eq!(compiled.endpoints[0].global_pmu_id[6] >> 4, 4);
        assert_eq!(compiled.endpoints[0].global_pmu_id[8] >> 6, 2);
    }

    #[test]
    fn rejects_a_fleet_above_the_declared_limit() {
        let error = parse_profile(&profile(MAX_LOGICAL_PMUS + 1)).expect_err("must reject count");

        assert!(error.to_string().contains("fleet.count"));
    }

    #[test]
    fn rejects_unknown_profile_keys() {
        let contents = format!("{}unknown: value\n", profile(1));
        let error = parse_profile(&contents).expect_err("must reject unknown key");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn compiles_compact_v2_metadata_without_retaining_v3_names() {
        let contents = profile(1).replace("protocol_version: 3", "protocol_version: 2");
        let endpoint = parse_profile(&contents)
            .expect("V2 profile must compile")
            .endpoints
            .remove(0);
        let v2 = endpoint.v2.expect("V2 metadata must be present");

        assert_eq!(endpoint.wire_version, WireVersion::V2);
        assert!(endpoint.pdc_name.is_empty());
        assert!(endpoint.pmu_name.is_empty());
        assert!(endpoint.channel_names.is_empty());
        assert_eq!(endpoint.global_pmu_id, [0; 16]);
        assert_eq!(&v2.station_name, b"WAMA-PMU-001    ");
        assert_eq!(v2.phunits[0], 1_000_000);
        assert_eq!(v2.phunits[3], 0x0101_86a0);
    }

    #[test]
    fn loads_the_shipped_five_pmu_v2_profile() {
        let compiled = load_profile(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/profiles/five-pmu-v2.yaml"
        ))
        .expect("five-PMU V2 profile must compile");

        assert_eq!(compiled.endpoints.len(), 5);
        assert!(
            compiled
                .endpoints
                .iter()
                .all(|endpoint| endpoint.wire_version == WireVersion::V2)
        );
        assert_eq!(compiled.endpoints[0].address.port(), 4712);
        assert_eq!(compiled.endpoints[4].address.port(), 4716);
        assert_eq!(compiled.endpoints[0].stream_id, 1001);
        assert_eq!(compiled.endpoints[4].stream_id, 1005);
        assert_eq!(compiled.endpoints[0].pmu_id, 1001);
        assert_eq!(compiled.endpoints[4].pmu_id, 1005);
        let good_stats: Vec<bool> = compiled
            .endpoints
            .iter()
            .map(|endpoint| endpoint.v2.as_ref().expect("V2 metadata must be present").good_stat)
            .collect();
        assert_eq!(good_stats, vec![true, true, false, false, false]);
    }

    #[test]
    fn loads_the_shipped_one_hundred_fifty_pmu_profiles() {
        for (profile_path, expected_wire_version) in [
            ("/profiles/one-hundred-fifty-pmu.yaml", WireVersion::V3),
            ("/profiles/one-hundred-fifty-pmu-v2.yaml", WireVersion::V2),
        ] {
            let compiled = load_profile(format!("{}{}", env!("CARGO_MANIFEST_DIR"), profile_path))
                .expect("150-PMU profile must compile");

            assert_eq!(compiled.endpoints.len(), 150);
            assert_eq!(compiled.endpoints[0].address.port(), 4712);
            assert_eq!(compiled.endpoints[149].address.port(), 4861);
            assert_eq!(compiled.endpoints[149].stream_id, 1150);
            assert!(
                compiled
                    .endpoints
                    .iter()
                    .all(|endpoint| endpoint.wire_version == expected_wire_version)
            );
        }
    }

    #[test]
    fn rejects_good_stat_pmu_ids_outside_the_v2_fleet() {
        let contents = profile(1).replace(
            "protocol_version: 3",
            "protocol_version: 2\n  v2_good_stat_pmu_ids:\n    - 1002",
        );

        let error = parse_profile(&contents).expect_err("must reject an unknown PMU_ID");

        assert!(error.to_string().contains("v2_good_stat_pmu_ids"));
    }

    #[test]
    fn rejects_a_fixed_point_magnitude_that_cannot_fit_the_v3_wire_format() {
        let contents = profile(1).replace("voltage_scale: 10.0", "voltage_scale: 1.0");

        let error = parse_profile(&contents).expect_err("must reject magnitude overflow");

        assert!(error.to_string().contains("fixed-point magnitude range"));
    }
}
