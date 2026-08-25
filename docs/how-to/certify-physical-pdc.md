(how_certify_physical_pdc)=

```{meta}
:description: Certify the C37.118 simulator's V2 and V3 behavior with an approved physical PDC.
```

# Certify With A Physical PDC

Use this guide after the PDC product, version, and operator-approved private
network are available. The procedure produces Compatibility Evidence for the
Production-like PMU Emulator. It does not make an interoperability claim until
both V2 and V3 runs pass and the evidence is retained.

## Prepare The Run

Record the PDC product name, firmware or software version, connection settings,
operator, date, and evidence location before starting the simulator.

Build the tested image and record its image ID. Select one reviewed profile and
scenario catalog for each wire version. Keep the profile and catalog SHA-256
values with the PDC evidence.

Start the simulator on the approved Private Routed Network. Verify readiness
through the Management Plane before connecting the PDC:

```sh
docker compose exec c37-118-simulator \
  c37-118-simulator healthcheck --management-address 127.0.0.1:8080
```

## Verify Each Version

Run the following sequence once with a V2 profile and once with a V3 profile.
Configure the PDC to connect to one emulator listener for the selected version.

1. Request the version-specific configuration exchange.
2. Start periodic data and retain a PDC capture that shows valid stream identity,
   configuration, timestamps, and periodic data.
3. Stop and restart the stream from the PDC.
4. Use the Management Plane to prepare and confirm the `degraded-time`,
   `missing-frames`, `disconnect-pdc`, and `signal-excursion` Fault Scenarios.
5. Record the PDC behavior for each scenario and its recovery.
6. Disconnect the PDC, reconnect it, and confirm that the documented command
   exchange and periodic data resume.

Keep the simulator JSON logs, Management Plane state, profile, catalog, image
identity, PDC capture, and PDC logs together as one evidence set.

## Record The Outcome

Mark a version as passed only when the PDC completes configuration, start/stop,
periodic data, scenario behavior, recovery, and reconnect validation without an
unexplained error. Record failures with the exact profile, catalog, PDC version,
and capture timestamp.

Do not describe the simulator as interoperable until approved physical-PDC
evidence exists for both V2 and V3. The physical-PDC procedure is manually run
and does not block normal local image builds or the release baseline.