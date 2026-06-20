//! Multi-desktop view state (workspace ↔ splash) with fake horizontal slide.

/// Which full-width desktop is active once any slide animation completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveDesktop {
    Workspace,
    Splash,
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
}

/// Number of animation frames for a desktop transition (~300ms at 50ms idle poll).
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

    pub fn showing_splash(&self) -> bool {
        match self.slide {
            Some(SlideState {
                direction: SlideDirection::ToSplash,
                frame,
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

    pub fn start_slide_to_splash(&mut self, pane: WorkspacePane) {
        self.workspace_pane = pane;
        self.slide = Some(SlideState {
            direction: SlideDirection::ToSplash,
            frame: 0,
        });
    }

    pub fn start_slide_to_workspace(&mut self) {
        self.slide = Some(SlideState {
            direction: SlideDirection::ToWorkspace,
            frame: 0,
        });
    }

    /// Advance the slide by one frame. Returns `true` while animation continues.
    pub fn tick(&mut self) -> bool {
        let Some(mut slide) = self.slide else {
            return false;
        };
        slide.frame = slide.frame.saturating_add(1);
        if slide.frame >= SLIDE_FRAMES {
            self.active = match slide.direction {
                SlideDirection::ToSplash => ActiveDesktop::Splash,
                SlideDirection::ToWorkspace => ActiveDesktop::Workspace,
            };
            self.slide = None;
            false
        } else {
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