#!/usr/bin/env bash
# Launch rafp, building it first if needed. Usage: ./run.sh [file.afp]
set -euo pipefail
args=()
for a in "$@"; do
    if [ -e "$a" ]; then args+=("$(realpath "$a")"); else args+=("$a"); fi
done
# Remember where we were launched from: file dialogs start there.
export RAFP_START_DIR="$PWD"
cd "$(dirname "$(realpath "$0")")"
exec cargo run --quiet --release -p afp-viewer -- "${args[@]}"
