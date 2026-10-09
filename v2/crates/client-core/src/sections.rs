use super::*;
use portfolio_v2_scene::{ArtCell, CellArt};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

#[derive(Clone, Debug, Default)]
pub struct Pages {
    pub at: usize,
    pub scroll: u16,
    pub clock: f64,
    pub drift: (f64, f64),
    pub cursor: Option<(f64, f64)>,
    pub velocity: (f64, f64),
    pub dragging: bool,
    pub drifting: bool,
    pub question: String,
    pub edit_revision: u64,
    pub pending_submission: Option<(String, String, u64)>,
    pub session: Option<portfolio_v2_protocol::session::Snapshot>,
    pub status: String,
    pub diagram: Option<portfolio_v2_protocol::session::Presentation>,
    pub museum: Option<crate::museum::Museum>,
    pub projects: Vec<skysheet::data::Project>,
}
impl Pages {
    pub fn load(&mut self, content: &Bootstrap) {
        self.museum = Some(crate::museum::Museum::new(&content.taste));
        self.projects = content
            .projects
            .iter()
            .map(|p| skysheet::data::Project {
                id: p.id.clone(),
                name: p.name.clone(),
                mark: p.mark.clone(),
                year: p.year.clone(),
                repo: p.repo.clone(),
                tag: p.tag.clone(),
                stats: p.stats.clone(),
                tools: p.tools.clone(),
                draft: p.draft,
                beats: p
                    .beats
                    .iter()
                    .map(|b| skysheet::data::Beat {
                        head: b.head.clone(),
                        body: b.body.clone(),
                    })
                    .collect(),
            })
            .collect();
    }
    pub fn tick(&mut self, seconds: f64, section: Section) {
        if seconds.is_finite() {
            let dt = seconds.clamp(0.0, 10.0);
            self.clock += dt;
            if section == Section::Skills && !self.dragging {
                let rest = if self.drifting {
                    (0.8, 0.45)
                } else {
                    (0.0, 0.0)
                };
                let decay = (-1.35 * dt).exp();
                self.drift.0 += rest.0 * dt + (self.velocity.0 - rest.0) * (1.0 - decay) / 1.35;
                self.drift.1 += rest.1 * dt + (self.velocity.1 - rest.1) * (1.0 - decay) / 1.35;
                self.velocity.0 = rest.0 + (self.velocity.0 - rest.0) * decay;
                self.velocity.1 = rest.1 + (self.velocity.1 - rest.1) * decay;
                if !self.drifting && self.velocity.0.abs() < 0.02 && self.velocity.1.abs() < 0.02 {
                    self.velocity = (0.0, 0.0);
                }
            }
            if let Some(museum) = self.museum.as_mut().filter(|_| section == Section::Taste) {
                museum.tick(dt);
            }
        }
    }
    pub fn key(&mut self, section: Section, key: &str, content: Option<&Bootstrap>) -> bool {
        match (section, key) {
            (Section::Skills, " ") => self.drifting = !self.drifting,
            (Section::Taste, "ArrowRight" | "l" | "n") => {
                if let Some(museum) = &mut self.museum {
                    museum.next();
                }
                self.scroll = 0;
            }
            (Section::Taste, "ArrowLeft" | "h" | "b") => {
                if let Some(museum) = &mut self.museum {
                    museum.prev();
                }
                self.scroll = 0;
            }
            (Section::Projects, "ArrowRight" | "l" | "n") => {
                let n = content.map_or(0, |c| c.projects.len());
                if n > 0 {
                    self.at = (self.at + 1) % n;
                    self.scroll = 0;
                }
            }
            (Section::Projects, "ArrowLeft" | "h" | "b") => {
                let n = content.map_or(0, |c| c.projects.len());
                if n > 0 {
                    self.at = (self.at + n - 1) % n;
                    self.scroll = 0;
                }
            }
            (Section::Projects | Section::Taste, "ArrowDown" | "j") => {
                self.scroll = self.scroll.saturating_add(1)
            }
            (Section::Projects | Section::Taste, "ArrowUp" | "k") => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            (Section::Projects | Section::Taste, "PageDown") => {
                self.scroll = self.scroll.saturating_add(12)
            }
            (Section::Projects | Section::Taste, "PageUp") => {
                self.scroll = self.scroll.saturating_sub(12)
            }
            (Section::Skills, "ArrowLeft" | "h") => self.drift.0 -= 6.0,
            (Section::Skills, "ArrowRight" | "l") => self.drift.0 += 6.0,
            (Section::Skills, "ArrowUp" | "k") => self.drift.1 -= 3.0,
            (Section::Skills, "ArrowDown" | "j") => self.drift.1 += 3.0,
            _ => return false,
        }
        true
    }
    pub fn ask_key(&mut self, key: &str) -> bool {
        if key == "Backspace" {
            self.question.pop();
            self.edit_revision += 1;
            return true;
        }
        if key.chars().count() == 1
            && !key.chars().any(char::is_control)
            && self.question.len() + key.len() <= 4096
        {
            self.question.push_str(key);
            self.edit_revision += 1;
            return true;
        }
        false
    }
    pub fn semantics(&self, section: Section, bootstrap: Option<&Bootstrap>) -> serde_json::Value {
        let Some(content) = bootstrap else {
            return serde_json::Value::Null;
        };
        match section {
            Section::Projects => {
                serde_json::to_value(content.projects.get(self.at)).unwrap_or_default()
            }
            Section::Skills => {
                serde_json::json!({"skills":skysheet::logos::LOGOS.iter().map(|l|l.name).collect::<Vec<_>>()})
            }
            Section::Taste => {
                serde_json::json!({"sheet":content.taste,"selected":self.museum.as_ref().map_or(0,|m|m.sel)})
            }
            Section::Ask => {
                serde_json::json!({"question":self.question,"status":self.status,"exchanges":self.session.as_ref().map(|s|s.exchanges.clone()).unwrap_or_default(),"diagram":self.diagram})
            }
            _ => serde_json::Value::Null,
        }
    }
}

