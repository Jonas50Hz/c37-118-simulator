#!/usr/bin/env bash

set -euo pipefail

readonly ACTIVE_DURATION_SECONDS=300
readonly IDLE_DURATION_SECONDS=900
readonly DATA_RATE_HZ=50
readonly ENDPOINT_COUNT=10
readonly FIRST_STREAM_ID=1001
readonly FIRST_LISTEN_PORT=4712
readonly MEMORY_LIMIT_MIB=64
readonly MEMORY_LIMIT_BYTES=$((MEMORY_LIMIT_MIB * 1024 * 1024))
readonly PIDS_LIMIT=256
readonly READY_RETRIES=30
readonly PROBE_WAIT_TIMEOUT_SECONDS=360
readonly ACTIVE_MONITOR_MAX_SECONDS="$PROBE_WAIT_TIMEOUT_SECONDS"
readonly PROBE_OUTPUT_MAX_BYTES=16384

if [[ "${C37_118_RUN_BASELINE:-}" != "1" ]]; then
  printf '%s\n' "Refusing the manual release-baseline test. Set C37_118_RUN_BASELINE=1 to continue." >&2
  exit 2
fi

if [[ -n "${C37_118_BASELINE_WIRE_VERSION:-}" ]]; then
  printf '%s\n' "C37_118_BASELINE_WIRE_VERSION is not supported; release baseline always validates wire versions 2 and 3." >&2
  exit 2
fi

selected_wire_versions=(2 3)

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
catalog_path="$repository_root/scenarios/baseline.yaml"
artifact_directory="${C37_118_BASELINE_ARTIFACT_DIR:-${TMPDIR:-/tmp}}"
run_id="c37-118-baseline-$(date -u +%Y%m%dT%H%M%SZ)-$$"
started_at_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
selected_wire_versions_csv="$(IFS=,; printf '%s' "${selected_wire_versions[*]}")"

artifact_path=""
scratch_directory=""
failure_reasons_file=""
records_directory=""
current_version_record=""
kernel_release="unavailable"
docker_server_version="unavailable"
cgroup_version="unavailable"
overall_pass=false
owned_containers=()
owned_networks=()
owned_images=()
active_monitor_pid=""
active_monitor_metrics_path=""
active_monitor_record_path=""

record_failure() {
  local reason="$1"

  printf 'release_baseline_failure=%s\n' "$reason" >&2
  if [[ -n "$failure_reasons_file" ]]; then
    printf '%s\n' "$reason" >> "$failure_reasons_file" || true
  fi
  if [[ -n "$current_version_record" && -f "$current_version_record" ]] && command -v python3 >/dev/null 2>&1; then
    python3 - "$current_version_record" "$reason" <<'PY' || true
import json
import sys

path, reason = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    record = json.load(source)
record["reasons"].append(reason)
with open(path, "w", encoding="utf-8") as destination:
    json.dump(record, destination, indent=2, sort_keys=True)
    destination.write("\n")
PY
  fi
}

fatal() {
  record_failure "$1"
  exit 1
}

