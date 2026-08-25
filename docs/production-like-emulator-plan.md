# Production-Like PMU Emulator Plan

## Purpose

This plan evolves the standalone C37.118 simulator into a production-like PMU
emulator for PDC integration work. It preserves the simulator's boundaries: it
remains a manually operated C37.118 TCP source, not a gateway, WAMA service,
Kafka producer, or physical measurement device.

The accepted vocabulary and durable decisions live in
[`CONTEXT.md`](../CONTEXT.md) and [`docs/adr/`](adr/). This document is an
implementation sequence, not a change to those decisions.

## Implementation Status

The immutable startup contract, bounded two-PDC fan-out, deterministic scenario
catalogs, Time Health behavior, Management Plane, protocol readiness, JSON logs,
metrics, 10-PMU release-baseline script, and 150-PMU best-effort profiles are
implemented. The physical-PDC procedure is documented but remains manually run
and non-blocking until an approved PDC product and version are available.

The delivery order below remains the implementation record for these decisions.

## Scope

- Support the documented C37.118 V2 and V3 TCP subsets only.
- Rely on the IT-managed private routed network for network security; do not
  add TLS, authentication, or simulator-side network admission controls.
- Use Docker Compose `restart: unless-stopped` and protocol-level readiness.
- Release-gate 10 PMUs, two PDCs per PMU, and 50 frames per second per PMU.
  This is 1,000 frames per second. A 150-PMU run is best-effort benchmark
  evidence, not a release criterion.
- Add a private-network JSON/HTTP management plane for read-only operational
  state, metrics, and confirmed fault-scenario control.
- Keep physical-PDC V2/V3 certification manual and non-blocking until the PDC
  product and version are known.

## Delivery Order

### 1. Establish Immutable Startup Inputs

Add a startup contract that loads a PMU profile and a separately versioned
scenario catalog exactly once. The process must record the profile and catalog
SHA-256 values, image tag or ID, and configured deployment label as its runtime
identity.

Likely changes:

- Update `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/config.rs`, and
  `src/main.rs`.
- Add `src/identity.rs` and `src/scenario.rs`.
- Add a versioned catalog directory, for example `scenarios/`, containing the
  baseline catalog.
- Update shipped profiles to allow two PDCs per endpoint and add the explicit
  scenario-catalog and deployment-label startup inputs.

The catalog must reject unknown fields and invalid targets, use only
frame-relative offsets and durations, and contain the baseline scenarios:
normal, degraded time, missing frames, PDC disconnect, phasor/frequency/ROCOF
excursion, and recovery. It must reject target conflicts before an action can
be scheduled.

Acceptance checks:

- Invalid profiles and catalogs fail before any listener is bound.
- Profile/catalog hashes are stable for unchanged files and differ after a
  content change.
- Existing V2 and V3 profile validation remains covered, and the ten-PMU
  profiles compile with two PDC slots per endpoint.

### 2. Refactor PDC Connections for Bounded Fan-out

Replace each endpoint's single connection with exactly two bounded PDC slots.
The server remains the sole owner of socket, scenario, and scheduling state.
Each slot needs an internal connection identifier so a newly connected PDC does
not inherit a scenario intended for a disconnected predecessor.

Likely changes:

- Update `src/server.rs`.
- Extend `src/bin/c37-118-probe.rs` and
  `src/bin/c37_118_probe_v2.rs` to drive two concurrent PDCs and assert their
  separate rates.

Implement the following PDC policy:

- A PDC has 15 seconds to send its first valid command.
- A stopped, non-streaming PDC can remain idle for five minutes.
- A PDC unable to drain a periodic frame by the next reporting boundary is
  disconnected without disrupting its peer.
- A malformed or unsupported V3 command receives the implemented V3 error
  response, then the session closes. The V2 session closes without a V3-style
  response.

Acceptance checks:

- V2 and V3 loopback tests prove two PDCs can stream from one endpoint.
- Handshake and idle deadlines free only the expired PDC slot.
- A slow or malformed PDC does not interrupt the peer or another endpoint.

### 3. Add Deterministic Scenario and Time Behavior

Introduce a wire-independent frame plan evaluated only at reporting
boundaries. It supplies timestamp, time quality, omission, signal overrides,
and per-PDC disconnect actions before the V2 or V3 encoder produces a frame.

Likely changes:

- Update `src/server.rs`, `src/wire_v2.rs`, and `src/wire_v3.rs`.
- Add `src/time_health.rs`.
- Extend `src/scenario.rs` with the catalog state machine.

The scenario controller provides prepare, confirm, clear, advance-boundary,
and snapshot operations. A prepare request creates a 60-second token. Confirm
or clear requests apply at the next reporting boundary. Exactly one scenario
can be active for an endpoint or PDC target; conflicting actions are rejected.
Transient scenarios recover automatically, while sustained scenarios require a
confirmed clear.

