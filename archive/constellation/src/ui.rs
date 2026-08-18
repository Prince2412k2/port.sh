//! Terminal chrome, and the order the frame is built in.
//!
//! There is no side panel. The sky is the whole window and the text is set in
//! the middle of it, so the ordering here matters more than it looks:
//!
//! 1. the sky is painted, stars and all;
//! 2. the card fades a clearing in it and writes into the gap;
//! 3. only then are the star names placed, with that clearing marked occupied.
//!
//! Do it in any other order and a label lands under the description, or the
//! description lands on a star, and there is no third layer to arbitrate.

use ratatui::layout::{Constraint, Layout as L, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::canvas::{Brush, Canvas, Overlay, TINT_CON, TINT_DIM, SUB_X, SUB_Y};
use crate::card;
use crate::draw::{self, Scene};
use crate::labels::{place, Occupancy};

const BG: Color = Color::Rgb(6, 7, 10);
const FG: Color = Color::Rgb(196, 200, 206);
const DIM: Color = Color::Rgb(96, 102, 112);
const FAINT: Color = Color::Rgb(54, 58, 68);
const ACCENT: Color = Color::Rgb(230, 172, 110);
const CYAN: Color = Color::Rgb(110, 224, 255);

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    Block::default().style(Style::default().bg(BG)).render(area, f.buffer_mut());

    let [head, body, foot] = L::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    header(f, head, app);
    sky_view(f, body, app);
    status(f, foot, app);

    if app.mode == Mode::Help {
        help(f, area);
    }
}

fn sky_view(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Hover is resolved against the previous frame, before the canvas and the
    // label runs are cleared out from under it.
    app.sky_area = area;
    app.update_hover();

    let (cw, ch) = (area.width as usize, area.height as usize);
    if app.canvas.cw != cw || app.canvas.ch != ch {
        app.canvas = Canvas::new(cw, ch);
    } else {
        app.canvas.clear();
    }
    app.view.sw = app.canvas.sw as f64;
    app.view.sh = app.canvas.sh as f64;

    // The card is laid out before the camera is placed, because the camera is
    // placed to clear it.
    let card = card::build(app, cw, ch);
    match (app.take_pending_focus(), &card) {
        (Some(con), Some(card)) => app.frame_project(con, card.rect()),
        (Some(con), None) => app.frame_project(con, (0, 0, 0, 0)),
        (None, _) => app.apply_pending(),
    }
    app.reveal_pending();

    let matches = app.active_matches().map(|m| m.to_vec());
    let scene = Scene {
        sky: &app.sky,
        lay: &app.lay,
        view: &app.view,
        focus: app.focus,
        selected: app.selected,
        hover: app.hover,
        matches: matches.as_deref(),
        dust: app.dust,
        figures: app.figures,
    };
    let painted = draw::frame(&mut app.canvas, &scene);

    let mut occ = Occupancy::new(cw, ch);
    for (x, y) in painted.occupied {
        occ.block(x, y, 1, 1);
    }
    if let Some(c) = &card {
        let (x, y, w, h) = card::draw(&mut app.canvas, c, app.story_scroll as usize);
        occ.block(x, y, w, h);
    }

    app.label_hits.clear();
    // Project names claim their space before the star names are placed, so a
    // skill can never be written across the name of the project it belongs to.
    for t in painted.titles {
        let len = t.text.chars().count();
        if !occ.free_run(t.cell.0, t.cell.1, len) {
            continue;
        }
        occ.block(t.cell.0.saturating_sub(1), t.cell.1, len + 2, 1);
        for (i, ch) in t.text.chars().enumerate() {
            app.canvas.set_overlay(
                t.cell.0 + i,
                t.cell.1,
                Overlay { ch, tint: t.tint, lum: t.lum, bold: true },
            );
        }
        app.label_hits.push((
            t.cell.0 as u16,
            t.cell.1 as u16,
            len as u16,
            u32::MAX - 1 - t.con as u32,
        ));
    }

    write_labels(app, occ, painted.names);
    app.canvas.resolve(f.buffer_mut(), area, &Default::default(), app.mono);
}

/// Place the names, draw their leaders, and remember where each one landed so
/// it can be clicked.
///
/// Leaders go down as coverage so they fog with everything else; the text goes
/// down as overlays, which own their cell outright — a label half-eaten by the
/// star field is worse than no label.
fn write_labels(app: &mut App, mut occ: Occupancy, names: Vec<crate::labels::Candidate>) {
    let placed = place(names, &mut occ, SUB_X, SUB_Y);
    let c = &mut app.canvas;

    for p in &placed {
        if let Some((a, b)) = p.leader {
            let brush = Brush::new(p.depth.max(0.55), TINT_DIM);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let n = (dx * dx + dy * dy).sqrt().ceil() as usize;
            for i in 0..=n {
                let t = i as f64 / n.max(1) as f64;
                // Skip the first stretch so the leader does not smother the
                // star it is pointing at.
                if t * (n as f64) < 2.5 {
                    continue;
                }
                c.splat(a[0] + dx * t, a[1] + dy * t, 0.20, &brush);
            }
        }
    }

    for p in placed {
        let lum = 0.42 + 0.58 * (1.0 - p.depth).clamp(0.0, 1.0);
        let len = p.text.chars().count();
        for (i, ch) in p.text.chars().enumerate() {
            c.set_overlay(
                p.cell.0 + i,
                p.cell.1,
                Overlay { ch, tint: p.tint, lum, bold: p.bold },
            );
        }
        app.label_hits.push((
            p.cell.0 as u16,
            p.cell.1 as u16,
            len as u16,
            p.feature,
        ));
    }
}

// ── chrome ───────────────────────────────────────────────────────────────────

