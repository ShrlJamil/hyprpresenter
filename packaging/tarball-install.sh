#!/bin/sh
# HyprPresenter release installer: copies the prebuilt `hypresent`
# binary from this tarball into ~/.local/bin, so `hypresent` works
# from any terminal afterwards. No build, no dependencies.
#
#   ./install.sh
#
# Re-running is safe: the binary is simply copied over the old one.
set -eu

cd "$(dirname "$0")"

BIN_DIR="$HOME/.local/bin"
mkdir -p "$BIN_DIR"
cp ./hypresent "$BIN_DIR/hypresent"
chmod 755 "$BIN_DIR/hypresent"
echo "Installed: $BIN_DIR/hypresent"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo "Note: $BIN_DIR is not on your PATH yet. Add it to your shell profile:"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    ;;
esac
