//! Experimental visuals playground for Raven TUI.
//!
//! Run with:
//!   cargo run --example raven_visuals
//!
//! It now tries to be smart about terminal capabilities:
//! - Lets ratatui-image auto-detect the best protocol (Kitty / iTerm2 / Sixel / Halfblocks)
//! - Detects if you're inside GNU screen or tmux
//! - Shows what protocol it actually got
//! - You can still manually cycle modes with Tab
//!
//! Outside screen (direct in a good terminal) → often gets nice rendering.
//! Inside screen → almost always falls back to halfblocks, which can look
//! much worse because screen mangles colors and advanced sequences.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::f64::consts::PI;

use ratatui_image::{Image, picker::{Picker, ProtocolType}, protocol::Protocol, Resize};
use ratatui::layout::Rect as LayoutRect;
use ratatui::widgets::canvas::{Canvas, Circle, Line as CanvasLine, Points};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_visuals(&mut terminal);

    // Restore
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = result {
        eprintln!("Error: {:?}", err);
    }
    Ok(())
}

struct VisualsApp {
    raven_protocol: Option<Box<dyn Protocol>>,
    chosen_protocol: Option<ProtocolType>,
    in_multiplexer: bool,
    ascii_raven: Option<String>,
    mode: Mode,
    frame: u64,
    start_time: Instant,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    RavenImage,
    RavenWithSwirl,
    AnimatedSplash,
    AsciiComparison,
}

impl VisualsApp {
    fn new() -> Self {
        let (raven_protocol, chosen_protocol) = match load_raven_image() {
            Ok((proto, ptype)) => (Some(proto), Some(ptype)),
            Err(e) => {
                eprintln!("Could not load raven image: {}", e);
                (None, None)
            }
        };

        let ascii_raven = std::fs::read_to_string("/tmp/raven1.txt").ok();

        let in_multiplexer = is_in_screen() || is_in_tmux();

        Self {
            raven_protocol,
            chosen_protocol,
            in_multiplexer,
            ascii_raven,
            mode: if in_multiplexer { Mode::AsciiComparison } else { Mode::RavenWithSwirl },
            frame: 0,
            start_time: Instant::now(),
        }
    }

    fn next_mode(&mut self) {
        self.mode = match self.mode {
            Mode::RavenImage => Mode::RavenWithSwirl,
            Mode::RavenWithSwirl => Mode::AnimatedSplash,
            Mode::AnimatedSplash => Mode::AsciiComparison,
            Mode::AsciiComparison => Mode::RavenImage,
        };
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab | KeyCode::Char(' ') => self.next_mode(),
            KeyCode::Char('1') => self.mode = Mode::RavenImage,
            KeyCode::Char('2') => self.mode = Mode::RavenWithSwirl,
            KeyCode::Char('3') => self.mode = Mode::AnimatedSplash,
            KeyCode::Char('4') => self.mode = Mode::AsciiComparison,
            _ => {}
        }
        false
    }
}

fn is_in_screen() -> bool {
    std::env::var("STY").is_ok()
}

fn is_in_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

fn protocol_name(pt: ProtocolType) -> &'static str {
    match pt {
        ProtocolType::Halfblocks => "Halfblocks (unicode)",
        ProtocolType::Sixel => "Sixel",
        ProtocolType::Kitty => "Kitty graphics",
        ProtocolType::Iterm2 => "iTerm2 inline images",
    }
}

