//! Who came, read back out of the visit log.
//!
//! `visits.rs` writes it, one line per thing that happened. That is the right
//! shape to append to and the wrong one to read: a single visit is an arrival,
//! a geolocation that lands whenever the lookup returns, a line per question
//! and a departure, interleaved with everybody else's. This puts them back
//! together, folds the visits into the people who made them, and draws it.
//!
//! Reading it is the same program as being it, on purpose. Opening a
//! conversation here does not print a transcript -- it hands the screen to the
//! chat, in read-only mode, with the map or the diagram or the project card
//! that came with each answer. Those renderers already exist and there is only
//! one honest way to show what somebody saw, which is to show them it.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::paint::{ACCENT, CYAN, DIM, FAINT, FG};

/// One question, and what became of it.
///
/// `times` because somebody typing `hey` into a dead agent nine times is one
/// thing that happened nine times, and nine identical rows bury every other
/// question in the visit underneath them.
pub struct Said {
    pub q: String,
    pub a: String,
    pub status: String,
    pub times: usize,
}

/// One visit, from arrival to departure.
#[derive(Default)]
pub struct Stay {
    pub session: String,
    pub at: u64,
    pub secs: Option<u64>,
    pub via: String,
    pub user: String,
    pub id: String,
    pub ip: String,
    pub client: String,
    pub place: String,
    pub said: Vec<Said>,
    sent: Vec<String>,
}

impl Stay {
    /// What to call them.
    ///
    /// Over ssh there is a name, and a better one than an account: any string
    /// is accepted as a username, so it is what somebody chose to be called. A
    /// browser has nowhere to type one, so its stable id stands in -- not a
    /// name, but the thing that makes two visits the same person.
    pub fn name(&self) -> String {
        if !self.user.is_empty() {
            return self.user.clone();
        }
        match self.id.is_empty() {
            true => "someone".to_string(),
            false => self.id.chars().take(14).collect(),
        }
    }

    fn asked(&self) -> usize {
        self.said.iter().map(|s| s.times).sum()
    }
}

/// Every visit one person made.
pub struct Visitor {
    pub name: String,
    pub id: String,
    pub stays: Vec<Stay>,
}

impl Visitor {
    pub fn asked(&self) -> usize {
        self.stays.iter().map(Stay::asked).sum()
    }
    pub fn secs(&self) -> u64 {
        self.stays.iter().filter_map(|s| s.secs).sum()
    }
    pub fn last(&self) -> u64 {
        self.stays.iter().map(|s| s.at).max().unwrap_or(0)
    }
    fn first(&self) -> u64 {
        self.stays.iter().map(|s| s.at).min().unwrap_or(0)
    }
    pub fn wheres(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for stay in &self.stays {
            if !stay.place.is_empty() && !out.contains(&stay.place) {
                out.push(stay.place.clone());
            }
        }
        out
    }
    fn client(&self) -> String {
        self.stays
            .iter()
            .rev()
            .map(|s| s.client.clone())
            .find(|c| !c.is_empty())
            .unwrap_or_default()
    }
    fn via(&self) -> String {
        self.stays
            .iter()
            .rev()
            .map(|s| s.via.clone())
            .find(|v| !v.is_empty())
            .unwrap_or_default()
    }
}

