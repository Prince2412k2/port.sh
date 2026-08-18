//! The `.sky` sheet: constellations, stars, and the parser for both.
//!
//! A flat indented text format rather than TOML or JSON, for the same reason
//! termap has `.tmap`: this file is meant to be *edited by hand*, and it is the
//! only real content in the program. Adding a serialisation crate to read forty
//! lines of key/value pairs would cost a dependency and buy nothing.
//!
//! Two spaces starts a key. Four or more continues the previous value. That is
//! the entire grammar.

/// One project. Draws as a constellation.
#[derive(Debug)]
pub struct Constellation {
    pub id: String,
    pub name: String,
    pub year: String,
    pub repo: String,
    /// Anchor in sky units. Only a starting position — the layout pulls stars
    /// around it — but moving it here moves that constellation's patch of sky.
    pub at: [f64; 2],
    /// The one-line version, set large when the project is opened.
    pub blurb: String,
    /// The paragraph under it.
    pub about: String,
    /// Languages, size, and whatever else is countable.
    pub stats: String,
}

/// One skill. Draws as a star.
#[derive(Debug)]
pub struct Star {
    pub id: String,
    pub name: String,
    /// Constellation indices. The first is where the story happened.
    pub members: Vec<usize>,
    /// Parallel to `members`: does that project actually lean on this skill.
    pub load: Vec<bool>,
    pub story: String,
}

impl Star {
    /// How bright the star is drawn, 0..1.
    ///
    /// This is the one number in the app that could have been a self-assessed
    /// proficiency score, and deliberately is not. It counts how many projects
    /// claim the skill and how many of them lean on it — both facts that are
    /// checkable against the sheet and against the repositories. Nothing here
    /// encodes an opinion about how good anyone is at anything.
    pub fn magnitude(&self) -> f32 {
        let claims = self.members.len() as f32;
        let bearing = self.load.iter().filter(|&&b| b).count() as f32;
        // The floor matters: a skill used once, incidentally, is still a star
        // and not a gap in the sky. Reuse is weighted above load-bearing
        // because it is the harder thing to fake — a second project either
        // used it or did not.
        (0.16 + 0.19 * (claims - 1.0) + 0.15 * bearing).clamp(0.0, 1.0)
    }

    /// The project the story is told about.
    pub fn home(&self) -> usize {
        self.members[0]
    }
}

#[derive(Debug)]
pub struct Sky {
    pub cons: Vec<Constellation>,
    pub stars: Vec<Star>,
}

impl Sky {
    /// Stars belonging to a constellation, in sheet order.
    pub fn members_of(&self, con: usize) -> Vec<usize> {
        (0..self.stars.len())
            .filter(|&s| self.stars[s].members.contains(&con))
            .collect()
    }

    pub fn con_by_id(&self, id: &str) -> Option<usize> {
        self.cons.iter().position(|c| c.id == id)
    }

    pub fn star_by_id(&self, id: &str) -> Option<usize> {
        self.stars.iter().position(|s| s.id == id)
    }
}

/// A record under construction, so `constellation` and `star` can share the
/// key-accumulating loop.
#[derive(Default)]
struct Fields {
    id: String,
    name: String,
    year: String,
    repo: String,
    at: String,
    blurb: String,
    about: String,
    stats: String,
    r#in: String,
    story: String,
}

impl Fields {
    fn slot(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "name" => &mut self.name,
            "year" => &mut self.year,
            "repo" => &mut self.repo,
            "at" => &mut self.at,
            "blurb" => &mut self.blurb,
            "about" => &mut self.about,
            "stats" => &mut self.stats,
            "in" => &mut self.r#in,
            "story" => &mut self.story,
            _ => return None,
        })
    }
}

enum Kind {
    Con,
    Star,
}

