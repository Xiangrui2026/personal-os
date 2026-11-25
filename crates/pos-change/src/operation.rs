use crate::model::{ChangeError, Operation, Patch, conflict, invalid, matching_ranges};
use pos_vault::{NoteSource, inspect_markdown};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::ops::Range;

pub struct OperationEdit {
    pub range: Range<usize>,
    pub replacement: String,
    pub reads: Vec<Range<usize>>,
    pub reversible: bool,
}

pub trait OperationHandler: Send + Sync {
    fn kind(&self) -> &'static str;
    fn payload_version(&self) -> u32 {
        1
    }
    fn prepare(
        &self,
        operation: &Operation,
        source: &NoteSource,
    ) -> Result<OperationEdit, ChangeError>;
}

pub struct OperationRegistry {
    handlers: BTreeMap<(&'static str, u32), Box<dyn OperationHandler>>,
}

impl Default for OperationRegistry {
    fn default() -> Self {
        let handlers: [Box<dyn OperationHandler>; 2] = [Box::new(LinkAdd), Box::new(LinkRemove)];
        Self {
            handlers: handlers
                .into_iter()
                .map(|handler| ((handler.kind(), handler.payload_version()), handler))
                .collect(),
        }
    }
}

impl OperationRegistry {
    pub fn register(
        &mut self,
        handler: impl OperationHandler + 'static,
    ) -> Result<(), ChangeError> {
        let key = (handler.kind(), handler.payload_version());
        if key.0 == "change.revert"
            || key.0.trim().is_empty()
            || key.1 == 0
            || self.handlers.contains_key(&key)
        {
            return Err(invalid(
                "reserved, invalid, or duplicate operation registration",
            ));
        }
        self.handlers.insert(key, Box::new(handler));
        Ok(())
    }

    pub(crate) fn prepare(
        &self,
        operation: &Operation,
        source: &NoteSource,
    ) -> Result<OperationEdit, ChangeError> {
        let key = (operation.kind.as_str(), operation.payload_version);
        let handler = self
            .handlers
            .get(&key)
            .ok_or_else(|| invalid(format!("unsupported operation {} version {}", key.0, key.1)))?;
        handler.prepare(operation, source)
    }
}

impl Operation {
    pub fn link_add(text: &str, target: &str, offset: Option<usize>) -> Self {
        Self {
            kind: "link.add".into(),
            payload_version: 1,
            payload: json!({ "text": text, "target": target, "offset": offset }),
        }
    }

    pub fn link_remove(link: &str, offset: Option<usize>) -> Self {
        Self {
            kind: "link.remove".into(),
            payload_version: 1,
            payload: json!({ "link": link, "offset": offset }),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddPayload {
    text: String,
    target: String,
    offset: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovePayload {
    link: String,
    offset: Option<usize>,
}

struct LinkAdd;
struct LinkRemove;

impl OperationHandler for LinkAdd {
    fn kind(&self) -> &'static str {
        "link.add"
    }

    fn prepare(
        &self,
        operation: &Operation,
        source: &NoteSource,
    ) -> Result<OperationEdit, ChangeError> {
        let payload: AddPayload = serde_json::from_value(operation.payload.clone())?;
        if [&payload.text, &payload.target].into_iter().any(|value| {
            value.trim().is_empty()
                || value.trim() != value
                || value.contains(['[', ']', '|', '\r', '\n', '\\'])
        }) {
            return Err(invalid(
                "text and target must fit on one line and exclude WikiLink delimiters",
            ));
        }
        let markup = inspect_markdown(&source.text).map_err(invalid)?;
        let ranges = matching_ranges(&source.text, &payload.text)
            .filter(|range| markup.is_plain_text(range));
        let range = select(ranges, payload.offset)?;
        let replacement = if payload.text == payload.target {
            format!("[[{}]]", payload.target)
        } else {
            format!("[[{}|{}]]", payload.target, payload.text)
        };
        validate_link_edit(&source.text, &range, &replacement, true)?;
        Ok(OperationEdit {
            range,
            replacement,
            reads: Vec::new(),
            reversible: true,
        })
    }
}

impl OperationHandler for LinkRemove {
    fn kind(&self) -> &'static str {
        "link.remove"
    }

    fn prepare(
        &self,
        operation: &Operation,
        source: &NoteSource,
    ) -> Result<OperationEdit, ChangeError> {
        let payload: RemovePayload = serde_json::from_value(operation.payload.clone())?;
        let markup = inspect_markdown(&source.text).map_err(invalid)?;
        let candidates = markup.links.iter().filter(|located| {
            !located.link.embed
                && source.text.get(located.range.clone()) == Some(payload.link.as_str())
        });
        let range = select(
            candidates.clone().map(|located| located.range.clone()),
            payload.offset,
        )?;
        let link = &candidates
            .into_iter()
            .find(|located| located.range == range)
            .ok_or_else(|| conflict("the WikiLink occurrence is missing"))?
            .link;
        let replacement = link.label.clone().unwrap_or_else(|| {
            if link.target.is_empty() {
                link.fragment.clone().unwrap_or_default()
            } else {
                link.target.clone()
            }
        });
        validate_link_edit(&source.text, &range, &replacement, false)?;
        Ok(OperationEdit {
            range,
            replacement,
            reads: Vec::new(),
            reversible: true,
        })
    }
}

fn select(
    ranges: impl Iterator<Item = Range<usize>>,
    offset: Option<usize>,
) -> Result<Range<usize>, ChangeError> {
    let mut matches = ranges.filter(|range| offset.is_none_or(|offset| range.start == offset));
    match (matches.next(), matches.next()) {
        (Some(range), None) => Ok(range),
        (None, _) => Err(conflict("no matching editable occurrence")),
        _ => Err(conflict(
            "multiple editable occurrences; provide an exact byte offset",
        )),
    }
}

pub(crate) fn validate_link_edit(
    source: &str,
    range: &Range<usize>,
    replacement: &str,
    add: bool,
) -> Result<(), ChangeError> {
    let patch = Patch::new(source, range.clone(), replacement.into())?;
    let edited = patch.apply(source)?;
    let before = inspect_markdown(source).map_err(invalid)?;
    let after = inspect_markdown(&edited).map_err(invalid)?;
    let new_range = range.start..range.start + replacement.len();
    let plain = if add {
        before.is_plain_text(range)
    } else {
        after.is_plain_text(&new_range)
    };
    let link = if add {
        after
            .links
            .iter()
            .any(|link| !link.link.embed && link.range == new_range)
    } else {
        before
            .links
            .iter()
            .any(|link| !link.link.embed && link.range == *range)
    };
    let old_other = before
        .links
        .iter()
        .filter(|link| link.range != *range)
        .map(|link| {
            let start = if link.range.start >= range.end {
                link.range.start - range.end + new_range.end
            } else {
                link.range.start
            };
            (start, &source[link.range.clone()])
        });
    let new_other = after
        .links
        .iter()
        .filter(|link| link.range != new_range)
        .map(|link| (link.range.start, &edited[link.range.clone()]));
    if !plain || !link || !old_other.eq(new_other) {
        return Err(conflict(
            "the patch would change surrounding Markdown or another WikiLink",
        ));
    }
    Ok(())
}
