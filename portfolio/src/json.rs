//! Just enough JSON for one protocol.
//!
//! The rest of this project hand-rolls its formats because they are small and a
//! dependency would cost more than it saves. This one is different: JSON is not
//! small, and I would rather have had `serde_json`. It is not in the offline
//! registry, so here is a parser for the subset the Agent Client Protocol
//! actually sends — objects, arrays, strings with escapes, numbers, the three
//! literals — with the awkward parts (surrogate pairs, exponents) handled
//! rather than assumed away, because those are where a hand-rolled parser
//! quietly goes wrong.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    /// Field of an object, or `None` for anything else. Chains, so a nested
    /// lookup reads as a path.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Only the tests read booleans out of a document — the protocol carries
    /// none. It is here so the opencode config can be checked as parsed JSON
    /// rather than as a string, which is the only way that test means anything.
    #[allow(dead_code)]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Used by the tests; the protocol client itself never needs an array.
    #[allow(dead_code)]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(v) => Some(v),
            _ => None,
        }
    }
}

pub fn parse(src: &str) -> Option<Value> {
    let b = src.as_bytes();
    let mut i = 0;
    let v = value(b, &mut i)?;
    skip_ws(b, &mut i);
    // Trailing garbage means this was not one document, and silently accepting
    // it would hide a framing bug rather than surface it.
    if i == b.len() {
        Some(v)
    } else {
        None
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize) -> Option<Value> {
    skip_ws(b, i);
    match *b.get(*i)? {
        b'{' => object(b, i),
        b'[' => array(b, i),
        b'"' => string(b, i).map(Value::Str),
        b't' => lit(b, i, "true", Value::Bool(true)),
        b'f' => lit(b, i, "false", Value::Bool(false)),
        b'n' => lit(b, i, "null", Value::Null),
        _ => number(b, i),
    }
}

fn lit(b: &[u8], i: &mut usize, word: &str, v: Value) -> Option<Value> {
    if b[*i..].starts_with(word.as_bytes()) {
        *i += word.len();
        Some(v)
    } else {
        None
    }
}

fn object(b: &[u8], i: &mut usize) -> Option<Value> {
    *i += 1; // {
    let mut m = BTreeMap::new();
    skip_ws(b, i);
    if *b.get(*i)? == b'}' {
        *i += 1;
        return Some(Value::Obj(m));
    }
    loop {
        skip_ws(b, i);
        let k = string(b, i)?;
        skip_ws(b, i);
        if *b.get(*i)? != b':' {
            return None;
        }
        *i += 1;
        m.insert(k, value(b, i)?);
        skip_ws(b, i);
        match *b.get(*i)? {
            b',' => *i += 1,
            b'}' => {
                *i += 1;
                return Some(Value::Obj(m));
            }
            _ => return None,
        }
    }
}

fn array(b: &[u8], i: &mut usize) -> Option<Value> {
    *i += 1; // [
    let mut v = Vec::new();
    skip_ws(b, i);
    if *b.get(*i)? == b']' {
        *i += 1;
        return Some(Value::Arr(v));
    }
    loop {
        v.push(value(b, i)?);
        skip_ws(b, i);
        match *b.get(*i)? {
            b',' => *i += 1,
            b']' => {
                *i += 1;
                return Some(Value::Arr(v));
            }
            _ => return None,
        }
    }
}

fn string(b: &[u8], i: &mut usize) -> Option<String> {
    if *b.get(*i)? != b'"' {
        return None;
    }
    *i += 1;
    let mut s = String::new();
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'"' => return Some(s),
            b'\\' => {
                let e = *b.get(*i)?;
                *i += 1;
                match e {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'b' => s.push('\u{8}'),
                    b'f' => s.push('\u{c}'),
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'u' => {
                        let hi = hex4(b, i)?;
                        // Astral characters arrive as a surrogate pair. Decoding
                        // each half on its own yields an unpaired surrogate,
                        // which is not a `char` — every emoji would be dropped.
                        let ch = if (0xD800..0xDC00).contains(&hi) {
                            if b.get(*i) == Some(&b'\\') && b.get(*i + 1) == Some(&b'u') {
                                *i += 2;
                                let lo = hex4(b, i)?;
                                let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                char::from_u32(c)?
                            } else {
                                '\u{fffd}'
                            }
                        } else {
                            char::from_u32(hi).unwrap_or('\u{fffd}')
                        };
                        s.push(ch);
                    }
                    _ => return None,
                }
            }
            // Multi-byte UTF-8 passes through a byte at a time; the string is
            // built from the original bytes so the sequence stays intact.
            _ => {
                let start = *i - 1;
                let len = utf8_len(c);
                let end = start + len;
                s.push_str(std::str::from_utf8(b.get(start..end)?).ok()?);
                *i = end;
            }
        }
    }
}

