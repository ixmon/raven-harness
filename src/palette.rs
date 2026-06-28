//! Terminal color-capability detection and RGB downsample.
//!
//! Problem this solves: the TUI uses 24-bit `Color::Rgb(...)` extensively. When
//! the terminal does not understand truecolor escape sequences (notably
//! `screen`, or any terminal behind it that lacks 24-bit support), the raw
//! `\x1b[38;2;r;g;bm` codes are misinterpreted — commonly rendering as black
//! text on a light-gray background. The "thinking" lines in the trace pane are
//! the most visible victim.
//!
//! Fix: detect the terminal's color depth once at startup and, when it is
//! less than 24-bit, downsample every `Color::Rgb` to the nearest entry in the
//! supported palette (256-color xterm palette, or the 16 ANSI colors as a
//! last resort). Named colors and `Color::Default` pass through unchanged.
//!
//! The detection is intentionally conservative:
//!   - `COLORTERM=truecolor` or `COLORTERM=24bit`  → 24-bit (no downsample)
//!   - `TERM` contains `truecolor` / `24bit`      → 24-bit
//!   - `TERM=screen*` or `$STY` set without the above → assume 256-color max
//!   - otherwise fall back to a `tput colors` probe, then to 16-color.
//!
//! A `RAVEN_COLOR_DEPTH` env var overrides everything (values: `24`, `256`,
//! `16`, `0`) for users who want to force a mode.

use std::sync::OnceLock;

use ratatui::buffer::Cell;
use ratatui::style::{Color, Modifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit truecolor. No downsample needed.
    True,
    /// 256-color xterm palette.
    Indexed256,
    /// 16 ANSI colors (the safe baseline).
    Ansi16,
    /// Monochrome / no color support.
    None,
}

static DEPTH: OnceLock<ColorDepth> = OnceLock::new();

/// Initialize color-depth detection. Safe to call multiple times; the first
/// call wins. Returns the resolved depth.
pub fn init() -> ColorDepth {
    *DEPTH.get_or_init(detect)
}

/// Current depth. Calls `init()` if not yet set.
pub fn depth() -> ColorDepth {
    *DEPTH.get_or_init(detect)
}

fn detect() -> ColorDepth {
    // 1. Explicit override.
    if let Ok(v) = std::env::var("RAVEN_COLOR_DEPTH") {
        match v.trim() {
            "24" | "true" | "truecolor" => return ColorDepth::True,
            "256" => return ColorDepth::Indexed256,
            "16" => return ColorDepth::Ansi16,
            "0" | "none" | "mono" => return ColorDepth::None,
            _ => {}
        }
    }

    // 2. COLORTERM hint (set by modern terminals).
    if let Ok(ct) = std::env::var("COLORTERM") {
        let ct = ct.to_ascii_lowercase();
        if ct == "truecolor" || ct == "24bit" {
            return ColorDepth::True;
        }
    }

    // 3. TERM hint.
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.contains("truecolor") || term.contains("24bit") || term.contains("direct") {
        return ColorDepth::True;
    }

    // 4. Detect screen / tmux without truecolor hints.
    let in_gnu_screen = std::env::var("STY").is_ok() || term.starts_with("screen");
    let in_tmux = std::env::var("TMUX").is_ok() || term.starts_with("tmux");

    // 5. GNU screen: its 256-color indexed escape handling is broken — it
    //    claims 256 via `tput colors` but mangles the actual SGR sequences,
    //    producing garbled backgrounds. Cap at Ansi16 which it handles fine.
    if in_gnu_screen {
        return ColorDepth::Ansi16;
    }

    // 6. Probe `tput colors` if available; trust it for non-screen terminals.
    if let Some(n) = tput_colors() {
        if n >= 16_777_216 {
            return ColorDepth::True;
        } else if n >= 256 {
            return ColorDepth::Indexed256;
        } else if n >= 16 {
            return ColorDepth::Ansi16;
        } else if n > 0 {
            return ColorDepth::Ansi16; // 8-color → treat as 16 (bold-as-bright)
        } else {
            return ColorDepth::None;
        }
    }

    // 7. Fallback: tmux without tput → assume 256 (tmux handles it fine).
    if in_tmux {
        return ColorDepth::Indexed256;
    }

    // 7. Default for unknown terminals: assume truecolor (preserves current
    //    behavior on native linux/PowerShell-ssh where it already looks right).
    ColorDepth::True
}

/// Run `tput colors` and parse the integer. Returns None if unavailable or
/// unparseable. We shell out (short-lived) only once at startup.
#[cfg(unix)]
fn tput_colors() -> Option<u32> {
    let out = std::process::Command::new("tput")
        .arg("colors")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u32>().ok()
}

#[cfg(windows)]
fn tput_colors() -> Option<u32> {
    None // Windows Terminal supports truecolor natively
}