fn load_raven_image() -> Result<(Box<dyn Protocol>, ProtocolType), Box<dyn std::error::Error>> {
    // IMPORTANT: raven_low.bmp was created by generate_raven.sh with heavy
    // thresholding + dithering *specifically for ASCII art*. It looks terrible
    // when fed directly to ratatui-image (especially in halfblock fallback).
    //
    // For ratatui-image you want a *clean* high-quality image:
    //   - Generate with Grok Imagine: "detailed gothic black raven, dark moody lighting,
    //     clear silhouette or features, good contrast, on dark background"
    //   - Save as PNG (preferred) at reasonable resolution (300-600px wide)
    //   - Do **not** run it through the old low-res threshold/dither steps.

    let candidates = ["assets/raven.png", "assets/raven.jpg", "assets/raven_low.bmp"];
    let mut dyn_img = None;

    for path in candidates {
        if let Ok(reader) = image::io::Reader::open(path) {
            if let Ok(img) = reader.decode() {
                println!("Loaded image source: {}", path);
                dyn_img = Some(img);
                if path.contains("raven_low") {
                    println!("WARNING: Using the ASCII-oriented low-res BMP. Results will be poor.");
                }
                break;
            }
        }
    }

    let dyn_img = dyn_img.ok_or_else(|| {
        "No raven image found. Place a clean raven.png (or .jpg) in tui/assets/"
    })?;

    // Auto-detect what the terminal (through any multiplexers) actually supports.
    let mut picker = match Picker::from_termios() {
        Ok(mut p) => {
            let _ = p.guess_protocol();
            p
        }
        Err(_) => {
            // Common when behind ssh + screen. Fall back to basic detection.
            let mut p = Picker::new((8, 16));
            let _ = p.guess_protocol();
            p
        }
    };

    // Protocol size for the splash image (must match render size in mode 3).
    let target_cells = LayoutRect::new(0, 0, 40, 30);

    // To make the raven appear centered (not top-left biased) when using
    // real graphics protocols, we resize the source image ourselves to exactly
    // fill the target cell area (preserving aspect) and center it with dark padding.
    let font = picker.font_size;
    let target_px_w = font.0 as u32 * 40;
    let target_px_h = font.1 as u32 * 30;

    let src_w = dyn_img.width();
    let src_h = dyn_img.height();

    let scale = (target_px_w as f32 / src_w as f32)
        .min(target_px_h as f32 / src_h as f32);

    let fit_w = (src_w as f32 * scale) as u32;
    let fit_h = (src_h as f32 * scale) as u32;

    let resized = dyn_img.resize(fit_w, fit_h, image::imageops::FilterType::Lanczos3);

    // Create a full-size target image with dark background (matches TUI theme)
    let mut padded = image::DynamicImage::new_rgb8(target_px_w, target_px_h);
    let bg = image::Rgb([18, 16, 28]); // dark background
    for pixel in padded.as_mut_rgb8().unwrap().pixels_mut() {
        *pixel = bg;
    }

    // Center the resized image
    let off_x = (target_px_w - fit_w) / 2;
    let off_y = (target_px_h - fit_h) / 2;
    image::imageops::replace(
        padded.as_mut_rgb8().unwrap(),
        resized.as_rgb8().unwrap(),
        off_x as i64,
        off_y as i64,
    );

    let protocol = picker.new_protocol(padded, target_cells, Resize::Fit(None))?;
    let chosen = picker.protocol_type;

    let in_mux = is_in_screen() || is_in_tmux();
    println!(
        "Detected graphics: {}   (inside screen/tmux: {})",
        protocol_name(chosen),
        in_mux
    );
    if in_mux {
        println!("→ GNU screen almost always forces Halfblocks (or breaks Sixel).");
        println!("   tmux is a little better but still limited for images.");
    }

    Ok((protocol, chosen))
}

fn run_visuals(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = VisualsApp::new();

    loop {
        terminal.draw(|f| draw(f, &app))?;

        // Event handling with a little timeout so animation keeps running
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if app.handle_key(key.code) {
                        return Ok(());
                    }
                }
            }
        }

        app.tick();

        // For the splash demo we can auto-advance ideas after some time,
        // but manual Tab is more useful for experimentation.
    }
}

fn draw(f: &mut Frame, app: &VisualsApp) {
    let area = f.area();

    // Main vertical split: visuals on top, help on bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(7)])
        .split(area);

    let visual_area = chunks[0];
    let help_area = chunks[1];

    match app.mode {
        Mode::RavenImage => draw_raven_only(f, app, visual_area),
        Mode::RavenWithSwirl => draw_raven_with_swirl(f, app, visual_area),
        Mode::AnimatedSplash => draw_animated_splash(f, app, visual_area),
        Mode::AsciiComparison => draw_ascii_comparison(f, app, visual_area),
    }

    draw_help(f, help_area, app);
}

fn draw_raven_only(f: &mut Frame, app: &VisualsApp, area: Rect) {
    let block = Block::default()
        .title(" Raven (ratatui-image) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(ref proto) = app.raven_protocol {
        let img = Image::new(proto.as_ref());
        f.render_widget(img, inner);
    } else {
        let msg = Paragraph::new(
            "No usable raven image loaded.\n\n\
             Put a clean raven.png (recommended) or raven.jpg\n\
             in the assets/ directory and restart the example.\n\n\
             See comments in load_raven_image() for best results."
        )
        .style(Style::default().fg(Color::Red));
        f.render_widget(msg, inner);
    }
}

