//! Transport-agnostic command core (Phase 4).
//!
//! Everything in this module is pure data and pure functions: no Wayland
//! connection, no sockets, no I/O.  The local UNIX-socket handler in
//! `overlay.rs` and the WebSocket handler in `net.rs` are both thin adapters over
//! this core — they parse a transport-specific request into a [`Command`],
//! hand it to [`LaserState::apply`], and act on the returned [`Outcome`]
//! (reply text, redraw flag, and at most one side-effecting [`Action`]).
//!
//! Keeping validation and state transitions here means they are unit-tested
//! without a compositor and cannot drift between transports.

use crate::input::PresentationKey;

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// A strictly-validated sRGB colour.  The overlay has no separate opacity
/// setting: the dot is fully opaque in the centre and only the anti-aliased
/// edge is partially transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const RED: Self = Self {
        r: 0xff,
        g: 0,
        b: 0,
    };

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Named colours accepted by `overlay color`.  Hex input is `#RRGGBB`.
const NAMED_COLORS: &[(&str, Rgb)] = &[
    (
        "red",
        Rgb {
            r: 0xff,
            g: 0,
            b: 0,
        },
    ),
    (
        "green",
        Rgb {
            r: 0,
            g: 0xff,
            b: 0,
        },
    ),
    (
        "blue",
        Rgb {
            r: 0,
            g: 0,
            b: 0xff,
        },
    ),
    (
        "yellow",
        Rgb {
            r: 0xff,
            g: 0xff,
            b: 0,
        },
    ),
    (
        "cyan",
        Rgb {
            r: 0,
            g: 0xff,
            b: 0xff,
        },
    ),
    (
        "magenta",
        Rgb {
            r: 0xff,
            g: 0,
            b: 0,
        },
    ),
    (
        "white",
        Rgb {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        },
    ),
    (
        "orange",
        Rgb {
            r: 0xff,
            g: 0x80,
            b: 0,
        },
    ),
];

/// Parse a named colour (case-insensitive) or a strict `#RRGGBB` hex value.
pub fn parse_color(raw: &str) -> std::result::Result<Rgb, String> {
    let lower = raw.to_ascii_lowercase();
    if let Some((_, color)) = NAMED_COLORS.iter().find(|(name, _)| *name == lower) {
        return Ok(*color);
    }
    let hex = lower
        .strip_prefix('#')
        .ok_or_else(|| format!("color `{raw}` must be a name or #RRGGBB"))?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("color `{raw}` must be #RRGGBB (six hex digits)"));
    }
    let channel = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).expect("validated hex digits")
    };
    Ok(Rgb {
        r: channel(0..2),
        g: channel(2..4),
        b: channel(4..6),
    })
}

// ---------------------------------------------------------------------------
// Appearance
// ---------------------------------------------------------------------------

/// Default dot radius in logical pixels.
pub const DEFAULT_RADIUS: i32 = 12;
/// Smallest and largest accepted dot radius, in logical pixels.
pub const MIN_RADIUS: i32 = 1;
pub const MAX_RADIUS: i32 = 500;

/// Parse and range-check a dot radius.
pub fn parse_radius(raw: &str) -> std::result::Result<i32, String> {
    let radius = raw
        .parse::<i32>()
        .map_err(|_| format!("radius must be an integer, got `{raw}`"))?;
    check_radius(radius)?;
    Ok(radius)
}

/// Range-check an already-numeric radius (used by the parser and again by
/// the state machine, so a programmatically built command cannot smuggle an
/// out-of-range value past the text parser).
pub fn check_radius(radius: i32) -> std::result::Result<(), String> {
    if !(MIN_RADIUS..=MAX_RADIUS).contains(&radius) {
        return Err(format!(
            "radius must be between {MIN_RADIUS} and {MAX_RADIUS} logical pixels, got {radius}"
        ));
    }
    Ok(())
}

/// Rendering properties that can change at runtime (and, later, come from
/// the Android command handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    pub radius: i32,
    pub color: Rgb,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            radius: DEFAULT_RADIUS,
            color: Rgb::RED,
        }
    }
}

// ---------------------------------------------------------------------------
// Global layout geometry
// ---------------------------------------------------------------------------