fn utf8_len(c: u8) -> usize {
    match c {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn hex4(b: &[u8], i: &mut usize) -> Option<u32> {
    let s = std::str::from_utf8(b.get(*i..*i + 4)?).ok()?;
    *i += 4;
    u32::from_str_radix(s, 16).ok()
}

fn number(b: &[u8], i: &mut usize) -> Option<Value> {
    let start = *i;
    if *b.get(*i)? == b'-' {
        *i += 1;
    }
    while *i < b.len() && matches!(b[*i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
        *i += 1;
    }
    std::str::from_utf8(&b[start..*i]).ok()?.parse().ok().map(Value::Num)
}

/// Escape a string for embedding in JSON, quotes included.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below space must be escaped; the rest can go out as
            // UTF-8, which every JSON reader accepts.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_the_shape_the_protocol_actually_sends() {
        let src = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1",
            "update":{"sessionUpdate":"agent_message_chunk",
                      "content":{"type":"text","text":"hello"}}}}"#;
        let v = parse(src).expect("parses");
        assert_eq!(v.get("method").unwrap().as_str(), Some("session/update"));
        let t = v
            .get("params").unwrap()
            .get("update").unwrap()
            .get("content").unwrap()
            .get("text").unwrap();
        assert_eq!(t.as_str(), Some("hello"));
    }

    #[test]
    fn escapes_survive_the_round_trip() {
        let awkward = "line\nbreak \"quoted\" back\\slash\ttab";
        let wrapped = format!(r#"{{"t":{}}}"#, quote(awkward));
        let v = parse(&wrapped).expect("parses");
        assert_eq!(v.get("t").unwrap().as_str(), Some(awkward));
    }

    /// The case a naive `\u` decoder gets wrong: an astral character arrives as
    /// a surrogate pair and each half alone is not a `char`.
    #[test]
    fn surrogate_pairs_become_one_character() {
        let v = parse(r#"{"t":"a🚀b"}"#).expect("parses");
        assert_eq!(v.get("t").unwrap().as_str(), Some("a\u{1F680}b"));
    }

    #[test]
    fn multibyte_utf8_passes_through_intact() {
        let v = parse("{\"t\":\"café — naïve\"}").unwrap();
        assert_eq!(v.get("t").unwrap().as_str(), Some("café — naïve"));
    }

    #[test]
    fn numbers_arrays_and_literals() {
        let v = parse(r#"{"a":[1,-2.5,1e3,true,false,null],"b":{}}"#).unwrap();
        let a = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(a[0].as_f64(), Some(1.0));
        assert_eq!(a[1].as_f64(), Some(-2.5));
        assert_eq!(a[2].as_f64(), Some(1000.0));
        assert_eq!(a[3], Value::Bool(true));
        assert_eq!(a[5], Value::Null);
    }

    #[test]
    fn junk_is_rejected_rather_than_half_read() {
        assert!(parse("{\"a\":1} trailing").is_none());
        assert!(parse("{\"a\":}").is_none());
        assert!(parse("{unquoted:1}").is_none());
        assert!(parse("").is_none());
    }
}
