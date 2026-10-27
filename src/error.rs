/// Single error type for the whole desktop component. Modules return
/// `Result<_, Error>` and `main` converts to an exit code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The `HYPRLAND_INSTANCE_SIGNATURE` env var is not set (not running
    /// under Hyprland, or run from a session without the env exported).
    #[error("HYPRLAND_INSTANCE_SIGNATURE is not set — are you running inside a Hyprland session?")]
    NoHyprlandSignature,

    /// The per-instance Hyprland IPC socket does not exist / is not connectable.
    #[error("cannot connect to the Hyprland IPC socket at {path}: {source}")]
    HyprlandSocket {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Hyprland replied with an error or the command was not understood.
    #[error("Hyprland IPC error: {0}")]
    HyprlandReply(String),

    /// A reply that should be JSON was not valid JSON.
    #[error("Hyprland sent a non-JSON reply: {0}")]
    InvalidJson(#[source] serde_json::Error),

    /// A generic I/O failure on the IPC socket (timeout, write error, ...).
    #[error("I/O error on the Hyprland IPC socket: {0}")]
    Io(#[from] std::io::Error),

    /// Errors surfaced by the Wayland connection (probe / future overlay).
    #[error("Wayland error: {0}")]
    Wayland(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Process exit code for `main`. Distinct codes make scripting
    /// (`hyprpresenter status && ...`) and future systemd units easier.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::NoHyprlandSignature => 2,
            Error::HyprlandSocket { .. } | Error::Io(_) => 3,
            Error::HyprlandReply(_) => 4,
            Error::InvalidJson(_) => 5,
            Error::Wayland(_) => 6,
        }
    }
}
