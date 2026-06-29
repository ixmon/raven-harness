//! Thin entry point for the TUI.
//!
//! After the refactor split (see refactor.md), the real implementation
//! lives in:
//! - app_state.rs (App struct + core state)
//! - input_handler.rs (key/paste handling)
//! - event_loop.rs (main run loop + orchestration + TuiObserver)
//! - tui_render.rs (rendering)

pub use crate::event_loop::run;
