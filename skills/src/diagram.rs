use std::collections::{HashMap, HashSet};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use crate::scene::{FAINT, INK, MUTE, PASS, STOP, bar, note, spark, text};

const ACCENT: (u8, u8, u8) = (104, 174, 238);
const WARN: (u8, u8, u8) = (232, 186, 92);
const MAX_TEXT: usize = 8 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub title: String,
    pub elements: Vec<Element>,
    pub connectors: Vec<Connector>,
    pub beats: Vec<Beat>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RectSpec {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub id: String,
    pub rect: RectSpec,
    pub tone: Tone,
    pub kind: ElementKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ElementKind {
    Group {
        title: String,
    },
    Box {
        title: String,
        lines: Vec<String>,
        frame: Frame,
    },
    Text {
        text: String,
        role: TextRole,
        align: Align,
    },
    Meter {
        label: String,
        value: f64,
        unit: String,
    },
    Buffer {
        label: String,
        cells: Vec<CellState>,
    },
    Plot {
        label: String,
        samples: Vec<f64>,
        kind: PlotKind,
    },
    Timeline {
        label: String,
        markers: Vec<Marker>,
        cursor: f64,
    },
    Status {
        label: String,
        state: StatusState,
        detail: String,
    },
}

impl Default for ElementKind {
    fn default() -> Self {
        Self::Text {
            text: String::new(),
            role: TextRole::Body,
            align: Align::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tone {
    #[default]
    Normal,
    Accent,
    Pass,
    Warn,
    Stop,
    Muted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    #[default]
    Plain,
    Strong,
    Double,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextRole {
    Heading,
    #[default]
    Body,
    Annotation,
    Callout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellState {
    #[default]
    Empty,
    Ready,
    Active,
    Done,
    Blocked,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlotKind {
    #[default]
    Sparkline,
    Waveform,
    Bars,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusState {
    #[default]
    Idle,
    Active,
    Pass,
    Warn,
    Stop,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub at: f64,
    pub label: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connector {
    pub id: String,
    pub from: String,
    pub to: String,
    pub label: String,
    pub tone: Tone,
    pub style: ConnectorStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectorStyle {
    #[default]
    Arrow,
    Bidirectional,
    Blocked,
    Dashed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    pub caption: String,
    pub duration: f64,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    Focus { targets: Vec<String> },
    Flow { target: String, reverse: bool },
    Pulse { target: String },
    Meter { target: String, from: f64, to: f64 },
    Timeline { target: String, from: f64, to: f64 },
    Scan { target: String },
    Shift { target: String },
}

fn safe_text(what: &str, value: &str, max: usize, required: bool) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} exceeds {max} characters"));
    }
    if value.chars().any(char::is_control) || value.contains('\x1b') {
        return Err(format!("{what} contains control characters"));
    }
    Ok(())
}

fn safe_id(what: &str, id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 32
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("{what} id {id:?} is unsafe"));
    }
    Ok(())
}

fn unit_interval(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

pub fn validate(spec: &Spec) -> Result<(), String> {
    if !(1..=32).contains(&spec.elements.len()) {
        return Err("scene must contain 1..=32 elements".into());
    }
    if spec.connectors.len() > 24 {
        return Err("scene must contain <=24 connectors".into());
    }
    if spec.beats.len() > 12 {
        return Err("scene must contain <=12 beats".into());
    }
    safe_text("title", &spec.title, 72, false)?;

    let mut ids = HashSet::new();
    for (what, id) in spec
        .elements
        .iter()
        .map(|element| ("element", element.id.as_str()))
        .chain(
            spec.connectors
                .iter()
                .map(|connector| ("connector", connector.id.as_str())),
        )
    {
        safe_id(what, id)?;
        if !ids.insert(id) {
            return Err(format!("duplicate id {id:?}"));
        }
    }

    let mut text_bytes = spec.title.len();
    let elements: HashMap<&str, &ElementKind> = spec
        .elements
        .iter()
        .map(|element| (element.id.as_str(), &element.kind))
        .collect();
    for element in &spec.elements {
        let rect = element.rect;
        if rect.width == 0
            || rect.height == 0
            || rect
                .x
                .checked_add(rect.width)
                .is_none_or(|right| right > 100)
            || rect
                .y
                .checked_add(rect.height)
                .is_none_or(|bottom| bottom > 100)
        {
            return Err(format!("element {:?} has invalid geometry", element.id));
        }
        match &element.kind {
            ElementKind::Group { title } => {
                safe_text("group title", title, 80, true)?;
                text_bytes += title.len();
            }
            ElementKind::Box {
                title,
                lines,
                frame: _,
            } => {
                safe_text("box title", title, 80, true)?;
                if lines.len() > 8 {
                    return Err(format!("box {:?} has more than 8 lines", element.id));
                }
                text_bytes += title.len();
                for line in lines {
                    safe_text("box line", line, 240, true)?;
                    text_bytes += line.len();
                }
            }
            ElementKind::Text { text, .. } => {
                safe_text("text", text, 240, true)?;
                text_bytes += text.len();
            }
            ElementKind::Meter { label, value, unit } => {
                safe_text("meter label", label, 80, true)?;
                safe_text("meter unit", unit, 80, false)?;
                if !unit_interval(*value) {
                    return Err(format!(
                        "meter {:?} value must be finite in 0..=1",
                        element.id
                    ));
                }
                text_bytes += label.len() + unit.len();
            }
            ElementKind::Buffer { label, cells } => {
                safe_text("buffer label", label, 80, true)?;
                if cells.is_empty() || cells.len() > 32 {
                    return Err(format!("buffer {:?} must have 1..=32 cells", element.id));
                }
                text_bytes += label.len();
            }
            ElementKind::Plot { label, samples, .. } => {
                safe_text("plot label", label, 80, true)?;
                if !(2..=64).contains(&samples.len())
                    || samples.iter().any(|sample| !sample.is_finite())
                {
                    return Err(format!(
                        "plot {:?} must have 2..=64 finite samples",
                        element.id
                    ));
                }
                text_bytes += label.len();
            }
            ElementKind::Timeline {
                label,
                markers,
                cursor,
            } => {
                safe_text("timeline label", label, 80, true)?;
                if markers.len() > 16 || !unit_interval(*cursor) {
                    return Err(format!(
                        "timeline {:?} has invalid markers or cursor",
                        element.id
                    ));
                }
                text_bytes += label.len();
                for marker in markers {
                    if !unit_interval(marker.at) {
                        return Err(format!("timeline {:?} has an invalid marker", element.id));
                    }
                    safe_text("marker label", &marker.label, 80, true)?;
                    text_bytes += marker.label.len();
                }
            }
            ElementKind::Status { label, detail, .. } => {
                safe_text("status label", label, 80, true)?;
                safe_text("status detail", detail, 240, true)?;
                text_bytes += label.len() + detail.len();
            }
        }
    }

    for connector in &spec.connectors {
        safe_text("connector label", &connector.label, 80, false)?;
        text_bytes += connector.label.len();
        if !elements.contains_key(connector.from.as_str())
            || !elements.contains_key(connector.to.as_str())
        {
            return Err(format!(
                "connector {:?} has an unknown endpoint",
                connector.id
            ));
        }
        if matches!(elements[connector.from.as_str()], ElementKind::Group { .. })
            || matches!(elements[connector.to.as_str()], ElementKind::Group { .. })
        {
            return Err(format!(
                "connector {:?} cannot target a group",
                connector.id
            ));
        }
    }

    let connector_ids: HashSet<&str> = spec
        .connectors
        .iter()
        .map(|connector| connector.id.as_str())
        .collect();
    for (beat_index, beat) in spec.beats.iter().enumerate() {
        if beat.actions.len() > 12 {
            return Err(format!("beat {beat_index} has more than 12 actions"));
        }
        if !beat.duration.is_finite() || !(0.1..=5.0).contains(&beat.duration) {
            return Err(format!("beat {beat_index} duration must be 0.1..=5.0"));
        }
        safe_text("beat caption", &beat.caption, 240, true)?;
        text_bytes += beat.caption.len();
        for action in &beat.actions {
            let compatible = match action {
                Action::Focus { targets } => {
                    if targets.is_empty() {
                        return Err(format!("beat {beat_index} focus has no targets"));
                    }
                    for target in targets {
                        if !ids.contains(target.as_str()) {
                            return Err(format!(
                                "beat {beat_index} references unknown target {target:?}"
                            ));
                        }
                    }
                    true
                }
                Action::Flow { target, .. } => connector_ids.contains(target.as_str()),
                Action::Pulse { target } => elements.contains_key(target.as_str()),
                Action::Meter { target, from, to } => {
                    if !unit_interval(*from) || !unit_interval(*to) {
                        return Err(format!(
                            "beat {beat_index} meter values must be finite in 0..=1"
                        ));
                    }
                    matches!(
                        elements.get(target.as_str()),
                        Some(ElementKind::Meter { .. })
                    )
                }
                Action::Timeline { target, from, to } => {
                    if !unit_interval(*from) || !unit_interval(*to) {
                        return Err(format!(
                            "beat {beat_index} timeline values must be finite in 0..=1"
                        ));
                    }
                    matches!(
                        elements.get(target.as_str()),
                        Some(ElementKind::Timeline { .. })
                    )
                }
                Action::Scan { target } => {
                    matches!(
                        elements.get(target.as_str()),
                        Some(ElementKind::Plot { .. })
                    )
                }
                Action::Shift { target } => {
                    matches!(
                        elements.get(target.as_str()),
                        Some(ElementKind::Buffer { .. })
                    )
                }
            };
            if !compatible {
                let target = match action {
                    Action::Focus { .. } => unreachable!(),
                    Action::Flow { target, .. }
                    | Action::Pulse { target }
                    | Action::Meter { target, .. }
                    | Action::Timeline { target, .. }
                    | Action::Scan { target }
                    | Action::Shift { target } => target,
                };
                if !ids.contains(target.as_str()) {
                    return Err(format!(
                        "beat {beat_index} references unknown target {target:?}"
                    ));
                }
                return Err(format!(
                    "beat {beat_index} action is incompatible with target {target:?}"
                ));
            }
        }
    }
    if text_bytes > MAX_TEXT {
        return Err("scene text exceeds 8 KiB".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}

struct Story<'a> {
    beat: Option<&'a Beat>,
    index: usize,
    progress: f64,
    phase: f64,
    running: bool,
}

fn story(spec: &Spec, t: f64, running: bool) -> Story<'_> {
    if spec.beats.is_empty() {
        return Story {
            beat: None,
            index: 0,
            progress: 1.0,
            phase: 0.0,
            running,
        };
    }
    if !running {
        let index = spec.beats.len() - 1;
        return Story {
            beat: Some(&spec.beats[index]),
            index,
            progress: 1.0,
            phase: 0.0,
            running,
        };
    }
    let total: f64 = spec.beats.iter().map(|beat| beat.duration).sum();
    let clock = if t.is_finite() {
        t.rem_euclid(total)
    } else {
        0.0
    };
    let mut start = 0.0;
    for (index, beat) in spec.beats.iter().enumerate() {
        if clock < start + beat.duration || index + 1 == spec.beats.len() {
            let progress = ((clock - start) / beat.duration).clamp(0.0, 1.0);
            return Story {
                beat: Some(beat),
                index,
                progress,
                phase: clock,
                running,
            };
        }
        start += beat.duration;
    }
    unreachable!()
}

fn ease(value: f64) -> f64 {
    value * value * (3.0 - 2.0 * value)
}

fn tone_color(tone: Tone, focused: bool, pulsing: bool) -> (u8, u8, u8) {
    if !focused {
        return FAINT;
    }
    if pulsing {
        return ACCENT;
    }
    match tone {
        Tone::Normal => INK,
        Tone::Accent => ACCENT,
        Tone::Pass => PASS,
        Tone::Warn => WARN,
        Tone::Stop => STOP,
        Tone::Muted => MUTE,
    }
}

fn put(buf: &mut Buffer, clip: Rect, point: Point, ch: char, color: (u8, u8, u8), bold: bool) {
    if point.x < i32::from(clip.x)
        || point.y < i32::from(clip.y)
        || point.x >= i32::from(clip.right())
        || point.y >= i32::from(clip.bottom())
    {
        return;
    }
    if let Some(cell) = buf.cell_mut((point.x as u16, point.y as u16)) {
        let mut style = Style::default().fg(Color::Rgb(color.0, color.1, color.2));
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        cell.set_char(ch).set_style(style);
    }
}

#[cfg(test)]
fn render_night(buf: &mut Buffer, area: Rect, spec: &Spec, t: f64, running: bool) -> bool {
    render(buf, area, spec, t, running, termap::canvas::Theme::Night)
}

fn mapped(content: Rect, logical: RectSpec) -> Rect {
    let x = u32::from(content.x) + u32::from(logical.x) * u32::from(content.width) / 100;
    let y = u32::from(content.y) + u32::from(logical.y) * u32::from(content.height) / 100;
    let right = u32::from(content.x)
        + u32::from(logical.x + logical.width) * u32::from(content.width) / 100;
    let bottom = u32::from(content.y)
        + u32::from(logical.y + logical.height) * u32::from(content.height) / 100;
    Rect::new(
        x as u16,
        y as u16,
        right.saturating_sub(x).max(1) as u16,
        bottom.saturating_sub(y).max(1) as u16,
    )
}

fn draw_frame(buf: &mut Buffer, clip: Rect, rect: Rect, frame: Frame, color: (u8, u8, u8)) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let glyphs = match frame {
        Frame::Plain => ['╭', '╮', '╰', '╯', '─', '│'],
        Frame::Strong => ['┏', '┓', '┗', '┛', '━', '┃'],
        Frame::Double => ['╔', '╗', '╚', '╝', '═', '║'],
    };
    let right = i32::from(rect.right()) - 1;
    let bottom = i32::from(rect.bottom()) - 1;
    for x in i32::from(rect.x)..=right {
        put(
            buf,
            clip,
            Point {
                x,
                y: i32::from(rect.y),
            },
            glyphs[4],
            color,
            false,
        );
        put(buf, clip, Point { x, y: bottom }, glyphs[4], color, false);
    }
    for y in i32::from(rect.y)..=bottom {
        put(
            buf,
            clip,
            Point {
                x: i32::from(rect.x),
                y,
            },
            glyphs[5],
            color,
            false,
        );
        put(buf, clip, Point { x: right, y }, glyphs[5], color, false);
    }
    for (x, y, ch) in [
        (i32::from(rect.x), i32::from(rect.y), glyphs[0]),
        (right, i32::from(rect.y), glyphs[1]),
        (i32::from(rect.x), bottom, glyphs[2]),
        (right, bottom, glyphs[3]),
    ] {
        put(buf, clip, Point { x, y }, ch, color, false);
    }
}

fn title_on_frame(buf: &mut Buffer, clip: Rect, rect: Rect, title: &str, color: (u8, u8, u8)) {
    if rect.width < 5 {
        return;
    }
    let available = rect.width.saturating_sub(4) as usize;
    let shown: String = title.chars().take(available).collect();
    text(
        buf,
        clip,
        i32::from(rect.x) + 2,
        i32::from(rect.y),
        &format!(" {shown} "),
        color,
        true,
    );
}

fn focus_targets(story: &Story<'_>) -> Option<HashSet<String>> {
    if !story.running {
        return None;
    }
    let targets: HashSet<String> = story
        .beat
        .into_iter()
        .flat_map(|beat| &beat.actions)
        .filter_map(|action| match action {
            Action::Focus { targets } => Some(targets.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    (!targets.is_empty()).then_some(targets)
}

fn is_pulsing(story: &Story<'_>, id: &str) -> bool {
    story.running
        && story.beat.is_some_and(|beat| {
            beat.actions
                .iter()
                .any(|action| matches!(action, Action::Pulse { target } if target == id))
        })
        && (story.phase * 4.0).sin() >= 0.0
}

fn numeric_value(
    spec: &Spec,
    story: &Story<'_>,
    target: &str,
    initial: f64,
    timeline: bool,
) -> f64 {
    let mut value = initial;
    for (index, beat) in spec.beats.iter().enumerate() {
        if index > story.index || (story.beat.is_none() && index == 0) {
            break;
        }
        for action in &beat.actions {
            let range = match action {
                Action::Meter {
                    target: id,
                    from,
                    to,
                } if !timeline && id == target => Some((*from, *to)),
                Action::Timeline {
                    target: id,
                    from,
                    to,
                } if timeline && id == target => Some((*from, *to)),
                _ => None,
            };
            if let Some((from, to)) = range {
                value = if index < story.index || !story.running {
                    to
                } else {
                    from + (to - from) * ease(story.progress)
                };
            }
        }
    }
    value
}

fn connector_path(from: Rect, to: Rect) -> Vec<Point> {
    let from_center = Point {
        x: i32::from(from.x) + i32::from(from.width) / 2,
        y: i32::from(from.y) + i32::from(from.height) / 2,
    };
    let to_center = Point {
        x: i32::from(to.x) + i32::from(to.width) / 2,
        y: i32::from(to.y) + i32::from(to.height) / 2,
    };
    let horizontal = (to_center.x - from_center.x).abs() >= (to_center.y - from_center.y).abs();
    let (start, end, bend) = if horizontal {
        let forward = to_center.x >= from_center.x;
        let start = Point {
            x: if forward {
                i32::from(from.right())
            } else {
                i32::from(from.x) - 1
            },
            y: from_center.y,
        };
        let end = Point {
            x: if forward {
                i32::from(to.x) - 1
            } else {
                i32::from(to.right())
            },
            y: to_center.y,
        };
        (
            start,
            end,
            Point {
                x: (start.x + end.x) / 2,
                y: start.y,
            },
        )
    } else {
        let forward = to_center.y >= from_center.y;
        let start = Point {
            x: from_center.x,
            y: if forward {
                i32::from(from.bottom())
            } else {
                i32::from(from.y) - 1
            },
        };
        let end = Point {
            x: to_center.x,
            y: if forward {
                i32::from(to.y) - 1
            } else {
                i32::from(to.bottom())
            },
        };
        (
            start,
            end,
            Point {
                x: start.x,
                y: (start.y + end.y) / 2,
            },
        )
    };
    let corner = Point {
        x: bend.x,
        y: end.y,
    };
    let mut path = Vec::new();
    append_segment(&mut path, start, bend);
    append_segment(&mut path, bend, corner);
    append_segment(&mut path, corner, end);
    path
}

fn append_segment(path: &mut Vec<Point>, from: Point, to: Point) {
    let mut point = from;
    if path
        .last()
        .is_none_or(|last| last.x != point.x || last.y != point.y)
    {
        path.push(point);
    }
    while point.x != to.x || point.y != to.y {
        point.x += (to.x - point.x).signum();
        point.y += (to.y - point.y).signum();
        path.push(point);
    }
}

fn draw_connector(
    buf: &mut Buffer,
    clip: Rect,
    connector: &Connector,
    path: &[Point],
    color: (u8, u8, u8),
    story: &Story<'_>,
) {
    if path.is_empty() {
        return;
    }
    for (index, point) in path.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|at| path.get(at));
        let next = path.get(index + 1);
        let vertical = previous.zip(next).is_some_and(|(a, b)| a.x == b.x)
            || previous.is_some_and(|a| a.x == point.x)
            || next.is_some_and(|b| b.x == point.x);
        let mut glyph = if vertical { '│' } else { '─' };
        if connector.style == ConnectorStyle::Dashed && index % 3 == 1 {
            glyph = ' ';
        }
        put(buf, clip, *point, glyph, color, false);
    }
    let first = path[0];
    let last = path[path.len() - 1];
    let end_arrow = if path.len() > 1 {
        arrow_glyph(path[path.len() - 2], last)
    } else {
        '▶'
    };
    match connector.style {
        ConnectorStyle::Bidirectional => {
            put(buf, clip, first, arrow_glyph(path[1], first), color, true);
            put(buf, clip, last, end_arrow, color, true);
        }
        ConnectorStyle::Blocked => {
            put(buf, clip, path[path.len() / 2], '✕', STOP, true);
        }
        ConnectorStyle::Arrow | ConnectorStyle::Dashed => {
            put(buf, clip, last, end_arrow, color, true)
        }
    }
    let middle = path[path.len() / 2];
    if story.running {
        let flow = story.beat.and_then(|beat| {
            beat.actions.iter().find_map(|action| match action {
                Action::Flow { target, reverse } if target == &connector.id => Some(*reverse),
                _ => None,
            })
        });
        if let Some(reverse) = flow {
            for offset in [0.0, 0.34, 0.67] {
                let phase = (story.progress + offset) % 1.0;
                let phase = if reverse { 1.0 - phase } else { phase };
                let index = (phase * (path.len() - 1) as f64).round() as usize;
                put(buf, clip, path[index], '●', color, true);
            }
        }
    }
    if !connector.label.is_empty() {
        let shown: String = connector.label.chars().take(24).collect();
        text(
            buf,
            clip,
            middle.x - shown.chars().count() as i32 / 2,
            middle.y - 1,
            &shown,
            color,
            false,
        );
    }
}

fn arrow_glyph(from: Point, to: Point) -> char {
    match ((to.x - from.x).signum(), (to.y - from.y).signum()) {
        (1, _) => '▶',
        (-1, _) => '◀',
        (_, 1) => '▼',
        _ => '▲',
    }
}

fn draw_element(
    buf: &mut Buffer,
    clip: Rect,
    spec: &Spec,
    element: &Element,
    rect: Rect,
    color: (u8, u8, u8),
    story: &Story<'_>,
) {
    let x = i32::from(rect.x);
    let y = i32::from(rect.y);
    match &element.kind {
        ElementKind::Group { .. } => {}
        ElementKind::Box {
            title,
            lines,
            frame,
        } => {
            draw_frame(buf, clip, rect, *frame, color);
            title_on_frame(buf, clip, rect, title, color);
            let width = rect.width.saturating_sub(4) as usize;
            for (row, line) in lines
                .iter()
                .take(rect.height.saturating_sub(2) as usize)
                .enumerate()
            {
                let shown: String = line.chars().take(width).collect();
                text(buf, clip, x + 2, y + row as i32 + 1, &shown, color, false);
            }
        }
        ElementKind::Text {
            text: value,
            role,
            align,
        } => {
            let (tone, bold) = match role {
                TextRole::Heading => (color, true),
                TextRole::Body => (color, false),
                TextRole::Annotation => (if color == FAINT { FAINT } else { MUTE }, false),
                TextRole::Callout => (color, true),
            };
            if *role == TextRole::Callout {
                put(buf, clip, Point { x, y }, '▌', tone, true);
                note(
                    buf,
                    clip,
                    x + 2,
                    y,
                    rect.width.saturating_sub(2) as usize,
                    value,
                    tone,
                );
            } else if *role == TextRole::Body && value.chars().count() > rect.width as usize {
                note(buf, clip, x, y, rect.width as usize, value, tone);
            } else {
                let shown: String = value.chars().take(rect.width as usize).collect();
                let offset = match align {
                    Align::Left => 0,
                    Align::Center => {
                        (rect.width.saturating_sub(shown.chars().count() as u16) / 2) as i32
                    }
                    Align::Right => rect.width.saturating_sub(shown.chars().count() as u16) as i32,
                };
                text(buf, clip, x + offset, y, &shown, tone, bold);
            }
        }
        ElementKind::Meter { label, value, unit } => {
            let current = numeric_value(spec, story, &element.id, *value, false);
            let label_width = label.chars().count().min(rect.width as usize);
            text(
                buf,
                clip,
                x,
                y,
                &label.chars().take(label_width).collect::<String>(),
                color,
                true,
            );
            if rect.height > 1 {
                let meter_width = i32::from(rect.width.saturating_sub(10)).max(3);
                bar(buf, clip, x, y + 1, meter_width, current, color);
                let amount = if unit.is_empty() {
                    format!("{:>3.0}%", current * 100.0)
                } else {
                    format!("{:>4.0} {unit}", current * 100.0)
                };
                text(buf, clip, x + meter_width + 1, y + 1, &amount, color, false);
            }
        }
        ElementKind::Buffer { label, cells } => {
            text(buf, clip, x, y, label, color, true);
            if rect.height < 2 {
                return;
            }
            let shift = story.beat.and_then(|beat| {
                beat.actions.iter().find_map(|action| {
                    matches!(action, Action::Shift { target } if target == &element.id)
                        .then_some((story.progress * cells.len() as f64).floor() as usize)
                })
            });
            let shown = cells
                .len()
                .min(rect.width.saturating_sub(1) as usize / 2)
                .max(1);
            for index in 0..shown {
                let state = cells[(index + shift.unwrap_or(0)) % cells.len()];
                let (glyph, cell_color) = match state {
                    CellState::Empty => ('·', FAINT),
                    CellState::Ready => ('▪', MUTE),
                    CellState::Active => ('◆', ACCENT),
                    CellState::Done => ('■', PASS),
                    CellState::Blocked => ('×', STOP),
                };
                put(
                    buf,
                    clip,
                    Point {
                        x: x + index as i32 * 2,
                        y: y + 1,
                    },
                    glyph,
                    cell_color,
                    state == CellState::Active,
                );
            }
        }
        ElementKind::Plot {
            label,
            samples,
            kind,
        } => {
            text(buf, clip, x, y, label, color, true);
            if rect.height < 2 || rect.width == 0 {
                return;
            }
            let scan = story.beat.and_then(|beat| {
                beat.actions.iter().find_map(|action| {
                    matches!(action, Action::Scan { target } if target == &element.id)
                        .then_some(story.progress)
                })
            });
            let columns = rect.width as usize;
            let reveal = (columns as f64 * scan.unwrap_or(1.0)).ceil() as usize;
            let values: Vec<f64> = (0..columns)
                .map(|column| {
                    samples[column * (samples.len() - 1) / columns.max(1)].clamp(0.0, 1.0)
                })
                .collect();
            match kind {
                PlotKind::Sparkline => spark(
                    buf,
                    clip,
                    x,
                    y + 1,
                    &values[..reveal.min(values.len())],
                    color,
                ),
                PlotKind::Waveform => {
                    let height = i32::from(rect.height.saturating_sub(1)).max(1);
                    let middle = y + 1 + height / 2;
                    for (column, sample) in values.iter().take(reveal).enumerate() {
                        let py = y + 1 + ((1.0 - sample) * f64::from(height - 1)).round() as i32;
                        put(
                            buf,
                            clip,
                            Point {
                                x: x + column as i32,
                                y: middle,
                            },
                            '·',
                            FAINT,
                            false,
                        );
                        put(
                            buf,
                            clip,
                            Point {
                                x: x + column as i32,
                                y: py,
                            },
                            '●',
                            color,
                            false,
                        );
                    }
                }
                PlotKind::Bars => {
                    let height = i32::from(rect.height.saturating_sub(1)).max(1);
                    for (column, sample) in values.iter().take(reveal).enumerate() {
                        let filled = (sample * f64::from(height)).round() as i32;
                        for row in 0..height {
                            if row < filled {
                                put(
                                    buf,
                                    clip,
                                    Point {
                                        x: x + column as i32,
                                        y: y + height - row,
                                    },
                                    '█',
                                    color,
                                    false,
                                );
                            }
                        }
                    }
                }
            }
        }
        ElementKind::Timeline {
            label,
            markers,
            cursor,
        } => {
            text(buf, clip, x, y, label, color, true);
            if rect.height < 2 || rect.width < 2 {
                return;
            }
            let width = i32::from(rect.width) - 1;
            for offset in 0..=width {
                put(
                    buf,
                    clip,
                    Point {
                        x: x + offset,
                        y: y + 1,
                    },
                    '─',
                    FAINT,
                    false,
                );
            }
            for marker in markers {
                let mx = x + (marker.at * f64::from(width)).round() as i32;
                put(
                    buf,
                    clip,
                    Point { x: mx, y: y + 1 },
                    '┬',
                    tone_color(marker.tone, color != FAINT, false),
                    false,
                );
                if rect.height > 2 {
                    text(
                        buf,
                        clip,
                        mx,
                        y + 2,
                        &marker.label,
                        tone_color(marker.tone, color != FAINT, false),
                        false,
                    );
                }
            }
            let current = numeric_value(spec, story, &element.id, *cursor, true);
            put(
                buf,
                clip,
                Point {
                    x: x + (current * f64::from(width)).round() as i32,
                    y: y + 1,
                },
                '●',
                color,
                true,
            );
        }
        ElementKind::Status {
            label,
            state,
            detail,
        } => {
            let (glyph, state_color, word) = match state {
                StatusState::Idle => ('○', MUTE, "idle"),
                StatusState::Active => ('◉', ACCENT, "active"),
                StatusState::Pass => ('✓', PASS, "pass"),
                StatusState::Warn => ('▲', WARN, "warn"),
                StatusState::Stop => ('✕', STOP, "stop"),
            };
            put(
                buf,
                clip,
                Point { x, y },
                glyph,
                if color == FAINT { FAINT } else { state_color },
                true,
            );
            text(buf, clip, x + 2, y, label, color, true);
            text(
                buf,
                clip,
                x + 2 + label.chars().count() as i32 + 1,
                y,
                word,
                if color == FAINT { FAINT } else { state_color },
                false,
            );
            if rect.height > 1 {
                note(
                    buf,
                    clip,
                    x + 2,
                    y + 1,
                    rect.width.saturating_sub(2) as usize,
                    detail,
                    if color == FAINT { FAINT } else { MUTE },
                );
            }
        }
    }
}

pub fn render(
    buf: &mut Buffer,
    area: Rect,
    spec: &Spec,
    t: f64,
    running: bool,
    th: termap::canvas::Theme,
) -> bool {
    if area.width < 20 || area.height < 9 || validate(spec).is_err() {
        return false;
    }
    let content = Rect::new(area.x, area.y + 2, area.width, area.height - 4);
    let story = story(spec, t, running);
    let focus = focus_targets(&story);
    let rects: HashMap<&str, Rect> = spec
        .elements
        .iter()
        .map(|element| (element.id.as_str(), mapped(content, element.rect)))
        .collect();

    for element in spec
        .elements
        .iter()
        .filter(|element| matches!(element.kind, ElementKind::Group { .. }))
    {
        let focused = focus
            .as_ref()
            .is_none_or(|targets| targets.contains(element.id.as_str()));
        let element_color = tone_color(element.tone, focused, is_pulsing(&story, &element.id));
        let rect = rects[element.id.as_str()];
        draw_frame(
            buf,
            area,
            rect,
            Frame::Plain,
            if focused { MUTE } else { FAINT },
        );
        if let ElementKind::Group { title } = &element.kind {
            title_on_frame(buf, area, rect, title, element_color);
        }
    }

    for connector in &spec.connectors {
        let focused = focus
            .as_ref()
            .is_none_or(|targets| targets.contains(connector.id.as_str()));
        let connector_color = tone_color(connector.tone, focused, false);
        let path = connector_path(rects[connector.from.as_str()], rects[connector.to.as_str()]);
        draw_connector(buf, area, connector, &path, connector_color, &story);
    }

    for element in spec
        .elements
        .iter()
        .filter(|element| !matches!(element.kind, ElementKind::Group { .. }))
    {
        let focused = focus
            .as_ref()
            .is_none_or(|targets| targets.contains(element.id.as_str()));
        let element_color = tone_color(element.tone, focused, is_pulsing(&story, &element.id));
        draw_element(
            buf,
            area,
            spec,
            element,
            rects[element.id.as_str()],
            element_color,
            &story,
        );
    }

    let shown_title: String = spec.title.chars().take(area.width as usize).collect();
    let title_x = i32::from(area.x)
        + i32::from(
            area.width
                .saturating_sub(shown_title.chars().count() as u16),
        ) / 2;
    text(
        buf,
        area,
        title_x,
        i32::from(area.y),
        &shown_title,
        INK,
        true,
    );
    if let Some(beat) = story.beat {
        let caption = format!("{}/{}  {}", story.index + 1, spec.beats.len(), beat.caption);
        let shown: String = caption.chars().take(area.width as usize).collect();
        let x = i32::from(area.x)
            + i32::from(area.width.saturating_sub(shown.chars().count() as u16)) / 2;
        text(
            buf,
            area,
            x,
            i32::from(area.bottom()) - 1,
            &shown,
            INK,
            true,
        );
    }
    // As in `scene::draw`: the colours in here are literals chosen against
    // black, turned round in one sweep rather than threaded through every
    // `put`. Diagram outlines going white on cream was the visible half of
    // this; the boxes were drawn in `MUTE` and `FAINT`, which are pale greys.
    termap::canvas::recast_region(buf, area, th);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: &str, rect: RectSpec, kind: ElementKind) -> Element {
        Element {
            id: id.into(),
            rect,
            tone: Tone::Normal,
            kind,
        }
    }

    fn dense_spec() -> Spec {
        let r = |x, y, width, height| RectSpec {
            x,
            y,
            width,
            height,
        };
        Spec {
            title: "Streaming control plane".into(),
            elements: vec![
                element(
                    "zone",
                    r(0, 0, 100, 100),
                    ElementKind::Group {
                        title: "runtime".into(),
                    },
                ),
                element(
                    "box",
                    r(2, 8, 27, 25),
                    ElementKind::Box {
                        title: "ingress".into(),
                        lines: vec!["decode request".into(), "check quota".into()],
                        frame: Frame::Double,
                    },
                ),
                element(
                    "heading",
                    r(34, 2, 30, 8),
                    ElementKind::Text {
                        text: "CONTROL LOOP".into(),
                        role: TextRole::Heading,
                        align: Align::Center,
                    },
                ),
                element(
                    "callout",
                    r(67, 3, 30, 14),
                    ElementKind::Text {
                        text: "Backpressure keeps the producer bounded".into(),
                        role: TextRole::Callout,
                        align: Align::Left,
                    },
                ),
                element(
                    "meter",
                    r(34, 15, 28, 13),
                    ElementKind::Meter {
                        label: "capacity".into(),
                        value: 0.2,
                        unit: "%".into(),
                    },
                ),
                element(
                    "buffer",
                    r(67, 20, 30, 12),
                    ElementKind::Buffer {
                        label: "segments".into(),
                        cells: vec![
                            CellState::Empty,
                            CellState::Ready,
                            CellState::Active,
                            CellState::Done,
                            CellState::Blocked,
                        ],
                    },
                ),
                element(
                    "spark",
                    r(2, 43, 28, 12),
                    ElementKind::Plot {
                        label: "latency".into(),
                        samples: vec![0.1, 0.8, 0.3, 0.7],
                        kind: PlotKind::Sparkline,
                    },
                ),
                element(
                    "wave",
                    r(35, 39, 27, 20),
                    ElementKind::Plot {
                        label: "signal".into(),
                        samples: vec![0.1, 0.9, 0.2, 0.8, 0.4],
                        kind: PlotKind::Waveform,
                    },
                ),
                element(
                    "bars",
                    r(68, 40, 28, 20),
                    ElementKind::Plot {
                        label: "workers".into(),
                        samples: vec![0.2, 0.8, 0.4, 1.0],
                        kind: PlotKind::Bars,
                    },
                ),
                element(
                    "timeline",
                    r(3, 68, 58, 20),
                    ElementKind::Timeline {
                        label: "release".into(),
                        markers: vec![
                            Marker {
                                at: 0.2,
                                label: "build".into(),
                                tone: Tone::Accent,
                            },
                            Marker {
                                at: 0.8,
                                label: "ship".into(),
                                tone: Tone::Pass,
                            },
                        ],
                        cursor: 0.1,
                    },
                ),
                element(
                    "status",
                    r(68, 70, 28, 16),
                    ElementKind::Status {
                        label: "gateway".into(),
                        state: StatusState::Pass,
                        detail: "all probes healthy".into(),
                    },
                ),
            ],
            connectors: vec![
                Connector {
                    id: "flow".into(),
                    from: "box".into(),
                    to: "meter".into(),
                    label: "evt".into(),
                    tone: Tone::Accent,
                    style: ConnectorStyle::Arrow,
                },
                Connector {
                    id: "duplex".into(),
                    from: "meter".into(),
                    to: "buffer".into(),
                    label: "sync".into(),
                    tone: Tone::Normal,
                    style: ConnectorStyle::Bidirectional,
                },
                Connector {
                    id: "blocked".into(),
                    from: "spark".into(),
                    to: "wave".into(),
                    label: "reject".into(),
                    tone: Tone::Stop,
                    style: ConnectorStyle::Blocked,
                },
                Connector {
                    id: "dashed".into(),
                    from: "wave".into(),
                    to: "bars".into(),
                    label: "sample".into(),
                    tone: Tone::Muted,
                    style: ConnectorStyle::Dashed,
                },
            ],
            beats: vec![Beat {
                caption: "Traffic crosses the control loop".into(),
                duration: 2.0,
                actions: vec![
                    Action::Flow {
                        target: "flow".into(),
                        reverse: false,
                    },
                    Action::Pulse {
                        target: "box".into(),
                    },
                    Action::Meter {
                        target: "meter".into(),
                        from: 0.2,
                        to: 0.9,
                    },
                    Action::Timeline {
                        target: "timeline".into(),
                        from: 0.1,
                        to: 0.85,
                    },
                    Action::Scan {
                        target: "wave".into(),
                    },
                    Action::Shift {
                        target: "buffer".into(),
                    },
                ],
            }],
        }
    }

    fn symbols(buf: &Buffer) -> String {
        let mut output = String::new();
        for y in buf.area.y..buf.area.bottom() {
            for x in buf.area.x..buf.area.right() {
                output.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn dense_scene_renders_every_rich_primitive_and_moves() {
        let spec = dense_spec();
        let area = Rect::new(0, 0, 120, 38);
        let mut first = Buffer::empty(area);
        let mut later = Buffer::empty(area);
        assert!(render(&mut first, area, &spec, 0.2, true, termap::canvas::Theme::Night));
        assert!(render(&mut later, area, &spec, 1.2, true, termap::canvas::Theme::Night));
        let output = symbols(&later);
        for text in [
            "ingress",
            "CONTROL LOOP",
            "capacity",
            "segments",
            "latency",
            "signal",
            "workers",
            "release",
            "gateway",
            "evt",
            "sync",
            "reject",
            "sample",
        ] {
            assert!(output.contains(text), "missing {text:?}\n{output}");
        }
        for glyph in ['╔', '▓', '◆', '▂', '●', '█', '┬', '✓', '✕'] {
            assert!(output.contains(glyph), "missing {glyph:?}\n{output}");
        }
        assert_ne!(first, later);
    }

    #[test]
    fn settled_scene_holds_final_meter_and_timeline_values() {
        let spec = dense_spec();
        let area = Rect::new(0, 0, 120, 38);
        let mut early = Buffer::empty(area);
        let mut late = Buffer::empty(area);
        assert!(render(&mut early, area, &spec, 0.0, false, termap::canvas::Theme::Night));
        assert!(render(&mut late, area, &spec, 99.0, false, termap::canvas::Theme::Night));
        assert_eq!(early, late);
        let output = symbols(&early);
        assert!(output.contains("90 %"), "{output}");
        let mut lines = output.lines();
        lines.find(|line| line.contains("release")).unwrap();
        let timeline_row = lines.next().unwrap();
        assert!(timeline_row.find('●').unwrap() > timeline_row.len() / 2);
    }

    #[test]
    fn settled_scene_does_not_keep_the_last_focus_dim() {
        let mut focused = dense_spec();
        focused.beats[0].actions.push(Action::Focus {
            targets: vec!["box".into()],
        });
        let mut plain = focused.clone();
        plain.beats[0]
            .actions
            .retain(|action| !matches!(action, Action::Focus { .. }));
        let area = Rect::new(0, 0, 120, 38);
        let mut focused_frame = Buffer::empty(area);
        let mut plain_frame = Buffer::empty(area);
        assert!(render(&mut focused_frame, area, &focused, 99.0, false, termap::canvas::Theme::Night));
        assert!(render(&mut plain_frame, area, &plain, 99.0, false, termap::canvas::Theme::Night));
        assert_eq!(
            focused_frame, plain_frame,
            "the final focus left the scene faint"
        );
    }

    #[test]
    fn validation_rejects_geometry_references_types_and_limits() {
        let mut spec = dense_spec();
        assert_eq!(validate(&spec), Ok(()));
        spec.elements[0].rect.width = 101;
        assert!(validate(&spec).unwrap_err().contains("geometry"));

        let mut spec = dense_spec();
        spec.connectors[0].to = "missing".into();
        assert!(validate(&spec).unwrap_err().contains("endpoint"));

        let mut spec = dense_spec();
        spec.beats[0].actions.push(Action::Scan {
            target: "meter".into(),
        });
        assert!(validate(&spec).unwrap_err().contains("incompatible"));

        let mut spec = dense_spec();
        spec.elements.extend((0..22).map(|index| {
            element(
                &format!("extra{index}"),
                RectSpec {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                ElementKind::Text {
                    text: "x".into(),
                    role: TextRole::Body,
                    align: Align::Left,
                },
            )
        }));
        assert!(validate(&spec).unwrap_err().contains("1..=32"));
    }

    #[test]
    fn rendering_is_deterministic_and_clipped() {
        let spec = dense_spec();
        let whole = Rect::new(0, 0, 90, 30);
        let clip = Rect::new(4, 3, 80, 24);
        let mut first = Buffer::empty(whole);
        let mut same = Buffer::empty(whole);
        assert!(render(&mut first, clip, &spec, 0.75, true, termap::canvas::Theme::Night));
        assert!(render(&mut same, clip, &spec, 0.75, true, termap::canvas::Theme::Night));
        assert_eq!(first, same);
        for y in whole.y..whole.bottom() {
            for x in whole.x..whole.right() {
                if !clip.contains((x, y).into()) {
                    assert_eq!(first.cell((x, y)).unwrap().symbol(), " ");
                }
            }
        }
        assert!(!render_night(
            &mut first,
            Rect::new(0, 0, 19, 8),
            &spec,
            0.0,
            true
        ));
    }
}
