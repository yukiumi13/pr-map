#!/usr/bin/env bash
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${1:-$HOME/.local}"
mkdir -p "$prefix"
prefix="$(cd -- "$prefix" && pwd)"
cargo build --manifest-path "$source_dir/Cargo.toml" --release --locked
python3 -m venv "$prefix/share/pr-map/venv"
"$prefix/share/pr-map/venv/bin/python" -m pip install --disable-pip-version-check --constraint "$source_dir/requirements.lock" "$source_dir"
mkdir -p "$prefix/bin"
install -m 755 "${CARGO_TARGET_DIR:-$source_dir/target}/release/prmap" "$prefix/bin/prmap"
# The launcher resolves its own prefix so an installation may live outside PATH.
cat > "$prefix/bin/pr-map" <<'EOF'
#!/bin/sh
case "$0" in */*) bindir=${0%/*} ;; *) bindir=. ;; esac
prefix=$(CDPATH= cd -- "$bindir/.." && pwd)
export PRMAP_BIN="$prefix/bin/prmap"
# Herdr runs the panel and the report every few seconds; they skip Python's startup.
case "$*" in
  --minimap) exec "$PRMAP_BIN" minimap ;;
  --report) exec "$PRMAP_BIN" report ;;
esac
exec "$prefix/share/pr-map/venv/bin/pr-map" "$@"
EOF
chmod 755 "$prefix/bin/pr-map"
printf 'Installed pr-map and prmap in %s/bin\nTry: %s/bin/pr-map --demo\n' "$prefix" "$prefix"
