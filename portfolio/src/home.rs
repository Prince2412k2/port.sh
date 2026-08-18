//! The landing section.
//!
//! Deliberately the quietest screen in the app. The other three are animated,
//! dense and doing something; if this one competed with them it would be noise
//! before anyone had read a word. It is a name, a paragraph, and a way in.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::about::About;
use crate::paint::{wrap, ACCENT, BG, CYAN, DIM, FAINT, FG};

/// Text is set against this rather than the terminal's full width. A pitch
/// measured across 200 columns is one line the eye cannot track back from.
const MEASURE: u16 = 66;

pub fn render(f: &mut Frame, area: Rect, a: &About) {
    if area.width < 24 || area.height < 8 {
        return;
    }
    let w = MEASURE.min(area.width.saturating_sub(8));
    let x = area.x + (area.width.saturating_sub(w)) / 2;

    // Vertical rhythm is computed rather than hard-coded, so the block sits on
    // the same optical centre whether the terminal is 24 rows or 60.
    let pitch = wrap(&a.pitch, w as usize);
    let now = wrap(&a.now, w as usize);
    let body = 3 + 1 + pitch.len() + if now.is_empty() { 0 } else { 2 + now.len() } + 2 + 4;
    let mut y = area.y + ((area.height as usize).saturating_sub(body) / 2).max(1) as u16;

    let put = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>| {
        if y < area.y + area.height {
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
        }
    };

    put(f, y, vec![Span::styled(
        a.name.to_uppercase(),
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    )]);
    y += 1;
    put(f, y, vec![
        Span::styled(a.role.clone(), Style::default().fg(ACCENT)),
        Span::styled("   ", Style::default()),
        Span::styled(a.where_.clone(), Style::default().fg(DIM)),
    ]);
    y += 2;

    for l in &pitch {
        put(f, y, vec![Span::styled(l.clone(), Style::default().fg(FG))]);
        y += 1;
    }

    if !now.is_empty() {
        y += 1;
        put(f, y, vec![Span::styled("NOW", Style::default().fg(FAINT))]);
        y += 1;
        for l in &now {
            put(f, y, vec![Span::styled(l.clone(), Style::default().fg(DIM))]);
            y += 1;
        }
    }

    y += 1;
    // The way in. On a server nobody has the keys memorised, and a portfolio
    // that has to be guessed at is one nobody sees past the first screen.
    for (key, label, blurb) in [
        ("1", "experience", "five places, flown between"),
        ("2", "projects", "nine of them, and how they work"),
        ("3", "skills", "the tools, and where they came from"),
        ("4", "taste", "what I think is worth building for"),
        ("5", "ask", "put a question to the resident agent"),
    ] {
        put(f, y, vec![
            Span::styled(format!("{key}  "), Style::default().fg(ACCENT)),
            Span::styled(format!("{label:<12}"), Style::default().fg(FG)),
            Span::styled(blurb.to_string(), Style::default().fg(FAINT)),
        ]);
        y += 1;
    }

    y += 1;
    let mut links: Vec<Span> = Vec::new();
    for (i, s) in [&a.github, &a.email, &a.ssh].iter().filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            links.push(Span::styled("   ·   ", Style::default().fg(FAINT)));
        }
        links.push(Span::styled((*s).clone(), Style::default().fg(CYAN)));
    }
    // The contact row is the one line that must not wrap or clip, and three
    // links do not fit the prose measure. It gets the rest of the frame.
    if y < area.y + area.height {
        let lw = (area.x + area.width).saturating_sub(x).saturating_sub(1);
        f.render_widget(
            Paragraph::new(Line::from(links)),
            Rect { x, y, width: lw, height: 1 },
        );
    }
}

/// Every key in the app, in one place, because three embedded renderers means
/// three key maps and no single screen that admits it.
pub fn help(f: &mut Frame, area: Rect) {
    let rows: [(&str, &str); 14] = [
        ("tab / shift-tab", "move between sections"),
        ("1 – 5", "jump straight to one"),
        ("/", "this list, from anywhere"),
        ("q", "quit"),
        ("", ""),
        ("experience", "a map you can actually drive"),
        ("n / b", "next / previous place"),
        ("?", "find a state, town or landmark"),
        ("drag, wheel", "pan and zoom"),
        ("u / o, m", "tilt the camera, or level it"),
        ("", ""),
        ("projects / skills / taste", ""),
        ("← →", "browse the project cards"),
        ("drag, wheel, ↑ ↓", "slide the sheet, read the essay"),
    ];
    let w = 52.min(area.width.saturating_sub(4));
    let h = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(BG)),
        popup,
    );

    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if v.is_empty() && !k.is_empty() {
                Line::from(Span::styled(format!("  {k}"), Style::default().fg(ACCENT)))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<18}"), Style::default().fg(FG)),
                    Span::styled(v.to_string(), Style::default().fg(DIM)),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), popup);
}