/// Resolve a ratatui `Color` against the current terminal depth. Named colors
/// and `Color::Default` pass through unchanged; `Color::Rgb` is downsampled
/// when the depth is less than `True`.
pub fn resolve(c: Color) -> Color {
    match depth() {
        ColorDepth::True => c,
        ColorDepth::None => match c {
            Color::Rgb(..) => Color::Gray,
            other => other,
        },
        ColorDepth::Indexed256 => match c {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
            Color::Black => Color::Indexed(nearest_256(0, 0, 0)),
            Color::Red => Color::Indexed(nearest_256(170, 0, 0)),
            Color::Green => Color::Indexed(nearest_256(0, 170, 0)),
            Color::Yellow => Color::Indexed(nearest_256(170, 85, 0)),
            Color::Blue => Color::Indexed(nearest_256(0, 0, 170)),
            Color::Magenta => Color::Indexed(nearest_256(170, 0, 170)),
            Color::Cyan => Color::Indexed(nearest_256(0, 170, 170)),
            Color::White => Color::Indexed(nearest_256(170, 170, 170)),
            Color::Gray => Color::Indexed(nearest_256(170, 170, 170)),
            Color::LightRed => Color::Indexed(nearest_256(255, 85, 85)),
            Color::LightGreen => Color::Indexed(nearest_256(85, 255, 85)),
            Color::LightYellow => Color::Indexed(nearest_256(255, 255, 85)),
            Color::LightBlue => Color::Indexed(nearest_256(85, 85, 255)),
            Color::LightMagenta => Color::Indexed(nearest_256(255, 85, 255)),
            Color::LightCyan => Color::Indexed(nearest_256(85, 255, 255)),
            other => other,
        },
        ColorDepth::Ansi16 => match c {
            Color::Rgb(r, g, b) => nearest_16(r, g, b),
            Color::Black => Color::Black,
            Color::Red => Color::Red,
            Color::Green => Color::Green,
            Color::Yellow => Color::Yellow,
            Color::Blue => Color::Blue,
            Color::Magenta => Color::Magenta,
            Color::Cyan => Color::Cyan,
            Color::White => Color::White,
            Color::Gray => Color::White,
            Color::LightRed => Color::LightRed,
            Color::LightGreen => Color::LightGreen,
            Color::LightYellow => Color::LightYellow,
            Color::LightBlue => Color::LightBlue,
            Color::LightMagenta => Color::LightMagenta,
            Color::LightCyan => Color::LightCyan,
            other => other,
        },
    }
}

// ─── Wrapper backend ────────────────────────────────────────────────────────

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::layout::{Position, Size};
use std::io;

/// A `Backend` wrapper that downsamples every cell's `fg`/`bg` colors to the
/// terminal's detected color depth before forwarding to the inner backend.
///
/// This is the single chokepoint through which all ratatui drawing flows, so
/// wrapping here fixes `screen`/256-color/16-color rendering globally without
/// touching any individual render call site.
///
/// When the depth is `True`, `draw` forwards the cells unchanged (zero per-cell
/// clone) — the common case on modern terminals.
pub struct PaletteBackend<B: Backend> {
    inner: B,
}

impl<B: Backend> PaletteBackend<B> {
    pub fn new(inner: B) -> Self {
        Self { inner }
    }

    /// Borrow the inner backend (needed for crossterm `execute!` on teardown).
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }
}

impl<B: Backend> Backend for PaletteBackend<B> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        match depth() {
            // Fast path: no downsample needed. Forward as-is.
            ColorDepth::True => self.inner.draw(content),
            // Downsample path: clone each cell, rewrite colors, forward.
            _ => {
                let mapped: Vec<(u16, u16, Cell)> = content
                    .map(|(x, y, cell)| {
                        let mut c = cell.clone();
                        c.fg = resolve(c.fg);
                        c.bg = resolve(c.bg);
                        // GNU screen renders ITALIC as reverse video (swaps
                        // fg/bg), producing garbled pink backgrounds. Strip
                        // it when we're downsampling — the text is still
                        // visually distinguished by color alone.
                        c.modifier.remove(Modifier::ITALIC);
                        (x, y, c)
                    })
                    .collect();
                self.inner.draw(mapped.iter().map(|(x, y, c)| (*x, *y, c)))
            }
        }
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ─── 256-color palette mapping ──────────────────────────────────────────────

/// Nearest xterm-256 index for an RGB triple. Uses the 216-color cube +
/// 24 grayscale ramp. We do a coarse brute-force (cheap, runs only when
/// downsample is active) over the 256 entries.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    // xterm 256 palette as (index, r, g, b). The 16 ANSI colors (0-15) are
    // implementation-defined; we skip them for RGB matching and use the
    // 216-cube (16..232) + grayscale (232..256) which are well-defined.
    let mut best = 16u8;
    let mut best_dist = u32::MAX;
    for idx in 16..=231u8 {
        let (pr, pg, pb) = cube_rgb(idx);
        let d = dist(r, g, b, pr, pg, pb);
        if d < best_dist {
            best_dist = d;
            best = idx;
        }
    }
    for idx in 232..=255u8 {
        let (pr, pg, pb) = gray_rgb(idx);
        let d = dist(r, g, b, pr, pg, pb);
        if d < best_dist {
            best_dist = d;
            best = idx;
        }
    }
    best
}

