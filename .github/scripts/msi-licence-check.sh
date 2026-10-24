#!/bin/sh
# R14: proves that the licence files are INSIDE the MSI built by cargo-dist (wix/main.wxs).
# Usage: msi-licence-check.sh <path to the .msi>
# Needs 7z (on the windows-latest runners). Never hides the 7z output: the listing is printed.
#
# Evidence, strongest first:
#   1. the file itself, extracted from the embedded cabinet (found by its MSCF magic, whatever its name);
#   2. the file name or WiX File Id in the 7z listing of the MSI or of a cabinet;
#   3. the file name or WiX File Id in the MSI's own tables (the string pool is stored as plain text).
# The check fails (::error) for a file with none of them, and says what is missing.
set -u

msi=${1:-}
[ -n "$msi" ] && [ -f "$msi" ] || {
    echo "::error title=Licence bundle::MSI not found: '$msi'"
    exit 1
}
command -v 7z >/dev/null 2>&1 || {
    echo "::error title=Licence bundle::7z is not on the PATH"
    exit 1
}

win() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }

work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/msi" "$work/cab"

echo "== 7z l (MSI) =="
7z l -slt "$(win "$msi")" >"$work/msi-list.txt" 2>&1
rc=$?
cat "$work/msi-list.txt"
if [ "$rc" -ne 0 ]; then
    echo "::error title=Licence bundle::7z could not list the MSI (exit $rc)"
    exit 1
fi

7z x -y -o"$(win "$work/msi")" "$(win "$msi")" >"$work/msi-x.txt" 2>&1 || {
    cat "$work/msi-x.txt"
    echo "::error title=Licence bundle::7z could not extract the MSI"
    exit 1
}

# Cabinets: any extracted file that starts with the MSCF magic, plus anything named *.cab.
: >"$work/cabs.txt"
find "$work/msi" -type f | while IFS= read -r f; do
    if [ "$(head -c 4 "$f" 2>/dev/null)" = "MSCF" ]; then
        echo "$f"
    else
        case $(printf '%s' "$f" | tr '[:upper:]' '[:lower:]') in *.cab) echo "$f" ;; esac
    fi
done >"$work/cabs.txt"

echo "== extracted from the MSI =="
find "$work/msi" -type f | sed "s|^$work/||"
echo "== cabinets: $(wc -l <"$work/cabs.txt" | tr -d ' ') =="

n=0
while IFS= read -r cab; do
    [ -n "$cab" ] || continue
    n=$((n + 1))
    echo "== 7z l (cabinet $n) =="
    7z l -slt "$(win "$cab")" >"$work/cab-list-$n.txt" 2>&1 || true
    cat "$work/cab-list-$n.txt"
    7z x -y -o"$(win "$work/cab")" "$(win "$cab")" >"$work/cab-x-$n.txt" 2>&1 || {
        cat "$work/cab-x-$n.txt"
        echo "::warning title=Licence bundle::7z could not extract cabinet $n"
    }
done <"$work/cabs.txt"
echo "== extracted from the cabinets =="
find "$work/cab" -type f | sed "s|^$work/||"

missing=0
for pair in LicenseFile:LICENSE NoticeFile:NOTICE ThirdPartyFile:THIRD-PARTY-LICENSES.txt LucideFile:LUCIDE-ISC.txt; do
    id=${pair%%:*}
    name=${pair#*:}
    hit=$(find "$work/msi" "$work/cab" -type f \( -name "$id" -o -name "$name" -o -name "$id.*" \) 2>/dev/null | head -n 1)
    if [ -n "$hit" ] && [ -s "$hit" ]; then
        echo "ok (file)    $name"
    elif cat "$work"/msi-list.txt "$work"/cab-list-*.txt 2>/dev/null | grep -aqF -e "$name" -e "$id"; then
        echo "ok (listing) $name"
    elif find "$work/msi" -type f -exec grep -aqF -e "$name" -e "$id" {} + 2>/dev/null; then
        echo "ok (tables)  $name"
    else
        echo "::error title=Licence bundle::$id ($name) is missing from the MSI: not extracted, not in the 7z listing, not in the MSI tables"
        missing=1
    fi
done
[ "$missing" -eq 0 ] || exit 1
echo "msi bundle ok: $msi"
