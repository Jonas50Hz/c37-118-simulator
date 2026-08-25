#!/usr/bin/env bash

set -euo pipefail

if [[ "${C37_118_RUN_150_PMU:-}" != "1" ]]; then
  printf '%s\n' "Refusing the manual 150-PMU benchmark. Set C37_118_RUN_150_PMU=1 to continue." >&2
  exit 2
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$script_directory/run-scale.sh" 150 900 64 32 2 active