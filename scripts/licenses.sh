#!/usr/bin/env bash
# Regenerates licenses/rust-crates.tsv: every Rust crate compiled into the app (normal
# dependencies reachable from it, on Linux), with its version, license and repository.
# Run inside `nix develop`; needs jq (`nix shell nixpkgs#jq`).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo metadata --format-version 1 --filter-platform x86_64-unknown-linux-gnu | jq -r '
  # Normal (non-build, non-dev) dependencies reachable from the root package.
  (.resolve.root) as $root
  | (.resolve.nodes | map({key: .id, value: [.deps[] | select(any(.dep_kinds[]; .kind == null)) | .pkg]}) | from_entries) as $g
  | (.packages | map({key: .id, value: .}) | from_entries) as $p
  | def reach: . as $seen | ([$seen[] | $g[.][]] | unique) as $next
      | if ($next - $seen | length) == 0 then $seen else ($seen + $next | unique) | reach end;
    [$root] | reach | map(select(. != $root) | $p[.])
  | sort_by(.name | ascii_downcase)
  | .[] | [.name, .version, (.license // ("see " + (.license_file // "crate"))), (.repository // "")] | @tsv
' > licenses/rust-crates.tsv
echo "$(wc -l < licenses/rust-crates.tsv) crates written to licenses/rust-crates.tsv"
