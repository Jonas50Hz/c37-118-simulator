#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 6 ]]; then
  printf '%s\n' "usage: run-scale.sh <pmus> <seconds> <cgroup-mib> <rss-mib> <growth-mib> <active|idle>" >&2
  exit 2
fi

pmu_count="$1"
duration_seconds="$2"
cgroup_limit_mib="$3"
rss_limit_mib="$4"
growth_limit_mib="$5"
mode="$6"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
service_directory="$repository_root"
wire_version="${C37_118_WIRE_VERSION:-3}"
profile_name="one-hundred-pmu.yaml"
if [[ "$pmu_count" == "25" ]]; then
  profile_name="twenty-five-pmu.yaml"
fi
if [[ "$wire_version" == "2" ]]; then
  profile_name="${profile_name%.yaml}-v2.yaml"
elif [[ "$wire_version" != "3" ]]; then
  printf '%s\n' "C37_118_WIRE_VERSION must be 2 or 3" >&2
  exit 2
fi
profile_path="$service_directory/profiles/$profile_name"
image_name="wama-c37-118-simulator:manual-scale"
run_id="c37-118-$pmu_count-$$"
network_name="wama-c37-118-scale-$pmu_count-$$"
container_name="wama-c37-118-simulator-$pmu_count-$$"
lock_directory="${TMPDIR:-/tmp}/wama-c37-118-scale.lock"
metrics_file=""
simulator_id=""
monitor_pid=""
profile_sha256=""
image_id=""
cgroup_version=""

if [[ "$mode" != "active" && "$mode" != "idle" ]]; then
  printf '%s\n' "scale mode must be active or idle" >&2
  exit 2
fi

require_cgroup_support() {
  local version
  version="$(docker info --format '{{.CgroupVersion}}' 2>/dev/null || true)"
  if [[ "$version" != "1" && "$version" != "2" ]]; then
    printf '%s\n' "Docker cgroup-memory accounting is required for this manual scale test." >&2
    exit 2
  fi
}

cleanup() {
  local exit_code="$?"
  set +e
  if [[ -n "$monitor_pid" ]]; then
    kill "$monitor_pid" 2>/dev/null || true
    wait "$monitor_pid" 2>/dev/null || true
  fi
  if [[ -n "$simulator_id" ]]; then
    docker rm --force "$simulator_id" >/dev/null 2>&1 || true
  fi
  docker network rm "$network_name" >/dev/null 2>&1 || true
  [[ -n "$metrics_file" ]] && rm -f "$metrics_file"
  rmdir "$lock_directory" 2>/dev/null || true
  exit "$exit_code"
}

read_cgroup_memory() {
  docker exec "$simulator_id" sh -ec '
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
  docker exec "$simulator_id" awk '/VmRSS:/ { print $2 * 1024; exit }' /proc/1/status
}

monitor_memory() {
  local started_at="$SECONDS"
  local peak_cgroup=0
  local peak_rss=0
  local warmup_cgroup=""

  while [[ "$(docker inspect --format '{{.State.Running}}' "$simulator_id" 2>/dev/null || true)" == "true" ]]; do
    local cgroup rss elapsed
    cgroup="$(read_cgroup_memory 2>/dev/null || true)"
    rss="$(read_rss_memory 2>/dev/null || true)"
    if [[ "$cgroup" =~ ^[0-9]+$ ]] && (( cgroup > peak_cgroup )); then
      peak_cgroup="$cgroup"
    fi
    if [[ "$rss" =~ ^[0-9]+$ ]] && (( rss > peak_rss )); then
      peak_rss="$rss"
    fi
    elapsed=$((SECONDS - started_at))
    if [[ -z "$warmup_cgroup" && "$elapsed" -ge 60 && "$cgroup" =~ ^[0-9]+$ ]]; then
      warmup_cgroup="$cgroup"
    fi
    sleep 1
  done

  printf 'peak_cgroup_bytes=%s\npeak_rss_bytes=%s\nwarmup_cgroup_bytes=%s\n' \
    "$peak_cgroup" "$peak_rss" "${warmup_cgroup:-0}"
}

trap cleanup EXIT INT TERM

command -v docker >/dev/null
command -v timeout >/dev/null
require_cgroup_support
if ! mkdir "$lock_directory" 2>/dev/null; then
  printf '%s\n' "Another C37.118 manual scale test is already running." >&2
  exit 2
