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

Each endpoint owns exactly two bounded PDC slots. A third PDC is rejected. A
new PDC has 15 seconds to send a valid command, a non-streaming PDC expires
after five minutes, and a slow PDC is disconnected without interrupting its
peer. The process uses Compose `restart: unless-stopped`; it is not a
high-availability PMU.

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
maximum of 150 listeners in one single-threaded Rust event loop. The full fleet
uses ports `4712` through `4861` and stream/PMU identifiers `1001` through
`1150`.
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
  max_logical_pmus: 150
  max_clients_per_endpoint: 2
  max_command_frame_bytes: 4096
  requested_socket_receive_buffer_bytes: 4096
  requested_socket_send_buffer_bytes: 4096
fleet:
  count: 150
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
`one-hundred-pmu-v2.yaml`, plus `one-hundred-fifty-pmu-v2.yaml`; the existing
names without `-v2` remain V3.

The default Forgejo onboarding demonstration profile is `five-pmu-v2.yaml`.
This repository's Compose project assigns the manually started simulator its
stable `172.30.0.10` address on the external `wama-infra` network; the normal
infrastructure stack remains the usual creator of that network. For a
standalone run, the operator can provision the compatible default network
first. The inspect-then-create sequence is idempotent and leaves an existing
network untouched. Compose must not remove this external network, so do not use
`docker compose down` for cleanup:

```sh
network_name="${WAMA_INFRA_NETWORK:-wama-infra}"
if ! docker network inspect "$network_name" >/dev/null 2>&1; then
  docker network create --driver bridge --subnet 172.30.0.0/24 \
    --ip-range 172.30.0.128/25 "$network_name"
fi
```

The profile configures listeners `4712` through `4716`, with matching stream
and PMU IDs `1001` through `1005`. Its
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

## Management Plane

The Management Plane is one HTTP/1.1 listener on port `8080` of the private
routed network. It is not a per-PMU protocol port and does not publish a host
mapping by default. It uses the IT-managed trusted network boundary; it has no
TLS or application authentication.

The listener accepts one bounded request per connection and closes the
connection after its response. It rejects transfer encoding, ambiguous content
length, HTTP/1.1 requests without exactly one valid `Host` header, unknown JSON
fields, and request bodies larger than 8 KiB. Responses have a 64 KiB body
limit. Errors use this JSON envelope:

```json
{"error":{"code":"machine_readable_code","message":"human-readable message"}}
```

| Method and path | Response | Description |
| --- | --- | --- |
| `GET /healthz` | `200` JSON | Process liveness. |
| `GET /readyz` | `200` or `503` JSON | All C37.118 and Management Plane listeners are bound and the selected wire version has passed the local frame self-check. PDC connections and Time Health do not determine readiness. |
| `GET /metrics` | `200` text | Prometheus-compatible process, readiness, Time Health, PDC, scenario, and low-cardinality per-stream metrics. |
| `GET /v1/state` | `200` JSON | Runtime identity, Time Health, counters, PDC connection IDs, and scenario-controller state. |
| `POST /v1/scenarios/prepare` | `202` JSON | Prepares a named Fault Scenario for an endpoint or PDC target. |
| `POST /v1/scenarios/confirm` | `202` JSON | Consumes a 60-second preparation token and queues the action for the next reporting boundary. |
| `POST /v1/scenarios/cancel` | `202` JSON | Cancels a prepared action by canonical decimal-string token, with an optional actor label; pending and active actions are unchanged. |
| `POST /v1/scenarios/clear` | `202` JSON | Prepares a confirmed clear for an active sustained scenario. |

`POST /v1/scenarios/prepare` accepts the following shape. Omit
`connection_id` for an endpoint target. `disconnect-pdc` requires a PDC target;
the other shipped scenarios require an endpoint target.

```json
{
  "target":{"stream_id":1001,"connection_id":42},
  "scenario_name":"disconnect-pdc",
  "actor_label":"operator-label"
}
```

