//! The project sheet: what each card says, and the parser for it.
//!
//! A flat indented text format for the same reason the rest of this project
//! hand-rolls its formats — the file is the content, it is meant to be edited
//! by hand, and a serialisation crate would cost a dependency to read forty
//! key/value pairs. Two spaces starts a key, four or more continues the
//! previous value.

/// One section of the engineering explanation: a claim and its argument.
#[derive(Debug, Clone)]
pub struct Beat {
    pub head: String,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Which extruded mark to put on the card.
    pub mark: String,
    pub year: String,
    pub repo: String,
    pub tag: String,
    pub stats: String,
    /// Tool logo ids, in the order they should scroll.
    pub tools: Vec<String>,
    /// The explanation was written from a summary rather than from the source,
    /// and says so on the card. Better a visible caveat than a quiet one.
    pub draft: bool,
    pub beats: Vec<Beat>,
}

pub fn parse(src: &str) -> Result<Vec<Project>, String> {
    let mut out: Vec<Project> = Vec::new();
    let mut last_key = String::new();

    for (n, line) in src.lines().enumerate() {
        let line = line.trim_end();
        let no = n + 1;
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();

        if indent == 0 {
            let (word, id) = bare
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("line {no}: `{bare}` needs an id"))?;
            if word != "project" {
                return Err(format!("line {no}: unknown record `{word}`"));
            }
            out.push(Project { id: id.trim().to_string(), ..Default::default() });
            last_key.clear();
            continue;
        }

        let p = out
            .last_mut()
            .ok_or_else(|| format!("line {no}: indented text before any project"))?;

        // Continuation of whatever key was last opened.
        if indent >= 4 && !last_key.is_empty() {
            let slot: &mut String = match last_key.as_str() {
                "tag" => &mut p.tag,
                "stats" => &mut p.stats,
                "beat" => {
                    &mut p
                        .beats
                        .last_mut()
                        .ok_or_else(|| format!("line {no}: continuation before any beat"))?
                        .body
                }
                _ => return Err(format!("line {no}: `{last_key}` takes one line")),
            };
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
        last_key = key.to_string();
        match key {
            "name" => p.name = rest.into(),
            "mark" => p.mark = rest.into(),
            "year" => p.year = rest.into(),
            "repo" => p.repo = rest.into(),
            "tag" => p.tag = rest.into(),
            "stats" => p.stats = rest.into(),
            "draft" => p.draft = matches!(rest, "yes" | "true" | "1"),
            "tools" => {
                p.tools = rest
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            "beat" => p.beats.push(Beat { head: rest.into(), body: String::new() }),
            other => return Err(format!("line {no}: unknown key `{other}`")),
        }
    }

    if out.is_empty() {
        return Err("no projects in the sheet".into());
    }
    for p in &out {
        if p.mark.is_empty() {
            return Err(format!("{} has no mark", p.id));
        }
        if p.beats.is_empty() {
            return Err(format!("{} has no engineering to show", p.id));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = "\
project alpha
  name   Alpha
  mark   alpha
  tag    a thing
  tools  go, rust ,, python
  beat   First
    one two
    three
  beat   Second
    four

project beta
  name   Beta
  mark   beta
  draft  yes
  beat   Only
    body
";

    #[test]
    fn parses_records_beats_and_continuations() {
        let ps = parse(SHEET).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].name, "Alpha");
        assert_eq!(ps[0].tools, vec!["go", "rust", "python"]);
        assert_eq!(ps[0].beats.len(), 2);
        assert_eq!(ps[0].beats[0].head, "First");
        // Continuations join with a space: the line breaks in the file are the
        // author's convenience and the column is decided at render time.
        assert_eq!(ps[0].beats[0].body, "one two three");
        assert_eq!(ps[0].beats[1].body, "four");
        assert!(!ps[0].draft);
        assert!(ps[1].draft);
    }

    #[test]
    fn a_project_with_no_engineering_is_an_error() {
        let err = parse("project a\n  mark m\n").unwrap_err();
        assert!(err.contains("engineering"), "{err}");
    }

    #[test]
    fn unknown_keys_name_their_line() {
        let err = parse("project a\n  colour red\n").unwrap_err();
        assert!(err.contains("line 2") && err.contains("colour"), "{err}");
    }

    #[test]
    fn the_shipped_sheet_parses_and_points_at_real_art() {
        let ps = parse(include_str!("../data/projects.txt")).unwrap();
        // A floor rather than an exact count: adding a project should not mean
        // editing a number in a test that is about whether art resolves.
        assert!(ps.len() >= 10, "only {} projects", ps.len());
        for p in &ps {
            assert!(
                crate::marks::find(&p.mark).is_some(),
                "{} wants mark {:?}, which does not exist",
                p.id,
                p.mark
            );
            for t in &p.tools {
                assert!(
                    crate::logos::find(t).is_some(),
                    "{} wants tool {:?}, which has no logo",
                    p.id,
                    t
                );
            }
            assert!(!p.tag.is_empty(), "{} has no tagline", p.id);
        }
    }
}
