# C37.118 Simulator

This context defines the vocabulary of a standalone C37.118 source simulator used to exercise PDC and integration behavior.

## Language

**Production-like PMU Emulator**:
A C37.118 source simulator intended to reproduce the operational and interoperability behavior that PDC integrations expect from a PMU, without becoming a production measurement device, gateway, or WAMA infrastructure component.
_Avoid_: production PMU, gateway, WAMA service

**Compatibility Contract**:
The documented C37.118 V2 and V3 subset that the emulator supports.
_Avoid_: full standard support

**Compatibility Evidence**:
Evidence that the emulator meets its Compatibility Contract. Built-in probes provide the initial evidence; an independently sourced decoder or capture is required before making an external interoperability claim.
_Avoid_: self-certified interoperability

**Private Routed Network**:
An IT-managed secure, non-public network where approved PDC hosts route to emulator listeners without application-layer TLS or client authentication.
_Avoid_: public endpoint, Internet-facing service

**Trusted Network Boundary**:
The IT-owned security boundary of the Private Routed Network on which the emulator relies instead of implementing TLS, client authentication, or simulator-side network access controls. It includes every host interface on which the PMU Control Console default binding is reachable.
_Avoid_: application-layer security boundary

**PDC Fan-out**:
The configured bounded number of simultaneous PDC connections that a PMU endpoint serves.
_Avoid_: single-client endpoint, unlimited clients

**Automatic Recovery**:
The emulator's state-free operational mode in which Docker restarts the service unless an operator stops it, and PDCs can reconnect to a ready listener.
_Avoid_: high availability, manual recovery

**Readiness Contract**:
The condition in which every configured listener is bound, the Management Plane responds, and an internal selected-version protocol self-check has completed. PDC connections are not a readiness prerequisite.
_Avoid_: container liveness, consumer-dependent readiness

**Capacity Contract**:
The release-gated workload of 10 PMU endpoints, each serving two PDC connections and producing 50 data frames per second, for 1,000 frames per second in total. A 150-PMU workload is a best-effort benchmark, not a release gate.
_Avoid_: unlimited scale, implied 150-PMU guarantee

**Baseline Soak**:
The Capacity Contract's required validation: five minutes with active PDCs and fifteen minutes with listeners idle.
_Avoid_: hardware-dependent large-fleet release gate

**Fault Scenario**:
A deterministic, configurable sequence of PMU conditions, missing reporting intervals, PDC disconnects, and recovery used to exercise PDC behavior.
_Avoid_: historical replay

**Baseline Scenario Catalog**:
The initial named Fault Scenarios: normal operation, degraded time, missing frames, PDC disconnection, phasor/frequency/ROCOF excursion, and recovery.
_Avoid_: unbounded scenario surface

**Scenario Catalog**:
A separately versioned YAML definition of Fault Scenarios referenced by startup profiles, keeping PMU identity and wire configuration independent from test behavior.
_Avoid_: scenario definitions duplicated in profiles, code-only scenarios

**Startup Catalog Binding**:
An explicit scenario catalog path supplied beside the startup profile path and loaded and validated once when the emulator starts.
_Avoid_: runtime catalog discovery, hot-reloaded catalog

**Frame-Relative Timing**:
The Fault Scenario timing language expressed in reporting-frame offsets and durations, rather than host wall-clock durations.
_Avoid_: wall-clock scenario timing

**Runtime Scenario Control**:
A management interface that confirms activation of named Fault Scenarios and applies them at the next reporting boundary while the emulator is running.
_Avoid_: restart-only scenario activation, public management API

**Scenario Activation Confirmation**:
A two-step prepare and confirm operation, using a 60-second activation token, that makes Runtime Scenario Control intentional and auditable.
_Avoid_: one-step live activation

**Preparation Cancellation**:
An explicit operation that releases a prepared but unconfirmed Fault Scenario action.
_Avoid_: cancelling an active Fault Scenario

