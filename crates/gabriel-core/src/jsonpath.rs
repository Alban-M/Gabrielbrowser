//! A deliberately small JSON path: `data.items[0].id`.
//!
//! Full JSONPath (filters, wildcards, recursive descent) is a dependency and a
//! surface area we don't need yet — captures and assertions address concrete
//! fields. A leading `$.` is accepted so paths copied out of other tools work.

use crate::error::{Error, Result};
use serde_json::Value;

pub fn select<'a>(root: &'a Value, path: &str) -> Result<Option<&'a Value>> {
    let mut current = root;
    for segment in parse(path)? {
        let next = match segment {
            Segment::Key(key) => current.get(&key),
            Segment::Index(index) => current.get(index),
        };
        match next {
            Some(value) => current = value,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// Render a selected value the way a shell user wants it: strings unquoted,
/// everything else as compact JSON.
pub fn to_plain_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

enum Segment {
    Key(String),
    Index(usize),
}

fn parse(path: &str) -> Result<Vec<Segment>> {
    let path = path.trim();
    let path = path.strip_prefix("$.").unwrap_or_else(|| path.strip_prefix('$').unwrap_or(path));
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    segments.push(Segment::Key(std::mem::take(&mut current)));
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(Segment::Key(std::mem::take(&mut current)));
                }
                let mut index = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    index.push(ch);
                }
                if !closed {
                    return Err(Error::BadJsonPath(path.to_string()));
                }
                let index = index.trim().trim_matches(['\'', '"']);
                match index.parse::<usize>() {
                    Ok(n) => segments.push(Segment::Index(n)),
                    // `["key with spaces"]` is worth supporting; it costs nothing.
                    Err(_) if !index.is_empty() => segments.push(Segment::Key(index.to_string())),
                    Err(_) => return Err(Error::BadJsonPath(path.to_string())),
                }
            }
            ']' => return Err(Error::BadJsonPath(path.to_string())),
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        segments.push(Segment::Key(current));
    }
    if segments.is_empty() {
        return Err(Error::BadJsonPath(path.to_string()));
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "data": { "items": [ { "id": 7, "name": "ada" }, { "id": 8 } ] },
            "ok": true,
            "odd key": 1
        })
    }

    #[test]
    fn walks_objects_and_arrays() {
        let doc = doc();
        let value = select(&doc, "data.items[0].name").unwrap().unwrap();
        assert_eq!(value, &json!("ada"));
    }

    #[test]
    fn accepts_a_dollar_prefix() {
        assert_eq!(select(&doc(), "$.ok").unwrap().unwrap(), &json!(true));
        assert_eq!(select(&doc(), "$ok").unwrap().unwrap(), &json!(true));
    }

    #[test]
    fn bracket_strings_address_awkward_keys() {
        let doc = doc();
        let value = select(&doc, r#"["odd key"]"#).unwrap().unwrap();
        assert_eq!(value, &json!(1));
    }

    #[test]
    fn missing_paths_are_none_not_errors() {
        assert!(select(&doc(), "data.items[9].id").unwrap().is_none());
        assert!(select(&doc(), "nope").unwrap().is_none());
    }

    #[test]
    fn malformed_paths_are_errors() {
        assert!(select(&doc(), "data[0").is_err());
        assert!(select(&doc(), "").is_err());
    }

    #[test]
    fn strings_render_without_quotes() {
        assert_eq!(to_plain_string(&json!("ada")), "ada");
        assert_eq!(to_plain_string(&json!(7)), "7");
    }
}
