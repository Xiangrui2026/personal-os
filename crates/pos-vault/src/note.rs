use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::ops::Range;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WikiLink {
    pub target: String,
    pub fragment: Option<String>,
    pub label: Option<String>,
    pub embed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NoteMetadata {
    pub path: String,
    pub title: String,
    pub id: String,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub links: Vec<WikiLink>,
}

#[derive(Debug, Serialize)]
pub struct Note {
    pub metadata: NoteMetadata,
    pub frontmatter: Map<String, Value>,
    pub body: String,
}

pub(crate) fn parse(path: String, source: &str) -> Result<Note, String> {
    let (frontmatter, body) = split_frontmatter(source)?;
    let mut tags = text_list(frontmatter.get("tags"), "tags")?;
    for tag in &mut tags {
        if let Some(text) = tag.strip_prefix('#') {
            *tag = text.to_owned();
        }
    }
    tags.retain(|tag| !tag.is_empty());
    tags.sort();
    tags.dedup();
    let aliases = text_list(frontmatter.get("aliases"), "aliases")?;

    let id = match frontmatter.get("id") {
        Some(Value::String(id)) if !id.trim().is_empty() => format!("yaml:{}", id.trim()),
        Some(Value::Number(id)) if id.is_i64() || id.is_u64() => format!("yaml:{id}"),
        Some(Value::Null) | None => format!("path:{path}"),
        _ => return Err("frontmatter id must be a non-empty string or integer".into()),
    };
    let title = Path::new(&path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(&path)
        .to_owned();
    Ok(Note {
        metadata: NoteMetadata {
            path,
            title,
            id,
            tags,
            aliases,
            links: wiki_links(body),
        },
        frontmatter,
        body: body.to_owned(),
    })
}

fn text_list(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let values = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(value) => std::slice::from_ref(value),
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(|text| text.trim().to_owned())
                .ok_or_else(|| format!("frontmatter {field} must contain text"))
        })
        .filter(|result| !matches!(result, Ok(text) if text.is_empty()))
        .collect()
}

fn split_frontmatter(source: &str) -> Result<(Map<String, Value>, &str), String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.split_inclusive('\n');
    let Some(opening) = lines.next() else {
        return Ok((Map::new(), source));
    };
    if opening.trim_end() != "---" {
        return Ok((Map::new(), source));
    }
    let mut offset = opening.len();
    for line in lines {
        if matches!(line.trim_end(), "---" | "...") {
            let yaml = &source[opening.len()..offset];
            let parsed = yaml_serde::from_str::<Option<Map<String, Value>>>(yaml)
                .map_err(|error| format!("invalid YAML frontmatter: {error}"))?;
            return Ok((parsed.unwrap_or_default(), &source[offset + line.len()..]));
        }
        offset += line.len();
    }
    // Without a closing delimiter the opening line is a Markdown thematic break.
    Ok((Map::new(), source))
}

fn wiki_links(body: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();
    let mut literal = Vec::new();
    let options = Options::ENABLE_WIKILINKS | Options::ENABLE_MATH;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        let embed = match event {
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                ..
            }) => false,
            Event::Start(Tag::Image {
                link_type: LinkType::WikiLink { .. },
                ..
            }) => true,
            Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::Start(Tag::CodeBlock(_)) => {
                literal.push(range);
                continue;
            }
            _ => continue,
        };
        let raw = &body[range.clone()];
        let inner = raw
            .trim_start_matches('!')
            .trim_start_matches("[[")
            .trim_end_matches("]]");
        let (destination, label) = match inner.split_once('|') {
            Some((target, label)) => (
                target.strip_suffix('\\').unwrap_or(target),
                Some(label.trim().to_owned()),
            ),
            None => (inner, None),
        };
        let (target, fragment) = match destination.split_once('#') {
            Some((target, fragment)) => (target, Some(fragment.trim().to_owned())),
            None => (destination, None),
        };
        if !target.trim().is_empty() || fragment.as_ref().is_some_and(|value| !value.is_empty()) {
            links.push((
                range.start,
                WikiLink {
                    target: target.trim().to_owned(),
                    fragment,
                    label,
                    embed,
                },
            ));
        }
    }

    let comments = comment_ranges(body, &literal);
    let mut comments = comments.into_iter().peekable();
    links
        .into_iter()
        .filter_map(|(offset, link)| {
            while comments.peek().is_some_and(|range| range.end <= offset) {
                comments.next();
            }
            (!comments.peek().is_some_and(|range| range.contains(&offset))).then_some(link)
        })
        .collect()
}

fn comment_ranges(text: &str, literal: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut opening = None;
    for (offset, _) in text.match_indices("%%") {
        let escaped = text.as_bytes()[..offset]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b'\\')
            .count()
            % 2
            != 0;
        if escaped || literal.iter().any(|range| range.contains(&offset)) {
            continue;
        }
        if let Some(start) = opening.take() {
            ranges.push(start..offset + 2);
        } else {
            opening = Some(offset);
        }
    }
    if let Some(start) = opening {
        ranges.push(start..text.len());
    }
    ranges
}