fn draw_raven_with_swirl(f: &mut Frame, app: &VisualsApp, area: Rect) {
    let block = Block::default()
        .title(" Raven + Canvas Swirl (press Tab) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split the inner area: image on left, canvas effect on right
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(10)])
        .split(inner);

    // Raven image
    if let Some(ref proto) = app.raven_protocol {
        let img = Image::new(proto.as_ref());
        f.render_widget(img, parts[0]);
    }

    // Canvas swirl / particle effect next to it
    let t = (app.frame as f64) * 0.12;

    let canvas = Canvas::default()
        .block(Block::default().title("Swirling Effect"))
        .paint(move |ctx| {
            // Simple orbiting "feathers" or energy using braille-capable points
            let cx = 0.0;
            let cy = 0.0;

            let points: Vec<(f64, f64)> = (0..24)
                .map(|i| {
                    let angle = (i as f64) * 0.26 + t * 1.8;
                    let r = 8.0 + (t * 0.7 + i as f64 * 0.3).sin() * 1.5;
                    (cx + angle.cos() * r, cy + angle.sin() * r * 0.6)
                })
                .collect();

            ctx.draw(&Points {
                coords: &points,
                color: Color::Rgb(180, 160, 220),
            });

            // A couple of rotating lines for "wind" or wing motion
            for k in 0..3 {
                let phase = t * 2.0 + k as f64 * 1.1;
                let x1 = cx + phase.cos() * 6.0;
                let y1 = cy + phase.sin() * 4.0;
                let x2 = cx - phase.cos() * 9.0;
                let y2 = cy - phase.sin() * 5.5;

                ctx.draw(&CanvasLine {
                    x1,
                    y1,
                    x2,
                    y2,
                    color: Color::Rgb(100, 90, 140),
                });
            }

            // Small pulsing center "eye" or core
            let pulse = (t * 4.0).sin().abs() * 0.8 + 0.6;
            ctx.draw(&Circle {
                x: cx,
                y: cy,
                radius: pulse,
                color: Color::Rgb(220, 200, 255),
            });
        })
        .x_bounds([-14.0, 14.0])
        .y_bounds([-10.0, 10.0]);

    f.render_widget(canvas, parts[1]);
}