/// One output's rectangle in the **global logical** coordinate space.
///
/// `x`/`y` are the output's logical position in the compositor layout (as
/// reported by Hyprland IPC `j/monitors`, whose positions are already
/// logical); `width`/`height` are the logical size (physical mode size
/// divided by the output scale).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRect {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl OutputRect {
    /// Logical centre of this output, in global coordinates.
    pub fn center(&self) -> (i32, i32) {
        (
            self.x + self.width as i32 / 2,
            self.y + self.height as i32 / 2,
        )
    }

    /// Does the global point fall on this output?
    pub fn contains(&self, gx: i32, gy: i32) -> bool {
        gx >= self.x
            && gx < self.x + self.width as i32
            && gy >= self.y
            && gy < self.y + self.height as i32
    }

    /// Clamp a global point into this output, inset by `margin` on every
    /// side (used with the dot radius so the whole dot stays on the output
    /// it is confined to).
    pub fn clamp_inside(&self, x: i32, y: i32, margin: i32) -> (i32, i32) {
        let max_x = self.x + self.width as i32 - 1;
        let max_y = self.y + self.height as i32 - 1;
        (
            x.clamp(self.x + margin, (max_x - margin).max(self.x + margin)),
            y.clamp(self.y + margin, (max_y - margin).max(self.y + margin)),
        )
    }

    /// Convert a global point to output-local logical pixels, or `None`
    /// when the point is not on this output.
    pub fn to_local(&self, gx: i32, gy: i32) -> Option<(i32, i32)> {
        self.contains(gx, gy).then_some((gx - self.x, gy - self.y))
    }
}

/// The set of outputs the laser can be drawn on, in global logical space.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub outputs: Vec<OutputRect>,
    /// Name of the focused output, when the compositor reports one.  Used
    /// as the target of the `center` command.
    pub focused: Option<String>,
}

impl Layout {
    pub fn new(outputs: Vec<OutputRect>, focused: Option<String>) -> Self {
        Self { outputs, focused }
    }