pub fn render(scene: &mut VisualScene, section: Section, pages: &Pages, content: &Bootstrap) {
    let gutter = if scene.viewport.cols >= 90 { 7 } else { 0 };
    let area = Rect::new(
        gutter,
        1,
        scene.viewport.cols.saturating_sub(gutter),
        scene.viewport.rows.saturating_sub(3),
    );
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = match scene.theme {
        Theme::Dark => termap::canvas::Theme::System(termap::canvas::Ground {
            rgb: (8, 9, 11),
            dark: true,
        }),
        Theme::Light => termap::canvas::Theme::Paper,
    };
    match section {
        Section::Projects if !content.projects.is_empty() => {
            let projects = &pages.projects;
            if area.height < 32 {
                let project = &projects[pages.at.min(projects.len() - 1)];
                let width = area.width.saturating_sub(6).min(90).max(1);
                let mut lines = vec![(project.name.to_uppercase(), true)];
                paragraph(&mut lines, &project.tag, width);
                paragraph(&mut lines, &project.stats, width);
                paragraph(
                    &mut lines,
                    &format!("tools: {}", project.tools.join(", ")),
                    width,
                );
                for beat in &project.beats {
                    lines.push((beat.head.to_uppercase(), true));
                    paragraph(&mut lines, &beat.body, width);
                }
                let start =
                    (pages.scroll as usize).min(lines.len().saturating_sub(area.height as usize));
                for (row, (text, heading)) in lines
                    .iter()
                    .skip(start)
                    .take(area.height as usize)
                    .enumerate()
                {
                    put(
                        scene,
                        area.x + 3,
                        area.y + row as u16,
                        text,
                        if *heading {
                            PaletteRole::Amber
                        } else {
                            PaletteRole::Ink
                        },
                        *heading,
                    );
                }
                footer(scene, "← → projects   j k scroll   esc home");
                return;
            }
            let mut buffer = Buffer::empty(Rect::new(0, 0, area.width, area.height));
            buffer.set_style(buffer.area, Style::default().bg(theme.page()));
            let hit = skysheet::cards::render(
                &mut buffer,
                Rect::new(0, 0, area.width, area.height),
                &skysheet::cards::View {
                    projects: &projects,
                    at: pages.at.min(projects.len() - 1),
                    scroll: pages.scroll,
                    t: pages.clock,
                    theme,
                },
            );
            for index in 0..projects.len() {
                scene.hits.push(HitRegion {
                    id: format!("project:{index}"),
                    x: area.x + hit.pips.x + index as u16 * 2,
                    y: area.y + hit.pips.y,
                    width: 1,
                    height: 1,
                });
            }
            append_buffer(scene, area, buffer, theme);
            footer(scene, "← → projects   j k scroll   esc home");
        }
        Section::Skills => {
            let mut buffer = Buffer::empty(Rect::new(0, 0, area.width, area.height));
            buffer.set_style(buffer.area, Style::default().bg(theme.page()));
            let sheet = skysheet::grid::Sheet {
                drift: pages.drift,
                cursor: pages.cursor,
                w: area.width as f64,
                h: area.height as f64,
            };
            for item in sheet.tiles() {
                let (w, h) = skysheet::tile::size(item.logo, false);
                let x = (item.x - w as f64 * 0.5).round() as i32;
                let y = (item.y - h as f64 * 0.5).round() as i32;
                skysheet::tile::draw(
                    &mut buffer,
                    Rect::new(0, 0, area.width, area.height),
                    (x, y),
                    item.logo,
                    false,
                    0.42 + 0.58 * item.lift as f32,
                    theme,
                );
                if item.lift > 0.3 {
                    skysheet::tile::caption(
                        &mut buffer,
                        Rect::new(0, 0, area.width, area.height),
                        (x + w as i32 / 2, y + h as i32),
                        item.logo.name,
                        (200, 206, 214),
                        0.95,
                        theme,
                    );
                }
            }
            append_buffer(scene, area, buffer, theme);
            footer(
                scene,
                "arrows pan   pointer lifts marks   drag to move   esc home",
            );
        }
        Section::Taste => {
            if pages.scroll == 0 {
                if let Some(museum) = &pages.museum {
                    let mut buffer = Buffer::empty(Rect::new(0, 0, area.width, area.height));
                    buffer.set_style(buffer.area, Style::default().bg(theme.page()));
                    crate::museum::render(
                        &mut buffer,
                        Rect::new(0, 0, area.width, area.height),
                        museum,
                        theme,
                    );
                    append_buffer(scene, area, buffer, theme);
                    let width = (museum.len() * 2) as u16;
                    if width + 2 <= area.width {
                        let x = area.x + (area.width - width) / 2;
                        for index in 0..museum.len() {
                            scene.hits.push(HitRegion {
                                id: format!("taste:{index}"),
                                x: x + index as u16 * 2,
                                y: area.y + area.height.saturating_sub(1),
                                width: 2,
                                height: 1,
                            });
                        }
                    }
                    footer(scene, "← → exhibits   j read essay   esc home");
                    return;
                }
            }
            let mut lines = vec![("TASTE".into(), true)];
            paragraph(
                &mut lines,
                &content.taste.open,
                area.width.saturating_sub(8).min(72),
            );
            for entry in content
                .taste
                .figures
                .iter()
                .chain(&content.taste.works)
                .chain(&content.taste.threads)
            {
                lines.push((entry.name.to_uppercase(), true));
                lines.push((entry.from.clone(), false));
                if !entry.quote.is_empty() {
                    paragraph(
                        &mut lines,
                        &format!("“{}”", entry.quote),
                        area.width.saturating_sub(8).min(72),
                    );
                }
                paragraph(
                    &mut lines,
                    &entry.body,
                    area.width.saturating_sub(8).min(72),
                );
            }
            paragraph(
                &mut lines,
                &content.taste.close,
                area.width.saturating_sub(8).min(72),
            );
            let start =
                (pages.scroll as usize).min(lines.len().saturating_sub(area.height as usize));
            for (row, (text, heading)) in lines
                .iter()
                .skip(start)
                .take(area.height as usize)
                .enumerate()
            {
                put(
                    scene,
                    area.x + 3,
                    area.y + row as u16,
                    text,
                    if *heading {
                        PaletteRole::Amber
                    } else {
                        PaletteRole::Ink
                    },
                    *heading,
                );
            }
            footer(scene, "j k / arrows scroll   page up/down   esc home");
        }
        Section::Ask => {
            put(
                scene,
                area.x + 3,
                area.y + 2,
                "ASK",
                PaletteRole::Amber,
                true,
            );
            let width = area.width.saturating_sub(6).min(100).max(1);
            let mut lines = Vec::new();
            let mut links = std::collections::BTreeMap::new();
            if let Some(session) = &pages.session {
                put(
                    scene,
                    area.x + 9,
                    area.y + 2,
                    &format!("resume {}", session.session_id),
                    PaletteRole::Faint,
                    false,
                );
                for exchange in &session.exchanges {
                    paragraph(&mut lines, &format!("› {}", exchange.question), width);
                    paragraph(&mut lines, &exchange.answer, width);
                    if let Some(error) = &exchange.error {
                        paragraph(&mut lines, error, width);
                    }
                    if exchange.status == portfolio_v2_protocol::session::RequestStatus::Cancelled {
                        lines.push(("[cancelled]".into(), false));
                    }
                    for (index, panel) in exchange.presentations.iter().enumerate() {
                        let label = match panel {
                            portfolio_v2_protocol::session::Presentation::Map { title, .. } => {
                                format!("show map · {title}")
                            }
                            portfolio_v2_protocol::session::Presentation::Project { id } => {
                                format!("show project · {id}")
                            }
                            portfolio_v2_protocol::session::Presentation::Diagram {
                                title, ..
                            } => format!("show diagram · {title}"),
                        };
                        links.insert(
                            lines.len(),
                            format!("panel:{}:{index}", exchange.request_id),
                        );
                        lines.push((label, true));
                    }
                }
            }
            if let Some(portfolio_v2_protocol::session::Presentation::Diagram {
                title,
                nodes,
                edges,
            }) = &pages.diagram
            {
                lines.clear();
                links.clear();
                lines.push((title.clone(), true));
                let column = (width.saturating_sub(3) / 2).max(4) as usize;
                for pair in nodes.chunks(2) {
                    let border = "─".repeat(column.saturating_sub(2));
                    lines.push((
                        pair.iter()
                            .map(|_| format!("┌{border}┐"))
                            .collect::<Vec<_>>()
                            .join("   "),
                        false,
                    ));
                    lines.push((
                        pair.iter()
                            .map(|n| {
                                let text = format!("{}: {}", n.id, n.label)
                                    .chars()
                                    .take(column.saturating_sub(2))
                                    .collect::<String>();
                                format!(
                                    "│{}{}│",
                                    text,
                                    " ".repeat(column.saturating_sub(2 + text.chars().count()))
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("   "),
                        false,
                    ));
                    lines.push((
                        pair.iter()
                            .map(|_| format!("└{border}┘"))
                            .collect::<Vec<_>>()
                            .join("   "),
                        false,
                    ));
                    lines.push((String::new(), false));
                }
                for edge in edges {
                    paragraph(
                        &mut lines,
                        &format!("{} → {}   {}", edge.from, edge.to, edge.label),
                        width,
                    );
                }
            }
            let visible = area.height.saturating_sub(7) as usize;
            let start = lines.len().saturating_sub(visible + pages.scroll as usize);
            for (row, (text, _)) in lines.iter().skip(start).take(visible).enumerate() {
                put(
                    scene,
                    area.x + 3,
                    area.y + 4 + row as u16,
                    text,
                    if links.contains_key(&(start + row)) {
                        PaletteRole::Amber
                    } else {
                        PaletteRole::Ink
                    },
                    false,
                );
                if let Some(id) = links.get(&(start + row)) {
                    scene.hits.push(HitRegion {
                        id: id.clone(),
                        x: area.x + 3,
                        y: area.y + 4 + row as u16,
                        width: text.chars().count().min(width as usize) as u16,
                        height: 1,
                    });
                }
            }
            let input_row = area.y + area.height.saturating_sub(2);
            let prompt = pages
                .question
                .chars()
                .rev()
                .take(width.saturating_sub(2) as usize)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            put(
                scene,
                area.x + 3,
                input_row,
                &format!("› {prompt}"),
                PaletteRole::Ink,
                false,
            );
            put(
                scene,
                area.x + 3,
                input_row.saturating_sub(1),
                &pages.status,
                PaletteRole::Faint,
                false,
            );
            footer(scene, "enter submit   ctrl+x cancel   esc home");
        }
        _ => (),
    }
}
fn paragraph(lines: &mut Vec<(String, bool)>, text: &str, width: u16) {
    for line in super::wrap(text, width.max(1) as usize) {
        lines.push((line, false));
    }
    lines.push((String::new(), false));
}
fn footer(scene: &mut VisualScene, text: &str) {
    put(
        scene,
        9.min(scene.viewport.cols.saturating_sub(1)),
        scene.viewport.rows.saturating_sub(2),
        text,
        PaletteRole::Faint,
        false,
    );
}
fn append_buffer(
    scene: &mut VisualScene,
    area: Rect,
    buffer: Buffer,
    theme: termap::canvas::Theme,
) {
    let rgb = |color: Color| super::map::rgba(color, theme);
    let cells = buffer
        .content
        .iter()
        .map(|cell| ArtCell {
            glyph: cell.symbol().chars().next().unwrap_or(' '),
            foreground: rgb(cell.fg),
            background: Some(rgb(cell.bg)),
            bold: cell.modifier.contains(Modifier::BOLD),
            detail: 0,
        })
        .collect();
    scene.primitives.push(Primitive::CellArt(CellArt {
        x: area.x,
        y: area.y,
        cols: area.width,
        rows: area.height,
        cells,
    }));
}
