#!/usr/bin/env bash
# La version doit être la même partout : Cargo.toml, Cargo.lock, les trois PKGBUILD
# (0.4.0-beta.1 ↔ 0.4.0_beta.1), le journal, les notes de release, et l'étiquette si fournie.
#   scripts/ci/check-version.sh [vX.Y.Z[-pre]]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

v="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
[[ -n "$v" ]] || { echo "version introuvable dans Cargo.toml" >&2; exit 1; }
fail=0
check() { if [[ "$2" != "$3" ]]; then echo "✗ $1 : « $2 » ≠ « $3 »" >&2; fail=1; else echo "✓ $1 = $3"; fi; }

for pkg in packaging/arch/PKGBUILD packaging/aur/dotmon/PKGBUILD.in packaging/aur/dotmon-bin/PKGBUILD.in; do
    pv="$(sed -n 's/^pkgver=//p' "$pkg")"
    check "$pkg" "${pv//_/-}" "$v"
done
for crate in dotmon dotmon-core dotmon-sys; do
    lv="$(awk -v n="$crate" '$1=="name"&&$3=="\""n"\""{f=1;next} f&&$1=="version"{gsub(/"/,"",$3);print $3;exit}' Cargo.lock)"
    check "Cargo.lock ($crate)" "$lv" "$v"
done
if grep -q "^## \[$v\]" CHANGELOG.md; then echo "✓ CHANGELOG.md : section [$v]"; else echo "✗ CHANGELOG.md : pas de section [$v]" >&2; fail=1; fi
if [[ -f "docs/releases/v$v.md" ]]; then echo "✓ docs/releases/v$v.md"; else echo "✗ notes de release docs/releases/v$v.md absentes" >&2; fail=1; fi
if [[ -n "${1:-}" ]]; then check "étiquette" "$1" "v$v"; fi
exit $fail
