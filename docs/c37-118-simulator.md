(ref_c37_118_simulator)=

```{meta}
:description: Reference for the manually operated C37.118.2 version 2 and version 3 TCP source simulator.
```

# C37.118.2-2024 V2/V3 simulator

## Scope and authority

`c37-118-simulator` is a manually operated C37.118 TCP source simulator. Its
separate Compose project uses the five-PMU V2 profile for the onboarding
demonstration and attaches to the existing external `wama-infra` network. It is
not a gateway and has no Kafka, Common Format, Protobuf, Druid, SeaweedFS,
Forgejo, or data-plane dependency.

The normative wire reference is the approved local
[`IEEE Std C37.118.2-2024.PDF`](IEEE%20Std%20C37.118.2%E2%84%A2-2024.PDF), SHA-256
`ee776f9b78ccc95980d05e04e570f6dbbdad3993ae7412dc81ed772d5cbd7546`.
This document summarizes the implemented V2 and V3 subsets; on any conflict,
the PDF wins.

Each profile explicitly selects C37.118.2-2011 V2 or C37.118.2-2024 V3 through
`fleet.protocol_version`. An endpoint accepts only commands and emits only
frames for its selected version. It does not negotiate versions, accept V1, or
bridge V2/V3 traffic.

## Wire subsets

All fields are encoded in network byte order and use CRC-CCITT with seed
`0xFFFF` and no final XOR.

### V2

V2 uses the Annex-A common envelope:

```text
SYNC | FRAMESIZE | IDCODE | SOC | FRACSEC_AND_MSG_TQ | payload | CHK
```

`IDCODE` identifies the endpoint stream. `FRACSEC_AND_MSG_TQ` is four bytes:
the high byte holds message time quality and the low 24 bits hold the
`TIME_BASE` fraction. The simulator reports conservative unknown message time
quality and PMU time-quality status by default instead of claiming unavailable
clock accuracy. The five-PMU V2 onboarding profile is the controlled exception:
it sets STAT `0` for PMU IDs `1001` and `1002`, allowing their adapters to emit
`quality.valid=true`; PMU IDs `1003` through `1005` retain the conservative
status. The V2 message-time-quality byte remains unknown for every endpoint.

| Purpose | SYNC byte | Command code when requested |
| --- | ---: | ---: |
| Periodic data | `0x02` | N/A |
| Header | `0x12` | `0x0003` |
| CFG-1 | `0x22` | `0x0004` |
| CFG-2 | `0x32` | `0x0005` |
| Command | `0x42` | `0x0001` stop, `0x0002` start |

The `...1` examples printed in some Annex-A tables are V1 illustrations. The
implemented V2 frames always use nibble `0010` and therefore the bytes above.

The V2 exchange is:

```text
HDR command -> header frame
CFG-1 command -> CFG-1 frame
CFG-2 command -> CFG-2 frame
start command -> periodic data frames
stop command -> periodic data stops
```

The header payload is a nonempty printable ASCII station description. CFG-1 and
CFG-2 contain one PMU, six fixed-point polar phasors, no analog or digital
channels, fixed 16-byte ASCII station/channel fields, V2 PHUNIT scaling, FNOM,
and a 50 Hz DATA_RATE. The V2 periodic frame is 46 bytes: STAT, six phasors,
FREQ, DFREQ, and CRC after the 14-byte envelope.

### V3

Every V3 frame starts with the V3 common envelope:

```text
SYNC | FRAMESIZE | STREAM_ID | SOC | LEAP_BYTE | FRACSEC | payload | CHK
```

`STREAM_ID` identifies the endpoint output stream and must be in `1..=65534`.
The simulator verifies it before accepting a command. `SOC` is Unix seconds;
`FRACSEC` is a three-byte counter based on `TIME_BASE`. The service emits zero
leap bits and reports conservative unknown time quality rather than asserting
an unavailable clock-accuracy guarantee.

The implementation supports these V3 frames:

| Purpose | SYNC byte | Command code when requested |
| --- | ---: | ---: |
| Periodic data | `0x83` | N/A |
| Capability | `0xA3` | `0x0040` |
| Stream configuration | `0xB3` | `0x0060` |
| Command | `0xC3` | `0x0010` stop, `0x0020` start |
| Error response | `0xF3` | N/A |

The minimal client exchange is:

```text
capability command -> capability frame
stream-configuration command -> stream-configuration frame
start command -> periodic data frames
stop command -> periodic data stops
```

Unsupported, malformed, wrong-version, or wrong-stream V3 commands receive the
implemented V3 error response. Invalid V2 requests are counted and the
connection is closed because the V3 error-response framing is not a V2 feature.
Remote rename/configure-stream commands, extended commands, old-data requests,
discrete-event data, V1, CFG-3, UDP, TLS, multicast, PDC aggregation, and
raw-frame retention are excluded.

## Fixed PMU profile

One listener models one independently addressable PMU. The simulator has a hard
maximum of 100 listeners in one single-threaded Rust event loop. The full fleet uses
ports `4712` through `4811` and stream/PMU identifiers `1001` through `1100`.
No service port is mapped to the host by default.

Each V3 configuration frame has one PMU and declares:

