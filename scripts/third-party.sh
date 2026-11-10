#!/usr/bin/env bash
# Résume les licences des dépendances Rust liées à Dotmon sous Linux (cargo metadata).
# Usage : scripts/third-party.sh > /tmp/licences.txt
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu | python3 -c '
import sys, json, collections
m = json.load(sys.stdin)
ws = set(m["workspace_members"])
deps = [p for p in m["packages"] if p["id"] not in ws]
count = collections.Counter(p.get("license") or "INCONNUE" for p in deps)
print(f"{len(deps)} dépendances Rust (cibles Linux x86_64)\n")
for lic, n in count.most_common():
    print(f"{n:4d}  {lic}")
print("\nÀ examiner (copyleft / spécifiques) :")
for p in sorted(deps, key=lambda p: p["name"]):
    lic = p.get("license") or "INCONNUE"
    if "GPL" in lic or "MPL" in lic or lic == "INCONNUE":
        print(f"  {p[\"name\"]} {p[\"version\"]} — {lic}")
'
