//! byte — claude account switcher

pub mod atomic;
pub mod autostart;
pub mod claude;
pub mod cli;
pub mod desktop;
pub mod error;
pub mod lock;
pub mod ops;
pub mod output;
pub mod paths;
pub mod store;
pub mod tray;

pub use error::{Error, Result};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