**Management Plane**:
One shared HTTP endpoint on the Private Routed Network through which operators inspect emulator state and control Fault Scenarios. It does not alter PMU identity, wire configuration, or capacity while the emulator is running.
_Avoid_: per-PMU management endpoint, live profile editor

**PMU Control Console**:
A desktop browser application on the Private Routed Network that presents PMU and PDC operational state and invokes confirmed Fault Scenario controls through the Management Plane. It has no narrow-viewport workflow and does not own or change startup profiles, PMU identity, capacity, wire version, or simulator lifecycle.
_Avoid_: PMU configuration editor, gateway dashboard

**Console PMU Table**:
The dense, searchable PMU Control Console view that presents each PMU's immutable endpoint facts, Time Health, active Fault Scenario, and expandable PDC connections. It filters by stream ID, wire version, PDC occupancy, active Fault Scenario, and Time Health.
_Avoid_: paginated dashboard, per-PMU configuration page

**Console Scenario Catalog**:
The read-only representation of the immutable Scenario Catalog that the PMU Control Console obtains through a dedicated Management Plane endpoint. It contains scenario name, kind, target compatibility, lifecycle, frame duration, and signal-excursion values.
_Avoid_: hardcoded baseline-scenario buttons

**Console Scenario Detail**:
The read-only presentation of a catalog scenario's target compatibility, lifecycle, frame duration, and signal-excursion values during action confirmation.
_Avoid_: opaque scenario button, implicit scenario action

**Console Single-Target Action**:
A PMU Control Console Fault Scenario command for exactly one Scenario Target. Batch actions across PMUs or PDCs are outside the console's initial authority.
_Avoid_: bulk scenario action

**Console Action Menu**:
The Console Scenario Catalog entries that are compatible with a selected Scenario Target and change that target. It excludes `normal` and `recovery`; clear is available only for an active sustained scenario.
_Avoid_: no-op scenario button, indiscriminate clear control

**Console Stale State**:
The last successfully retrieved Console PMU Table state, shown with all controls disabled while the PMU Control Console cannot reach the Management Plane.
_Avoid_: blank outage screen, actionable stale state

**Console Independent Availability**:
The PMU Control Console remains reachable while the Management Plane is unavailable, so it can present Console Stale State rather than failing to load.
_Avoid_: simulator-dependent console startup

**Management API**:
The JSON-over-HTTP interface of the Management Plane, with conventional health, readiness, metrics, scenario-prepare, and scenario-confirm paths.
_Avoid_: opaque control protocol

**Control-Plane Error Isolation**:
A recoverable Management Plane error returns a structured HTTP error without interrupting PMU streams. An error that makes scenario state inconsistent fails the emulator process for supervised recovery.
_Avoid_: management fault silently changing measurement behavior

**Scenario Target**:
An explicit PMU endpoint or PDC connection selected for a Fault Scenario.
_Avoid_: fleet-wide fault by default

**Scenario Lifecycle**:
The explicitly selected transient or sustained behavior of a Fault Scenario. A transient scenario returns its target to baseline automatically; a sustained scenario remains active until an operator clears it.
_Avoid_: implicit recovery behavior

**Scenario Conflict Rule**:
Only one Fault Scenario may be active for a Scenario Target. A conflicting activation is rejected until the active scenario completes or is cleared through Scenario Activation Confirmation.
_Avoid_: composed or replacement scenarios

**Confirmed Scenario Clearing**:
The same two-step 60-second-token operation used for activation, applied to clear a sustained Fault Scenario at the next reporting boundary.
_Avoid_: unconfirmed scenario clearing

**Observability Contract**:
The JSON-line logs, Prometheus-compatible metrics, and protocol-based readiness evidence that describe emulator operation.
_Avoid_: container liveness alone

**Baseline Metrics**:
The minimum Management Plane metrics: process/readiness/time health; active, rejected, slow, and disconnected PDCs; sent and skipped frames; scenario state and actions; and per-endpoint connection counts.
_Avoid_: aggregate-only operational evidence