    /// Union bounds of all outputs as `(min_x, min_y, max_x, max_y)`
    /// (max is exclusive).  `None` when there are no outputs.
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let mut iter = self.outputs.iter();
        let first = iter.next()?;
        let (mut min_x, mut min_y) = (first.x, first.y);
        let (mut max_x, mut max_y) = (first.x + first.width as i32, first.y + first.height as i32);
        for o in iter {
            min_x = min_x.min(o.x);
            min_y = min_y.min(o.y);
            max_x = max_x.max(o.x + o.width as i32);
            max_y = max_y.max(o.y + o.height as i32);
        }
        Some((min_x, min_y, max_x, max_y))
    }

    /// Clamp a global point into the union bounds.  With no outputs the
    /// point is only kept non-negative, matching the pre-Phase-4 behaviour
    /// before the surface size was known.
    pub fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        match self.bounds() {
            Some((min_x, min_y, max_x, max_y)) => (
                x.clamp(min_x, max_x.saturating_sub(1)),
                y.clamp(min_y, max_y.saturating_sub(1)),
            ),
            None => (x.max(0), y.max(0)),
        }
    }

    /// The output a global point falls on, if any.  A point can fall into
    /// dead space between outputs in non-rectangular layouts.
    pub fn output_at(&self, x: i32, y: i32) -> Option<&OutputRect> {
        self.outputs.iter().find(|o| o.contains(x, y))
    }

    /// Look an output up by name.
    pub fn output_named(&self, name: &str) -> Option<&OutputRect> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// Centre of the focused output, falling back to the centre of the
    /// union bounds, then to the origin.
    pub fn center(&self) -> (i32, i32) {
        if let Some(name) = &self.focused {
            if let Some(output) = self.outputs.iter().find(|o| &o.name == name) {
                return output.center();
            }
        }
        match self.bounds() {
            Some((min_x, min_y, max_x, max_y)) => ((min_x + max_x) / 2, (min_y + max_y) / 2),
            None => (0, 0),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands and state transitions
// ---------------------------------------------------------------------------

/// One transport-agnostic command.  Adapters parse their wire format into
/// this; the variants carry already-validated values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Move the dot to a global logical position.
    Move(i32, i32),
    /// Move the dot to the centre of the focused output.
    Center,
    Show,
    Hide,
    Radius(i32),
    Color(Rgb),
    Previous,
    Next,
    Status,
    Quit,
    /// Confine the dot to one output (Phase 6.2): `Some(name)` clamps all
    /// movement into that output and retargets `center` at it; `None`
    /// releases the dot to roam the whole layout again.
    Confine(Option<String>),
    /// Fullscreen the compositor's **active window** (the presentation,
    /// not the phone UI): `None` toggles, `Some(true/false)` sets it
    /// explicitly.  Like navigation, it is an adapter side effect — the
    /// core only records the intent.
    Fullscreen(Option<bool>),
}

/// Parse the line-based command language used by the local control socket
/// (and by the CLI, which forwards through that socket).  The WebSocket
/// handler builds `Command` values from JSON instead (see `net.rs`).
pub fn parse_command(raw: &str) -> std::result::Result<Command, String> {
    let mut parts = raw.split_whitespace();
    let command = match parts.next() {
        Some("move") => {
            let x = parts
                .next()
                .ok_or_else(|| "move requires X Y".to_string())?
                .parse::<i32>()
                .map_err(|_| "move X must be an integer".to_string())?;
            let y = parts
                .next()
                .ok_or_else(|| "move requires X Y".to_string())?
                .parse::<i32>()
                .map_err(|_| "move Y must be an integer".to_string())?;
            Command::Move(x, y)
        }
        Some("center") => Command::Center,
        Some("show") => Command::Show,
        Some("hide") => Command::Hide,
        Some("radius") => Command::Radius(parse_radius(
            parts
                .next()
                .ok_or_else(|| "radius requires N".to_string())?,
        )?),
        Some("color") => Command::Color(parse_color(
            parts
                .next()
                .ok_or_else(|| "color requires NAME or #RRGGBB".to_string())?,
        )?),
        Some("previous") | Some("prev") => Command::Previous,
        Some("next") => Command::Next,
        Some("status") => Command::Status,
        Some("quit") | Some("exit") => Command::Quit,
        Some("confine") => {
            let target = parts
                .next()
                .ok_or_else(|| "confine requires an output name or `off`".to_string())?;
            if target == "off" || target == "none" {
                Command::Confine(None)
            } else {
                Command::Confine(Some(target.to_string()))
            }
        }
        Some("fullscreen") => match parts.next() {
            None => Command::Fullscreen(None),
            Some("on") => Command::Fullscreen(Some(true)),
            Some("off") => Command::Fullscreen(Some(false)),
            Some(other) => {
                return Err(format!(
                    "fullscreen takes `on`, `off`, or no argument (got `{other}`)"
                ));
            }
        },
        Some(other) => return Err(format!("unknown command `{other}`")),
        None => return Err("empty command".to_string()),
    };
    if parts.next().is_some() {
        return Err("too many arguments".to_string());
    }
    Ok(command)
}

/// A side effect the adapter must perform on behalf of the core.  The core
/// never touches Wayland or the process lifecycle itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Inject a presentation key through the input adapter.
    Navigate(PresentationKey),
    /// Fullscreen (`Some(true)`), un-fullscreen (`Some(false)`), or
    /// toggle (`None`) the compositor's active window via IPC.
    Fullscreen(Option<bool>),
    /// Shut the overlay down cleanly.
    Quit,
}

/// The result of applying one command: the reply text for the transport,
/// whether any surface needs a redraw, and an optional side effect.
///
/// For [`Action::Navigate`] the reply is empty: the adapter performs the
/// injection and produces the final reply, because only it knows whether
/// the virtual keyboard is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub reply: String,
    pub redraw: bool,
    pub action: Option<Action>,
}

impl Outcome {
    fn plain(reply: String, redraw: bool) -> Self {
        Self {
            reply,
            redraw,
            action: None,
        }
    }
}