Host-clock synchronization is observable rather than assumed. A clock state
that cannot be verified, or a backward regression greater than one reporting
interval, makes time health degraded. Streaming continues with conservative
wire-quality fields and strictly monotonic timestamps. Normal time health
returns automatically at a later verified boundary.

Acceptance checks:

- A fake clock proves boundary-aligned degradation, monotonic timestamps, and
  automatic recovery.
- V2 and V3 byte-level tests prove conservative quality and signal overrides.
- Scenario tests cover token expiry, conflict rejection, confirmed clearing,
  omissions, disconnects, and transient recovery.

### 4. Expose the Management and Observability Plane

Add one bounded JSON/HTTP listener on the private routed network. It owns no
PMU configuration and cannot change PMU identity, wire version, profile,
catalog, or capacity after startup.

Likely changes:

- Add `src/management.rs` and `src/observability.rs`.
- Update `src/main.rs`, `src/server.rs`, `compose.yaml`, and `Dockerfile`.

The management plane exposes conventional endpoints for health, readiness,
metrics, operational state, scenario preparation, confirmation, and confirmed
clearing. Recoverable management errors return structured HTTP errors while
PDC streaming continues. An internal scenario-state inconsistency terminates
the process so Docker can restore a clean state.

Readiness requires all configured PMU listeners and the management listener to
be bound, plus a successful selected-version protocol self-check. It does not
depend on external PDC connections. Degraded time remains ready. Replace the
current liveness-only Compose healthcheck with a real local readiness request,
and change Compose to `restart: unless-stopped` without publishing host ports.

Emit JSON lines and Prometheus-compatible metrics for:

- process, readiness, time health, and runtime identity;
- active, rejected, slow, and disconnected PDCs;
- sent and skipped frames;
- scenario state and control actions; and
- per-endpoint connection counts.

Scenario action logs include timestamp, deployment label, optional unverified
actor label, scenario, target, confirmation result, and resulting state.

Acceptance checks:

- HTTP tests cover success and bounded error responses for every endpoint.
- Readiness fails before startup is complete and succeeds after the local
  protocol self-check.
- Metrics and logs expose runtime identity and the selected scenario state.
- Degraded-time streaming remains ready.

### 5. Make the Baseline Gate Executable

Replace the current scale-only evidence with a small mandatory acceptance
harness and retain large-fleet work as manually armed benchmark evidence.

Likely changes:

- Update `scripts/run-scale.sh` and its wrappers.
- Add a baseline runner and an isolated management-plane test helper under
  `scripts/` or `src/bin/`.
- Update ten-PMU V2/V3 profiles and add 150-PMU V2/V3 benchmark profiles.
- Expand the Compose listener exposure range for the optional 150-PMU
  benchmark.

Run the V2 and V3 baseline separately. Each run starts 10 endpoints with two
PDCs each at 50 Hz, executes five active minutes and fifteen idle minutes, and
uses a confirmed scenario to disconnect one PDC. The harness proves that the
peer PDC on that endpoint continues at 50 Hz.

The release gate permits expected scenario-induced disconnects but requires:

- zero skipped ticks;
- no unexpected protocol, rate, connection-isolation, memory, or timestamp
  monotonicity failures; and
- a JSON artifact containing runtime identity, host/container resources,
  aggregate metrics, scenario evidence, and explicit pass/fail reasons.

The 150-PMU V2/V3 runs remain manually armed and informational. They must not
block the baseline release gate on constrained hardware.

### 6. Document Operation and Physical-PDC Certification

Update the operator documentation after the preceding behavior exists.

Likely changes:

- Update `README.md` and `docs/c37-118-simulator.md`.
- Add a manual physical-PDC certification procedure once the PDC product and
  version are known.

Document the startup inputs, catalog schema, management endpoints, metrics,
time-health semantics, PDC deadlines, restart behavior, baseline command, and
JSON soak artifact. Preserve the five-PMU V2 onboarding fixture as the Compose
default, but make the ten-PMU acceptance profile and catalog invocation
explicit.

Physical-PDC certification later validates V2 and V3 configuration and data
decoding, documented command exchanges, fault-scenario behavior, and reconnect
recovery. Until that run exists, documentation must describe built-in probes as
implementation validation, not external interoperability certification.

## Explicit Non-Goals

- No WAMA infrastructure, Forgejo deployment, Kafka, gateway, or data-plane
  changes.
- No TLS, application authentication, client allowlists, or public management
  endpoint.
- No UDP, multicast, CFG-3, remote configuration, dynamic PMU topology,
  old-data, discrete-event data, or raw-frame retention in this increment.
- No claim of physical-PMU timing accuracy or physical-PDC interoperability
  before corresponding evidence exists.