**Scenario Audit Record**:
A structured log record of a Runtime Scenario Control action, including timestamp, configured deployment label, optional unverified actor label, scenario, target, confirmation, and resulting state.
_Avoid_: metric-only audit trail

**Console Operator Label**:
A required nonempty, unverified label entered by a PMU Control Console user and recorded with that user's Fault Scenario controls. The console resets the label when its page reloads.
_Avoid_: authenticated identity, optional console attribution

**Time Health**:
The observable state of host-clock synchronization. When synchronization cannot be verified, the emulator remains available with a visible degraded-time signal.
_Avoid_: asserted clock accuracy

**Time-Health Recovery**:
The automatic return to normal Time Health at the next reporting boundary when host-clock synchronization becomes verifiable, recorded in metrics and a structured log.
_Avoid_: operator-gated clock recovery

**Material Clock Regression**:
A backward host-clock movement greater than one reporting interval, which triggers degraded Time Health while reporting timestamps remain monotonic.
_Avoid_: treating any minor clock adjustment as a failure

**Degraded-Time Streaming**:
Periodic data remains available during degraded Time Health with conservative C37.118 wire-quality fields and Management Plane evidence.
_Avoid_: silent degradation, unexplained stream stop

**Degraded-Time Readiness**:
The rule that degraded Time Health does not make the emulator unready; `/readyz` remains successful while metrics and wire fields show the condition.
_Avoid_: readiness as a time-accuracy assertion

**TCP Streaming Baseline**:
The first production-like emulator increment, limited to the Compatibility Contract's V2 and V3 TCP streaming subsets. Other transport and protocol features require a named consumer need.
_Avoid_: implied full protocol support

**Failure Isolation**:
The policy that startup listener binding failures fail the process, while runtime endpoint and PDC failures remain isolated; material backward host-clock movement produces degraded Time Health while reporting timestamps stay monotonic.
_Avoid_: silent failure, fleet-wide runtime failure

**PDC Session Deadlines**:
Separately configured limits of 15 seconds for a PDC's initial command handshake and five minutes for an idle non-streaming session.
_Avoid_: indefinite inactive PDC session

**Slow-PDC Isolation**:
The policy that disconnects a PDC unable to drain its periodic frame by the next reporting boundary without affecting other PDCs on that endpoint.
_Avoid_: shared backpressure

**Baseline Release Gate**:
The Baseline Soak allows expected Scenario-induced disconnects but zero skipped ticks, and fails on unexpected protocol exchange, frame-rate, connection-isolation, memory, or timestamp-monotonicity errors.
_Avoid_: process-survival-only acceptance

**Baseline Soak Artifact**:
A JSON summary of a Baseline Soak including Runtime Identity, host and container resources, aggregate metrics, scenario results, and pass/fail reasons.
_Avoid_: console-only acceptance evidence

**Fan-out Isolation Proof**:
The Baseline Release Gate evidence that, when a Fault Scenario disconnects one PDC on an endpoint, the other PDC continues receiving data at 50 frames per second.
_Avoid_: inferred PDC fan-out

**Malformed-Command Isolation**:
The policy that closes a malformed or unsupported PDC session after the supported V3 error response when applicable; V2 closes the session without a V3-style response. The endpoint and emulator remain available.
_Avoid_: process-wide protocol failure

**Interoperability Claim**:
A claim made only after independent evidence validates configuration and data decoding, documented command exchanges, Fault Scenario behavior, and reconnect recovery for both supported wire versions.
_Avoid_: self-certified compatibility

**Physical PDC Evidence**:
Future independently run certification evidence produced by exercising the emulator against a physical PDC for both V2 and V3. Until the PDC is identified, built-in probes are the available Compatibility Evidence; physical-PDC certification does not block releases.
_Avoid_: built-in-only interoperability evidence, mandatory physical-PDC release gate

**Runtime Identity**:
The image ID or tag and the SHA-256 values of the selected startup profile and scenario catalog, emitted in readiness, metrics, and startup logs.
_Avoid_: untraceable manual build