/// The shared laser state: dot position in **global logical coordinates**,
/// visibility, and appearance.  One value is shared by every output
/// surface, so the dot can travel across monitors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaserState {
    /// Output the dot is confined to (Phase 6.2), if any.  While set, moves
    /// clamp into that output (inset by the dot radius) and `center`
    /// targets it.  A name that no longer exists in the layout is ignored
    /// (treated as unconfined) rather than trapping the dot.
    pub confined: Option<String>,
    pub pos: Option<(i32, i32)>,
    /// True while the dot tracks the layout centre (set at startup and by
    /// `center`); cleared by an explicit `move`.
    pub center_mode: bool,
    pub visible: bool,
    pub appearance: Appearance,
}

impl Default for LaserState {
    fn default() -> Self {
        Self {
            confined: None,
            pos: None,
            center_mode: true,
            visible: true,
            appearance: Appearance::default(),
        }
    }
}

impl LaserState {
    /// Apply one command against the current layout.  Pure: no I/O, no
    /// Wayland, fully unit-testable.
    pub fn apply(&mut self, command: Command, layout: &Layout) -> Outcome {
        match command {
            Command::Move(x, y) => {
                let (x, y) = match self.confined_output(layout) {
                    Some(output) => output.clamp_inside(x, y, self.appearance.radius),
                    None => layout.clamp(x, y),
                };
                self.pos = Some((x, y));
                self.center_mode = false;
                Outcome::plain(format!("ok dot={x},{y}"), true)
            }
            Command::Center => {
                let (x, y) = match self.confined_output(layout) {
                    Some(output) => output.center(),
                    None => layout.center(),
                };
                self.center_mode = true;
                self.pos = Some((x, y));
                Outcome::plain(format!("ok dot={x},{y}"), true)
            }
            Command::Confine(target) => match target {
                Some(name) => match layout.output_named(&name) {
                    Some(output) => {
                        self.confined = Some(name.clone());
                        // Snap a dot that is stranded outside the chosen
                        // output into it right away.
                        if let Some((x, y)) = self.pos {
                            self.pos = Some(output.clamp_inside(x, y, self.appearance.radius));
                        }
                        Outcome::plain(format!("ok confined={name}"), true)
                    }
                    None => {
                        let available = layout
                            .outputs
                            .iter()
                            .map(|o| o.name.as_str())
                            .collect::<Vec<_>>()
                            .join(",");
                        Outcome::plain(
                            format!("error: unknown output `{name}` (available: {available})"),
                            false,
                        )
                    }
                },
                None => {
                    self.confined = None;
                    Outcome::plain("ok confined=none".to_string(), false)
                }
            },
            Command::Show => {
                self.visible = true;
                Outcome::plain("ok visible=1".to_string(), true)
            }
            Command::Hide => {
                self.visible = false;
                Outcome::plain("ok visible=0".to_string(), true)
            }
            Command::Radius(radius) => {
                if let Err(e) = check_radius(radius) {
                    return Outcome::plain(format!("error: {e}"), false);
                }
                let redraw = self.appearance.radius != radius;
                self.appearance.radius = radius;
                Outcome::plain(format!("ok radius={radius}"), redraw)
            }
            Command::Color(color) => {
                let redraw = self.appearance.color != color;
                self.appearance.color = color;
                Outcome::plain(format!("ok color={}", color.to_hex()), redraw)
            }
            Command::Fullscreen(on) => Outcome {
                reply: String::new(),
                redraw: false,
                action: Some(Action::Fullscreen(on)),
            },
            Command::Previous => Outcome {
                reply: String::new(),
                redraw: false,
                action: Some(Action::Navigate(PresentationKey::Previous)),
            },
            Command::Next => Outcome {
                reply: String::new(),
                redraw: false,
                action: Some(Action::Navigate(PresentationKey::Next)),
            },
            Command::Status => Outcome::plain(self.state_line(layout), false),
            Command::Quit => Outcome {
                reply: "ok shutting down".to_string(),
                redraw: false,
                action: Some(Action::Quit),
            },
        }
    }

