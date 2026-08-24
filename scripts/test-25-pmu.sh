#!/usr/bin/env bash

set -euo pipefail

if [[ "${C37_118_RUN_25_PMU:-}" != "1" ]]; then
  printf '%s\n' "Refusing the manual 25-PMU test. Set C37_118_RUN_25_PMU=1 to continue." >&2
  exit 2
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$script_directory/run-scale.sh" 25 300 32 16 1 active