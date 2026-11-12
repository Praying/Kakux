#!/usr/bin/env bash
# Installs a Linux build of Kaku for the current user (or a given prefix):
#   - kaku / kaku-gui symlinks onto the multicall binary
#   - the hicolor icon set (resolved by compositors through the window's
#     app_id, fun.tw93.kaku)
#   - a desktop entry so the app menu and taskbar show name and icon
#
# Usage: scripts/install_linux.sh [--prefix DIR] [--bin PATH]
#   --prefix DIR   install root, default ~/.local (use /usr/local for system)
#   --bin PATH     the built multicall binary, default: first of
#                  target/release/kaku-gui, target/debug/kaku-gui,
#                  $(command -v kaku-gui)
#
# The `k` chat alias is NOT installed by default: the one-character name
# is commonly taken (kubectl users, fish abbreviations). Run the binary
# as `k` from a directory of your choice, or use `kaku chat`.

set -euo pipefail

cd "$(dirname "$0")/.."

PREFIX="${HOME}/.local"
BIN=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--prefix) PREFIX="$2"; shift 2 ;;
	--bin) BIN="$2"; shift 2 ;;
	*) echo "unknown argument: $1" >&2; exit 1 ;;
	esac
done

if [[ -z "$BIN" ]]; then
	for candidate in target/release/kaku-gui target/debug/kaku-gui; do
		if [[ -x "$candidate" ]]; then BIN="$candidate"; break; fi
	done
	if [[ -z "$BIN" ]]; then
		BIN="$(command -v kaku-gui || true)"
	fi
fi
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
	echo "error: no kaku-gui binary found; build one with:" >&2
	echo "  cargo build --release -p kaku-gui --bin kaku-gui" >&2
	echo "or pass --bin /path/to/kaku-gui" >&2
	exit 1
fi
BIN="$(readlink -f "$BIN")"

BIN_DIR="$PREFIX/bin"
mkdir -p "$BIN_DIR" "$PREFIX/share/applications"

ln -sf "$BIN" "$BIN_DIR/kaku"
ln -sf "$BIN" "$BIN_DIR/kaku-gui"

find assets/linux/icons/hicolor -name '*.png' | while read -r icon; do
	rel="${icon#assets/linux/icons/}"
	install -Dm644 "$icon" "$PREFIX/share/icons/$rel"
done

sed "s|^Exec=@EXEC@|Exec=$BIN_DIR/kaku-gui start|" \
	assets/linux/fun.tw93.kaku.desktop \
	> "$PREFIX/share/applications/fun.tw93.kaku.desktop"

update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 --noincremental 2>/dev/null || true

echo "installed into $PREFIX:"
echo "  $BIN_DIR/kaku, $BIN_DIR/kaku-gui -> $BIN"
echo "  icons for app_id fun.tw93.kaku"
echo "  $PREFIX/share/applications/fun.tw93.kaku.desktop"