    /// The confined output resolved against the current layout, if the
    /// confinement is set and that output still exists.
    fn confined_output<'a>(&self, layout: &'a Layout) -> Option<&'a OutputRect> {
        self.confined
            .as_deref()
            .and_then(|name| layout.output_named(name))
    }

    /// The transport-independent part of the `status` reply: dot position
    /// (global), the output the dot is on, visibility, and appearance.
    /// Adapters append their own session details (buffers, keyboard, …).
    pub fn state_line(&self, layout: &Layout) -> String {
        let (x, y) = self.pos.unwrap_or((0, 0));
        let on = layout
            .output_at(x, y)
            .map(|o| o.name.as_str())
            .unwrap_or("none");
        let confined = self.confined.as_deref().unwrap_or("none");
        let areas = layout
            .outputs
            .iter()
            .map(|o| format!("{}:{},{},{},{}", o.name, o.x, o.y, o.width, o.height))
            .collect::<Vec<_>>()
            .join(";");
        format!(
            "dot={x},{y} on={on} visible={} radius={} color={} confined={confined} areas={areas}",
            u8::from(self.visible),
            self.appearance.radius,
            self.appearance.color.to_hex(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verification machine's real layout: eDP-1 is 1920x1200 physical
    /// at scale 1.25 (1536x960 logical) positioned left of and slightly
    /// below DP-1 (1920x1080 at scale 1.0).
    fn real_layout() -> Layout {
        Layout::new(
            vec![
                OutputRect {
                    name: "eDP-1".to_string(),
                    x: -256,
                    y: 120,
                    width: 1536,
                    height: 960,
                },
                OutputRect {
                    name: "DP-1".to_string(),
                    x: 1280,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            ],
            Some("eDP-1".to_string()),
        )
    }

    #[test]
    fn fullscreen_is_an_adapter_action_with_no_core_reply() {
        // The core cannot talk to the compositor; like navigation, a
        // fullscreen command must surface as an Action with an empty
        // reply for the adapter to complete.
        let layout = real_layout();
        let mut state = LaserState::default();
        let out = state.apply(Command::Fullscreen(None), &layout);
        assert_eq!(out.action, Some(Action::Fullscreen(None)));
        assert_eq!(out.reply, "");
        assert!(!out.redraw);
        let out = state.apply(Command::Fullscreen(Some(true)), &layout);
        assert_eq!(out.action, Some(Action::Fullscreen(Some(true))));
    }

    #[test]
    fn confinement_clamps_moves_and_retargets_center() {
        let layout = real_layout();
        let mut state = LaserState::default();
        // Confine to DP-1 (1280,0 1920x1080), radius 12.
        let out = state.apply(Command::Confine(Some("DP-1".to_string())), &layout);
        assert_eq!(out.reply, "ok confined=DP-1");
        // A move aimed at eDP-1 lands clamped inside DP-1, inset by radius.
        let out = state.apply(Command::Move(0, 600), &layout);
        assert_eq!(state.pos, Some((1280 + 12, 600)));
        assert!(out.reply.starts_with("ok dot=1292,600"));
        // Center now targets DP-1's centre, not the focused output's.
        let out = state.apply(Command::Center, &layout);
        assert_eq!(state.pos, Some((2240, 540)));
        assert_eq!(out.reply, "ok dot=2240,540");
        // Release: moves roam the whole layout again.
        state.apply(Command::Confine(None), &layout);
        state.apply(Command::Move(0, 600), &layout);
        assert_eq!(state.pos, Some((0, 600)));
    }

    #[test]
    fn confinement_rejects_unknown_outputs_and_reports_state() {
        let layout = real_layout();
        let mut state = LaserState::default();
        let out = state.apply(Command::Confine(Some("HDMI-9".to_string())), &layout);
        assert!(out.reply.starts_with("error: unknown output `HDMI-9`"));
        assert_eq!(state.confined, None);
        state.apply(Command::Confine(Some("eDP-1".to_string())), &layout);
        let line = state.state_line(&layout);
        assert!(line.contains("confined=eDP-1"), "{line}");
        assert!(
            line.contains("areas=eDP-1:-256,120,1536,960;DP-1:1280,0,1920,1080"),
            "{line}"
        );
        // Text protocol parses both forms.
        assert_eq!(
            parse_command("confine DP-1"),
            Ok(Command::Confine(Some("DP-1".to_string())))
        );
        assert_eq!(parse_command("confine off"), Ok(Command::Confine(None)));
        assert_eq!(parse_command("fullscreen"), Ok(Command::Fullscreen(None)));
        assert_eq!(
            parse_command("fullscreen on"),
            Ok(Command::Fullscreen(Some(true)))
        );
        assert_eq!(
            parse_command("fullscreen off"),
            Ok(Command::Fullscreen(Some(false)))
        );
        assert!(parse_command("fullscreen wide").is_err());
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command("move 12 34"), Ok(Command::Move(12, 34)));
        assert_eq!(parse_command("move -5 0"), Ok(Command::Move(-5, 0)));
        assert_eq!(parse_command("center"), Ok(Command::Center));
        assert_eq!(parse_command("show"), Ok(Command::Show));
        assert_eq!(parse_command("hide"), Ok(Command::Hide));
        assert_eq!(parse_command("radius 12"), Ok(Command::Radius(12)));
        assert_eq!(parse_command("radius 1"), Ok(Command::Radius(1)));
        assert_eq!(parse_command("radius 500"), Ok(Command::Radius(500)));
        assert_eq!(
            parse_command("color #FF3333"),
            Ok(Command::Color(Rgb {
                r: 0xff,
                g: 0x33,
                b: 0x33
            }))
        );
        assert_eq!(
            parse_command("color blue"),
            Ok(Command::Color(Rgb {
                r: 0,
                g: 0,
                b: 0xff
            }))
        );
        assert_eq!(parse_command("previous"), Ok(Command::Previous));
        assert_eq!(parse_command("prev"), Ok(Command::Previous));
        assert_eq!(parse_command("next"), Ok(Command::Next));
        assert_eq!(parse_command("status"), Ok(Command::Status));
        assert_eq!(parse_command("quit"), Ok(Command::Quit));
        assert_eq!(parse_command("exit"), Ok(Command::Quit));

        assert!(parse_command("").is_err());
        assert!(parse_command("move 1").is_err());
        assert!(parse_command("move 1 2 3").is_err());
        assert!(parse_command("move a b").is_err());
        assert!(parse_command("radius 0").is_err());
        assert!(parse_command("radius 501").is_err());
        assert!(parse_command("radius -3").is_err());
        assert!(parse_command("radius 1.5").is_err());
        assert!(parse_command("radius").is_err());
        assert!(parse_command("radius 1 2").is_err());
        assert!(parse_command("color #ff33").is_err());
        assert!(parse_command("color #gggggg").is_err());
        assert!(parse_command("color 123456").is_err());
        assert!(parse_command("color").is_err());
        assert!(parse_command("color red blue").is_err());
        assert!(parse_command("nope").is_err());
    }

    #[test]
    fn parses_named_and_hex_colors() {
        assert_eq!(parse_color("red"), Ok(Rgb::RED));
        assert_eq!(
            parse_color("GrEeN"),
            Ok(Rgb {
                r: 0,
                g: 0xff,
                b: 0
            })
        );
        assert_eq!(
            parse_color("#00ff80"),
            Ok(Rgb {
                r: 0,
                g: 0xff,
                b: 0x80
            })
        );
        assert!(parse_color("reddish").is_err());
        assert!(parse_color("#f00").is_err());
        assert!(parse_color("#ff00000").is_err());
        assert!(parse_color("#ff00zz").is_err());
    }

    #[test]
    fn layout_geometry_converts_global_to_local() {
        let layout = real_layout();
        let edp = &layout.outputs[0];
        // eDP-1 centre in global space is its local centre offset by origin.
        assert_eq!(edp.center(), (512, 600));
        assert_eq!(edp.to_local(512, 600), Some((768, 480)));
        // A point on DP-1 is not on eDP-1.
        assert_eq!(edp.to_local(2240, 540), None);
        let dp = &layout.outputs[1];
        assert_eq!(dp.to_local(2240, 540), Some((960, 540)));
        assert_eq!(
            layout.output_at(2240, 540).map(|o| o.name.as_str()),
            Some("DP-1")
        );
        assert_eq!(
            layout.output_at(512, 600).map(|o| o.name.as_str()),
            Some("eDP-1")
        );
        // Dead space above eDP-1 (it starts at y=120) belongs to no output.
        assert!(layout.output_at(0, 0).is_none());
    }

    #[test]
    fn layout_clamps_and_centers() {
        let layout = real_layout();
        // Union bounds: x in [-256, 3200), y in [0, 1080).
        assert_eq!(layout.bounds(), Some((-256, 0, 3200, 1080)));
        assert_eq!(layout.clamp(-9999, 9999), (-256, 1079));
        assert_eq!(layout.clamp(512, 600), (512, 600));
        // Centre targets the focused output (eDP-1), not the union centre.
        assert_eq!(layout.center(), (512, 600));

        let empty = Layout::default();
        assert_eq!(empty.bounds(), None);
        assert_eq!(empty.clamp(-5, -5), (0, 0));
        assert_eq!(empty.center(), (0, 0));
    }

    #[test]
    fn state_transitions_move_and_center() {
        let layout = real_layout();
        let mut state = LaserState::default();
        assert!(state.center_mode);
        assert_eq!(state.pos, None);

        let out = state.apply(Command::Center, &layout);
        assert_eq!(out.reply, "ok dot=512,600");
        assert!(out.redraw);
        assert_eq!(out.action, None);
        assert_eq!(state.pos, Some((512, 600)));

        // A move is clamped into the union bounds and clears center mode.
        let out = state.apply(Command::Move(99999, -50), &layout);
        assert_eq!(out.reply, "ok dot=3199,0");
        assert_eq!(state.pos, Some((3199, 0)));
        assert!(!state.center_mode);
    }

    #[test]
    fn state_transitions_appearance_and_visibility() {
        let layout = real_layout();
        let mut state = LaserState::default();

        // Radius: a real change redraws, a no-op does not, invalid errors.
        let out = state.apply(Command::Radius(24), &layout);
        assert_eq!(out.reply, "ok radius=24");
        assert!(out.redraw);
        let out = state.apply(Command::Radius(24), &layout);
        assert!(!out.redraw, "unchanged radius must not redraw");
        let out = state.apply(Command::Radius(0), &layout);
        assert!(out.reply.starts_with("error: radius must be between"));
        assert!(!out.redraw);
        assert_eq!(
            state.appearance.radius, 24,
            "invalid radius leaves state unchanged"
        );

        // Color mirrors radius behaviour.
        let out = state.apply(
            Command::Color(Rgb {
                r: 0,
                g: 0xff,
                b: 0,
            }),
            &layout,
        );
        assert_eq!(out.reply, "ok color=#00ff00");
        assert!(out.redraw);
        let out = state.apply(
            Command::Color(Rgb {
                r: 0,
                g: 0xff,
                b: 0,
            }),
            &layout,
        );
        assert!(!out.redraw);

        let out = state.apply(Command::Hide, &layout);
        assert_eq!(out.reply, "ok visible=0");
        assert!(!state.visible);
        let out = state.apply(Command::Show, &layout);
        assert_eq!(out.reply, "ok visible=1");
        assert!(state.visible);
    }

    #[test]
    fn state_transitions_actions() {
        let layout = real_layout();
        let mut state = LaserState::default();

        let out = state.apply(Command::Next, &layout);
        assert_eq!(out.action, Some(Action::Navigate(PresentationKey::Next)));
        assert!(out.reply.is_empty(), "adapter fills the navigate reply");
        let out = state.apply(Command::Previous, &layout);
        assert_eq!(
            out.action,
            Some(Action::Navigate(PresentationKey::Previous))
        );

        let out = state.apply(Command::Quit, &layout);
        assert_eq!(out.reply, "ok shutting down");
        assert_eq!(out.action, Some(Action::Quit));
    }

    #[test]
    fn state_line_reports_global_position_and_output() {
        let layout = real_layout();
        let mut state = LaserState::default();
        state.apply(Command::Move(2240, 540), &layout);
        assert_eq!(
            state.state_line(&layout),
            "dot=2240,540 on=DP-1 visible=1 radius=12 color=#ff0000 confined=none areas=eDP-1:-256,120,1536,960;DP-1:1280,0,1920,1080"
        );
        // Dead space reports on=none instead of guessing an output.
        state.apply(Command::Move(0, 0), &layout);
        assert!(state.state_line(&layout).contains("on=none"));
    }
}
