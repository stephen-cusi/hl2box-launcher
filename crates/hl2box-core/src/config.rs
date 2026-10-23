//! Launcher configuration (re-exported from [`crate::launch`]).
//!
//! Kept as its own module name so callers can `hl2box_core::config::*`
//! naturally; the implementation lives with the launch logic it feeds.

pub use crate::launch::{
    build_args, command_line, guess_engine_root, split_args, start_game, LauncherConfig,
};
