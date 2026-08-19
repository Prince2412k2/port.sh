//! The taste sheet, and the page it becomes.
//!
//! A long-form essay rather than a screen: the other three sections are fixed
//! frames that hold still and animate in place, and doing a fourth of those
//! would have made this a card wall. Writing wants to be scrolled, so this one
//! scrolls, with the emblems alternating sides down the page the way plates
//! alternate in a printed piece.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Figure,
    Work,
    Thread,
}

#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Read by the tests, which are the only thing that checks an entry
    /// against the drawing it names.
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    pub from: String,
    pub emblem: String,
    pub quote: String,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub open: String,
    pub close: String,
    pub figures: Vec<Entry>,
    pub works: Vec<Entry>,
    pub threads: Vec<Entry>,
}

pub fn load() -> Sheet {
    let disk = std::fs::read_to_string("portfolio/data/taste.txt")
        .or_else(|_| std::fs::read_to_string("data/taste.txt"))
        .ok();
    parse(disk.as_deref().unwrap_or(include_str!("../data/taste.txt")))
}

pub fn parse(src: &str) -> Sheet {
    let mut s = Sheet::default();
    // What the last indent-0 line opened, so a continuation knows where to go.
    let mut cur: Option<(Kind, usize)> = None;
    let mut top: Option<&'static str> = None;
    let mut key = String::new();

    for line in src.lines() {
        let line = line.trim_end();
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();

        if indent == 0 {
            let (word, rest) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
            let rest = rest.trim();
            key.clear();
            match word {
                "open" => {
                    top = Some("open");
                    cur = None;
                    s.open.push_str(rest);
                }
                "close" => {
                    top = Some("close");
                    cur = None;
                    s.close.push_str(rest);
                }
                "figure" | "work" | "thread" => {
                    top = None;
                    let e = Entry { id: rest.to_string(), ..Default::default() };
                    let k = match word {
                        "figure" => Kind::Figure,
                        "work" => Kind::Work,
                        _ => Kind::Thread,
                    };
                    let v = match k {
                        Kind::Figure => &mut s.figures,
                        Kind::Work => &mut s.works,
                        Kind::Thread => &mut s.threads,
                    };
                    v.push(e);
                    cur = Some((k, v.len() - 1));
                }
                _ => {}
            }
            continue;
        }

        // A continuation of whatever was last named. Joined with a space, so a
        // paragraph wrapped in the file reflows to the terminal's measure
        // rather than keeping the file's line breaks.
        if let Some(t) = top {
            let f = if t == "open" { &mut s.open } else { &mut s.close };
            if !f.is_empty() {
                f.push(' ');
            }
            f.push_str(bare);
            continue;
        }

        let Some((k, i)) = cur else { continue };
        let e = match k {
            Kind::Figure => &mut s.figures[i],
            Kind::Work => &mut s.works[i],
            Kind::Thread => &mut s.threads[i],
        };

        if indent >= 4 && !key.is_empty() {
            if let Some(f) = field_mut(e, &key) {
                if !f.is_empty() {
                    f.push(' ');
                }
                f.push_str(bare);
            }
            continue;
        }
        let (k2, val) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        key = k2.to_string();
        if let Some(f) = field_mut(e, k2) {
            f.push_str(val.trim());
        }
    }
    s
}

fn field_mut<'a>(e: &'a mut Entry, key: &str) -> Option<&'a mut String> {
    Some(match key {
        "name" => &mut e.name,
        "from" => &mut e.from,
        "emblem" => &mut e.emblem,
        "quote" => &mut e.quote,
        "body" => &mut e.body,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emblems;

    #[test]
    fn the_shipped_sheet_parses_into_a_whole_essay() {
        let s = parse(include_str!("../data/taste.txt"));
        assert!(!s.open.is_empty() && !s.close.is_empty());
        assert!(s.figures.len() >= 4, "figures: {}", s.figures.len());
        assert!(s.works.len() >= 2, "works: {}", s.works.len());
        for e in s.figures.iter().chain(&s.works) {
            assert!(!e.name.is_empty(), "{} has no name", e.id);
            assert!(!e.from.is_empty(), "{} has no source", e.id);
            assert!(!e.quote.is_empty(), "{} has no quote", e.id);
            // Two lines at the gallery's measure. Longer than this and the
            // shelf turns back into an essay.
            assert!(e.quote.len() < 170, "{} has grown into a paragraph", e.id);
        }
    }

    /// Every entry names a drawing, and the script that draws them is a
    /// separate program — so nothing but a test connects the two.
    #[test]
    fn every_entry_points_at_a_drawing_that_exists() {
        let s = parse(include_str!("../data/taste.txt"));
        for e in s.figures.iter().chain(&s.works) {
            assert!(
                emblems::find(&e.emblem).is_some(),
                "{} wants emblem `{}`, which emblems.py did not draw",
                e.id,
                e.emblem
            );
        }
    }

    #[test]
    fn a_wrapped_value_comes_back_as_one_line() {
        let sheet = parse("figure x\n  name X\n  quote one\n    two\n");
        let p = &sheet.figures[0];
        assert_eq!(p.quote, "one two");
    }
}
