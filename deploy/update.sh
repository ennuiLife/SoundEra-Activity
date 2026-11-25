#!/bin/sh
# Update SoundEra Activity to the newest release, after checking that it is signed by the maintainer.
#
#   sudo deploy/update.sh            check, download, verify, install and restart
#   deploy/update.sh --check         only say whether a newer version exists
#
# It needs only curl, tar and sha256sum. The signature is checked by the program you already have installed
# (`soundera-activity --verify`), which carries the maintainer's public key; if the check fails, nothing is installed.
# For Docker, update with: docker compose pull && docker compose up -d
set -eu

REPO="ennuiLife/SoundEra-Activity"
BASE_URL="${BASE_URL:-https://github.com/$REPO/releases}"
BIN="${BIN:-/usr/local/bin/soundera-activity}"
SERVICE="${SERVICE:-soundera-activity}"

say() { printf '%s\n' "$*"; }
die() { printf 'update: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Linux" ] && [ "$(uname -m)" = "x86_64" ] || die "the ready-made program is for Linux x86_64; build from source instead (see the README)"
[ -x "$BIN" ] || die "$BIN is not installed (set BIN=/path/to/soundera-activity)"

current="$("$BIN" --version | awk '{print $2}')"

if [ -n "${VERSION:-}" ]; then
    tag="$VERSION"
else
    # The address /releases/latest redirects to the newest tag: no API call, so no rate limit.
    final="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$BASE_URL/latest")" || die "could not reach GitHub"
    tag="${final##*/}"
fi
case "$tag" in v[0-9]*.[0-9]*.[0-9]*) ;; *) die "the newest version has an odd name: $tag" ;; esac
latest="${tag#v}"

if [ "$current" = "$latest" ] || [ "$(printf '%s\n%s\n' "$current" "$latest" | sort -V | tail -n1)" = "$current" ]; then
    say "soundera-activity $current is the newest version"
    exit 0
fi
say "soundera-activity $current -> $latest"
[ "${1:-}" = "--check" ] && exit 0

[ "$(id -u)" -eq 0 ] || [ "${NO_ROOT_CHECK:-}" = "1" ] || die "run this as root (sudo), or set NO_ROOT_CHECK=1 if you can write $BIN"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
name="soundera-activity-$tag-linux-x86_64.tar.gz"
for f in "$name" "$name.minisig"; do
    curl -fsSL -o "$work/$f" "$BASE_URL/download/$tag/$f" || die "could not download $f"
done

# 1. The signature, by the program already installed. Its signed comment must name this version and this file, so a
#    genuine old file cannot be passed off as the new one.
signed="$("$BIN" --verify "$work/$name" "$work/$name.minisig")" || die "the signature does not match: nothing was installed"
[ "$signed" = "version:$tag file:$name" ] || die "the signature is for something else ($signed): nothing was installed"
say "signature ok ($signed)"

tar -xzf "$work/$name" -C "$work"
new="$work/soundera-activity-$tag-linux-x86_64/soundera-activity"
[ -x "$new" ] || die "the download does not hold the program"
[ "$("$new" --version | awk '{print $2}')" = "$latest" ] || die "the program inside says another version: nothing was installed"

# 2. Install next to the old one and rename over it, so a failure never leaves half a program.
install -m 755 "$new" "$BIN.new"
mv -f "$BIN.new" "$BIN"
say "installed $BIN ($latest)"

if command -v systemctl >/dev/null 2>&1 && systemctl is-enabled "$SERVICE" >/dev/null 2>&1; then
    systemctl restart "$SERVICE"
    say "restarted $SERVICE"
else
    say "restart the service yourself to start using the new version"
fi
