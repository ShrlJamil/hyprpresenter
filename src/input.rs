//! Presentation keyboard input through `zwp_virtual_keyboard_v1`.
//!
//! The compositor's own XKB keymap is obtained from `wl_keyboard.keymap` and
//! forwarded verbatim to the virtual keyboard, so the user's real layout is
//! used instead of a hardcoded or synthesized one.  Key events use raw Linux
//! evdev keycodes: the virtual-keyboard protocol expects those, and the
//! compositor translates them through the forwarded keymap.
//!
//! This module uses its own Wayland connection so a virtual-keyboard failure
//! cannot tear down the laser overlay.  It never changes focus; the compositor
//! routes injected keys to whatever window is currently focused.

use std::{
    os::fd::{AsFd, OwnedFd},
    thread,
    time::{Duration, Instant},
};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_keyboard, wl_registry, wl_seat},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{Event as ManagerEvent, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{Event as VirtualKeyboardEvent, ZwpVirtualKeyboardV1},
};

/// Default presentation mappings, as raw Linux evdev keycodes.
///
/// These are physical key positions, not characters (PageUp = 104,
/// PageDown = 109 in the Linux input event code table).
const DEFAULT_PREVIOUS_KEY: u32 = 104;
const DEFAULT_NEXT_KEY: u32 = 109;

/// Which evdev keycodes `previous` / `next` inject.
///
/// This mapping is the single place presentation navigation keys are
/// defined (Phase 4): the CLI turns `--previous-key` / `--next-key` into a
/// `NavKeys` value, and the protocol code below only consumes it — changing
/// the mapping never requires touching the virtual-keyboard code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavKeys {
    pub previous: u32,
    pub next: u32,
}

impl Default for NavKeys {
    fn default() -> Self {
        Self {
            previous: DEFAULT_PREVIOUS_KEY,
            next: DEFAULT_NEXT_KEY,
        }
    }
}

/// How long to hold a key down.  Long enough for toolkits to observe a real
/// press/release pair, short enough not to make navigation feel sluggish.
const KEY_HOLD: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationKey {
    Previous,
    Next,
}

impl PresentationKey {
    /// The evdev keycode this navigation action injects under `keys`.
    pub fn evdev_code(self, keys: NavKeys) -> u32 {
        match self {
            PresentationKey::Previous => keys.previous,
            PresentationKey::Next => keys.next,
        }
    }

    pub fn command_name(self) -> &'static str {
        match self {
            PresentationKey::Previous => "previous",
            PresentationKey::Next => "next",
        }
    }

    pub fn key_name(self) -> &'static str {
        match self {
            PresentationKey::Previous => "PageUp",
            PresentationKey::Next => "PageDown",
        }
    }
}

#[derive(Default)]
struct InitState {
    keymap: Option<(OwnedFd, u32)>,
    keyboard_capable: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for InitState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The GlobalList keeps its own copy of the registry contents.
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InitState {
    fn event(
        state: &mut Self,
        _proxy: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            state.keyboard_capable = capabilities.contains(wl_seat::Capability::Keyboard);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for InitState {
    fn event(
        state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            if matches!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1)) {
                state.keymap = Some((fd, size));
            }
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for InitState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: ManagerEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for InitState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: VirtualKeyboardEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// A live virtual keyboard.  Dropping it releases any held key and destroys
/// the protocol object.
pub struct Input {
    conn: Connection,
    keyboard: ZwpVirtualKeyboardV1,
    start: Instant,
    pressed: Option<u32>,
}

impl Input {
    /// Connect, obtain the compositor keymap, and create the virtual keyboard.
    ///
    /// Returns a human-readable error when the protocol is missing, the seat
    /// has no keyboard, or the compositor rejects the keymap.  Callers should
    /// keep running the overlay and surface this error on navigation commands.
    pub fn connect() -> std::result::Result<Self, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("cannot connect to Wayland for input: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<InitState>(&conn)
            .map_err(|e| format!("cannot initialise Wayland registry: {e}"))?;
        let qh = queue.handle();

        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("zwp_virtual_keyboard_manager_v1 unavailable: {e}"))?;
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|e| format!("wl_seat unavailable: {e}"))?;

        let mut state = InitState::default();
        let keyboard = seat.get_keyboard(&qh, ());

        // The keymap normally arrives in the first roundtrip.  A few retries
        // tolerate an extra seat-capabilities event without blocking forever.
        for _ in 0..4 {
            queue
                .roundtrip(&mut state)
                .map_err(|e| format!("Wayland roundtrip while reading keymap: {e}"))?;
            if state.keymap.is_some() {
                break;
            }
        }

        if !state.keyboard_capable {
            return Err("the seat does not advertise keyboard capability".to_string());
        }
        let (keymap_fd, keymap_size) = state
            .keymap
            .take()
            .ok_or_else(|| "compositor did not provide an XKB keymap".to_string())?;

        let virtual_keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        virtual_keyboard.keymap(
            wl_keyboard::KeymapFormat::XkbV1 as u32,
            keymap_fd.as_fd(),
            keymap_size,
        );
        virtual_keyboard.modifiers(0, 0, 0, 0);
        conn.flush()
            .map_err(|e| format!("failed to send virtual keyboard keymap: {e}"))?;

        // A rejected keymap is a protocol error; surface it here rather than at
        // the first navigation command.
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("compositor rejected the virtual keyboard keymap: {e}"))?;

        // The keymap fd is no longer needed once the compositor has it.  The
        // observer keyboard can go away too; it only existed to fetch the map.
        if keyboard.version() >= 3 {
            keyboard.release();
        }
        if seat.version() >= 5 {
            seat.release();
        }
        conn.flush()
            .map_err(|e| format!("failed to release keymap observer: {e}"))?;

        Ok(Self {
            conn,
            keyboard: virtual_keyboard,
            start: Instant::now(),
            pressed: None,
        })
    }

    /// Send a complete press/release pair for `key`, using the keycode
    /// mapping in `keys`.
    pub fn tap(&mut self, key: PresentationKey, keys: NavKeys) -> std::result::Result<(), String> {
        let code = key.evdev_code(keys);
        let start = self.elapsed_ms();
        self.pressed = Some(code);

        self.keyboard
            .key(start, code, wl_keyboard::KeyState::Pressed as u32);
        self.conn
            .flush()
            .map_err(|e| format!("failed to send key press: {e}"))?;
        thread::sleep(KEY_HOLD);
        self.keyboard.key(
            start.wrapping_add(KEY_HOLD.as_millis() as u32),
            code,
            wl_keyboard::KeyState::Released as u32,
        );
        self.conn
            .flush()
            .map_err(|e| format!("failed to send key release: {e}"))?;

        self.pressed = None;
        Ok(())
    }

    fn elapsed_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Some(code) = self.pressed.take() {
            self.keyboard.key(
                self.elapsed_ms(),
                code,
                wl_keyboard::KeyState::Released as u32,
            );
        }
        self.keyboard.destroy();
        let _ = self.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mapping_is_pageup_pagedown() {
        let keys = NavKeys::default();
        assert_eq!(PresentationKey::Previous.evdev_code(keys), 104);
        assert_eq!(PresentationKey::Next.evdev_code(keys), 109);
    }

    #[test]
    fn custom_mapping_is_respected() {
        // e.g. Left/Right arrows (105/106) instead of PageUp/PageDown.
        let keys = NavKeys {
            previous: 105,
            next: 106,
        };
        assert_eq!(PresentationKey::Previous.evdev_code(keys), 105);
        assert_eq!(PresentationKey::Next.evdev_code(keys), 106);
    }
}
