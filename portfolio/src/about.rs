//! The landing sheet: who this is.
//!
//! Flat key/value in the same grammar as `places.txt` and `projects.txt` — two
//! spaces starts a key, four or more continues the previous value. Three files
//! in three crates share one format because the alternative is three formats,
//! and a portfolio whose own data files disagree with each other is not making
//! a good argument for the person who wrote it.

#[derive(Debug, Clone, Default)]
pub struct About {
    pub name: String,
    pub role: String,
    pub where_: String,
    pub handle: String,
    pub pitch: String,
    pub now: String,
    pub email: String,
    pub github: String,
    pub ssh: String,
    pub mosh: String,
}

pub fn load() -> About {
    let disk = std::fs::read_to_string("portfolio/data/about.txt")
        .or_else(|_| std::fs::read_to_string("data/about.txt"))
        .ok();
    parse(disk.as_deref().unwrap_or(include_str!("../data/about.txt")))
}

pub fn parse(src: &str) -> About {
    let mut a = About::default();
    let mut last = String::new();

    for line in src.lines() {
        let line = line.trim_end();
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();

        if indent >= 4 && !last.is_empty() {
            if let Some(f) = field_mut(&mut a, &last) {
                if !f.is_empty() {
                    f.push(' ');
                }
                f.push_str(bare);
            }
            continue;
        }
        let (key, val) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        last = key.to_string();
        if let Some(f) = field_mut(&mut a, key) {
            f.push_str(val.trim());
        }
    }
    a
}

fn field_mut<'a>(a: &'a mut About, key: &str) -> Option<&'a mut String> {
    Some(match key {
        "name" => &mut a.name,
        "role" => &mut a.role,
        "where" => &mut a.where_,
        "handle" => &mut a.handle,
        "pitch" => &mut a.pitch,
        "now" => &mut a.now,
        "email" => &mut a.email,
        "github" => &mut a.github,
        "ssh" => &mut a.ssh,
        "mosh" => &mut a.mosh,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_sheet_has_the_things_the_landing_page_needs() {
        let a = parse(include_str!("../data/about.txt"));
        assert!(!a.name.is_empty());
        assert!(!a.pitch.is_empty());
        assert!(!a.github.is_empty());
        // The paragraph is wrapped across several lines in the file and must
        // come back as one reflowable string.
        assert!(a.pitch.len() > 120, "pitch did not join: {:?}", a.pitch);
        assert!(!a.pitch.contains('\n'));
    }
}
