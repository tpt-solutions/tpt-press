#!/usr/bin/env bash
# A walkthrough of the tpt-press command line, for bash.
#
# It builds a small set of sample inputs in a temporary folder, packs and
# unpacks each kind of archive, and checks that the round trips are exact.
#
#   cargo build --release -p tpt-press-cli
#   bash crates/tpt-press-cli/examples/workflow.sh
#
# Set TPT_PRESS to use a different binary.

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tpt="${TPT_PRESS:-$here/../../../target/release/tpt-press}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

same() {
    cmp -s "$1" "$2" || { echo "$3 differs after a round trip" >&2; exit 1; }
    echo "  ok: $3 matches the original"
}

echo "== generic archive"
plain="$work/notes.txt"
for i in $(seq 1 20000); do echo "line $i the quick brown fox"; done > "$plain"
"$tpt" pack "$plain" "$work/notes.tppr" --level 9
"$tpt" inspect "$work/notes.tppr"
"$tpt" unpack "$work/notes.tppr" "$work/notes.out.txt"
same "$plain" "$work/notes.out.txt" notes.txt

echo "== logs"
jsonl="$work/app.jsonl"
for i in $(seq 0 2879); do
    s=$((i * 30))
    ts=$(printf '2026-03-01T%02d:%02d:%02dZ' $((s / 3600)) $((s / 60 % 60)) $((s % 60)))
    level=info
    [ $((i % 10)) -eq 0 ] && level=warn
    echo "{\"timestamp\":\"$ts\",\"level\":\"$level\",\"ms\":$((i % 500))}"
done > "$jsonl"
"$tpt" pack-logs "$jsonl" "$work/app.tplg" --format jsonl --timestamp-field timestamp
"$tpt" query "$work/app.tplg" --from 2026-03-01T01:00:00Z --to 2026-03-01T01:02:00Z --columns timestamp,level

echo "== genomics"
fasta="$work/reads.fna"
printf '>read1 sample\nACGTACGTTAGC\nGGATCCAT\n>read2 sample\nTTGACCAGTAAC\n' > "$fasta"
"$tpt" pack-genomics "$fasta" "$work/reads.tpgn"
"$tpt" fetch-genomics "$work/reads.tpgn" read2 --start 3 --end 8
"$tpt" unpack-genomics "$work/reads.tpgn" "$work/reads.out.fna"
same "$fasta" "$work/reads.out.fna" reads.fna

echo "== assets"
mkdir -p "$work/assets/ui"
for i in $(seq 1 400); do printf 'button'; done > "$work/assets/ui/button.txt"
echo "level data" > "$work/assets/level.dat"
"$tpt" pack-assets "$work/assets" "$work/game.tpa" --store
"$tpt" list-assets "$work/game.tpa" --verify
"$tpt" unpack-assets "$work/game.tpa" "$work/assets-out"
same "$work/assets/level.dat" "$work/assets-out/level.dat" level.dat

echo "== encryption"
# keygen writes the secret identity to a file, and prints the public key, which
# is what senders need.
public=$("$tpt" keygen "$work/me.key" | sed -n 's/^public key: //p')
"$tpt" pack "$plain" "$work/secret.tppr" --recipient "$public"
"$tpt" unpack "$work/secret.tppr" "$work/secret.out.txt" --identity "$work/me.key"
same "$plain" "$work/secret.out.txt" "encrypted notes.txt"

echo
echo "all round trips passed"
