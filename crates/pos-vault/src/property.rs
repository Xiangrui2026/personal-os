use crate::note::{frontmatter_range, split_frontmatter};
use serde_json::{Map, Value};
use std::ops::Range;

pub struct PropertyEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

/// Rewrites only the selected top-level property, then checks the full parsed map.
pub fn prepare_property(
    source: &str,
    key: &str,
    value: Option<Value>,
) -> Result<PropertyEdit, String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err("property name must be a simple YAML key".into());
    }
    let (mut expected, _) = split_frontmatter(source)?;
    let old = match &value {
        Some(value) => expected.insert(key.into(), value.clone()),
        None => expected.remove(key),
    };
    if old == value {
        return Err("property already has the requested value".into());
    }
    let newline = if source
        .lines()
        .next()
        .is_some_and(|line| source.as_bytes().get(line.len()) == Some(&b'\r'))
    {
        "\r\n"
    } else {
        "\n"
    };
    let definition = value.as_ref().map(|value| format!("{key}: {value}"));
    let edit = match frontmatter_range(source) {
        None => {
            let start = source
                .starts_with('\u{feff}')
                .then_some('\u{feff}'.len_utf8())
                .unwrap_or(0);
            let definition = definition.ok_or("property is absent")?;
            PropertyEdit {
                range: start..start,
                replacement: format!("---{newline}{definition}{newline}---{newline}"),
            }
        }
        Some((yaml, _)) if old.is_none() => {
            let definition = definition.ok_or("property is absent")?;
            PropertyEdit {
                range: yaml.end..yaml.end,
                replacement: format!("{definition}{newline}"),
            }
        }
        Some((yaml, _)) => {
            let content = &source[yaml.clone()];
            let mut start = None;
            let mut end = yaml.end;
            let mut offset = yaml.start;
            for line in content.split_inclusive('\n') {
                if let Some(candidate) = line.split_once(':').map(|(name, _)| name) {
                    let candidate = candidate.trim_end();
                    if !candidate.starts_with(char::is_whitespace) && !candidate.starts_with('#') {
                        if start.is_some() {
                            end = offset;
                            break;
                        }
                        if candidate == key
                            || candidate == format!("\"{key}\"")
                            || candidate == format!("'{key}'")
                        {
                            start = Some(offset);
                        }
                    }
                }
                offset += line.len();
            }
            let start = start.ok_or("unable to find the original YAML property")?;
            // Leave blank lines and unrelated top-level comments after this value in place.
            while end > start {
                let tail = &source[start..end];
                let without_lf = tail.strip_suffix('\n').unwrap_or(tail);
                let without_eol = without_lf.strip_suffix('\r').unwrap_or(without_lf);
                let line_start = without_eol.rfind('\n').map_or(0, |i| i + 1);
                let line = &without_eol[line_start..];
                if line.trim().is_empty() || line.starts_with('#') {
                    end = start + line_start;
                } else {
                    break;
                }
            }
            let first_end = source[start..end].find('\n').map_or(end, |i| start + i + 1);
            let first_line = source[start..first_end].trim_end_matches(['\r', '\n']);
            let comment = first_line.match_indices(" #").find_map(|(i, _)| {
                let prefix = &first_line[..i];
                let parsed = yaml_serde::from_str::<Map<String, Value>>(prefix).ok()?;
                (parsed.get(key) == old.as_ref() || prefix.trim_end().ends_with([':', '|', '>']))
                    .then_some(&first_line[i..])
            });
            let replacement = match definition {
                Some(definition) => format!("{definition}{}{newline}", comment.unwrap_or("")),
                None => comment.map_or(String::new(), |text| {
                    format!("{}{newline}", text.trim_start())
                }),
            };
            PropertyEdit {
                range: start..end,
                replacement,
            }
        }
    };
    let mut preview = source.to_owned();
    preview.replace_range(edit.range.clone(), &edit.replacement);
    if split_frontmatter(&preview)?.0 != expected {
        return Err("property edit would change other YAML data".into());
    }
    Ok(edit)
}
