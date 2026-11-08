# HyprPresenter

A lightweight presentation remote for [Hyprland](https://hypr.land): control
slides and a laser pointer from your Android phone over the local network —
no cloud, no mouse takeover, no persistent desktop GUI.

Think of it as a physical presentation clicker that happens to live in your
pocket and draw a laser dot on your screen instead of on the wall.

## Status — laser overlay proof of concept

The desktop foundation and a minimal laser overlay now work. There is still no
networking and no phone app.

Implemented:

- `hyprpresenter status` — verifies it is running inside a Hyprland session,
  reports the compositor version and all monitors (geometry, focus, active).
- `hyprpresenter probe` — connects to the Wayland display and reports which
  protocols the compositor actually advertises.
- `hyprpresenter overlay` — a transparent, click-through layer-shell surface on
  the selected output, rendering a red dot through `wl_shm`. Dot position,
  visibility, and shutdown are controlled through a temporary local UNIX
  socket. The dot starts at the logical centre of `eDP-1`.

Not implemented yet (by design):

- Networking (WebSocket server), pairing/authentication
- Slide prev/next input injection / virtual pointer
- Android/Flutter controller app
- Configuration, packaging, systemd unit

## Prerequisites

- Linux with [Hyprland](https://hypr.land) running (tested on 0.56.2)
- Rust 1.80+ (edition 2021; no nightly features used)
- For `probe` only: a working Wayland connection (`WAYLAND_DISPLAY` set)

## Development

```sh
cargo build            # debug build
cargo run -- status    # health check against the live Hyprland session
cargo run -- probe     # list compositor Wayland protocols
cargo run -- overlay   # run the laser overlay
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets
```

The binary is a single crate — no workspace, no feature flags, no config file
yet. Dependencies are deliberately few (`serde`, `serde_json`, `thiserror`,
`wayland-client`, `wayland-protocols`, `wayland-backend`, and
`smithay-client-toolkit`) because each additional dependency in this project
must earn its place.

## Laser overlay (proof of concept)

Run the overlay in one terminal:

```sh
cargo run -- overlay
# or explicitly:
cargo run -- overlay --output eDP-1 --radius 12
```

It creates one surface on the `overlay` layer covering the full selected
monitor, draws a red dot at the monitor's logical centre, and then waits.

From another terminal, control the running overlay:

```sh
cargo run -- overlay status        # output, logical surface, buffer, dot, scale
cargo run -- overlay move 200 300  # output-local logical pixels
cargo run -- overlay center        # back to the logical centre
cargo run -- overlay hide          # draw an empty (transparent) surface
cargo run -- overlay show          # draw the dot again
cargo run -- overlay quit          # clean shutdown: surface + socket removed
```

You can also set the initial position and radius at launch:

```sh
cargo run -- overlay --x 200 --y 300 --radius 16
```

`hyprpresenter overlay --help` lists all options.

### Coordinates and scaling

Control coordinates are **output-local logical pixels**, not physical pixels.
The size of that logical surface comes from the compositor's layer-shell
configure event. On the verification machine `eDP-1` is 1920×1200 physical at
scale 1.25, so its logical surface is 1536×960; `move 200 300` therefore puts
the dot's centre at physical pixel ≈(250, 375).

The overlay renders a buffer at the compositor's preferred fractional scale
(`wp_fractional_scale_v1`) and maps it onto the logical surface with
`wp_viewporter`. The observed buffer on `eDP-1` was 1920×1200 for a 1536×960
logical surface (`scale=1.250`). If either fractional-scale protocol is
unavailable, it falls back to `wl_surface.set_buffer_scale()` with the output's
integer scale.

### Input and layer behaviour

- Layer: `Overlay`, namespace `hyprpresenter-laser`.
- Keyboard interactivity: `None`; the overlay never takes keyboard focus.
- Input region: explicitly empty (`wl_surface.set_input_region` with an empty
  `wl_region`), so pointer buttons and motion pass through to the window below.
- Exclusive zone: `-1`, so the surface spans the full output (including space
  reserved by a top bar) instead of being shrunk to the usable area.
- Rendering: one ARGB8888 `wl_shm` buffer, reused for every frame; no GPU
  framework. Commands only update state and redraw once per event-loop batch,
  and a redraw waits for `wl_buffer.release` before reusing the slot, so rapid
  moves keep memory flat instead of growing the buffer pool.
- Cursor: never read, moved, hidden, or re-shaped by the overlay.

The test controls use `$XDG_RUNTIME_DIR/hyprpresenter-overlay.sock`. Set
`HYPRPRESENTER_SOCKET` to use a different path. This channel is intentionally
temporary and separate from the renderer so the future network command handler
can replace it without touching the drawing code. A stale socket is removed on
the next launch; `quit` shuts down without leaving the process, surface, or
socket behind.

### Verified behaviour

Tested on the actual session (Hyprland 0.57.0, `eDP-1` 1920×1200 @ scale 1.25):

- The dot rendered at physical (960, 600), the centre of the 1536×960 logical
  surface, with a ~29 px physical diameter for a 12 px logical radius.
- `move 200 300` rendered at physical ≈(250, 375); `hide` removed it; `show`
  and `center` restored it.
- A real left-button event at the unchanged cursor position reached an
  underlying `wev` window (press + release), confirming click-through.
- The focused window did not change when the overlay started or while it was
  displayed, confirming that it does not take keyboard focus.
- The dot remained visible over a fullscreen window on the same monitor.
- A burst of 500 moves plus 50 hide/show cycles completed without protocol
  errors; RSS stayed ≈13.3 MB and the buffer was reused (VSZ and fd count did
  not grow).
- `quit` removed the layer surface and the socket and exited the process.

Fullscreen visibility was tested with a fullscreen toplevel, not with every
possible fullscreen client or direct-scanout/HDR path.

## Planned architecture

```
┌──────────────────────┐        Wi-Fi (LAN only)        ┌──────────────────────┐
│  Android app (future)│  ◄──── WebSocket + pairing ───► │  hyprpresenterd      │
│  Flutter             │      (this is Phase 5+)         │  (Rust daemon)       │
│  • prev/next buttons │                                 │  • networking layer  │
│  • laser hold button │                                 │  • pairing/auth      │
│  • orientation sens. │                                 │  • input adapter     │
└──────────────────────┘                                 │  • overlay renderer  │
                                                         └──────────────────────┘
```

The desktop side will be split into focused modules rather than a framework:

- `overlay` — implemented in `src/overlay.rs`: a transparent, click-through,
  always-on-top laser dot on the `overlay` wlr-layer-shell layer. The proof of
  concept renders one surface on the selected output; the command/state split
  is ready for the future network layer. Built with `smithay-client-toolkit` +
  `wayland-client` (the same general approach as
  [hyprlaser](https://github.com/dcaixinha/hyprlaser), which is Apache-2.0
  and serves as a technical reference, not copied code — this repo is MIT and
  must stay license-clean).
- `input` — presentation-control adapter. Prev/next are injected through
  `zwlr_virtual_pointer_v1` (button/key events) so any presentation app
  (Impress, PDF viewer, browser slides) works without being special-cased.
- `net` — LAN WebSocket server with a pairing step (likely QR-provisioned
  token) before any command is accepted.
- `main` — CLI entry point and lifecycle glue.

### Design notes / constraints carried forward

- **The laser dot must be independent of the real mouse cursor.** The phone
  drives an in-overlay coordinate, not `zwlr_virtual_pointer` motion, so the
  user's actual mouse is untouched. Virtual pointer is only for slide
  prev/next clicks. Hyprland supports
  [`zwlr_virtual_pointer_v1` v2](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1)
  (verified on 0.57.0).
- **Click-through is mandatory.** Layer-shell overlay layer + empty input
  region. Known Hyprland quirk (hyprwm/Hyprland#14136-style discussions):
  exclusive keyboard interactivity on an overlay layer can capture pointer
  events even with an empty input region; the overlay uses
  `keyboard_interactivity: none` to avoid that path entirely, and input
  pass-through has been verified on the real session.
- **Fractional scaling** on Hyprland needs `wp_fractional_scale_v1` +
  `wp_viewporter`; Hyprland advertises both (confirmed on this machine), and
  the overlay uses them when present.
- **Layer-shell sizing**: with all four edges anchored, `set_size(0, 0)` lets
  the compositor report the full logical output size. `exclusive_zone(-1)` is
  required for the surface to keep the full output height instead of being
  shrunk by an existing top panel's exclusive zone.
- **Multiple monitors**: the proof of concept draws on one selected output
  (`--output`, default `eDP-1`). The intended next step is one overlay surface
  per `wl_output`, positioned through Hyprland IPC (`j/monitors`) global layout
  coordinates.

## License

MIT — see [LICENSE](LICENSE). The upstream `hyprlaser` project is
Apache-2.0 and is used only as *architecture-level* inspiration; no source is
copied. If that ever changes (e.g. vendoring actual code), the license will
be revisited before the code lands.
