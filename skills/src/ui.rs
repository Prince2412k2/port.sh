//! Chrome, and the order each tab is built in.

use ratatui::layout::{Constraint, Layout as L, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, Tab};
use crate::tile;

const BG: Color = Color::Rgb(6, 7, 10);
const FG: Color = Color::Rgb(196, 200, 206);
const DIM: Color = Color::Rgb(96, 102, 112);
const FAINT: Color = Color::Rgb(54, 58, 68);

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    Block::default().style(Style::default().bg(BG)).render(area, f.buffer_mut());

    let [head, body, foot] = L::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    tabs(f, head, app);
    match app.tab {
        Tab::Skills => skills(f, body, app),
        Tab::Projects => projects(f, body, app),
    }
    status(f, foot, app);
}

fn tabs(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(" ", Style::default())];
    for (i, t) in Tab::ALL.iter().enumerate() {
        let on = *t == app.tab;
        spans.push(Span::styled(
            format!(" {} {} ", i + 1, t.label()),
            if on {
                Style::default().fg(BG).bg(FG).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ));
        spans.push(Span::styled(" ", Style::default()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn status(f: &mut Frame, area: Rect, app: &App) {
    let hint = match app.tab {
        Tab::Skills => "move the pointer   drag or scroll to throw it   space to hold   tab to switch",
        Tab::Projects => {
            "\u{2190} \u{2192} between projects   j k to read   tab for skills   q to quit"
        }
    };
    let left = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(hint, Style::default().fg(DIM)),
    ]);
    let right = Line::from(vec![Span::styled(
        match app.tab {
            Tab::Skills if app.animate => "drifting ".to_string(),
            Tab::Skills => "held ".to_string(),
            Tab::Projects => format!("{} of {} ", app.at + 1, app.projects.len()),
        },
        Style::default().fg(FAINT),
    )]);
    f.render_widget(Paragraph::new(left), area);
    let w = right.spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
    f.render_widget(
        Paragraph::new(right),
        Rect { x: area.x + area.width.saturating_sub(w), width: w, ..area },
    );
}

/// The sheet: marks in lattice order, then captions on whatever has risen far
/// enough to be worth naming.
fn skills(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    app.sheet_area = area;
    let sheet = app.sheet();

    let buf = f.buffer_mut();
    for t in sheet.tiles() {
        let (tw, th) = tile::size(t.logo, false);
        // Anchors are the centre of a tile, so the mark is offset by half its
        // own size — otherwise the lattice pitch would measure between corners
        // and the weave would run to the wrong place.
        let x = area.x as i32 + (t.x - tw as f64 * 0.5).round() as i32;
        let y = area.y as i32 + (t.y - th as f64 * 0.5).round() as i32;

        // Flat sheet is legible but quiet; what rises comes forward. The floor
        // is high enough that an untouched sheet still reads as a full board of
        // tools rather than as darkness with a spotlight in it.
        let light = 0.42 + 0.58 * t.lift as f32;
        tile::draw(buf, area, x, y, t.logo, false, light);

        // Names arrive with the lift. Labelling every tile at rest would be a
        // wall of type; labelling none would make the board a mystery.
        if t.lift > 0.30 {
            let name = (t.lift as f32 - 0.30) / 0.30;
            tile::caption(
                buf,
                area,
                x + tw as i32 / 2,
                y + th as i32,
                t.logo.name,
                (200, 206, 214),
                name.min(1.0) * 0.95,
            );
        }
    }
}

fn projects(f: &mut Frame, area: Rect, app: &mut App) {
    let view = crate::cards::View {
        projects: &app.projects,
        at: app.at,
        scroll: app.scroll,
        t: app.t,
    };
    app.hit = crate::cards::render(f, area, &view);
}
