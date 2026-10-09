use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub from: String,
    pub emblem: String,
    pub wall: String,
    pub quote: String,
    pub body: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sheet {
    pub open: String,
    pub close: String,
    pub figures: Vec<Entry>,
    pub works: Vec<Entry>,
    pub threads: Vec<Entry>,
}

pub fn parse(source: &str) -> Sheet {
    let mut sheet = Sheet::default();
    let mut group = 0u8;
    let mut last = String::new();
    for line in source.lines() {
        let bare = line.trim();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let (key, value) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        if indent == 0 {
            last.clear();
            match key {
                "open" => {
                    group = 1;
                    sheet.open = value.trim().into();
                }
                "close" => {
                    group = 2;
                    sheet.close = value.trim().into();
                }
                "figure" | "work" | "thread" => {
                    group = match key {
                        "figure" => 3,
                        "work" => 4,
                        _ => 5,
                    };
                    let entry = Entry {
                        id: value.trim().into(),
                        ..Default::default()
                    };
                    match group {
                        3 => sheet.figures.push(entry),
                        4 => sheet.works.push(entry),
                        _ => sheet.threads.push(entry),
                    };
                }
                _ => group = 0,
            }
            continue;
        }
        if group == 1 || group == 2 {
            let text = if group == 1 {
                &mut sheet.open
            } else {
                &mut sheet.close
            };
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(bare);
            continue;
        }
        let entry = match group {
            3 => sheet.figures.last_mut(),
            4 => sheet.works.last_mut(),
            5 => sheet.threads.last_mut(),
            _ => None,
        };
        let Some(entry) = entry else { continue };
        let continuation = indent >= 4 && !last.is_empty();
        if !continuation {
            last = key.into();
        }
        let field = match last.as_str() {
            "name" => &mut entry.name,
            "from" => &mut entry.from,
            "emblem" => &mut entry.emblem,
            "wall" => &mut entry.wall,
            "quote" => &mut entry.quote,
            "body" => &mut entry.body,
            _ => continue,
        };
        if continuation && !field.is_empty() {
            field.push(' ');
        }
        field.push_str(if continuation { bare } else { value.trim() });
    }
    sheet
}
