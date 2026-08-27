# C37.118 Simulator

`c37-118-simulator` is a separately managed C37.118 TCP source simulator.
Its Docker Compose project is deliberately started by an operator; it is not
included in the WAMA infrastructure Compose assembly. It attaches to the
existing external `wama-infra` network at `172.30.0.10` so the reviewed C37.118
C37.118 gateway adapters can reach its three-PMU legacy V2 fixture. It has no Kafka,
Common Format, Protobuf, Druid, SeaweedFS, Forgejo, or gateway dependency. It
is not a gateway and does not validate one.

The simulator runs one Rust event loop for 1 to 150 independent PMU listeners.
Each profile selects C37.118.2-2011 V2 or C37.118.2-2024 V3. V2 accepts
HDR/CFG-1/CFG-2/start/stop and emits fixed-point polar phasors, frequency
deviation, and ROCOF; V3 accepts capability/stream-configuration/start/stop.
The default three-PMU V2 profile listens on TCP/4712 through TCP/4714. Alternate
V2 and V3 profiles use the same internal port and ID conventions when selected
through `C37_118_SIMULATOR_PROFILE_SOURCE`.

V2 profiles omit `v2_good_stat_pmu_ids` by default and therefore emit a
conservative synchronization-uncertain STAT. The three-PMU V2 C37.118 gateway profile
is the explicit exception: PMU IDs `1001` and `1002` emit STAT `0`, so the
C37.118 gateway adapters normalize them as `quality.valid=true`; ID `1003` remains
conservative-invalid. This is controlled integration-fixture
behavior, not a claim about physical PMU clock quality.

Both wire subsets are derived from the authenticated local IEEE C37.118.2-2024
standard in [docs/](docs/). V2 uses the genuine nibble `0010` (`0x02` data, `0x12` HDR, `0x22`
CFG-1, `0x32` CFG-2, and `0x42` command), not Annex-A's V1 illustrations. An
approved external capture or decoder remains required before interoperability
claims.

Each PMU endpoint serves exactly two PDC connections. The release-gated
Capacity Contract is 10 PMUs, two PDCs per PMU, and 50 frames per second per
PMU. The 150-PMU profiles support a manually armed best-effort benchmark; they
do not define a release requirement.

The simulator exposes one HTTP Management Plane on the private routed network.
It provides protocol readiness, Prometheus-compatible metrics, operational
state, and confirmed Fault Scenario control. It relies on the IT-managed
network boundary and does not implement TLS or application authentication.

## Run

The project never starts itself or the WAMA infrastructure. Its external
network is normally created by the infrastructure repository, but an operator
can provision the compatible default network when running the simulator alone.
An existing network is inspected and left untouched; Compose treats it as
external, so `docker compose down` must not remove it. Start or rebuild the
default three-PMU V2 fixture manually:

```sh
network_name="${WAMA_INFRA_NETWORK:-wama-infra}"
if ! docker network inspect "$network_name" >/dev/null 2>&1; then
  docker network create --driver bridge --subnet 172.30.0.0/24 \
    --ip-range 172.30.0.128/25 "$network_name"
fi
docker compose up -d --build
```

Compose restarts the state-free simulator unless an operator stops it. Stop it
with `docker compose stop`; do not use `docker compose down` because this
repository does not own the external network.

Select a fleet profile with an absolute source path:

```sh
C37_118_SIMULATOR_PROFILE_SOURCE="$PWD/profiles/ten-pmu.yaml" \
  docker compose up -d --force-recreate
```

The default time-status mount is
[`runtime/time-sync-status`](runtime/time-sync-status). It contains `verified`.
Set `C37_118_TIME_SYNC_STATUS_SOURCE` to a readable status file when testing
time degradation. The status file must contain exactly `verified`, apart from
ASCII whitespace, to report verified Time Health. Any other content or a read
failure keeps streaming active with conservative time quality.

Select V2 explicitly with a V2 profile. A client on the Compose network uses
the matching probe flag:

```sh
C37_118_SIMULATOR_PROFILE_SOURCE="$PWD/profiles/ten-pmu-v2.yaml" \
  docker compose up -d --force-recreate

docker compose exec c37-118-simulator \
  c37-118-probe --wire-version 2 --host c37-118-simulator --first-port 4712 \
  --first-stream-id 1001 --count 10 --duration-seconds 1 --data-rate-hz 50
```

The default three-PMU Forgejo C37.118 gateway profile exposes three internal endpoints
at `172.30.0.10:4712` through
`172.30.0.10:4714`, with matching stream and PMU IDs `1001` through `1003`.
It configures good STAT for PMU IDs `1001` and `1002` only:

