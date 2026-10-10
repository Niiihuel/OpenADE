#!/bin/sh
# Copy what the export_*_visual_review tests wrote after <marker> was touched
# into <dest>, one folder per export directory. Usage:
#   touch /tmp/marker
#   cargo test -p agents-window --no-fail-fast -- --ignored export_
#   docs/ui-separation/collect-shots.sh /tmp/marker docs/ui-separation/shots/<section>
# Compare a later run with the baseline using: diff -rq <dest> shots/s00-baseline
set -eu
marker=$1
dest=$2
root=$(cd "$(dirname "$0")/../.." && pwd)
for dir in /tmp/openade-*-review /tmp/openide-*-review /tmp/openide-*-visual* \
    "$root"/target/*-visual-qa "$root"/target/settings-density-review; do
    [ -d "$dir" ] || continue
    find "$dir" -type f -newer "$marker" | while read -r file; do
        target="$dest/$(basename "$dir")/${file#"$dir"/}"
        mkdir -p "$(dirname "$target")"
        cp "$file" "$target"
    done
done