- six fixed-point polar phasors: voltage phases A/B/C and current phases A/B/C;
- one fixed-point frequency-deviation signal in millihertz;
- one fixed-point ROCOF signal in hundredths of hertz per second;
- no analog or digital signals and no data-attribute words;
- indexed UTF-8 PDC, PMU, and channel names;
- a deterministic RFC 4122 V4-shaped PMU identifier;
- a fixed 50 Hz reporting rate and a 1,000,000 tick `TIME_BASE`.

V2 profiles use the same six analytical signals and rate but compile fixed
16-byte printable ASCII station/channel fields and PHUNIT values. V3 profiles
retain indexed UTF-8 names, global PMU IDs, and V3 scaling metadata.

The configuration compiler rejects a profile above 100 PMUs, invalid
`STREAM_ID` or `PMU_ID`, protocol values other than 2 or 3, an unsupported
rate, a time base not divisible by the rate, invalid V3 UTF-8 names, invalid V2
fixed-width ASCII names or PHUNIT scales, non-finite signal values, and values
that cannot fit the fixed-point wire range.

The shipping YAML shape is:

```yaml
seed: 20260821
limits:
  max_logical_pmus: 100
  max_clients_per_endpoint: 1
  max_command_frame_bytes: 4096
  requested_socket_receive_buffer_bytes: 4096
  requested_socket_send_buffer_bytes: 4096
fleet:
  count: 100
  bind_address: 0.0.0.0
  first_listen_port: 4712
  first_stream_id: 1001
  first_pmu_id: 1001
  pdc_name: WAMA
  pmu_name_prefix: WAMA-PMU-
  protocol_version: 3
  data_rate_hz: 50
  time_base: 1000000
  nominal_frequency_hz: 50
  phasors:
    voltage_magnitude: 230000.0
    voltage_variation: 400.0
    voltage_class: 400000.0
    voltage_scale: 10.0
    current_magnitude: 500.0
    current_variation: 1.5
    current_scale: 1.0
  frequency_deviation_hz:
    nominal: 0.01
    variation: 0.002
  rocof_hz_per_s:
    nominal: 0.0
    variation: 0.001
```

Use `protocol_version: 2` for V2. The supplied profiles are
`one-pmu-v2.yaml`, `five-pmu-v2.yaml`, `ten-pmu-v2.yaml`, `twenty-five-pmu-v2.yaml`, and
`one-hundred-pmu-v2.yaml`; the existing names without `-v2` remain V3.

The default Forgejo onboarding demonstration profile is `five-pmu-v2.yaml`.
This repository's Compose project assigns the manually started simulator its
stable `172.30.0.10` address on the external `wama-infra` network; the profile
configures listeners `4712` through `4716`, with matching stream and PMU IDs
`1001` through `1005`. Its
`v2_good_stat_pmu_ids` lists only `1001` and `1002`:

```sh
docker network inspect "${WAMA_INFRA_NETWORK:-wama-infra}" >/dev/null
docker compose up -d --force-recreate

docker compose exec c37-118-simulator \
  c37-118-probe --wire-version 2 --host 172.30.0.10 --first-port 4712 \
  --first-stream-id 1001 --count 5 --duration-seconds 1 --data-rate-hz 50
```

The C37.118 listeners are internal to `wama-infra`; the stable address exists
for reviewed source adapters, not host or LAN clients.

Values are derived analytically from the seed, endpoint index, channel index,
and sample index. The service retains no sample history or random-event queue.
Its UTC measurement timestamps start at the next valid frame boundary and
advance in exact `TIME_BASE / data_rate_hz` steps.

## Memory and backpressure

The application-memory shape is bounded:

```text
base process + compiled endpoint descriptors + one data buffer per endpoint
  + active connections * one bounded command buffer
```

Each connection has a maximum 4 KiB command buffer. Each endpoint has one
current periodic-data buffer and no application-level transmit history. A client
that cannot drain its pending frame by the next reporting tick is closed rather
than causing an unbounded backlog. The simulator has no worker per PMU, client,
or sample.

## Verification

The regular image test verifies both envelopes and checksums, genuine V2 compared to
V1 SYNC handling, V2 HDR/CFG-1/CFG-2/data behavior, V3 command behavior,
profile rejection, fixed-point bounds, fragmented/concatenated command handling,
and standalone TCP exchanges:

```sh
docker build --target test --file Dockerfile .
```

`c37-118-probe` is a separate decoder selected with `--wire-version 2|3`
(default `3`). It independently traverses the selected envelope, CRC,
configuration, response identity, timestamp alignment, and periodic-frame
shape; it does not call a gateway or data-plane service.

The normal smoke stages are one PMU and ten PMUs for each wire version. The
ten-PMU stage retains 50 Hz per endpoint and validates every connection with the
standalone probe.

The 25-PMU five-minute and 100-PMU 15-minute tests are manually armed only:

```sh
C37_118_RUN_25_PMU=1 scripts/test-25-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu-idle.sh
```

Set `C37_118_WIRE_VERSION=2` only when deliberately running one of these
already-armed manual V2 soaks. The default is V3. Neither V2 large-fleet test is
part of default startup, lifecycle validation, or CI.

They use a labeled private Docker network rather than `wama-infra`, enforce a
single-run lock, require cgroup memory accounting, cap simulator memory, and
remove only their labeled resources. The active 100-PMU test requires 100
clients at 50 Hz for 15 minutes; the idle test separately checks listener-only
memory. Neither test is part of default startup, lifecycle validation, or CI.

An approved external V2 or V3 capture or decoder is still required before making
an interoperability or conformance claim.