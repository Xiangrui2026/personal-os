use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::ops::Range;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ChangeError {
    #[error(transparent)]
    Vault(#[from] pos_vault::VaultError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("change store: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("change data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("change store is not initialized; run pos change init")]
    NotInitialized,
    #[error("unsupported change store format: {0}")]
    StoreFormat(i64),
    #[error("change {0} was not found")]
    NotFound(Uuid),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("change {0} is pending; inspect it with pos change recover")]
    Pending(Uuid),
    #[error("change {id} needs recovery: {source}")]
    Interrupted { id: Uuid, source: Box<ChangeError> },
    #[error("unsafe file or directory: {0}")]
    UnsafePath(PathBuf),
    #[error("change store content is inconsistent: {0}")]
    Corrupt(String),
}

pub(crate) fn conflict(message: impl Into<String>) -> ChangeError {
    ChangeError::Conflict(message.into())
}

pub(crate) fn invalid(message: impl Into<String>) -> ChangeError {
    ChangeError::Invalid(message.into())
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").expect("writing to a String succeeds");
    }
    encoded
}

pub(crate) fn matching_ranges<'a>(
    source: &'a str,
    text: &'a str,
) -> impl Iterator<Item = Range<usize>> + 'a {
    source.char_indices().filter_map(move |(start, _)| {
        source[start..]
            .starts_with(text)
            .then(|| start..start + text.len())
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub kind: String,
    pub payload_version: u32,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeRequest {
    pub change_id: Uuid,
    pub path: String,
    pub actor_id: String,
    pub device_id: Option<String>,
    pub reason: Option<String>,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub start: usize,
    pub before: String,
    pub after: String,
    pub prefix: String,
    pub suffix: String,
}

impl Patch {
    pub(crate) fn new(
        source: &str,
        range: Range<usize>,
        after: String,
    ) -> Result<Self, ChangeError> {
        let before = source
            .get(range.clone())
            .ok_or_else(|| invalid("invalid UTF-8 byte range"))?;
        if before == after {
            return Err(invalid("a patch must change the document"));
        }
        let left = &source[..range.start];
        let right = &source[range.end..];
        let line_start = left.rfind('\n').unwrap_or(0);
        let line_end = right.find('\n').map_or(right.len(), |index| index + 1);
        let prefix = &left[line_start..];
        let suffix = &right[..line_end];
        let prefix = prefix
            .char_indices()
            .rev()
            .nth(31)
            .map_or(prefix, |(i, _)| &prefix[i..]);
        let suffix = suffix
            .char_indices()
            .nth(32)
            .map_or(suffix, |(i, _)| &suffix[..i]);
        Ok(Self {
            start: range.start,
            before: before.into(),
            after,
            prefix: prefix.into(),
            suffix: suffix.into(),
        })
    }

    pub(crate) fn apply(&self, source: &str) -> Result<String, ChangeError> {
        let end = self
            .start
            .checked_add(self.before.len())
            .ok_or_else(|| invalid("invalid patch offset"))?;
        if source.get(self.start..end) != Some(&self.before) {
            return Err(conflict("the approved patch no longer matches the note"));
        }
        let mut edited = source.to_owned();
        edited.replace_range(self.start..end, &self.after);
        Ok(edited)
    }

    pub(crate) fn matches<'a>(
        &'a self,
        source: &'a str,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        matching_ranges(source, &self.after)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePlan {
    pub vault_root: PathBuf,
    pub vault_id: Option<Uuid>,
    pub request: ChangeRequest,
    pub note_id: String,
    pub patch: Patch,
    pub before_hash: String,
    pub after_hash: String,
    #[serde(default = "present")]
    pub before_exists: bool,
    #[serde(default = "present")]
    pub after_exists: bool,
    pub depends_on: Vec<Uuid>,
    pub reverts: Option<Uuid>,
    pub reversible: bool,
}

fn present() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub seq: i64,
    pub vault_id: Uuid,
    pub executed_unix_ms: i64,
    pub plan: ChangePlan,
}

#[derive(Debug, Serialize)]
pub struct ChangeStoreInfo {
    pub vault_id: Uuid,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    NotApplied,
    AwaitingConfirmation,
    Recovered,
    Conflict,
    ClosedConflict,
}

#[derive(Debug, Serialize)]
pub struct RecoveryOutcome {
    pub change_id: Uuid,
    pub path: String,
    pub status: RecoveryStatus,
    pub current_hash: Option<String>,
    pub expected_before: String,
    pub expected_after: String,
    pub detail: Option<String>,
}
