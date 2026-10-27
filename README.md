# HyprPresenter

A lightweight presentation remote for [Hyprland](https://hypr.land): control
slides and a laser pointer from your Android phone over the local network —
no cloud, no mouse takeover, no persistent desktop GUI.

Think of it as a physical presentation clicker that happens to live in your
pocket and draw a laser dot on your screen instead of on the wall.

## Status — foundation phase

**This repository currently contains the inspection/health-check layer only.**
Nothing renders yet; there is no networking and no phone app.

Implemented:

- `hyprpresenter status` — verifies it is running inside a Hyprland session,
  reports the compositor version and all monitors (geometry, focus, active).
- `hyprpresenter probe` — connects to the Wayland display and reports which
  protocols the compositor actually advertises, so overlay feasibility is
  verifiable before any overlay code is written.

Not implemented yet (by design, see "Planned phases"):

- Laser overlay rendering
- Networking (WebSocket server), pairing/authentication
- Slide prev/next input injection
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
cargo fmt --check
cargo check
cargo test
```

The binary is a single crate — no workspace, no feature flags, no config file
yet. Dependencies are deliberately few (`serde`, `serde_json`, `thiserror`,
`wayland-client`) because each additional dependency in this project must earn
its place.

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

- `overlay` — transparent, click-through, always-on-top laser dot on the
  `overlay` wlr-layer-shell layer, one surface per output. Will use
  `smithay-client-toolkit` + `wayland-client` (same approach as
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
  (verified on 0.56.2).
- **Click-through is mandatory.** Layer-shell overlay layer + empty input
  region. Known Hyprland quirk (hyprwm/Hyprland#14136-style discussions):
  exclusive keyboard interactivity on an overlay layer can capture pointer
  events even with an empty input region; we will use
  `keyboard_interactivity: none` to avoid that path entirely.
- **Fractional scaling** on Hyprland needs `wp_fractional_scale_v1` +
  `wp_viewporter`; Hyprland advertises both (confirmed on this machine).
- **Multiple monitors**: one overlay surface per `wl_output`, positioned
  through Hyprland IPC (`j/monitors`) global layout coordinates.

## License

MIT — see [LICENSE](LICENSE). The upstream `hyprlaser` project is
Apache-2.0 and is used only as *architecture-level* inspiration; no source is
copied. If that ever changes (e.g. vendoring actual code), the license will
be revisited before the code lands.