fn draw_animated_splash(f: &mut Frame, app: &VisualsApp, area: Rect) {
    // This is the "startup splash screen" prototype (mode 3).
    // ASCII raven from /tmp/raven1.txt centered + dots swirling around it.
    // No unicode.

    let elapsed = app.start_time.elapsed().as_secs_f64();
    let t = app.frame as f64 * 0.085 + elapsed * 0.7;

    let block = Block::default()
        .title(" Raven Hotel — Splash (mode 3) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Layout: small title at top, big content area, small tagline at bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(32),   // enough space for the 40x30 image + swirls
            Constraint::Length(3),
        ])
        .split(inner);

    // Static title (no unicode)
    let title_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let raven_title = " R A V E N   H O T E L ";

    let title = Paragraph::new(raven_title)
        .style(title_style)
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Main content area — full width for the swirl canvas
    let content_area = chunks[1];

    // Draw swirling dots on a Canvas first (background layer)
    let swirl = Canvas::default()
        .paint(move |ctx| {
            let cx = 0.0;
            let cy = 0.0;

            // Multiple rings of dots swirling around the centered image.
            // We start at a larger radius so dots appear *around* the raven, not on top of it.
            for ring in 0..5 {
                let count = 8 + ring * 4;
                let base_r = 14.0 + ring as f64 * 3.5;  // start farther out
                let speed = 1.1 - ring as f64 * 0.12;

                for i in 0..count {
                    let phase = (i as f64) * (2.0 * PI / count as f64);
                    let angle = phase + t * speed + ring as f64 * 0.4;

                    // Gentle breathing on radius
                    let breath = (t * 1.3 + ring as f64).sin() * 0.6;
                    let r = base_r + breath;

                    let x = cx + r * angle.cos();
                    let y = cy + r * angle.sin() * 0.65; // slightly flattened ellipse

                    // Color shifts with ring and time
                    let r_col = 140 + (ring * 11) as u8;
                    let g_col = 130 + ((ring * 7 + (t * 20.0) as i32) % 40) as u8;
                    let b_col = 190 + (ring * 5) as u8;

                    ctx.draw(&Points {
                        coords: &[(x, y)],
                        color: Color::Rgb(r_col, g_col, b_col),
                    });
                }
            }

            // A few slower outer accent dots (further out)
            for i in 0..4 {
                let angle = (i as f64) * 1.57 + t * 0.35;
                let r = 23.0 + (t * 0.25 + i as f64).sin() * 1.2;
                let x = cx + r * angle.cos();
                let y = cy + r * angle.sin() * 0.6;
                ctx.draw(&Points {
                    coords: &[(x, y)],
                    color: Color::Rgb(80, 70, 120),
                });
            }
        })
        .x_bounds([-26.0, 26.0])
        .y_bounds([-15.0, 15.0]);

    f.render_widget(swirl, content_area);

    // Centered ASCII raven from /tmp/raven1.txt on top of the swirl
    if let Some(ref art) = app.ascii_raven {
        let lines: Vec<&str> = art.lines().collect();
        let art_h = lines.len() as u16;
        let art_w = lines.iter()
            .map(|l| l.len() as u16)
            .max()
            .unwrap_or(20);

        let img_w = art_w.min(content_area.width);
        let img_h = art_h.min(content_area.height);

        let img_x = content_area.x + (content_area.width.saturating_sub(img_w)) / 2;
        let img_y = content_area.y + (content_area.height.saturating_sub(img_h)) / 2;

        let img_area = Rect::new(img_x, img_y, img_w, img_h);

        let para = Paragraph::new(art.as_str())
            .style(Style::default().fg(Color::Rgb(200, 180, 255)))
            .alignment(ratatui::layout::Alignment::Left);
        f.render_widget(para, img_area);
    } else {
        // Fallback text centered
        let fallback = Paragraph::new("raven\n(/tmp/raven1.txt not found)")
            .style(Style::default().fg(Color::Gray))
            .alignment(ratatui::layout::Alignment::Center);
        let fb_w = 24u16;
        let fb_h = 3u16;
        let fb_x = content_area.x + (content_area.width.saturating_sub(fb_w)) / 2;
        let fb_y = content_area.y + (content_area.height.saturating_sub(fb_h)) / 2;
        f.render_widget(fallback, Rect::new(fb_x, fb_y, fb_w, fb_h));
    }

    // Tagline at bottom
    let tag = Paragraph::new("Agent Harness • Local-first • Context-aware")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(tag, chunks[2]);
}

fn draw_ascii_comparison(f: &mut Frame, app: &VisualsApp, area: Rect) {
    let block = Block::default()
        .title(" Comparison + diagnosis ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Load the text version for side-by-side
    let ascii_content = include_str!("../assets/raven.txt");

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(inner);

    let old = Paragraph::new(ascii_content)
        .block(Block::default().title("assets/raven.txt (current ASCII)"))
        .style(Style::default().fg(Color::Rgb(120, 120, 120)));

    let note = Text::from(vec![
        Line::from("Detection in this run:"),
        Line::from(format!("  Graphics protocol: {}", 
            app.chosen_protocol.map(protocol_name).unwrap_or("none"))),
        Line::from(format!("  Inside multiplexer: {}", app.in_multiplexer)),
        Line::from(""),
        Line::from("screen (and often tmux) block Kitty/iTerm2/Sixel."),
        Line::from("Halfblocks may still look decent in a good font,"),
        Line::from("but the ASCII raven (raven.txt) is the reliable choice here."),
        Line::from(""),
        Line::from("Outside screen you usually get much better results."),
    ]);

    let new = Paragraph::new(note)
        .block(Block::default().title("Future possibilities"))
        .wrap(Wrap { trim: true });

    f.render_widget(old, chunks[0]);
    f.render_widget(new, chunks[1]);
}

fn draw_help(f: &mut Frame, area: Rect, app: &VisualsApp) {
    let proto = app.chosen_protocol
        .map(protocol_name)
        .unwrap_or("none");

    let mux_note = if app.in_multiplexer {
        " [limited by screen/tmux]"
    } else {
        ""
    };

    let help_text = format!(
        "Mode: {:?}   |   Graphics: {}{}   |   Tab/Space: next   |   1-4: direct   |   q/Esc: quit\n\
         This is a throwaway experiment binary. Copy cool parts back into the main TUI later.",
        app.mode, proto, mux_note
    );

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::Gray))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    f.render_widget(help, area);
}