```sh
docker compose up -d --force-recreate

docker compose exec c37-118-simulator \
  c37-118-probe --wire-version 2 --host 172.30.0.10 --first-port 4712 \
  --first-stream-id 1001 --count 3 --duration-seconds 1 --data-rate-hz 50
```

No C37.118 or Management Plane port is mapped to the host. A test client on the
Compose network can connect to `c37-118-simulator:4712` through
`c37-118-simulator:4861`, and to the Management Plane on port `8080`.

## Management

The Management Plane uses HTTP/1.1 JSON requests on port `8080`. Compose uses
`/readyz` as its health check. Check readiness from inside the container:

```sh
docker compose exec c37-118-simulator \
  c37-118-simulator healthcheck --management-address 127.0.0.1:8080
```

The API exposes `GET /healthz`, `GET /readyz`, `GET /metrics`,
`GET /v1/catalog`, and `GET /v1/state`. The catalog is the immutable startup
scenario read model, including scenario kind, target compatibility, lifecycle,
frame-relative timing, duration, and signal excursions where configured. State
preserves runtime and scenario-controller facts while adding browser-ready PMU
identity, listener, wire, text-name, reporting, Time Health, PDC, and active
scenario facts. Scenario control uses `POST /v1/scenarios/prepare`,
`POST /v1/scenarios/confirm`, `POST /v1/scenarios/cancel`, and
`POST /v1/scenarios/clear`. Cancellation uses a canonical decimal-string
token and an optional actor label, and changes neither pending nor active
actions. The detailed request and response contract is in
[docs/c37-118-simulator.md](docs/c37-118-simulator.md).

The desktop PMU Control Console is served at `http://<host>:8081` by default
inside the trusted-network boundary. It uses same-origin `/api` proxying and
adds no TLS or application authentication. The console requires a nonempty,
unverified Console Operator Label and offers only target-local compatible Fault
Scenario actions. Each action follows `prepare -> explicit confirm -> pending
-> active`; the server provides `confirm_expires_in_ms` and canonical decimal-
string tokens, while numeric confirm tokens remain accepted for legacy
compatibility. Preparation cancellation is scoped to its token, including a
prepared clear. Sustained scenarios use the same prepare/confirm flow to clear;
there are no batch actions or simulator lifecycle controls.

The server logs JSON lines. Startup and scenario-control records include the
deployment label and runtime identity, which contains the image reference and
the SHA-256 values of the selected profile and scenario catalog.

## Tests

The regular test target validates V2/V3 profile compilation, frame encoding,
bounded command parsing, and standalone TCP exchanges:

```sh
docker build --target test --file Dockerfile .
```

Run the manually armed release baseline to validate both V2 and V3. It runs a
fixed five-minute active soak and a fixed 15-minute idle soak for each version,
so plan for more than 40 minutes. The test creates only labeled temporary
containers, images, and private networks, and writes a retained JSON artifact
outside the repository by default:

```sh
C37_118_RUN_BASELINE=1 scripts/test-baseline.sh
```

The baseline requires 10 PMUs with two PDCs per endpoint at 50 Hz, zero skipped
ticks, an observed one-PDC disconnect with a surviving peer stream, and memory
within the 64 MiB cap. It reports the artifact path when it finishes.

The 25-PMU, 100-PMU, and 150-PMU benchmarks are manually armed only.
They build an isolated simulator and probe pair on a private Docker network,
set a simulator cgroup memory cap, and remove only labelled test resources.
They do not use `wama-infra` or start the root infrastructure stack.

```sh
C37_118_RUN_25_PMU=1 scripts/test-25-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu.sh
C37_118_RUN_100_PMU=1 scripts/test-100-pmu-idle.sh
C37_118_RUN_150_PMU=1 scripts/test-150-pmu.sh
```

Set `C37_118_WIRE_VERSION=2` together with an existing manual-arm variable to
run the V2 form of a 25-PMU or 100-PMU benchmark. The 150-PMU benchmark is
best-effort and does not block the release baseline.

The 100-PMU and 150-PMU runners require cgroup memory accounting. They remain
informational manual benchmarks and do not block the 10-PMU release gate.

## Physical PDC Evidence

Built-in probes validate the implemented subset. A physical PDC remains the
independent evidence source for interoperability. Follow
[docs/how-to/certify-physical-pdc.md](docs/how-to/certify-physical-pdc.md) after
the PDC product and version are known. Do not claim V2 or V3 interoperability
until that manual procedure has produced evidence for both versions.