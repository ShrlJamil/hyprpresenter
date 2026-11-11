#!/usr/bin/env bash
# HyprPresenter installer: builds the desktop program and links it into
# ~/.local/bin, so `hypresent` works from any terminal afterwards.
#
#   ./install.sh              build (if needed) + symlink
#   ./install.sh --link-only  skip the build, just (re)create the symlink
#
# Re-running is safe: the release build is incremental and the symlink
# is simply repointed at the fresh binary.
set -euo pipefail

cd "$(dirname "$0")"
LINK_ONLY=0
[ "${1:-}" = "--link-only" ] && LINK_ONLY=1
[ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ] && {
  sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

BIN_NAME="hypresent"
TARGET_DIR="$PWD/target/release"
LINK_DIR="$HOME/.local/bin"

if [ "$LINK_ONLY" -eq 0 ]; then
  # --- Rust toolchain -------------------------------------------------
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed (cargo not found)."
    if command -v rustup >/dev/null 2>&1 || [ -t 0 ]; then
      read -r -p "Install Rust now via rustup? [y/N] " answer
      case "$answer" in
        y|Y)
          curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --default-toolchain stable
          export PATH="$HOME/.cargo/bin:$PATH"
          ;;
        *) echo "Aborting. Install Rust from https://rustup.rs and re-run."; exit 1 ;;
      esac
    else
      echo "Install Rust from https://rustup.rs and re-run this script."
      exit 1
    fi
  fi

  # --- Wayland headers (needed to compile) ----------------------------
  if command -v pkg-config >/dev/null 2>&1 \
     && ! pkg-config --exists wayland-client 2>/dev/null; then
    echo "Warning: wayland-client development files not found."
    echo "If the build fails, install them first, e.g.:"
    echo "  Arch:   sudo pacman -S wayland"
    echo "  Fedora: sudo dnf install wayland-devel"
    echo "  Debian: sudo apt install libwayland-dev"
  fi

  echo "Building hypresent (release)..."
  cargo build --release
fi

[ -x "$TARGET_DIR/$BIN_NAME" ] || {
  echo "No binary at $TARGET_DIR/$BIN_NAME — run without --link-only first."
  exit 1
}

mkdir -p "$LINK_DIR"
ln -sf "$TARGET_DIR/$BIN_NAME" "$LINK_DIR/$BIN_NAME"
echo "Linked: $LINK_DIR/$BIN_NAME -> $TARGET_DIR/$BIN_NAME"

case ":$PATH:" in
  *":$LINK_DIR:"*) ;;
  *) echo "Note: $LINK_DIR is not in your PATH yet."
     echo "Most desktops add it automatically at next login; for this"
     echo "terminal run:  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac

cat <<EOF

Done. Next steps:
  1. Start the overlay:   hypresent overlay
  2. Pair your phone:     hypresent pair        (in another terminal)
  3. Install the Android app (see README, "Setup" step 3) and scan the QR.
EOF
