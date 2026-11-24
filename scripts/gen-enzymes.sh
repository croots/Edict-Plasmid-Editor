#!/usr/bin/env bash
# Regenerate src/enzyme_data.rs from REBASE: every enzyme New England Biolabs supplies,
# with its recognition site and cut positions (EMBOSS format).
#
# Usage: scripts/gen-enzymes.sh [dir-with-rebase-files]
# Without a directory, downloads link_emboss_e and link_emboss_r into a temp dir.
set -euo pipefail

out="$(cd "$(dirname "$0")/.." && pwd)/src/enzyme_data.rs"
dir="${1:-}"
if [ -z "$dir" ]; then
    dir="$(mktemp -d)"
    for f in link_emboss_e link_emboss_r; do
        curl -sSL -m 120 -o "$dir/$f" "http://rebase.neb.com/rebase/$f"
    done
fi

ver="$(grep -o 'emboss_e\.[0-9]*' "$dir/link_emboss_e" | head -1)"

# Supplier codes are the 6th line of each record in emboss_r; N is NEB.
awk 'BEGIN { RS = "//\n" }
     { n = split($0, L, "\n"); i = 1; while (i <= n && L[i] == "") i++
       if (L[i + 5] ~ /N/) print L[i] }' "$dir/link_emboss_r" | sort -u > "$dir/neb.txt"

{
    echo "//! Generated from REBASE ($ver, http://rebase.neb.com): every enzyme New England"
    echo "//! Biolabs supplies, as (name, site, c1, c2, c3, c4) in EMBOSS's cut notation."
    echo "//! Regenerate with scripts/gen-enzymes.sh; don't edit by hand."
    echo
    echo "pub const ENZYMES: &[(&str, &str, i16, i16, i16, i16)] = &["
    awk -F'\t' 'NR == FNR { neb[$1] = 1; next }
                !/^#/ && NF >= 9 && ($1 in neb) && $4 > 0 {
                    printf "    (\"%s\", \"%s\", %d, %d, %d, %d),\n", $1, toupper($2), $6, $7, $8, $9 }' \
        "$dir/neb.txt" "$dir/link_emboss_e" | sort -f
    echo "];"
} > "$out"

echo "wrote $(grep -c '^    (' "$out") enzymes to $out"