write_summary() {
  local exit_code="$1"

  if [[ -z "$artifact_path" ]] || ! command -v python3 >/dev/null 2>&1; then
    return 0
  fi

  python3 - "$artifact_path" "$run_id" "$started_at_utc" "$selected_wire_versions_csv" \
    "$kernel_release" "$docker_server_version" "$cgroup_version" "$failure_reasons_file" \
    "$records_directory" "$overall_pass" "$exit_code" <<'PY'
import json
import os
import sys
from pathlib import Path

(
    artifact_path,
    run_id,
    started_at_utc,
    selected_wire_versions_csv,
    kernel_release,
    docker_server_version,
    cgroup_version,
    failure_reasons_path,
    records_directory,
    overall_pass,
    exit_code,
) = sys.argv[1:]

failure_reasons = []
failure_path = Path(failure_reasons_path)
if failure_reasons_path and failure_path.exists():
    failure_reasons = [
        line.strip()
        for line in failure_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]

versions = {}
records_path = Path(records_directory)
if records_directory and records_path.exists():
    for record_path in sorted(records_path.glob("version-*.json")):
        try:
            record = json.loads(record_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            failure_reasons.append(f"could not read version record {record_path.name}: {error}")
            continue
        versions[str(record["wire_version"])] = record

selected_wire_versions = [
    int(value) for value in selected_wire_versions_csv.split(",") if value
]
pass_status = overall_pass == "true" and exit_code == "0" and not failure_reasons
pass_reasons = []
if pass_status:
    pass_reasons.append(
        "all selected wire versions completed the fixed active and idle release checks"
    )

summary = {
    "schema_version": 1,
    "pass": pass_status,
    "outcome": {
        "pass_reasons": pass_reasons,
        "failure_reasons": failure_reasons,
        "exit_code": int(exit_code),
    },
    "run": {
        "identity": run_id,
        "started_at_utc": started_at_utc,
        "selected_wire_versions": selected_wire_versions,
        "image_ids": {
            wire_version: record.get("image", {}).get("id")
            for wire_version, record in versions.items()
        },
    },
    "host": {
        "kernel_release": kernel_release,
        "docker_server_version": docker_server_version,
        "cgroup_version": cgroup_version,
    },
    "versions": versions,
}

temporary_path = f"{artifact_path}.tmp"
with open(temporary_path, "w", encoding="utf-8") as destination:
    json.dump(summary, destination, indent=2, sort_keys=True)
    destination.write("\n")
os.replace(temporary_path, artifact_path)
PY
}

cleanup() {
  local exit_code="$?"
  local container_name network_name image_name

  trap - EXIT INT TERM
  set +e
  if [[ "$exit_code" -ne 0 ]]; then
    record_failure "script exited with status $exit_code"
  fi
  if [[ -n "$active_monitor_pid" ]]; then
    kill "$active_monitor_pid" 2>/dev/null || true
    wait "$active_monitor_pid" 2>/dev/null || true
    active_monitor_pid=""
  fi
  if [[ -n "$active_monitor_record_path" && -n "$active_monitor_metrics_path" && \
    -f "$active_monitor_record_path" ]]; then
    if ! record_active_memory_observations "$active_monitor_record_path" \
      "$active_monitor_metrics_path"; then
      append_record_reason "$active_monitor_record_path" \
        "active memory observations could not be fully validated during cleanup"
    fi
  fi
  active_monitor_metrics_path=""
  active_monitor_record_path=""
  write_summary "$exit_code"

  for container_name in "${owned_containers[@]}"; do
    docker rm --force "$container_name" >/dev/null 2>&1 || true
  done
  for network_name in "${owned_networks[@]}"; do
    docker network rm "$network_name" >/dev/null 2>&1 || true
  done
  for image_name in "${owned_images[@]}"; do
    docker image rm "$image_name" >/dev/null 2>&1 || true
  done
  if [[ -n "$scratch_directory" && -d "$scratch_directory" ]]; then
    rm -rf "$scratch_directory"
  fi
  if [[ -n "$artifact_path" ]]; then
    printf 'release_baseline_artifact=%s\n' "$artifact_path"
  fi
  exit "$exit_code"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$artifact_directory"
scratch_directory="$(mktemp -d "${TMPDIR:-/tmp}/c37-118-baseline.XXXXXX")"
failure_reasons_file="$scratch_directory/failure-reasons.txt"
records_directory="$scratch_directory/records"
mkdir -p "$records_directory"
: > "$failure_reasons_file"
artifact_path="$(mktemp "$artifact_directory/c37-118-baseline-${run_id}.XXXXXX.json")"

require_command() {
  local command_name="$1"

  command -v "$command_name" >/dev/null 2>&1 || fatal "required command is unavailable: $command_name"
}

require_cgroup_accounting() {
  cgroup_version="$(docker info --format '{{.CgroupVersion}}' 2>/dev/null || true)"
  if [[ "$cgroup_version" != "1" && "$cgroup_version" != "2" ]]; then
    fatal "Docker cgroup-memory accounting is required for the release baseline"
  fi
}

sha256_file() {
  local path="$1"

  python3 - "$path" <<'PY'
import hashlib
import sys

digest = hashlib.sha256()
with open(sys.argv[1], "rb") as source:
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
}

set_record_value() {
  local record_path="$1"
  local dotted_path="$2"
  local value_type="$3"
  local value="$4"

  python3 - "$record_path" "$dotted_path" "$value_type" "$value" <<'PY'
import json
import sys

record_path, dotted_path, value_type, value = sys.argv[1:]
with open(record_path, encoding="utf-8") as source:
    record = json.load(source)

target = record
parts = dotted_path.split(".")
for part in parts[:-1]:
    target = target[part]

if value_type == "bool":
    parsed_value = value == "true"
elif value_type == "int":
    parsed_value = int(value)
elif value_type == "string":
    parsed_value = value
else:
    raise ValueError(f"unsupported record value type: {value_type}")

target[parts[-1]] = parsed_value
with open(record_path, "w", encoding="utf-8") as destination:
    json.dump(record, destination, indent=2, sort_keys=True)
    destination.write("\n")
PY
}

record_active_state_snapshot() {
  local record_path="$1"
  local snapshot_name="$2"
  local state_path="$3"

  python3 - "$record_path" "$snapshot_name" "$state_path" <<'PY'
import json
import sys

record_path, snapshot_name, state_path = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
  state = json.load(source)
if not isinstance(state, dict):
  raise SystemExit("active state response was not a JSON object")

with open(record_path, encoding="utf-8") as source:
  record = json.load(source)

active = record["active"]
active.setdefault("state_snapshots", {})[snapshot_name] = state
observed_skipped_ticks = active.setdefault("observed_skipped_ticks", {})
stats = state.get("stats")
skipped_ticks = None
validation_error = None
if not isinstance(stats, dict):
  validation_error = "active state response did not contain a stats object"
else:
  candidate = stats.get("skipped_ticks")
  if isinstance(candidate, bool) or not isinstance(candidate, int) or candidate < 0:
    validation_error = "active state response did not contain a nonnegative integer stats.skipped_ticks"
  else:
    skipped_ticks = candidate
observed_skipped_ticks[snapshot_name] = skipped_ticks

with open(record_path, "w", encoding="utf-8") as destination:
  json.dump(record, destination, indent=2, sort_keys=True)
  destination.write("\n")

if validation_error:
  raise SystemExit(validation_error)
if skipped_ticks != 0:
  raise SystemExit(
    f"active state snapshot {snapshot_name} reported stats.skipped_ticks={skipped_ticks}"
  )
PY
}

record_idle_final_state_snapshot() {
  local record_path="$1"
  local state_path="$2"

  python3 - "$record_path" "$state_path" <<'PY'
import json
import sys

record_path, state_path = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
  state = json.load(source)
if not isinstance(state, dict):
  raise SystemExit("idle state response was not a JSON object")

with open(record_path, encoding="utf-8") as source:
  record = json.load(source)
record["idle"]["final_state_snapshot"] = state
with open(record_path, "w", encoding="utf-8") as destination:
  json.dump(record, destination, indent=2, sort_keys=True)
  destination.write("\n")
PY
}

record_probe_output() {
  local record_path="$1"
  local probe_record="$2"
  local probe_log_path="$3"
  local exit_status="$4"

  python3 - "$record_path" "$probe_record" "$probe_log_path" "$exit_status" \
  "$PROBE_OUTPUT_MAX_BYTES" <<'PY'
import json
import sys
from pathlib import Path

record_path, probe_record, probe_log_path, exit_status, maximum_bytes = sys.argv[1:]
exit_status = int(exit_status)
maximum_bytes = int(maximum_bytes)
if maximum_bytes <= 0:
  raise SystemExit("probe output retention limit must be positive")

raw_output = Path(probe_log_path).read_bytes()
total_bytes = len(raw_output)
truncated = total_bytes > maximum_bytes
retained_output = raw_output[-maximum_bytes:] if truncated else raw_output
expected_failure = b"peer closed during frame read"
contains_expected_failure = expected_failure in raw_output

with open(record_path, encoding="utf-8") as source:
  record = json.load(source)
probe = record["active"]["probes"][probe_record]
probe["output"] = {
  "content": retained_output.decode("utf-8", errors="replace"),
  "contains_expected_peer_close_failure": contains_expected_failure,
  "retained_bytes": len(retained_output),
  "total_bytes": total_bytes,
  "truncated": truncated,
}
if exit_status == 0:
  probe["exit_reason"] = "completed successfully"
  probe["failure_reason"] = None
elif contains_expected_failure:
  probe["exit_reason"] = f"exit status {exit_status}"
  probe["failure_reason"] = "peer closed during frame read"
else:
  probe["exit_reason"] = f"exit status {exit_status}"
  probe["failure_reason"] = (
    f"nonzero exit status {exit_status} without expected peer-close failure"
  )

with open(record_path, "w", encoding="utf-8") as destination:
  json.dump(record, destination, indent=2, sort_keys=True)
  destination.write("\n")
PY
}

probe_output_has_expected_peer_close_failure() {
  local probe_log_path="$1"

  python3 - "$probe_log_path" <<'PY'
import sys
from pathlib import Path

if b"peer closed during frame read" not in Path(sys.argv[1]).read_bytes():
  raise SystemExit("failed probe did not report peer closed during frame read")
PY
}

initialize_version_record() {
  local record_path="$1"
  local wire_version="$2"
  local profile_path="$3"
  local profile_sha256="$4"
  local catalog_sha256="$5"
  local image_name="$6"
  local active_simulator_name="$7"
  local probe_a_name="$8"
  local probe_b_name="$9"
  local idle_simulator_name="${10}"

  python3 - "$record_path" "$wire_version" "$profile_path" "$profile_sha256" \
    "$catalog_path" "$catalog_sha256" "$image_name" "$active_simulator_name" \
    "$probe_a_name" "$probe_b_name" "$idle_simulator_name" "$MEMORY_LIMIT_BYTES" \
    "$ACTIVE_DURATION_SECONDS" "$IDLE_DURATION_SECONDS" <<'PY'
import json
import sys

(
    record_path,
    wire_version,
    profile_path,
    profile_sha256,
    catalog_path,
    catalog_sha256,
    image_name,
    active_simulator_name,
    probe_a_name,
    probe_b_name,
    idle_simulator_name,
    memory_limit_bytes,
    active_duration_seconds,
    idle_duration_seconds,
) = sys.argv[1:]

record = {
    "wire_version": int(wire_version),
    "profile": {"path": profile_path, "sha256": profile_sha256},
    "catalog": {"path": catalog_path, "sha256": catalog_sha256},
    "image": {"name": image_name, "id": None},
    "active": {
        "status": "not_started",
        "duration_seconds": int(active_duration_seconds),
        "ready": False,
        "all_endpoints_have_two_streaming_connections": False,
        "minimum_endpoint_data_frames_required": 300 * 50,
      "memory": {
        "failure_reason": "",
        "memory_cap_bytes": int(memory_limit_bytes),
        "peak_cgroup_bytes": 0,
        "peak_rss_bytes": 0,
        "sample_count": 0,
        "status": "not_started",
      },
      "state_snapshots": {
        "after_probes": None,
        "after_disconnect": None,
        "before_disconnect": None,
      },
      "observed_skipped_ticks": {
        "after_probes": None,
        "after_disconnect": None,
        "before_disconnect": None,
      },
        "scenario": {
            "status": "not_started",
            "target_stream_id": 1001,
            "connection_id": None,
            "token": None,
            "target_disconnected": False,
            "prepare_http_status": None,
            "confirm_http_status": None,
        },
        "probes": {
            "probe_a": {
                "container": probe_a_name,
                "status": "not_started",
                "exit_status": None,
                "exit_reason": None,
                "failure_reason": None,
                "minimum_endpoint_data_frames": None,
                "output": {
                  "content": "",
                  "contains_expected_peer_close_failure": False,
                  "retained_bytes": 0,
                  "total_bytes": 0,
                  "truncated": False,
                },
            },
            "probe_b": {
                "container": probe_b_name,
                "status": "not_started",
                "exit_status": None,
                "exit_reason": None,
                "failure_reason": None,
                "minimum_endpoint_data_frames": None,
                "output": {
                  "content": "",
                  "contains_expected_peer_close_failure": False,
                  "retained_bytes": 0,
                  "total_bytes": 0,
                  "truncated": False,
                },
            },
        },
        "passing_probe": None,
        "failed_probe": None,
        "simulator": active_simulator_name,
    },
    "idle": {
        "status": "not_started",
        "duration_seconds": int(idle_duration_seconds),
        "simulator": idle_simulator_name,
        "memory_cap_bytes": int(memory_limit_bytes),
        "memory": {
          "failure_reason": "",
          "memory_cap_bytes": int(memory_limit_bytes),
          "peak_cgroup_bytes": 0,
          "peak_rss_bytes": 0,
          "sample_count": 0,
          "status": "not_started",
        },
        "peak_cgroup_bytes": 0,
        "peak_rss_bytes": 0,
        "final_state_snapshot": None,
        "readiness_checks": 0,
        "zero_active_pdc_checks": 0,
        "readiness_throughout": False,
        "zero_active_pdcs_throughout": False,
        "remained_running": False,
    },
    "reasons": [],
}

with open(record_path, "w", encoding="utf-8") as destination:
    json.dump(record, destination, indent=2, sort_keys=True)
    destination.write("\n")
PY
}

append_record_reason() {
  local record_path="$1"
  local reason="$2"

  python3 - "$record_path" "$reason" <<'PY'
import json
import sys

record_path, reason = sys.argv[1:]
with open(record_path, encoding="utf-8") as source:
    record = json.load(source)
record["reasons"].append(reason)
with open(record_path, "w", encoding="utf-8") as destination:
    json.dump(record, destination, indent=2, sort_keys=True)
    destination.write("\n")
PY
}

management_port_for() {
  local simulator_name="$1"

  docker port "$simulator_name" 8080/tcp | python3 -c '
import sys

bindings = [line.strip() for line in sys.stdin if line.strip()]
if len(bindings) != 1:
    raise SystemExit("expected exactly one management port binding")
binding = bindings[0]
if binding.startswith("["):
    host, separator, port = binding[1:].partition("]:")
else:
    host, separator, port = binding.rpartition(":")
if not separator or host not in {"127.0.0.1", "::1"}:
    raise SystemExit(f"management port is not loopback-only: {binding}")
if not port.isdigit() or not 0 < int(port) < 65536:
    raise SystemExit(f"invalid management port binding: {binding}")
print(port)
'
}

readyz_is_true() {
  local management_port="$1"

  curl --silent --show-error --fail --connect-timeout 2 --max-time 5 \
    --header 'Host: c37-118-baseline' \
    "http://127.0.0.1:${management_port}/readyz" | python3 -c '
import json
import sys

response = json.load(sys.stdin)
if response.get("ready") is not True:
    raise SystemExit("/readyz did not report ready=true")
'
}

wait_for_readyz() {
  local management_port="$1"
  local attempt

  for ((attempt = 1; attempt <= READY_RETRIES; attempt += 1)); do
    if readyz_is_true "$management_port"; then
      return 0
    fi
    sleep 1
  done
  return 1
}

fetch_state() {
  local management_port="$1"
  local destination="$2"

  curl --silent --show-error --fail --connect-timeout 2 --max-time 5 \
    --header 'Host: c37-118-baseline' \
    "http://127.0.0.1:${management_port}/v1/state" > "$destination"
}

state_has_two_streaming_connections_per_endpoint() {
  local state_path="$1"

  python3 - "$state_path" "$FIRST_STREAM_ID" "$ENDPOINT_COUNT" <<'PY'
import json
import sys

state_path, first_stream_id, endpoint_count = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
    state = json.load(source)

first_stream_id = int(first_stream_id)
endpoint_count = int(endpoint_count)
expected_stream_ids = set(range(first_stream_id, first_stream_id + endpoint_count))
endpoints = state.get("endpoints")
if not isinstance(endpoints, list) or len(endpoints) != endpoint_count:
    raise SystemExit("unexpected endpoint list")

seen_stream_ids = set()
for endpoint in endpoints:
    stream_id = endpoint.get("stream_id")
    connections = endpoint.get("connections")
    if stream_id not in expected_stream_ids or stream_id in seen_stream_ids:
        raise SystemExit("unexpected stream ID")
    if endpoint.get("active_connections") != 2:
        raise SystemExit(f"stream {stream_id} does not have exactly two active connections")
    if not isinstance(connections, list) or len(connections) != 2:
        raise SystemExit(f"stream {stream_id} does not expose exactly two connections")
    if not all(
        isinstance(connection.get("connection_id"), int)
        and connection["connection_id"] > 0
        and connection.get("streaming") is True
        for connection in connections
    ):
        raise SystemExit(f"stream {stream_id} does not have two streaming PDCs")
    seen_stream_ids.add(stream_id)

if seen_stream_ids != expected_stream_ids:
    raise SystemExit("missing expected stream IDs")
PY
}

wait_for_two_streaming_connections_per_endpoint() {
  local management_port="$1"
  local state_path="$2"
  local attempt

  for ((attempt = 1; attempt <= READY_RETRIES; attempt += 1)); do
    if fetch_state "$management_port" "$state_path" && \
      state_has_two_streaming_connections_per_endpoint "$state_path"; then
      return 0
    fi
    sleep 1
  done
  return 1
}

connection_id_for_stream_1001() {
  local state_path="$1"

  python3 - "$state_path" "$FIRST_STREAM_ID" <<'PY'
import json
import sys

state_path, stream_id = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
    state = json.load(source)

matching = [endpoint for endpoint in state["endpoints"] if endpoint.get("stream_id") == int(stream_id)]
if len(matching) != 1:
    raise SystemExit("stream 1001 is not uniquely present")
connections = matching[0].get("connections")
if not isinstance(connections, list) or len(connections) != 2:
    raise SystemExit("stream 1001 does not have two connections")
connection_ids = [connection.get("connection_id") for connection in connections]
if not all(isinstance(connection_id, int) and connection_id > 0 for connection_id in connection_ids):
    raise SystemExit("stream 1001 contains an invalid connection ID")
print(min(connection_ids))
PY
}

management_post_json() {
  local management_port="$1"
  local endpoint_path="$2"
  local payload="$3"
  local response_path="$4"

  curl --silent --show-error --connect-timeout 2 --max-time 5 \
    --output "$response_path" --write-out '%{http_code}' \
    --header 'Host: c37-118-baseline' \
    --header 'Content-Type: application/json' \
    --data "$payload" \
    "http://127.0.0.1:${management_port}${endpoint_path}"
}

prepare_token_from_response() {
  local response_path="$1"
  local connection_id="$2"

  python3 - "$response_path" "$connection_id" <<'PY'
import json
import sys

response_path, connection_id = sys.argv[1:]
with open(response_path, encoding="utf-8") as source:
    response = json.load(source)

expected_target = {
  "pdc": {"stream_id": 1001, "connection_id": int(connection_id)}
}
if (
  response.get("target") != expected_target
  or response.get("action") != {"activate": {"scenario_name": "disconnect-pdc"}}
  or response.get("actor_label") != "baseline"
):
  raise SystemExit("prepare response did not contain the expected scenario")
token = response.get("token")
if not isinstance(token, int) or token <= 0:
    raise SystemExit("prepare response did not contain a valid token")
print(token)
PY
}

confirm_response_has_scenario() {
  local response_path="$1"
  local connection_id="$2"

  python3 - "$response_path" "$connection_id" <<'PY'
import json
import sys

response_path, connection_id = sys.argv[1:]
with open(response_path, encoding="utf-8") as source:
    response = json.load(source)

expected_target = {
  "pdc": {"stream_id": 1001, "connection_id": int(connection_id)}
}
if (
  response.get("target") != expected_target
  or response.get("action") != {"activate": {"scenario_name": "disconnect-pdc"}}
  or response.get("actor_label") != "baseline"
):
    raise SystemExit("confirm response did not contain the expected scenario")
PY
}

state_matches_intentional_disconnect() {
  local state_path="$1"
  local connection_id="$2"
  local first_stream_id="$3"
  local endpoint_count="$4"

  python3 - "$state_path" "$connection_id" "$first_stream_id" "$endpoint_count" <<'PY'
import json
import sys

state_path, connection_id, first_stream_id, endpoint_count = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
    state = json.load(source)

connection_id = int(connection_id)
first_stream_id = int(first_stream_id)
endpoint_count = int(endpoint_count)
expected_stream_ids = set(range(first_stream_id, first_stream_id + endpoint_count))
endpoints = state.get("endpoints")
if not isinstance(endpoints, list) or len(endpoints) != endpoint_count:
  raise SystemExit("unexpected endpoint list after intentional disconnect")

seen_stream_ids = set()
for endpoint in endpoints:
  if not isinstance(endpoint, dict):
    raise SystemExit("endpoint state was not an object after intentional disconnect")
  stream_id = endpoint.get("stream_id")
  connections = endpoint.get("connections")
  if stream_id not in expected_stream_ids or stream_id in seen_stream_ids:
    raise SystemExit("unexpected stream ID after intentional disconnect")
  if not isinstance(connections, list) or not all(isinstance(connection, dict) for connection in connections):
    raise SystemExit(f"stream {stream_id} exposed invalid PDC connections")
  if any(connection.get("connection_id") == connection_id for connection in connections):
    raise SystemExit("target PDC connection is still present")
  if stream_id == first_stream_id:
    if endpoint.get("active_connections") != 1 or len(connections) != 1:
      raise SystemExit("stream 1001 does not have exactly one remaining PDC")
    remaining_connection = connections[0]
    if (
      not isinstance(remaining_connection.get("connection_id"), int)
      or remaining_connection["connection_id"] <= 0
      or remaining_connection.get("streaming") is not True
    ):
      raise SystemExit("stream 1001 does not have one remaining streaming PDC")
  else:
    if endpoint.get("active_connections") != 2 or len(connections) != 2:
      raise SystemExit(f"stream {stream_id} does not have exactly two remaining PDCs")
    if not all(
      isinstance(connection.get("connection_id"), int)
      and connection["connection_id"] > 0
      and connection.get("streaming") is True
      for connection in connections
    ):
      raise SystemExit(f"stream {stream_id} does not have two streaming PDCs")
  seen_stream_ids.add(stream_id)

if seen_stream_ids != expected_stream_ids:
  raise SystemExit("missing expected stream IDs after intentional disconnect")
PY
}

wait_for_target_disconnect() {
  local management_port="$1"
  local state_path="$2"
  local connection_id="$3"
  local attempt

  for ((attempt = 1; attempt <= READY_RETRIES; attempt += 1)); do
    if fetch_state "$management_port" "$state_path" && \
      state_matches_intentional_disconnect "$state_path" "$connection_id" \
        "$FIRST_STREAM_ID" "$ENDPOINT_COUNT"; then
      return 0
    fi
    sleep 1
  done
  return 1
}

run_probe() {
  local image_name="$1"
  local network_name="$2"
  local simulator_name="$3"
  local probe_name="$4"
  local wire_version="$5"

  docker run --detach --name "$probe_name" \
    --label "wama.c37-118.baseline=$run_id" \
    --network "$network_name" \
    --entrypoint /usr/local/bin/c37-118-probe \
    "$image_name" \
    --wire-version "$wire_version" \
    --host "$simulator_name" \
    --first-port "$FIRST_LISTEN_PORT" \
    --first-stream-id "$FIRST_STREAM_ID" \
    --count "$ENDPOINT_COUNT" \
    --duration-seconds "$ACTIVE_DURATION_SECONDS" \
    --data-rate-hz "$DATA_RATE_HZ" >/dev/null
  owned_containers+=("$probe_name")
}

wait_for_probes() {
  local probe_a_name="$1"
  local probe_b_name="$2"
  local probe_a_wait_status probe_b_wait_status

  timeout --preserve-status "${PROBE_WAIT_TIMEOUT_SECONDS}s" docker wait "$probe_a_name" >/dev/null &
  local probe_a_wait_pid=$!
  timeout --preserve-status "${PROBE_WAIT_TIMEOUT_SECONDS}s" docker wait "$probe_b_name" >/dev/null &
  local probe_b_wait_pid=$!

  set +e
  wait "$probe_a_wait_pid"
  probe_a_wait_status=$?
  wait "$probe_b_wait_pid"
  probe_b_wait_status=$?
  set -e

  if [[ "$probe_a_wait_status" -ne 0 || "$probe_b_wait_status" -ne 0 ]]; then
    return 1
  fi
}

container_exit_status() {
  local container_name="$1"

  docker inspect --format '{{.State.ExitCode}}' "$container_name"
}

minimum_frames_from_probe_log() {
  local probe_log_path="$1"

  python3 - "$probe_log_path" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
matches = re.findall(r"\bminimum_endpoint_data_frames=(\d+)\b", text)
if len(matches) != 1:
    raise SystemExit("probe log did not contain exactly one minimum_endpoint_data_frames value")
print(matches[0])
PY
}

start_simulator() {
  local image_name="$1"
  local network_name="$2"
  local simulator_name="$3"
  local profile_path="$4"
  local time_sync_status_file="$5"
  local wire_version="$6"

  docker run --detach --name "$simulator_name" \
    --label "wama.c37-118.baseline=$run_id" \
    --network "$network_name" \
    --memory "${MEMORY_LIMIT_MIB}m" \
    --memory-swap "${MEMORY_LIMIT_MIB}m" \
    --pids-limit "$PIDS_LIMIT" \
    --read-only \
    --publish 127.0.0.1::8080 \
    --volume "$profile_path:/etc/c37-118/profile.yaml:ro" \
    --volume "$catalog_path:/etc/c37-118/scenarios/baseline.yaml:ro" \
    --volume "$time_sync_status_file:/etc/c37-118/time-sync-status:ro" \
    "$image_name" run \
      --profile /etc/c37-118/profile.yaml \
      --scenario-catalog /etc/c37-118/scenarios/baseline.yaml \
      --deployment-label "release-baseline-v${wire_version}" \
      --management-bind 0.0.0.0:8080 \
      --time-sync-status-file /etc/c37-118/time-sync-status >/dev/null
  owned_containers+=("$simulator_name")
}

read_cgroup_memory() {
  local simulator_name="$1"

  docker exec "$simulator_name" sh -ec '
    if [ -r /sys/fs/cgroup/memory.current ]; then
      cat /sys/fs/cgroup/memory.current
    elif [ -r /sys/fs/cgroup/memory/memory.usage_in_bytes ]; then
      cat /sys/fs/cgroup/memory/memory.usage_in_bytes
    else
      exit 1
    fi
  '
}

read_rss_memory() {
  local simulator_name="$1"

  docker exec "$simulator_name" awk '/VmRSS:/ { print $2 * 1024; exit }' /proc/1/status
}

simulator_is_running() {
  local simulator_name="$1"

  [[ "$(docker inspect --format '{{.State.Running}}' "$simulator_name" 2>/dev/null || true)" == "true" ]]
}

write_active_memory_metrics() {
  local metrics_path="$1"
  local peak_cgroup_bytes="$2"
  local peak_rss_bytes="$3"
  local sample_count="$4"
  local failure_reason="$5"
  local temporary_path="${metrics_path}.tmp"

  {
    printf 'peak_cgroup_bytes=%s\n' "$peak_cgroup_bytes"
    printf 'peak_rss_bytes=%s\n' "$peak_rss_bytes"
    printf 'sample_count=%s\n' "$sample_count"
    printf 'failure_reason=%s\n' "$failure_reason"
  } > "$temporary_path"
  mv "$temporary_path" "$metrics_path"
}

monitor_active_memory() {
  local simulator_name="$1"
  local metrics_path="$2"
  local started_at="$SECONDS"
  local peak_cgroup_bytes=0
  local peak_rss_bytes=0
  local sample_count=0
  local failure_reason=""
  local cgroup_bytes rss_bytes

  write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
    "$sample_count" "$failure_reason"
  trap 'write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" "$sample_count" "$failure_reason"; exit 0' TERM INT

  while (( SECONDS - started_at < ACTIVE_MONITOR_MAX_SECONDS )); do
    if ! simulator_is_running "$simulator_name"; then
      failure_reason="active simulator stopped while memory was being monitored"
      write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
        "$sample_count" "$failure_reason"
      return 0
    fi
    cgroup_bytes="$(read_cgroup_memory "$simulator_name" 2>/dev/null || true)"
    rss_bytes="$(read_rss_memory "$simulator_name" 2>/dev/null || true)"
    if [[ ! "$cgroup_bytes" =~ ^[0-9]+$ || ! "$rss_bytes" =~ ^[0-9]+$ ]]; then
      failure_reason="could not read active cgroup or RSS memory accounting"
      write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
        "$sample_count" "$failure_reason"
      return 0
    fi
    if (( cgroup_bytes > peak_cgroup_bytes )); then
      peak_cgroup_bytes="$cgroup_bytes"
    fi
    if (( rss_bytes > peak_rss_bytes )); then
      peak_rss_bytes="$rss_bytes"
    fi
    sample_count=$((sample_count + 1))
    if (( cgroup_bytes > MEMORY_LIMIT_BYTES || rss_bytes > MEMORY_LIMIT_BYTES )); then
      failure_reason="active memory cap exceeded: cgroup=${cgroup_bytes} rss=${rss_bytes}"
      write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
        "$sample_count" "$failure_reason"
      return 0
    fi
    write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
      "$sample_count" "$failure_reason"
    sleep 1
  done

  failure_reason="active memory monitor exceeded the ${ACTIVE_MONITOR_MAX_SECONDS}-second bound"
  write_active_memory_metrics "$metrics_path" "$peak_cgroup_bytes" "$peak_rss_bytes" \
    "$sample_count" "$failure_reason"
}

record_active_memory_observations() {
  local record_path="$1"
  local metrics_path="$2"

  python3 - "$record_path" "$metrics_path" "$MEMORY_LIMIT_BYTES" <<'PY'
import json
import sys
from pathlib import Path

record_path, metrics_path, memory_cap_bytes = sys.argv[1:]
memory_cap_bytes = int(memory_cap_bytes)
with open(record_path, encoding="utf-8") as source:
    record = json.load(source)
memory = record["active"]["memory"]
failure_reason = ""

try:
    lines = Path(metrics_path).read_text(encoding="utf-8").splitlines()
except OSError as error:
    values = {}
    failure_reason = f"could not read active memory observations: {error}"
else:
    values = {}
    for line in lines:
        key, separator, value = line.partition("=")
        if not separator or not key:
            failure_reason = "active memory observations were malformed"
            break
        values[key] = value

numeric_keys = ("peak_cgroup_bytes", "peak_rss_bytes", "sample_count")
if not failure_reason:
    invalid_keys = [
        key for key in numeric_keys
        if key not in values or not values[key].isdigit()
    ]
    if invalid_keys or "failure_reason" not in values:
        failure_reason = "active memory observations were incomplete or unreadable"

if not failure_reason:
    peak_cgroup_bytes = int(values["peak_cgroup_bytes"])
    peak_rss_bytes = int(values["peak_rss_bytes"])
    sample_count = int(values["sample_count"])
    memory["peak_cgroup_bytes"] = peak_cgroup_bytes
    memory["peak_rss_bytes"] = peak_rss_bytes
    memory["sample_count"] = sample_count
    monitor_failure_reason = values["failure_reason"]
    if monitor_failure_reason:
        failure_reason = monitor_failure_reason
    elif sample_count <= 0:
        failure_reason = "active memory monitor did not collect a measurement"
    elif peak_cgroup_bytes > memory_cap_bytes or peak_rss_bytes > memory_cap_bytes:
        failure_reason = (
            f"active memory cap exceeded: cgroup={peak_cgroup_bytes} rss={peak_rss_bytes}"
        )

memory["memory_cap_bytes"] = memory_cap_bytes
memory["failure_reason"] = failure_reason
memory["status"] = "failed" if failure_reason else "passed"
with open(record_path, "w", encoding="utf-8") as destination:
    json.dump(record, destination, indent=2, sort_keys=True)
    destination.write("\n")

if failure_reason:
    raise SystemExit(failure_reason)
PY
}

start_active_memory_monitor() {
  local simulator_name="$1"
  local record_path="$2"
  local metrics_path="$3"

  if [[ -n "$active_monitor_pid" ]]; then
    return 1
  fi
  active_monitor_record_path="$record_path"
  active_monitor_metrics_path="$metrics_path"
  monitor_active_memory "$simulator_name" "$metrics_path" &
  active_monitor_pid="$!"
}

stop_active_memory_monitor() {
  local monitor_pid="$active_monitor_pid"
  local wait_status

  if [[ -z "$monitor_pid" ]]; then
    return 0
  fi
  if kill -0 "$monitor_pid" 2>/dev/null; then
    kill "$monitor_pid" 2>/dev/null || true
  fi
  if wait "$monitor_pid"; then
    wait_status=0
  else
    wait_status=$?
  fi
  active_monitor_pid=""
  return "$wait_status"
}

finish_active_memory_monitor() {
  local record_path="$1"
  local monitor_stopped=true
  local observations_recorded=true

  if ! stop_active_memory_monitor; then
    monitor_stopped=false
  fi
  if ! record_active_memory_observations "$record_path" "$active_monitor_metrics_path"; then
    observations_recorded=false
  fi
  active_monitor_metrics_path=""
  active_monitor_record_path=""
  [[ "$monitor_stopped" == true && "$observations_recorded" == true ]]
}

state_has_zero_active_pdcs() {
  local state_path="$1"

  python3 - "$state_path" "$FIRST_STREAM_ID" "$ENDPOINT_COUNT" <<'PY'
import json
import sys

state_path, first_stream_id, endpoint_count = sys.argv[1:]
with open(state_path, encoding="utf-8") as source:
    state = json.load(source)

expected_stream_ids = set(range(int(first_stream_id), int(first_stream_id) + int(endpoint_count)))
endpoints = state.get("endpoints")
if not isinstance(endpoints, list) or {endpoint.get("stream_id") for endpoint in endpoints} != expected_stream_ids:
    raise SystemExit("idle state did not contain the expected endpoints")
for endpoint in endpoints:
    if endpoint.get("active_connections") != 0 or endpoint.get("connections") != []:
        raise SystemExit(f"stream {endpoint.get('stream_id')} has an active PDC")
PY
}

record_idle_observations() {
  local record_path="$1"
  local peak_cgroup_bytes="$2"
  local peak_rss_bytes="$3"
  local readiness_checks="$4"
  local zero_active_pdc_checks="$5"
  local memory_samples="${6:-0}"
  local memory_failure_reason="${7:-}"

  set_record_value "$record_path" "idle.peak_cgroup_bytes" int "$peak_cgroup_bytes"
  set_record_value "$record_path" "idle.peak_rss_bytes" int "$peak_rss_bytes"
  set_record_value "$record_path" "idle.readiness_checks" int "$readiness_checks"
  set_record_value "$record_path" "idle.zero_active_pdc_checks" int "$zero_active_pdc_checks"
  set_record_value "$record_path" "idle.memory.peak_cgroup_bytes" int "$peak_cgroup_bytes"
  set_record_value "$record_path" "idle.memory.peak_rss_bytes" int "$peak_rss_bytes"
  set_record_value "$record_path" "idle.memory.sample_count" int "$memory_samples"
  if [[ -n "$memory_failure_reason" ]]; then
    set_record_value "$record_path" "idle.memory.status" string "failed"
    set_record_value "$record_path" "idle.memory.failure_reason" string "$memory_failure_reason"
  else
    set_record_value "$record_path" "idle.memory.status" string "observed"
    set_record_value "$record_path" "idle.memory.failure_reason" string ""
  fi
}

monitor_idle_phase() {
  local simulator_name="$1"
  local management_port="$2"
  local state_path="$3"
  local started_at="$SECONDS"
  local cgroup_bytes rss_bytes elapsed

  idle_peak_cgroup_bytes=0
  idle_peak_rss_bytes=0
  idle_readiness_checks=0
  idle_zero_active_pdc_checks=0
  idle_memory_samples=0
  idle_failure_reason=""
  idle_memory_failure_reason=""

  while (( SECONDS - started_at < IDLE_DURATION_SECONDS )); do
    if ! simulator_is_running "$simulator_name"; then
      idle_failure_reason="idle simulator stopped before the fixed idle phase completed"
      return 1
    fi
    if ! readyz_is_true "$management_port"; then
      idle_failure_reason="/readyz failed during the idle phase"
      return 1
    fi
    if ! fetch_state "$management_port" "$state_path" || ! state_has_zero_active_pdcs "$state_path"; then
      idle_failure_reason="idle state showed active PDCs or an unexpected endpoint set"
      return 1
    fi
    cgroup_bytes="$(read_cgroup_memory "$simulator_name" 2>/dev/null || true)"
    rss_bytes="$(read_rss_memory "$simulator_name" 2>/dev/null || true)"
    if [[ ! "$cgroup_bytes" =~ ^[0-9]+$ || ! "$rss_bytes" =~ ^[0-9]+$ ]]; then
      idle_memory_failure_reason="could not read idle cgroup or RSS memory accounting"
      idle_failure_reason="$idle_memory_failure_reason"
      return 1
    fi
    if (( cgroup_bytes > idle_peak_cgroup_bytes )); then
      idle_peak_cgroup_bytes="$cgroup_bytes"
    fi
    if (( rss_bytes > idle_peak_rss_bytes )); then
      idle_peak_rss_bytes="$rss_bytes"
    fi
    if (( cgroup_bytes > MEMORY_LIMIT_BYTES || rss_bytes > MEMORY_LIMIT_BYTES )); then
      idle_memory_failure_reason="idle memory cap exceeded: cgroup=${cgroup_bytes} rss=${rss_bytes}"
      idle_failure_reason="$idle_memory_failure_reason"
      return 1
    fi
    idle_memory_samples=$((idle_memory_samples + 1))
    ((idle_readiness_checks += 1))
    ((idle_zero_active_pdc_checks += 1))
    sleep 1
  done

  if ! simulator_is_running "$simulator_name"; then
    idle_failure_reason="idle simulator was not running after the fixed idle phase"
    return 1
  fi
  if ! readyz_is_true "$management_port"; then
    idle_failure_reason="/readyz failed after the fixed idle phase"
    return 1
  fi
  if ! fetch_state "$management_port" "$state_path" || ! state_has_zero_active_pdcs "$state_path"; then
    idle_failure_reason="idle state was not PDC-free after the fixed idle phase"
    return 1
  fi
  ((idle_readiness_checks += 1))
  ((idle_zero_active_pdc_checks += 1))
}

require_command docker
require_command curl
require_command python3
require_command timeout
if ! docker info >/dev/null 2>&1; then
  fatal "Docker is unavailable"
fi
require_cgroup_accounting
kernel_release="$(uname -r)"
docker_server_version="$(docker version --format '{{.Server.Version}}')"

if [[ ! -f "$catalog_path" ]]; then
  fatal "missing scenario catalog: $catalog_path"
fi
catalog_sha256="$(sha256_file "$catalog_path")"

for wire_version in "${selected_wire_versions[@]}"; do
  if [[ "$wire_version" == "2" ]]; then
    profile_path="$repository_root/profiles/ten-pmu-v2.yaml"
  else
    profile_path="$repository_root/profiles/ten-pmu.yaml"
  fi
  if [[ ! -f "$profile_path" ]]; then
    fatal "missing profile: $profile_path"
  fi

  profile_sha256="$(sha256_file "$profile_path")"
  network_name="${run_id}-v${wire_version}"
  image_name="wama-c37-118-simulator:release-baseline-${run_id}-v${wire_version}"
  active_simulator_name="${run_id}-v${wire_version}-active"
  probe_a_name="${run_id}-v${wire_version}-probe-a"
  probe_b_name="${run_id}-v${wire_version}-probe-b"
  idle_simulator_name="${run_id}-v${wire_version}-idle"
  time_sync_status_file="$scratch_directory/time-sync-v${wire_version}"
  active_before_disconnect_state_path="$scratch_directory/active-before-disconnect-v${wire_version}.json"
  active_after_disconnect_state_path="$scratch_directory/active-after-disconnect-v${wire_version}.json"
  active_after_probes_state_path="$scratch_directory/active-after-probes-v${wire_version}.json"
  active_memory_metrics_path="$scratch_directory/active-memory-v${wire_version}.txt"
  idle_final_state_path="$scratch_directory/idle-final-v${wire_version}.json"
  prepare_response_path="$scratch_directory/prepare-v${wire_version}.json"
  confirm_response_path="$scratch_directory/confirm-v${wire_version}.json"
  probe_a_log_path="$scratch_directory/probe-a-v${wire_version}.log"
  probe_b_log_path="$scratch_directory/probe-b-v${wire_version}.log"
  current_version_record="$records_directory/version-${wire_version}.json"

  initialize_version_record "$current_version_record" "$wire_version" "$profile_path" \
    "$profile_sha256" "$catalog_sha256" "$image_name" "$active_simulator_name" \
    "$probe_a_name" "$probe_b_name" "$idle_simulator_name"
  printf '%s\n' verified > "$time_sync_status_file"

  owned_images+=("$image_name")
  docker build --label "wama.c37-118.baseline=$run_id" --tag "$image_name" \
    --file "$repository_root/Dockerfile" "$repository_root"
  image_id="$(docker image inspect --format '{{.Id}}' "$image_name")"
  set_record_value "$current_version_record" "image.id" string "$image_id"

  docker network create --label "wama.c37-118.baseline=$run_id" "$network_name" >/dev/null
  owned_networks+=("$network_name")

  set_record_value "$current_version_record" "active.status" string "running"
  start_simulator "$image_name" "$network_name" "$active_simulator_name" "$profile_path" \
    "$time_sync_status_file" "$wire_version"
  active_management_port="$(management_port_for "$active_simulator_name")" || \
    fatal "could not derive the loopback management port for wire version $wire_version"
  if ! wait_for_readyz "$active_management_port"; then
    fatal "active simulator did not become ready for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.ready" bool true
  set_record_value "$current_version_record" "active.memory.status" string "running"
  if ! start_active_memory_monitor "$active_simulator_name" "$current_version_record" \
    "$active_memory_metrics_path"; then
    fatal "could not start active memory monitoring for wire version $wire_version"
  fi

  run_probe "$image_name" "$network_name" "$active_simulator_name" "$probe_a_name" "$wire_version"
  run_probe "$image_name" "$network_name" "$active_simulator_name" "$probe_b_name" "$wire_version"
  if ! wait_for_two_streaming_connections_per_endpoint "$active_management_port" \
    "$active_before_disconnect_state_path"; then
    fatal "active state did not show two streaming PDCs for every endpoint on wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.all_endpoints_have_two_streaming_connections" bool true
  if ! record_active_state_snapshot "$current_version_record" "before_disconnect" \
    "$active_before_disconnect_state_path"; then
    fatal "active state did not report zero skipped ticks after both probes were streaming for wire version $wire_version"
  fi

  target_connection_id="$(connection_id_for_stream_1001 "$active_before_disconnect_state_path")" || \
    fatal "could not select a stream 1001 PDC connection for wire version $wire_version"
  set_record_value "$current_version_record" "active.scenario.connection_id" int "$target_connection_id"
  prepare_payload="$(printf '{"target":{"stream_id":1001,"connection_id":%s},"scenario_name":"disconnect-pdc","actor_label":"baseline"}' "$target_connection_id")"
  prepare_http_status="$(management_post_json "$active_management_port" \
    "/v1/scenarios/prepare" "$prepare_payload" "$prepare_response_path")" || \
    fatal "scenario prepare request failed for wire version $wire_version"
  if [[ "$prepare_http_status" != "202" ]]; then
    fatal "scenario prepare request returned HTTP $prepare_http_status for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.scenario.prepare_http_status" int "$prepare_http_status"
  scenario_token="$(prepare_token_from_response "$prepare_response_path" "$target_connection_id")" || \
    fatal "scenario prepare response was invalid for wire version $wire_version"
  set_record_value "$current_version_record" "active.scenario.token" int "$scenario_token"
  set_record_value "$current_version_record" "active.scenario.status" string "prepared"

  confirm_payload="$(printf '{"token":%s,"actor_label":"baseline"}' "$scenario_token")"
  confirm_http_status="$(management_post_json "$active_management_port" \
    "/v1/scenarios/confirm" "$confirm_payload" "$confirm_response_path")" || \
    fatal "scenario confirm request failed for wire version $wire_version"
  if [[ "$confirm_http_status" != "202" ]]; then
    fatal "scenario confirm request returned HTTP $confirm_http_status for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.scenario.confirm_http_status" int "$confirm_http_status"
  if ! confirm_response_has_scenario "$confirm_response_path" "$target_connection_id"; then
    fatal "scenario confirm response was invalid for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.scenario.status" string "confirmed"
  if ! wait_for_target_disconnect "$active_management_port" "$active_after_disconnect_state_path" \
    "$target_connection_id"; then
    fatal "confirmed disconnect-pdc scenario did not preserve the expected endpoint topology for wire version $wire_version"
  fi
  if ! record_active_state_snapshot "$current_version_record" "after_disconnect" \
    "$active_after_disconnect_state_path"; then
    fatal "active state did not report zero skipped ticks after the intentional disconnect for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.scenario.target_disconnected" bool true
  set_record_value "$current_version_record" "active.scenario.status" string "confirmed_and_observed"

  if ! wait_for_probes "$probe_a_name" "$probe_b_name"; then
    fatal "probe wait timed out or failed for wire version $wire_version"
  fi
  if ! fetch_state "$active_management_port" "$active_after_probes_state_path"; then
    fatal "could not fetch active state after probes completed for wire version $wire_version"
  fi
  if ! record_active_state_snapshot "$current_version_record" "after_probes" \
    "$active_after_probes_state_path"; then
    fatal "active state did not report zero skipped ticks after probes completed for wire version $wire_version"
  fi
  if ! finish_active_memory_monitor "$current_version_record"; then
    fatal "active memory monitoring failed for wire version $wire_version"
  fi
  probe_a_exit_status="$(container_exit_status "$probe_a_name")"
  probe_b_exit_status="$(container_exit_status "$probe_b_name")"
  if [[ ! "$probe_a_exit_status" =~ ^[0-9]+$ || ! "$probe_b_exit_status" =~ ^[0-9]+$ ]]; then
    fatal "could not read probe exit statuses for wire version $wire_version"
  fi
  if ! docker logs "$probe_a_name" > "$probe_a_log_path" 2>&1; then
    fatal "could not collect probe A output for wire version $wire_version"
  fi
  if ! docker logs "$probe_b_name" > "$probe_b_log_path" 2>&1; then
    fatal "could not collect probe B output for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.probes.probe_a.exit_status" int "$probe_a_exit_status"
  set_record_value "$current_version_record" "active.probes.probe_b.exit_status" int "$probe_b_exit_status"
  if ! record_probe_output "$current_version_record" "probe_a" "$probe_a_log_path" \
    "$probe_a_exit_status"; then
    fatal "could not record probe A output for wire version $wire_version"
  fi
  if ! record_probe_output "$current_version_record" "probe_b" "$probe_b_log_path" \
    "$probe_b_exit_status"; then
    fatal "could not record probe B output for wire version $wire_version"
  fi
  if [[ "$probe_a_exit_status" == "0" ]]; then
    set_record_value "$current_version_record" "active.probes.probe_a.status" string "passed"
  else
    set_record_value "$current_version_record" "active.probes.probe_a.status" string "failed_after_intentional_disconnect"
  fi
  if [[ "$probe_b_exit_status" == "0" ]]; then
    set_record_value "$current_version_record" "active.probes.probe_b.status" string "passed"
  else
    set_record_value "$current_version_record" "active.probes.probe_b.status" string "failed_after_intentional_disconnect"
  fi

  if [[ "$probe_a_exit_status" == "0" && "$probe_b_exit_status" != "0" ]]; then
    passing_probe_name="$probe_a_name"
    passing_probe_record="probe_a"
    passing_probe_log_path="$probe_a_log_path"
    failed_probe_record="probe_b"
    failed_probe_log_path="$probe_b_log_path"
  elif [[ "$probe_b_exit_status" == "0" && "$probe_a_exit_status" != "0" ]]; then
    passing_probe_name="$probe_b_name"
    passing_probe_record="probe_b"
    passing_probe_log_path="$probe_b_log_path"
    failed_probe_record="probe_a"
    failed_probe_log_path="$probe_a_log_path"
  else
    fatal "expected exactly one probe to fail after the intentional disconnect on wire version $wire_version"
  fi
  if [[ ! -s "$probe_a_log_path" || ! -s "$probe_b_log_path" ]]; then
    fatal "a probe emitted no log after the intentional disconnect on wire version $wire_version"
  fi
  passing_probe_minimum_frames="$(minimum_frames_from_probe_log "$passing_probe_log_path")" || \
    fatal "could not parse minimum_endpoint_data_frames from the passing probe log for wire version $wire_version"
  if [[ ! "$passing_probe_minimum_frames" =~ ^[0-9]+$ ]]; then
    fatal "passing probe emitted an invalid minimum_endpoint_data_frames value for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.probes.${passing_probe_record}.minimum_endpoint_data_frames" \
    int "$passing_probe_minimum_frames"
  set_record_value "$current_version_record" "active.passing_probe" string "$passing_probe_name"
  set_record_value "$current_version_record" "active.failed_probe" string "$failed_probe_record"
  if ! probe_output_has_expected_peer_close_failure "$failed_probe_log_path"; then
    fatal "failed probe did not report the expected peer-close failure for wire version $wire_version"
  fi
  if (( passing_probe_minimum_frames < ACTIVE_DURATION_SECONDS * DATA_RATE_HZ )); then
    fatal "passing probe minimum_endpoint_data_frames was below $((ACTIVE_DURATION_SECONDS * DATA_RATE_HZ)) for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "active.status" string "passed"
  append_record_reason "$current_version_record" \
    "active phase passed with one intentional disconnect and minimum_endpoint_data_frames=${passing_probe_minimum_frames}"

  docker rm --force "$probe_a_name" "$probe_b_name" "$active_simulator_name" >/dev/null

  set_record_value "$current_version_record" "idle.status" string "running"
  start_simulator "$image_name" "$network_name" "$idle_simulator_name" "$profile_path" \
    "$time_sync_status_file" "$wire_version"
  idle_management_port="$(management_port_for "$idle_simulator_name")" || \
    fatal "could not derive the idle loopback management port for wire version $wire_version"
  if ! wait_for_readyz "$idle_management_port"; then
    fatal "idle simulator did not become ready for wire version $wire_version"
  fi
  if ! monitor_idle_phase "$idle_simulator_name" "$idle_management_port" "$idle_final_state_path"; then
    record_idle_observations "$current_version_record" "$idle_peak_cgroup_bytes" "$idle_peak_rss_bytes" \
      "$idle_readiness_checks" "$idle_zero_active_pdc_checks" "$idle_memory_samples" \
      "$idle_memory_failure_reason"
    fatal "$idle_failure_reason for wire version $wire_version"
  fi
  record_idle_observations "$current_version_record" "$idle_peak_cgroup_bytes" "$idle_peak_rss_bytes" \
    "$idle_readiness_checks" "$idle_zero_active_pdc_checks" "$idle_memory_samples" \
    "$idle_memory_failure_reason"
  if ! record_idle_final_state_snapshot "$current_version_record" "$idle_final_state_path"; then
    fatal "could not record final idle state for wire version $wire_version"
  fi
  set_record_value "$current_version_record" "idle.readiness_throughout" bool true
  set_record_value "$current_version_record" "idle.zero_active_pdcs_throughout" bool true
  set_record_value "$current_version_record" "idle.remained_running" bool true
  set_record_value "$current_version_record" "idle.memory.status" string "passed"
  set_record_value "$current_version_record" "idle.status" string "passed"
  append_record_reason "$current_version_record" \
    "idle phase passed with cgroup_peak=${idle_peak_cgroup_bytes} rss_peak=${idle_peak_rss_bytes}"

  docker rm --force "$idle_simulator_name" >/dev/null
  current_version_record=""
done

overall_pass=true
printf 'release_baseline_result=passed run_id=%s wire_versions=%s\n' "$run_id" "$selected_wire_versions_csv"