pub fn parse(src: &str) -> Result<Sky, String> {
    let mut cons: Vec<Constellation> = Vec::new();
    // Stars are collected with their `in` lists unresolved, because a star may
    // name a constellation the sheet has not reached yet.
    let mut raw: Vec<(Fields, usize)> = Vec::new();
    let mut open: Option<(Kind, Fields, usize)> = None;
    let mut last_key = String::new();

    let flush = |open: &mut Option<(Kind, Fields, usize)>,
                 cons: &mut Vec<Constellation>,
                 raw: &mut Vec<(Fields, usize)>|
     -> Result<(), String> {
        match open.take() {
            Some((Kind::Con, f, line)) => {
                let at = parse_at(&f.at)
                    .ok_or_else(|| format!("line {line}: `at` wants two numbers, got {:?}", f.at))?;
                cons.push(Constellation {
                    name: if f.name.is_empty() { f.id.clone() } else { f.name },
                    id: f.id,
                    year: f.year,
                    repo: f.repo,
                    at,
                    blurb: f.blurb,
                    about: f.about,
                    stats: f.stats,
                });
            }
            Some((Kind::Star, f, line)) => raw.push((f, line)),
            None => {}
        }
        Ok(())
    };

    for (n, line) in src.lines().enumerate() {
        let line = line.trim_end();
        let no = n + 1;
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();

        if indent == 0 {
            flush(&mut open, &mut cons, &mut raw)?;
            last_key.clear();
            let (word, id) = bare
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("line {no}: `{bare}` needs an id"))?;
            let f = Fields { id: id.trim().to_string(), ..Default::default() };
            open = Some(match word {
                "constellation" => (Kind::Con, f, no),
                "star" => (Kind::Star, f, no),
                _ => return Err(format!("line {no}: unknown record `{word}`")),
            });
            continue;
        }

        let Some((_, f, _)) = open.as_mut() else {
            return Err(format!("line {no}: indented text before any record"));
        };

        if indent >= 4 && !last_key.is_empty() {
            // Continuation. Joined with a space: every consumer wraps to its
            // own width, so the line breaks in the file are the author's
            // convenience and nothing else.
            let slot = f.slot(&last_key).expect("last_key came from slot()");
            if !slot.is_empty() {
                slot.push(' ');
            }
            slot.push_str(bare);
            continue;
        }

        let (key, rest) = match bare.split_once(char::is_whitespace) {
            Some((k, r)) => (k, r.trim()),
            None => (bare, ""),
        };
        if f.slot(key).is_none() {
            return Err(format!("line {no}: unknown key `{key}`"));
        }
        last_key = key.to_string();
        let slot = f.slot(key).expect("checked just above");
        slot.clear();
        slot.push_str(rest);
    }
    flush(&mut open, &mut cons, &mut raw)?;

    let mut stars = Vec::with_capacity(raw.len());
    for (f, line) in raw {
        let mut members = Vec::new();
        let mut load = Vec::new();
        for part in f.r#in.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            // A trailing `*` marks a project that leans on the skill, as
            // against one that merely used it.
            let bearing = part.ends_with('*');
            let id = part.trim_end_matches('*').trim();
            let idx = cons
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| format!("line {line}: `{}` is in no constellation named `{id}`", f.id))?;
            if !members.contains(&idx) {
                members.push(idx);
                load.push(bearing);
            }
        }
        if members.is_empty() {
            return Err(format!("line {line}: star `{}` belongs to nothing", f.id));
        }
        stars.push(Star {
            name: if f.name.is_empty() { f.id.clone() } else { f.name },
            id: f.id,
            members,
            load,
            story: f.story,
        });
    }

    if cons.is_empty() {
        return Err("the sheet has no constellations".into());
    }
    Ok(Sky { cons, stars })
}

fn parse_at(s: &str) -> Option<[f64; 2]> {
    let mut it = s.split_whitespace();
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    Some([x, y])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = "\
constellation a
  name   Ay
  at     -10 5
  blurb  first

constellation b
  at     10 5

star one
  name   The One
  in     a*, b
  year   2025
  story
    line one
    line two

star two
  in     b*
";

    #[test]
    fn parses_records_and_continuations() {
        let sky = parse(SHEET).unwrap();
        assert_eq!(sky.cons.len(), 2);
        assert_eq!(sky.cons[0].name, "Ay");
        assert_eq!(sky.cons[0].at, [-10.0, 5.0]);
        // A missing `name` falls back to the id, so a sheet can stay terse.
        assert_eq!(sky.cons[1].name, "b");

        assert_eq!(sky.stars.len(), 2);
        let one = &sky.stars[0];
        assert_eq!(one.name, "The One");
        assert_eq!(one.members, vec![0, 1]);
        assert_eq!(one.load, vec![true, false]);
        // Continuation lines join with a space, not a newline.
        assert_eq!(one.story, "line one line two");
        assert_eq!(one.home(), 0);
    }

    #[test]
    fn magnitude_rewards_reuse_not_opinion() {
        let sky = parse(SHEET).unwrap();
        // Two claims, one of them load-bearing, against one incidental claim.
        assert!(sky.stars[0].magnitude() > sky.stars[1].magnitude());
    }

    #[test]
    fn members_of_is_sheet_ordered() {
        let sky = parse(SHEET).unwrap();
        assert_eq!(sky.members_of(1), vec![0, 1]);
        assert_eq!(sky.members_of(0), vec![0]);
    }

    #[test]
    fn unknown_constellation_is_an_error() {
        let err = parse("constellation a\n  at 0 0\nstar s\n  in nope\n").unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn unknown_key_is_an_error() {
        let err = parse("constellation a\n  at 0 0\n  colour red\n").unwrap_err();
        assert!(err.contains("colour"), "{err}");
    }

    #[test]
    fn the_shipped_sheet_parses() {
        let sky = parse(include_str!("../data/skills.sky")).unwrap();
        assert_eq!(sky.cons.len(), 9);
        assert!(sky.stars.len() > 40);
        // Every constellation has to have something in it, or the layout will
        // place an empty anchor and label a patch of empty sky.
        for c in 0..sky.cons.len() {
            assert!(!sky.members_of(c).is_empty(), "{} is empty", sky.cons[c].id);
        }
        // Every star needs a story and every project needs its copy: between
        // them they are the entire content of the program.
        for s in &sky.stars {
            assert!(!s.story.is_empty(), "{} has no story", s.id);
        }
        for c in &sky.cons {
            assert!(!c.blurb.is_empty(), "{} has no blurb", c.id);
            assert!(!c.about.is_empty(), "{} has no about", c.id);
            assert!(!c.stats.is_empty(), "{} has no stats", c.id);
        }
    }
}