fi
if [[ ! -f "$profile_path" ]]; then
  printf 'Missing profile: %s\n' "$profile_path" >&2
  exit 2
fi

metrics_file="$(mktemp)"
profile_sha256="$(sha256sum "$profile_path" | awk '{ print $1 }')"
docker build --tag "$image_name" --file "$service_directory/Dockerfile" "$repository_root"
image_id="$(docker image inspect --format '{{.Id}}' "$image_name")"
cgroup_version="$(docker info --format '{{.CgroupVersion}}')"
docker network create --label "wama.c37-118.scale=$run_id" "$network_name" >/dev/null
simulator_id="$(docker run --detach --rm --name "$container_name" \
  --label "wama.c37-118.scale=$run_id" \
  --network "$network_name" \
  --memory "${cgroup_limit_mib}m" \
  --memory-swap "${cgroup_limit_mib}m" \
  --pids-limit 256 \
  --read-only \
  --volume "$profile_path:/etc/c37-118/profile.yaml:ro" \
  "$image_name" run --profile /etc/c37-118/profile.yaml)"

for _ in $(seq 1 30); do
  if docker logs "$simulator_id" 2>&1 | grep -q "starting C37.118 simulator"; then
    break
  fi
  sleep 1
done
if ! docker logs "$simulator_id" 2>&1 | grep -q "starting C37.118 simulator"; then
  printf '%s\n' "Simulator did not reach its listening state." >&2
  exit 1
fi

monitor_memory > "$metrics_file" &
monitor_pid="$!"
if [[ "$mode" == "active" ]]; then
  timeout --preserve-status "$((duration_seconds + 60))s" docker run --rm \
    --label "wama.c37-118.scale=$run_id" \
    --network "$network_name" \
    --entrypoint /usr/local/bin/c37-118-probe \
    "$image_name" \
    --wire-version "$wire_version" \
    --host "$container_name" \
    --first-port 4712 \
    --first-stream-id 1001 \
    --count "$pmu_count" \
    --duration-seconds "$duration_seconds" \
    --data-rate-hz 50
else
  sleep "$duration_seconds"
fi
docker stop --time 2 "$simulator_id" >/dev/null
simulator_id=""
wait "$monitor_pid"
monitor_pid=""

peak_cgroup="$(awk -F= '$1 == "peak_cgroup_bytes" { print $2 }' "$metrics_file")"
peak_rss="$(awk -F= '$1 == "peak_rss_bytes" { print $2 }' "$metrics_file")"
warmup_cgroup="$(awk -F= '$1 == "warmup_cgroup_bytes" { print $2 }' "$metrics_file")"
required_cgroup=$((cgroup_limit_mib * 1024 * 1024))
required_rss=$((rss_limit_mib * 1024 * 1024))
allowed_growth=$((growth_limit_mib * 1024 * 1024))

if [[ ! "$peak_cgroup" =~ ^[0-9]+$ || ! "$peak_rss" =~ ^[0-9]+$ || ! "$warmup_cgroup" =~ ^[0-9]+$ ]]; then
  printf '%s\n' "Cannot read required simulator cgroup/RSS measurements." >&2
  exit 1
fi
if (( peak_cgroup > required_cgroup || peak_rss > required_rss )); then
  printf 'Memory budget exceeded: cgroup=%s rss=%s\n' "$peak_cgroup" "$peak_rss" >&2
  exit 1
fi
if (( peak_cgroup - warmup_cgroup > allowed_growth )); then
  printf 'Post-warm-up cgroup growth exceeded budget: peak=%s warmup=%s\n' \
    "$peak_cgroup" "$warmup_cgroup" >&2
  exit 1
fi

printf 'manual_scale_result wire_version=%s mode=%s pmus=%s seconds=%s peak_cgroup_bytes=%s peak_rss_bytes=%s warmup_cgroup_bytes=%s\n' \
  "$wire_version" "$mode" "$pmu_count" "$duration_seconds" "$peak_cgroup" "$peak_rss" "$warmup_cgroup"
printf 'manual_scale_environment image_id=%s profile_sha256=%s kernel=%s docker=%s cgroup_version=%s\n' \
  "$image_id" "$profile_sha256" "$(uname -r)" "$(docker version --format '{{.Server.Version}}')" "$cgroup_version"