/// The log, as people.
pub fn read() -> Vec<Visitor> {
    let Ok(text) = std::fs::read_to_string(crate::visits::path()) else {
        return Vec::new();
    };

    let mut order: Vec<String> = Vec::new();
    let mut stays: std::collections::HashMap<String, Stay> = std::collections::HashMap::new();

    for line in text.lines() {
        // A half-written last line is skipped rather than fatal: this is read
        // against a log that is being appended to while it is read.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text_of = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
        let session = text_of("session");
        if session.is_empty() {
            continue;
        }
        let stay = stays.entry(session.clone()).or_insert_with(|| {
            order.push(session.clone());
            Stay { session: session.clone(), ..Stay::default() }
        });

        match v.get("event").and_then(|e| e.as_str()).unwrap_or_default() {
            "arrive" => {
                stay.at = num("at");
                stay.via = text_of("via");
                stay.user = text_of("user");
                stay.id = text_of("id");
                stay.ip = text_of("ip");
                stay.client = text_of("client");
            }
            "where" => {
                // A city that is also its own region -- Berlin, Singapore,
                // Delhi -- otherwise says itself twice.
                let mut parts: Vec<String> = Vec::new();
                for part in [text_of("city"), text_of("region"), text_of("country")] {
                    if !part.is_empty() && !parts.contains(&part) {
                        parts.push(part);
                    }
                }
                stay.place = parts.join(", ");
            }
            // A question is logged when it is sent and again when it comes
            // back. Listing both would list every answered one twice.
            "ask" => stay.said.push(Said {
                q: text_of("q"),
                a: text_of("a"),
                status: String::new(),
                times: 1,
            }),
            "question_status" => stay.said.push(Said {
                q: text_of("q"),
                a: String::new(),
                status: text_of("status"),
                times: 1,
            }),
            "question" => stay.sent.push(text_of("q")),
            "leave" => stay.secs = Some(num("secs")),
            _ => {}
        }
    }

    let mut people: Vec<Visitor> = Vec::new();
    for session in order {
        let Some(mut stay) = stays.remove(&session) else { continue };
        settle(&mut stay);

        // Folded by the stable identity where there is one. Without it every
        // visit is its own stranger, which is the honest answer: an ssh client
        // with no key and a browser with no storage have told us nothing that
        // would make two visits the same person.
        let key = stay.id.clone();
        let at = people.iter().position(|p| !key.is_empty() && p.id == key);
        match at {
            Some(at) => people[at].stays.push(stay),
            None => people.push(Visitor {
                name: stay.name(),
                id: key,
                stays: vec![stay],
            }),
        }
    }
    // Each person's visits in the order they made them, whatever order the
    // lines came in. The numbers beside them are keys somebody presses, and a
    // list numbered by file order would be numbered by nothing they can see.
    for person in &mut people {
        person.stays.sort_by_key(|s| s.at);
    }
    people.sort_by_key(|p| std::cmp::Reverse(p.last()));
    people
}

/// Every question once, however it ended, and a run of the same one folded.
fn settle(stay: &mut Stay) {
    let seen: Vec<String> = stay.said.iter().map(|s| s.q.clone()).collect();
    for q in std::mem::take(&mut stay.sent) {
        if !seen.contains(&q) {
            stay.said.push(Said {
                q,
                a: String::new(),
                status: "unanswered".into(),
                times: 1,
            });
        }
    }
    let mut folded: Vec<Said> = Vec::new();
    for said in std::mem::take(&mut stay.said) {
        match folded.last_mut() {
            Some(last) if last.q == said.q && last.a == said.a && last.status == said.status => {
                last.times += 1;
            }
            _ => folded.push(said),
        }
    }
    stay.said = folded;
}

/// `1h02m`, `8m14s`, `9s`. `open` for a visit with no departure -- one still
/// going, or one whose process was killed under it. Not a duration, and not
/// printed as one.
fn spell(secs: Option<u64>) -> String {
    let Some(s) = secs else { return "open".into() };
    match (s / 3600, (s % 3600) / 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

/// A unix second as `YYYY-MM-DD HH:MM`, in UTC.
///
/// Hand-rolled for the same reason `json.rs` is: twenty lines against a
/// dependency, and this is the only date this program formats. The
/// civil-from-days part is Howard Hinnant's, which is the one everybody uses
/// because getting it wrong is a leap year nobody notices for three years.
fn stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (h, m) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mth <= 2);
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}")
}

/// What the key just pressed means to whoever is driving the loop.
pub enum Went {
    Nowhere,
    Out,
    Into { session: String, whose: String },
}

/// Which column the list is ordered by.
#[derive(Clone, Copy, PartialEq)]
enum By {
    Last,
    Visits,
    Questions,
}

impl By {
    fn label(self) -> &'static str {
        match self {
            By::Last => "recent",
            By::Visits => "visits",
            By::Questions => "questions",
        }
    }
}

pub struct Browser {
    people: Vec<Visitor>,
    cursor: usize,
    top: usize,
    open: Option<usize>,
    scroll: usize,
    query: String,
    searching: bool,
    returning_only: bool,
    by: By,
    /// Which sessions the number keys currently open, in the order drawn.
    numbered: Vec<String>,
}

