# PMU Control Console Implementation Plan

## Purpose

This plan adds the desktop-only PMU Control Console defined in
[`CONTEXT.md`](../CONTEXT.md) and ADRs
[`0011`](adr/0011-static-pmu-control-console-service.md) and
[`0012`](adr/0012-expose-console-through-trusted-network.md).

The console is a separate static Caddy service. It presents Management Plane
state and invokes existing confirmed Fault Scenario controls. It does not own
simulator lifecycle, startup profiles, PMU identity, capacity, wire version,
authentication, or network security policy.

## Constraints

- Publish the console at `0.0.0.0:8081` by default. The Trusted Network
  Boundary is responsible for every reachable host interface.
- Use Caddy to serve the desktop-only static page and proxy same-origin
  `/api/*` requests to `c37-118-simulator:8080`.
- Do not add CORS, TLS, application authentication, a client framework, or a
  browser persistence layer.
- Start the console independently of simulator readiness. During API outages,
  it remains loadable and presents Console Stale State with controls disabled.
- Preserve the existing Management Plane request bounds, structured error
  envelope, prepare/confirm semantics, and one-target action policy.

## Delivery Order

### 1. Add Console Read Models

Extend the Management Plane before adding browser assets.

Update `src/management.rs` and `src/server.rs` with a read-only
`GET /v1/catalog` route. Its response represents the immutable startup catalog
and includes:

- catalog version;
- scenario name and kind;
- target compatibility;
- lifecycle, frame-relative start offset, and duration; and
- signal-excursion values when present.

Derive target compatibility from existing scenario kinds. `disconnect-pdc` is
PDC-only. Other changing Fault Scenarios are endpoint-only. Include `normal`
and `recovery` in the response, but let the console exclude them from its action
menu.

Extend `/v1/state` endpoint records with read-only PMU facts required by the
Console PMU Table:

- stream ID and PMU ID;
- listener port;
- wire version;
- PDC name and PMU name in text form;
- reporting rate and nominal frequency;
- Time Health, PDC connection state, and active scenario state.

Use explicit management response DTOs rather than serializing
`EndpointDescriptor` directly. Its encoded name fields are byte vectors and do
not form browser-ready text.

Acceptance checks:

- Rust management loopback tests validate `/v1/catalog` fields, target
  compatibility, and immutable catalog identity.
- Rust management loopback tests validate enriched `/v1/state` facts while
  preserving existing state fields.
- Catalog and state responses remain within the existing 64 KiB response limit
  for the 150-PMU profile.

### 2. Add The Static Caddy Service

Create these new files:

```text
pmu-control-console/
├── compose.yaml
├── Caddyfile
├── index.html
├── app.js
└── styles.css
```

Add `pmu-control-console` to the root `compose.yaml`. The service uses
`caddy:2-alpine`, mounts the Caddy configuration and static assets read-only,
joins the external `wama-infra` network, restarts unless stopped, and has its
own static-file health check.

Publish the console with configurable host binding and port defaults:

```text
${C37_118_CONSOLE_BIND_ADDRESS:-0.0.0.0}:${C37_118_CONSOLE_PORT:-8081}:80
```

Do not add `depends_on` for simulator readiness. Caddy starts and serves assets
even when the Management Plane is unavailable.

Configure Caddy with these routes:

- `/api/*`: strip `/api` and reverse proxy to `c37-118-simulator:8080`.
- `/`: serve static assets and the console entry point.

Do not add CORS headers. Browser requests stay same-origin through Caddy.

Acceptance checks:

- `docker compose config --quiet` renders the new service and default port
  mapping.
- `caddy validate --config /etc/caddy/Caddyfile` passes in the service image.
- The console static health check succeeds while simulator is unavailable.
- A same-origin request to `/api/v1/state` reaches the simulator when it is
  available.

### 3. Build The Desktop Console

Implement the static page without a client framework.

The page loads `/api/v1/catalog` once, then polls `/api/v1/state` every two
seconds. It retains the last successful state after a failed poll, displays a
stale indicator with the last successful update time, disables every control,
and retries on the normal polling interval. It re-enables controls after a
successful poll.

Build a dense Console PMU Table with:

- stream ID search;
- filters for wire version, PDC occupancy, active scenario, and Time Health;
- PMU ID, listener port, wire version, PMU/PDC names, PDC occupancy, Time
  Health, and active scenario columns; and
- expandable PDC rows that expose connection IDs and streaming state.

Target actions are single-target only. Endpoint rows show compatible changing
scenarios. PDC rows expose `disconnect-pdc` only. `normal` and `recovery` are
not action buttons. Show `Clear` only for an active sustained scenario.

The Console Operator Label is a required nonempty input. Keep it only in page
memory and clear it on reload. On an action request, show full Console Scenario
Detail, call prepare, display the returned 60-second token state, and require a
second explicit confirmation before calling confirm. Keep success and error
feedback in page memory only.

The console is desktop-only. It has no narrow-viewport control workflow.

Acceptance checks:

- Browser tests cover catalog rendering, filtering, PMU expansion, and PDC
  target selection.
- Browser tests cover prepare then confirm with the existing Management Plane
  request shapes.
- Browser tests cover stale state, disabled controls, recovery after a polling
  success, and console availability during simulator outage.
- Browser screenshots verify the supported desktop layout and explicit
  desktop-only behavior at a narrow viewport.

### 4. Integrate And Document The Console

Update `README.md` and `docs/c37-118-simulator.md` with:

- the default trusted-network console address;
- Caddy same-origin API proxying;
- Management Plane catalog and enriched state routes;
- desktop-only scope;
- Console Operator Label semantics; and
- outage and stale-state behavior.

Add the console to the Compose operator instructions without changing the
simulator's existing private Management Plane exposure.

Acceptance checks:

- Documentation matches the rendered Compose service, Caddy route, and API
response shapes.
- `git diff --check`, Markdown diagnostics, shell validation, Rust tests,
Compose rendering, and browser tests pass.

## Explicit Non-Goals

- No batch PMU or PDC actions.
- No profile, capacity, PMU identity, wire-version, or simulator lifecycle
  controls.
- No mobile or narrow-viewport control workflow.
- No persistent browser action history or local-storage state.
- No CORS, TLS, application authentication, or changes to the Trusted Network
  Boundary.