The desktop PMU Control Console is available at `http://<host>:8081` by
default. It is intended for the trusted-network boundary and adds no TLS or
application authentication. Its same-origin `/api` requests are proxied to
the Management Plane. The console is desktop-only and controls exactly one
Scenario Target at a time: a compatible endpoint-local Fault Scenario action
or a target-local `disconnect-pdc` action. It has no batch controls and does
not control startup profiles, PMU identity, capacity, wire version, or
simulator lifecycle.

The Console Operator Label is required and nonempty. It is unverified
attribution, not authentication, and is retained only in page memory. The
console presents the selected Console Scenario Detail, prepares the action,
and requires an explicit second confirmation. The server response supplies the
preparation token and `confirm_expires_in_ms`; the console uses that server
value when showing the confirmation window. The observable sequence is
`prepare -> explicit confirm -> pending -> active`, with activation applied at
the next reporting boundary.

Tokens are canonical positive decimal strings in JSON, including the token in
the preparation response and the `token` value sent to confirm. The confirm
operation also accepts the legacy numeric token form for compatibility. A
preparation cancellation is scoped to its token and releases only that
prepared action; it does not cancel an active Fault Scenario. A prepared clear
uses the same token-scoped cancellation and confirmation rules. `clear` is
available only for an active sustained scenario and starts the confirmed clear
flow; it is not an immediate clear. A transient scenario clears itself after
its frame-relative duration. One target can have only one prepared, pending, or
active scenario.

Startup logs, `/readyz`, `/v1/state`, and `c37_118_simulator_runtime_info`
identify the deployment label, image reference, profile SHA-256, and scenario
catalog SHA-256. Scenario-control actions are emitted as JSON log records.

The Compose time-status mount defaults to `runtime/time-sync-status`. A file
whose trimmed ASCII contents are exactly `verified` reports verified Time
Health. Any other value or an unreadable file produces conservative degraded
time quality without making `/readyz` fail. A host-clock regression greater
than one reporting interval also degrades Time Health. The scheduler retains
monotonic C37.118 timestamps and recovers Time Health automatically at a later
verified reporting boundary.

## Memory and backpressure

The application-memory shape is bounded:

```text
base process + compiled endpoint descriptors + one data buffer per endpoint
  + active connections * one bounded command buffer
```

Each connection has a maximum 4 KiB command buffer. Each endpoint has one
current periodic-data buffer, two PDC slots, and no application-level transmit
history. A client that cannot drain its pending frame by the next reporting tick
is closed rather than causing an unbounded backlog. The simulator has no worker
per PMU, client, or sample.

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

The release baseline is manually armed with `C37_118_RUN_BASELINE=1
scripts/test-baseline.sh`. It validates V2 and V3 separately with 10 PMUs, two
PDCs per PMU, and 50 Hz. Each version runs a fixed five-minute active phase and
a fixed 15-minute idle phase. The active phase requires zero skipped ticks,
proves a selected PDC disconnect leaves its peer at 50 Hz, and retains a JSON
artifact with profile/catalog hashes, image identity, resource observations,
state snapshots, and probe outcomes.

The 25-PMU, 100-PMU, and 150-PMU benchmarks are manually armed only:

```sh
C37_118_RUN_25_PMU=1 scripts/test-25-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu-idle.sh
C37_118_RUN_150_PMU=1 scripts/test-150-pmu.sh
```

Set `C37_118_WIRE_VERSION=2` only when deliberately running a 25-PMU or
100-PMU V2 benchmark. The 150-PMU benchmark is best-effort and does not block
the release baseline.

They use a labeled private Docker network rather than `wama-infra`, enforce a
single-run lock, require cgroup memory accounting, cap simulator memory, and
remove only their labeled resources. The 100-PMU and 150-PMU runs remain
best-effort informational benchmarks and do not block the 10-PMU release gate.

Use [the physical-PDC certification guide](how-to/certify-physical-pdc.md) when
an approved PDC is available. Built-in probes are implementation validation;
they are not independent interoperability evidence.