/// xterm 216-color cube: index 16..232, steps of 0,95,135,175,215,255.
fn cube_rgb(idx: u8) -> (u8, u8, u8) {
    let i = (idx - 16) as u32;
    let ri = (i / 36) % 6;
    let gi = (i / 6) % 6;
    let bi = i % 6;
    let step = |v: u32| -> u8 {
        match v {
            0 => 0,
            1 => 95,
            2 => 135,
            3 => 175,
            4 => 215,
            5 => 255,
            _ => 255,
        }
    };
    (step(ri), step(gi), step(bi))
}

/// xterm grayscale ramp: index 232..256, gray = 8 + 10*n (n=0..23).
fn gray_rgb(idx: u8) -> (u8, u8, u8) {
    let v = 8 + 10 * (idx - 232) as u32;
    (v as u8, v as u8, v as u8)
}

fn dist(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    (dr * dr + dg * dg + db * db) as u32
}

// ─── 16-color ANSI mapping ──────────────────────────────────────────────────

/// Map an RGB triple to the nearest of the 16 ANSI colors by brute-force
/// Euclidean distance against all 16 entries. This avoids the two-phase
/// "pick hue then decide bright" approach which incorrectly maps light
/// purples (e.g. thinking text 0xd0,0xa0,0xff) to White instead of
/// LightMagenta.
fn nearest_16(r: u8, g: u8, b: u8) -> Color {
    const PALETTE: [(u8, u8, u8, Color); 16] = [
        (0, 0, 0, Color::Black),
        (170, 0, 0, Color::Red),
        (0, 170, 0, Color::Green),
        (170, 85, 0, Color::Yellow),
        (0, 0, 170, Color::Blue),
        (170, 0, 170, Color::Magenta),
        (0, 170, 170, Color::Cyan),
        (170, 170, 170, Color::Gray),
        (85, 85, 85, Color::DarkGray),
        (255, 85, 85, Color::LightRed),
        (85, 255, 85, Color::LightGreen),
        (255, 255, 85, Color::LightYellow),
        (85, 85, 255, Color::LightBlue),
        (255, 85, 255, Color::LightMagenta),
        (85, 255, 255, Color::LightCyan),
        (255, 255, 255, Color::White),
    ];

    let mut best = Color::White;
    let mut best_dist = u32::MAX;
    for &(pr, pg, pb, color) in &PALETTE {
        let d = dist(r, g, b, pr, pg, pb);
        if d < best_dist {
            best_dist = d;
            best = color;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_16_picks_red() {
        assert_eq!(nearest_16(200, 10, 10), Color::Red);
    }
    #[test]
    fn nearest_16_picks_bright_green() {
        // (100, 255, 100) is closest to LightGreen (85, 255, 85)
        assert_eq!(nearest_16(100, 255, 100), Color::LightGreen);
    }
    #[test]
    fn nearest_16_picks_white() {
        assert_eq!(nearest_16(250, 250, 250), Color::White);
    }
    #[test]
    fn nearest_16_thinking_purple_is_magenta() {
        // The thinking text color (0xd0, 0xa0, 0xff) must map to LightMagenta,
        // not White. This was the bug that caused black-on-pink in screen.
        assert_eq!(nearest_16(0xd0, 0xa0, 0xff), Color::LightMagenta);
    }
    #[test]
    fn nearest_256_in_cube_range() {
        let i = nearest_256(0, 0, 0);
        assert!((16..=255).contains(&i));
        // Pure black should map to a very dark cube entry.
        let (r, g, b) = if (16..=231).contains(&i) {
            cube_rgb(i)
        } else {
            gray_rgb(i)
        };
        assert!(r + g + b <= 30, "black-ish expected, got {r},{g},{b}");
    }
    #[test]
    fn nearest_256_white_is_bright() {
        let i = nearest_256(255, 255, 255);
        let (r, g, b) = if (16..=231).contains(&i) {
            cube_rgb(i)
        } else {
            gray_rgb(i)
        };
        assert!(
            r as u32 + g as u32 + b as u32 >= 700,
            "white-ish expected, got {r},{g},{b}"
        );
    }
    #[test]
    fn resolve_truecolor_passes_through() {
        DEPTH.set(ColorDepth::True).ok(); // ignore if already set
                                          // Force via env-independent path: re-init not possible, so test the
                                          // match arm directly by checking a known rgb returns unchanged.
                                          // (DEPTH may already be initialized to the real depth in tests; this
                                          // test only asserts the True branch maps identity, which is trivial.)
        let c = Color::Rgb(1, 2, 3);
        // When depth is True, resolve is identity:
        if depth() == ColorDepth::True {
            assert_eq!(resolve(c), c);
        }
    }
}
