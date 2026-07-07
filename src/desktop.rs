//! Multi-desktop view state (workspace ↔ splash) with fake horizontal slide.

use std::time::{Duration, Instant};

/// Which full-width desktop is active once any slide animation completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveDesktop {
    Workspace,
    Splash,
    Picker,
    WikiViewer,
    /// Screen 2: workspace picker | nav (with Coding Harness at top) | wiki content or harness (status+conv+input)
    /// Reached by right from picker in Screen 1. Right cycles focus Picker->Nav->Content, right from Content snaps forward.
    Overview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideDirection {
    ToSplash,
    ToWorkspace,
}

#[derive(Clone, Copy, Debug)]
struct SlideState {
    direction: SlideDirection,
    frame: u8,
    /// When set, tick() will hold at current frame until this instant.
    pause_until: Option<Instant>,
}

/// Number of animation frames for a desktop transition.
/// A ~250ms pause is held at the midpoint (half-and-half view) before auto-completing.
pub const SLIDE_FRAMES: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspacePane {
    Left,
    Right,
}

#[derive(Debug)]
pub struct DesktopState {
    pub active: ActiveDesktop,
    slide: Option<SlideState>,
    /// Pane to restore when returning from splash (Conversation or Trace only).
    pub workspace_pane: WorkspacePane,
}

impl DesktopState {
    pub fn new() -> Self {
        Self {
            active: ActiveDesktop::Splash,
            slide: None,
            workspace_pane: WorkspacePane::Left,
        }
    }

    pub fn is_animating(&self) -> bool {
        self.slide.is_some()
    }

    #[allow(dead_code)]
    pub fn showing_splash(&self) -> bool {
        if self.active == ActiveDesktop::Picker {
            return false;
        }
        match self.slide {
            Some(SlideState {
                direction: SlideDirection::ToSplash,
                frame,
                ..
            }) if frame < SLIDE_FRAMES => false,
            Some(SlideState {
                direction: SlideDirection::ToSplash,
                ..
            }) => true,
            Some(SlideState {
                direction: SlideDirection::ToWorkspace,
                ..
            }) => false,
            None => self.active == ActiveDesktop::Splash,
        }
    }

    pub fn can_slide_to_splash(&self) -> bool {
        !self.is_animating() && self.active == ActiveDesktop::Workspace
    }

    pub fn can_slide_to_workspace(&self) -> bool {
        !self.is_animating() && self.active == ActiveDesktop::Splash
    }

    pub fn showing_picker(&self) -> bool {
        self.active == ActiveDesktop::Picker && !self.is_animating()
    }

    #[allow(dead_code)]
    pub fn can_enter_picker(&self) -> bool {
        !self.is_animating() && (self.active == ActiveDesktop::Splash || self.active == ActiveDesktop::Workspace)
    }

    pub fn showing_wiki_viewer(&self) -> bool {
        self.active == ActiveDesktop::WikiViewer && !self.is_animating()
    }

    #[allow(dead_code)]
    pub fn can_enter_wiki_viewer(&self) -> bool {
        !self.is_animating() && (self.active == ActiveDesktop::Picker || self.active == ActiveDesktop::Workspace)
    }

    #[allow(dead_code)]
    pub fn showing_overview(&self) -> bool {
        self.active == ActiveDesktop::Overview && !self.is_animating()
    }

    #[allow(dead_code)]
    pub fn can_enter_overview(&self) -> bool {
        !self.is_animating() && (self.active == ActiveDesktop::Splash || self.active == ActiveDesktop::Picker)
    }

    pub fn start_slide_to_splash(&mut self, pane: WorkspacePane) {
        self.workspace_pane = pane;
        self.slide = Some(SlideState {
            direction: SlideDirection::ToSplash,
            frame: 0,
            pause_until: None,
        });
    }

    pub fn start_slide_to_workspace(&mut self) {
        self.slide = Some(SlideState {
            direction: SlideDirection::ToWorkspace,
            frame: 0,
            pause_until: None,
        });
    }

    pub fn set_picker(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Picker;
    }

    pub fn exit_picker_to_splash(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Splash;
    }

    /// Force active to workspace (used when loading a session from picker).
    pub fn set_workspace(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Workspace;
    }

    pub fn set_wiki_viewer(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::WikiViewer;
    }

    #[allow(dead_code)]
    pub fn exit_wiki_viewer_to_picker(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Picker;
    }

    pub fn exit_wiki_viewer_to_workspace(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Workspace;
    }

    pub fn set_overview(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Overview;
    }

    pub fn exit_overview_to_splash(&mut self) {
        self.slide = None;
        self.active = ActiveDesktop::Splash;
    }

    /// Advance the slide by one frame. Returns `true` while animation continues.
    /// Inserts a 250ms pause when reaching the halfway frame so the split view
    /// is briefly visible before the transition completes automatically.
    pub fn tick(&mut self) -> bool {
        let Some(mut slide) = self.slide.take() else {
            return false;
        };

        if let Some(until) = slide.pause_until {
            if Instant::now() < until && !cfg!(test) {
                self.slide = Some(slide);
                return false;
            }
            slide.pause_until = None;
        }

        slide.frame = slide.frame.saturating_add(1);
        if slide.frame >= SLIDE_FRAMES {
            self.active = match slide.direction {
                SlideDirection::ToSplash => ActiveDesktop::Splash,
                SlideDirection::ToWorkspace => ActiveDesktop::Workspace,
            };
            self.slide = None;
            false
        } else {
            // At midpoint, pause so user sees the half-and-half transition briefly.
            let mid = SLIDE_FRAMES / 2;
            if slide.frame == mid {
                slide.pause_until = Some(Instant::now() + Duration::from_millis(250));
            }
            self.slide = Some(slide);
            true
        }
    }

    /// Slide progress 0.0 (start) → 1.0 (end).
    pub fn slide_progress(&self) -> f32 {
        match self.slide {
            Some(SlideState { frame, .. }) => frame as f32 / SLIDE_FRAMES as f32,
            None => 0.0,
        }
    }

    pub fn slide_direction(&self) -> Option<SlideDirection> {
        self.slide.map(|s| s.direction)
    }
}

/// Load ASCII raven art: prefer `/tmp/raven1.txt`, then bundled asset.
pub fn load_raven_art() -> String {
    if let Ok(s) = std::fs::read_to_string("/tmp/raven1.txt") {
        if !s.trim().is_empty() {
            return s;
        }
    }
    include_str!("../assets/raven.txt").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_completes_after_frames() {
        let mut d = DesktopState::new();
        d.start_slide_to_splash(WorkspacePane::Left);
        for i in 0..SLIDE_FRAMES {
            let still_going = d.tick();
            if i + 1 < SLIDE_FRAMES {
                assert!(still_going);
            } else {
                assert!(!still_going);
            }
        }
        assert!(!d.is_animating());
        assert_eq!(d.active, ActiveDesktop::Splash);
    }
}