impl Browser {
    pub fn new() -> Browser {
        Browser {
            people: read(),
            cursor: 0,
            top: 0,
            open: None,
            scroll: 0,
            query: String::new(),
            searching: false,
            returning_only: false,
            by: By::Last,
            numbered: Vec::new(),
        }
    }

    /// Who a conversation belonged to, and when, for the header of its replay.
    fn whose(&self, session: &str) -> String {
        for person in &self.people {
            for stay in &person.stays {
                if stay.session == session {
                    let mut out = format!("{}  ·  {}", person.name, stamp(stay.at));
                    if !stay.place.is_empty() {
                        out.push_str(&format!("  ·  {}", stay.place));
                    }
                    return out;
                }
            }
        }
        String::new()
    }

    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    fn shown(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        let mut out: Vec<usize> = (0..self.people.len())
            .filter(|&i| {
                let p = &self.people[i];
                if self.returning_only && p.stays.len() < 2 {
                    return false;
                }
                if q.is_empty() {
                    return true;
                }
                let mut hay = format!("{} {} {} {}", p.name, p.id, p.via(), p.wheres().join(" "));
                for stay in &p.stays {
                    hay.push_str(&stay.ip);
                    for said in &stay.said {
                        hay.push(' ');
                        hay.push_str(&said.q);
                    }
                }
                hay.to_lowercase().contains(&q)
            })
            .collect();
        match self.by {
            By::Last => out.sort_by_key(|&i| std::cmp::Reverse(self.people[i].last())),
            By::Visits => out.sort_by_key(|&i| std::cmp::Reverse(self.people[i].stays.len())),
            By::Questions => out.sort_by_key(|&i| std::cmp::Reverse(self.people[i].asked())),
        }
        out
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) -> Went {
        use crossterm::event::{KeyCode, KeyModifiers};
        if k.kind != crossterm::event::KeyEventKind::Press {
            return Went::Nowhere;
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Went::Out;
        }

        if self.searching {
            match k.code {
                KeyCode::Esc => {
                    self.searching = false;
                    self.query.clear();
                }
                KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            self.cursor = 0;
            self.top = 0;
            return Went::Nowhere;
        }

        if self.open.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    self.open = None;
                    self.scroll = 0;
                }
                KeyCode::Char('q') => return Went::Out,
                KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll += 15,
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
                KeyCode::Char(c @ '1'..='9') => {
                    let at = c as usize - '1' as usize;
                    if let Some(session) = self.numbered.get(at).cloned() {
                        return Went::Into { whose: self.whose(&session), session };
                    }
                }
                _ => {}
            }
            return Went::Nowhere;
        }

        let rows = self.shown();
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return Went::Out,
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(rows.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = rows.len().saturating_sub(1),
            KeyCode::Char('/') => {
                self.searching = true;
                self.query.clear();
            }
            KeyCode::Char('r') => {
                self.returning_only = !self.returning_only;
                self.cursor = 0;
            }
            KeyCode::Char('s') => {
                self.by = match self.by {
                    By::Last => By::Visits,
                    By::Visits => By::Questions,
                    By::Questions => By::Last,
                };
                self.cursor = 0;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if !rows.is_empty() {
                    self.open = Some(rows[self.cursor.min(rows.len() - 1)]);
                    self.scroll = 0;
                }
            }
            _ => {}
        }
        Went::Nowhere
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.height < 4 {
            return;
        }
        let body = Rect { y: area.y + 2, height: area.height.saturating_sub(3), ..area };

        let (title, keys) = match self.open {
            None => (
                format!(
                    "{} visitor{}   {} visit{}   {} question{}",
                    self.people.len(),
                    if self.people.len() == 1 { "" } else { "s" },
                    self.people.iter().map(|p| p.stays.len()).sum::<usize>(),
                    if self.people.iter().map(|p| p.stays.len()).sum::<usize>() == 1 { "" } else { "s" },
                    self.people.iter().map(Visitor::asked).sum::<usize>(),
                    if self.people.iter().map(Visitor::asked).sum::<usize>() == 1 { "" } else { "s" },
                ),
                "enter open   / search   s sort   r returning   q quit",
            ),
            Some(_) => (
                "one visitor".to_string(),
                "1-9 replay a conversation   esc back   q quit",
            ),
        };

        let mut head = vec![Span::styled(
            "  visitors  ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )];
        head.push(Span::styled(title, Style::default().fg(DIM)));
        if self.searching || !self.query.is_empty() {
            head.push(Span::styled(
                format!("   /{}", self.query),
                Style::default().fg(CYAN),
            ));
        }
        if self.open.is_none() && self.by != By::Last {
            head.push(Span::styled(
                format!("   by {}", self.by.label()),
                Style::default().fg(FAINT),
            ));
        }
        if self.returning_only {
            head.push(Span::styled("   returning only", Style::default().fg(FAINT)));
        }
        f.render_widget(
            Paragraph::new(Line::from(head)),
            Rect { height: 1, ..area },
        );

        match self.open {
            None => self.list(f, body),
            Some(at) => self.one(f, body, at),
        }

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {keys}"),
                Style::default().fg(FAINT),
            ))),
            Rect { y: area.y + area.height - 1, height: 1, ..area },
        );
    }

    fn list(&mut self, f: &mut Frame, area: Rect) {
        let rows = self.shown();
        if rows.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("  nobody matches that.", Style::default().fg(DIM))),
                area,
            );
            return;
        }
        self.cursor = self.cursor.min(rows.len() - 1);
        let room = area.height as usize;
        if self.cursor < self.top {
            self.top = self.cursor;
        }
        if self.cursor >= self.top + room {
            self.top = self.cursor + 1 - room;
        }

        for (row, &at) in rows.iter().skip(self.top).take(room).enumerate() {
            let p = &self.people[at];
            let here = self.top + row == self.cursor;
            let ink = if here { FG } else { DIM };
            let line = Line::from(vec![
                Span::styled(
                    if here { "  \u{203a} " } else { "    " },
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    format!("{:<18}", cut(&p.name, 18)),
                    match here {
                        true => Style::default().fg(FG).add_modifier(Modifier::BOLD),
                        false => Style::default().fg(ink),
                    },
                ),
                Span::styled(
                    format!(
                        "{:<11}{:<15}{:<9}",
                        format!("{} visit{}", p.stays.len(), if p.stays.len() == 1 { "" } else { "s" }),
                        format!("{} question{}", p.asked(), if p.asked() == 1 { "" } else { "s" }),
                        spell(Some(p.secs())),
                    ),
                    Style::default().fg(ink),
                ),
                Span::styled(
                    format!("{:<26}", cut(&p.wheres().join(" · "), 26)),
                    Style::default().fg(if here { CYAN } else { FAINT }),
                ),
                Span::styled(stamp(p.last()), Style::default().fg(FAINT)),
            ]);
            f.render_widget(
                Paragraph::new(line),
                Rect { y: area.y + row as u16, height: 1, ..area },
            );
        }
    }

    fn one(&mut self, f: &mut Frame, area: Rect, at: usize) {
        let p = &self.people[at];
        let mut lines: Vec<Line> = Vec::new();
        let dim = Style::default().fg(DIM);
        let faint = Style::default().fg(FAINT);

        lines.push(Line::from(Span::styled(
            format!("  {}", p.name),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        )));
        let via = p.via();
        lines.push(Line::from(Span::styled(
            format!(
                "    {}  ·  {}",
                if p.id.is_empty() { "no identity offered" } else { &p.id },
                if via.is_empty() { "?" } else { &via },
            ),
            faint,
        )));
        if !p.client().is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", cut(&p.client(), area.width.saturating_sub(6) as usize)),
                faint,
            )));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "    {} visit{}  ·  {} question{}  ·  {} in all  ·  first {}",
                p.stays.len(),
                if p.stays.len() == 1 { "" } else { "s" },
                p.asked(),
                if p.asked() == 1 { "" } else { "s" },
                spell(Some(p.secs())),
                stamp(p.first()),
            ),
            dim,
        )));
        lines.push(Line::from(""));

        self.numbered = p.stays.iter().rev().take(9).map(|s| s.session.clone()).collect();
        for (n, stay) in p.stays.iter().rev().enumerate() {
            let tag = match n < 9 {
                true => format!("[{}] ", n + 1),
                false => "    ".to_string(),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {tag}"), Style::default().fg(ACCENT)),
                Span::styled(
                    format!(
                        "{}   {:<8}{} question{}",
                        stamp(stay.at),
                        spell(stay.secs),
                        stay.asked(),
                        if stay.asked() == 1 { "" } else { "s" },
                    ),
                    Style::default().fg(FG),
                ),
                Span::styled(
                    match stay.place.is_empty() {
                        true => format!("   {}", stay.ip),
                        false => format!("   {}  ·  {}", stay.place, stay.ip),
                    },
                    faint,
                ),
            ]));
            for said in &stay.said {
                let mut marks: Vec<String> = Vec::new();
                if !said.status.is_empty() {
                    marks.push(said.status.clone());
                }
                if said.times > 1 {
                    marks.push(format!("x{}", said.times));
                }
                lines.push(Line::from(vec![
                    Span::styled("        ", faint),
                    Span::styled(
                        cut(&said.q, area.width.saturating_sub(24) as usize),
                        Style::default().fg(FG),
                    ),
                    Span::styled(
                        match marks.is_empty() {
                            true => String::new(),
                            false => format!("   [{}]", marks.join(", ")),
                        },
                        faint,
                    ),
                ]));
            }
            if stay.said.is_empty() {
                lines.push(Line::from(Span::styled("        said nothing", faint)));
            }
            lines.push(Line::from(""));
        }

        let room = area.height as usize;
        self.scroll = self.scroll.min(lines.len().saturating_sub(room.min(lines.len())));
        for (row, line) in lines.into_iter().skip(self.scroll).take(room).enumerate() {
            f.render_widget(
                Paragraph::new(line),
                Rect { y: area.y + row as u16, height: 1, ..area },
            );
        }
    }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one piece of arithmetic here with no second opinion.
    ///
    /// Checked against dates that are wrong in a different way each: the
    /// epoch, both kinds of leap year, and 2100 -- divisible by four, not a
    /// leap year, and the case a hand-rolled calendar gets wrong and nobody
    /// notices for decades.
    #[test]
    fn the_calendar_is_the_calendar() {
        for (secs, want) in [
            (0u64, "1970-01-01 00:00"),
            (951_782_400, "2000-02-29 00:00"),
            (1_709_164_800, "2024-02-29 00:00"),
            (1_787_600_000, "2026-08-24 19:33"),
            (4_102_444_800, "2100-01-01 00:00"),
        ] {
            assert_eq!(stamp(secs), want, "{secs}");
        }
    }

    #[test]
    fn how_long_they_stayed_reads_as_a_duration() {
        assert_eq!(spell(Some(9)), "9s");
        assert_eq!(spell(Some(494)), "8m14s");
        assert_eq!(spell(Some(3720)), "1h02m");
        assert_eq!(spell(None), "open");
    }

    /// A question is listed once, and a run of the same one is folded.
    #[test]
    fn every_question_once_and_repeats_counted() {
        let mut stay = Stay {
            said: vec![
                Said { q: "answered".into(), a: "yes".into(), status: String::new(), times: 1 },
                Said { q: "hey".into(), a: String::new(), status: "failed".into(), times: 1 },
                Said { q: "hey".into(), a: String::new(), status: "failed".into(), times: 1 },
                Said { q: "hey".into(), a: String::new(), status: "failed".into(), times: 1 },
            ],
            sent: vec!["answered".into(), "hey".into(), "never came back".into()],
            ..Stay::default()
        };
        settle(&mut stay);

        let qs: Vec<(&str, &str, usize)> = stay
            .said
            .iter()
            .map(|s| (s.q.as_str(), s.status.as_str(), s.times))
            .collect();
        assert_eq!(
            qs,
            vec![
                ("answered", "", 1),
                ("hey", "failed", 3),
                ("never came back", "unanswered", 1),
            ]
        );
        // Folded to three rows, and still ten things that happened... five here.
        assert_eq!(stay.asked(), 5);
    }
}