fn header(f: &mut Frame, area: Rect, app: &App) {
    let left = Line::from(vec![
        Span::styled(
            " skysheet ",
            Style::default().fg(BG).bg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {} skills", app.sky.stars.len()),
            Style::default().fg(FG),
        ),
        Span::styled("  across  ", Style::default().fg(FAINT)),
        Span::styled(format!("{} projects", app.sky.cons.len()), Style::default().fg(FG)),
    ]);

    // The right side is a trail, not a label: it says which of the three things
    // you are looking at and what backing out of it would give you.
    let mut right = Vec::new();
    if app.active_matches().is_some() {
        right.push(Span::styled("/", Style::default().fg(CYAN)));
        right.push(Span::styled(app.query.clone(), Style::default().fg(FG)));
        right.push(Span::styled(
            if app.mode == Mode::Search { "▏" } else { "" },
            Style::default().fg(CYAN),
        ));
    } else if let Some(c) = app.focus {
        right.push(Span::styled(
            app.sky.cons[c].name.clone(),
            Style::default().fg(con_color(c)).add_modifier(Modifier::BOLD),
        ));
        if let Some(s) = app.selected {
            right.push(Span::styled("  ›  ", Style::default().fg(FAINT)));
            right.push(Span::styled(
                app.sky.stars[s].name.clone(),
                Style::default().fg(CYAN),
            ));
        }
    } else {
        right.push(Span::styled("the whole sky", Style::default().fg(DIM)));
    }
    right.push(Span::styled(" ", Style::default()));

    let right = Line::from(right);
    f.render_widget(Paragraph::new(left), area);
    let w = line_width(&right).min(area.width);
    f.render_widget(
        Paragraph::new(right),
        Rect { x: area.x + area.width - w, width: w, ..area },
    );
}

fn status(f: &mut Frame, area: Rect, app: &App) {
    let mut left = vec![Span::styled(" ", Style::default())];

    if let Some(c) = app.hover_con {
        left.push(Span::styled("◈ ", Style::default().fg(con_color(c))));
        left.push(Span::styled(
            app.sky.cons[c].name.clone(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ));
        left.push(Span::styled(
            format!("   {}", app.sky.cons[c].blurb),
            Style::default().fg(DIM),
        ));
    } else if let Some(s) = app.hover {
        let star = &app.sky.stars[s];
        left.push(Span::styled("✦ ", Style::default().fg(con_color(star.home()))));
        left.push(Span::styled(
            star.name.clone(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ));
        left.push(Span::styled("   ", Style::default()));
        for (n, (&m, &load)) in star.members.iter().zip(&star.load).enumerate() {
            if n > 0 {
                left.push(Span::styled(" · ", Style::default().fg(FAINT)));
            }
            left.push(Span::styled(
                app.sky.cons[m].name.clone(),
                Style::default().fg(con_color(m)),
            ));
            if load {
                left.push(Span::styled("*", Style::default().fg(ACCENT)));
            }
        }
    } else {
        // The hint follows the state, because the useful key is different in
        // each of them and a fixed line spends its width on the other two.
        left.push(Span::styled(
            match (app.focus, app.selected) {
                (Some(_), Some(_)) => "n/p next skill   esc back to the project   / find   ? keys",
                (Some(_), None) => "n/p walk the skills   esc back to the sky   / find   ? keys",
                (None, _) => "click a project   1-9 open one   / find   ? keys   q quit",
            },
            Style::default().fg(DIM),
        ));
    }

    let right = Line::from(vec![Span::styled(
        format!("z{:.1} ", app.view.zoom),
        Style::default().fg(FAINT),
    )]);

    f.render_widget(Paragraph::new(Line::from(left)), area);
    let w = line_width(&right).min(area.width);
    f.render_widget(
        Paragraph::new(right),
        Rect { x: area.x + area.width - w, width: w, ..area },
    );
}

fn help(f: &mut Frame, area: Rect) {
    let Some(popup) = overlay_rect(area, 64, 24) else { return };

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .style(Style::default().bg(BG))
        .title(Span::styled(
            " skysheet ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let row = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<11}"), Style::default().fg(FG)),
            Span::styled(v.to_string(), Style::default().fg(DIM)),
        ])
    };
    let note = |t: &str| Line::from(Span::styled(format!("  {t}"), Style::default().fg(FAINT)));

    let lines = vec![
        Line::from(Span::styled(
            "  Every project is a constellation. Every skill is a star.",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        )),
        Line::default(),
        row("click", "open a project, or one of its skills"),
        row("1 - 9", "open a project by number"),
        row("n  p", "walk the skills of the open project"),
        row("esc", "back out one layer"),
        row("drag", "pan the sky"),
        row("wheel", "zoom, anchored at the cursor"),
        row("h j k l", "pan left/down/up/right"),
        row("+ -", "zoom in / out"),
        row("0  g", "back to the whole sky"),
        row("/", "find a skill, a story, or a project"),
        row("s  f", "dust / constellation figures"),
        row("m", "monochrome"),
        row("? q", "this, and quit"),
        Line::default(),
        note("A star's brightness is not a rating. It counts how many"),
        note("projects claim the skill and how many lean on it — both"),
        note("checkable against the sheet. A skill used by four projects"),
        note("is pulled four ways by the layout and comes to rest"),
        note("between them, so a shared skill is visibly shared."),
    ];

    f.render_widget(Paragraph::new(lines), inner);
}

fn overlay_rect(area: Rect, w: u16, h: u16) -> Option<Rect> {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    (w >= 20 && h >= 6).then(|| Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    })
}

fn con_color(con: usize) -> Color {
    Canvas::rgb(TINT_CON + con as u8, 1.0, false)
}

fn line_width(l: &Line) -> u16 {
    l.spans.iter().map(|s| s.content.chars().count() as u16).sum()
}
