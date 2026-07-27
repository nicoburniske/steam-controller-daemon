pub mod config;
mod error;
pub mod ipc;
pub mod protocol;

pub use config::Config;
pub use error::{Error, Result};
pub use ipc::{
    Client, HapticSound, OSK_PAD_LIMIT, OskBindings, OskClick, OskPad, OskPadSide, OskState,
    OskStream, Status,
};
pub use protocol::Button as ControllerButton;
