use crate::vault::{included, is_link, is_note, path_key};
use crate::{IssueKind, NoteMetadata, Vault, VaultChanges, VaultError, VaultIssue, WikiLink};
use ignore::gitignore::Gitignore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LinkResolution {
    Resolved { path: String },
    MissingFile,
    Ambiguous { paths: Vec<String> },
    MissingAnchor { path: String, fragment: String },
}

#[derive(Debug, Serialize)]
pub struct Backlink {
    pub source: String,
    pub link: WikiLink,
}

#[derive(Debug, Serialize)]
pub struct VaultStats {
    pub files: u64,
    pub notes: usize,
    pub tags: usize,
    pub links: usize,
    pub scan_issues: usize,
}

#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub stats: VaultStats,
    pub issues: Vec<VaultIssue>,
}

/// A point-in-time inventory with no on-disk database or journal.
pub struct VaultIndex {
    db: Connection,
    issues: Vec<VaultIssue>,
    root: std::path::PathBuf,
}

impl VaultIndex {
    pub(crate) fn build(
        vault: &Vault,
        ignore: &Gitignore,
        bindings: &crate::identity::Bindings,
    ) -> Result<Self, VaultError> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch(
            "PRAGMA temp_store = MEMORY;
            CREATE TABLE files (
                path TEXT PRIMARY KEY,
                path_key TEXT NOT NULL,
                name_key TEXT NOT NULL
            );
            CREATE INDEX file_path ON files(path_key);
            CREATE INDEX file_name ON files(name_key);
            CREATE TABLE notes (
                path TEXT PRIMARY KEY,
                id TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE INDEX note_id ON notes(id);",
        )?;

        let mut issues = Vec::new();
        let transaction = db.transaction()?;
        {
            let mut insert_file = transaction.prepare("INSERT INTO files VALUES (?1, ?2, ?3)")?;
            let mut insert_note = transaction.prepare("INSERT INTO notes VALUES (?1, ?2, ?3)")?;
            for entry in vault.entries(ignore) {
                let entry = match entry {
                    Ok(entry) if entry.file_type().is_file() => entry,
                    Ok(_) => continue,
                    Err(error) => {
                        issues.push(VaultIssue {
                            kind: IssueKind::UnreadableFile,
                            path: error
                                .path()
                                .map(|path| vault.relative(path).display().to_string())
                                .unwrap_or_else(|| ".".into()),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                let relative = vault.relative(entry.path());
                let Some(path) = path_key(relative) else {
                    issues.push(VaultIssue {
                        kind: IssueKind::UnreadableFile,
                        path: relative.display().to_string(),
                        message: "path cannot be represented as a portable UTF-8 path".into(),
                    });
                    continue;
                };
                insert_file.execute(params![
                    path,
                    path.to_lowercase(),
                    basename(&path).to_lowercase()
                ])?;
                if !is_note(relative) {
                    continue;
                }
                match vault.read_note(relative, ignore, bindings) {
                    Ok(note) => {
                        let metadata = serde_json::to_string(&note.metadata)?;
                        insert_note.execute(params![path, note.metadata.id, metadata])?;
                    }
                    Err(error) => {
                        let kind = match error {
                            VaultError::InvalidNote { .. } => IssueKind::InvalidNote,
                            _ => IssueKind::UnreadableFile,
                        };
                        issues.push(VaultIssue {
                            kind,
                            path,
                            message: error.to_string(),
                        });
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(Self {
            db,
            issues,
            root: vault.info().root,
        })
    }

    pub fn list(&self) -> Result<Vec<NoteMetadata>, VaultError> {
        let mut query = self
            .db
            .prepare("SELECT metadata FROM notes ORDER BY path")?;
        let encoded = query.query_map([], |row| row.get::<_, String>(0))?;
        encoded
            .map(|value| Ok(serde_json::from_str(&value?)?))
            .collect()
    }

    pub fn refresh(&mut self, vault: &Vault, changes: &VaultChanges) -> Result<(), VaultError> {
        let root = vault.info().root;
        if self.root != root
            || changes.rescan
            || changes
                .paths
                .iter()
                .any(|path| path.extension().is_none() || root.join(path).is_dir())
        {
            *self = vault.scan()?;
            return Ok(());
        }
        let ignore = vault.ignore_rules()?;
        let bindings = crate::identity::bindings(&root)?;
        let mut issues = self.issues.clone();
        let transaction = self.db.transaction()?;
        for relative in &changes.paths {
            let Some(path) = path_key(relative) else {
                continue;
            };
            let key = path.to_lowercase();
            if cfg!(windows) {
                transaction.execute(
                    "DELETE FROM notes WHERE path IN (SELECT path FROM files WHERE path_key = ?1)",
                    [&key],
                )?;
                transaction.execute("DELETE FROM files WHERE path_key = ?1", [&key])?;
            } else {
                transaction.execute("DELETE FROM notes WHERE path = ?1", [&path])?;
                transaction.execute("DELETE FROM files WHERE path = ?1", [&path])?;
            }
            issues.retain(|issue| issue.path != path);
            let full = root.join(relative);
            if !included(&ignore, relative, false)
                || !std::fs::symlink_metadata(&full)
                    .is_ok_and(|meta| meta.is_file() && !is_link(&meta))
                || std::fs::canonicalize(&full).is_ok_and(|actual| actual != full)
            {
                continue;
            }
            transaction.execute(
                "INSERT INTO files VALUES (?1, ?2, ?3)",
                params![path, key, basename(&path).to_lowercase()],
            )?;
            if is_note(relative) {
                match vault.read_note(relative, &ignore, &bindings) {
                    Ok(note) => {
                        transaction.execute(
                            "INSERT INTO notes VALUES (?1, ?2, ?3)",
                            params![
                                path,
                                note.metadata.id,
                                serde_json::to_string(&note.metadata)?
                            ],
                        )?;
                    }
                    Err(error) => issues.push(VaultIssue {
                        kind: if matches!(error, VaultError::InvalidNote { .. }) {
                            IssueKind::InvalidNote
                        } else {
                            IssueKind::UnreadableFile
                        },
                        path,
                        message: error.to_string(),
                    }),
                }
            }
        }
        transaction.commit()?;
        self.issues = issues;
        Ok(())
    }

    pub fn resolve(&self, source: &str, link: &WikiLink) -> Result<LinkResolution, VaultError> {
        let paths = if link.target.is_empty() {
            self.lookup("path_key", &source.to_lowercase())?
        } else {
            self.resolve_link(source, &link.target)?
        };
        let path = match paths.as_slice() {
            [path] => path,
            [] => return Ok(LinkResolution::MissingFile),
            _ => return Ok(LinkResolution::Ambiguous { paths }),
        };
        if let Some(fragment) = link
            .fragment
            .as_ref()
            .filter(|fragment| !fragment.is_empty())
            && is_note(Path::new(path))
        {
            let text: Option<String> = self
                .db
                .query_row(
                    "SELECT metadata FROM notes WHERE path = ?1",
                    [path],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(text) = text {
                let note: NoteMetadata = serde_json::from_str(&text)?;
                if !anchor_exists(&note, fragment) {
                    return Ok(LinkResolution::MissingAnchor {
                        path: path.clone(),
                        fragment: fragment.clone(),
                    });
                }
            }
        }
        Ok(LinkResolution::Resolved { path: path.clone() })
    }

    pub fn backlinks(&self, target: &str) -> Result<Vec<Backlink>, VaultError> {
        let mut backlinks = Vec::new();
        for note in self.list()? {
            for link in note.links {
                if matches!(self.resolve(&note.path, &link)?,
                    LinkResolution::Resolved { path } | LinkResolution::MissingAnchor { path, .. } if path == target)
                {
                    backlinks.push(Backlink {
                        source: note.path.clone(),
                        link,
                    });
                }
            }
        }
        Ok(backlinks)
    }

    pub fn stats(&self) -> Result<VaultStats, VaultError> {
        let notes = self.list()?;
        let tags: HashSet<_> = notes.iter().flat_map(|note| &note.tags).collect();
        Ok(VaultStats {
            files: self
                .db
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))?
                as u64,
            notes: notes.len(),
            tags: tags.len(),
            links: notes.iter().map(|note| note.links.len()).sum(),
            scan_issues: self.issues.len(),
        })
    }

    pub fn report(&self) -> Result<ScanReport, VaultError> {
        Ok(ScanReport {
            stats: self.stats()?,
            issues: self.issues.clone(),
        })
    }

    pub fn validate(&self) -> Result<Vec<VaultIssue>, VaultError> {
        let mut issues = self.issues.clone();
        let mut duplicates = self.db.prepare(
            "SELECT path, id FROM notes
             WHERE id IN (SELECT id FROM notes GROUP BY id HAVING COUNT(*) > 1)
             ORDER BY path",
        )?;
        let rows = duplicates.query_map([], |row| {
            Ok(VaultIssue {
                kind: IssueKind::DuplicateId,
                path: row.get(0)?,
                message: format!("shared note ID: {}", row.get::<_, String>(1)?),
            })
        })?;
        issues.extend(rows.collect::<Result<Vec<_>, _>>()?);

        for note in self.list()? {
            for link in &note.links {
                let (kind, message) = match self.resolve(&note.path, link)? {
                    LinkResolution::MissingFile => (
                        IssueKind::UnresolvedLink,
                        format!("no file matches [[{}]]", link.target),
                    ),
                    LinkResolution::Resolved { .. } => continue,
                    LinkResolution::MissingAnchor { path, fragment } => (
                        IssueKind::UnresolvedAnchor,
                        format!("no anchor #{fragment} in {path}"),
                    ),
                    LinkResolution::Ambiguous { paths } => (
                        IssueKind::AmbiguousLink,
                        format!(
                            "multiple files match [[{}]]: {}",
                            link.target,
                            paths.join(", ")
                        ),
                    ),
                };
                issues.push(VaultIssue {
                    kind,
                    path: note.path.clone(),
                    message,
                });
            }
        }
        Ok(issues)
    }

    fn resolve_link(&self, source: &str, target: &str) -> Result<Vec<String>, VaultError> {
        let target = target.replace('\\', "/").to_lowercase();
        let (parent, _) = source.rsplit_once('/').unwrap_or(("", source));
        let parent = parent.to_lowercase();
        let relative_only = target.starts_with("./") || target.starts_with("../");
        let root_only = target.starts_with('/');

        if !root_only
            && (relative_only || !target.contains('/'))
            && let Some(path) = normalize(&parent, &target)
        {
            let hits = self.lookup("path_key", &path)?;
            if relative_only || !hits.is_empty() {
                return Ok(hits);
            }
        }
        if relative_only {
            return Ok(Vec::new());
        }
        let Some(path) = normalize("", target.trim_start_matches('/')) else {
            return Ok(Vec::new());
        };
        let hits = self.lookup("path_key", &path)?;
        if root_only || !hits.is_empty() {
            return Ok(hits);
        }

        let suffix = format!("/{path}");
        let suffix_md = format!("{suffix}.md");
        let hits = self.lookup("name_key", basename(&path))?;
        Ok(hits
            .into_iter()
            .filter(|candidate| {
                let key = candidate.to_lowercase();
                key.ends_with(&suffix) || key.ends_with(&suffix_md)
            })
            .collect())
    }

    fn lookup(&self, key: &str, path: &str) -> Result<Vec<String>, VaultError> {
        let sql = match key {
            "path_key" => "SELECT path FROM files WHERE path_key IN (?1, ?2) ORDER BY path",
            _ => "SELECT path FROM files WHERE name_key IN (?1, ?2) ORDER BY path",
        };
        let with_extension = format!("{path}.md");
        let mut query = self.db.prepare_cached(sql)?;
        Ok(query
            .query_map(params![path, with_extension], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

fn anchor_exists(note: &NoteMetadata, fragment: &str) -> bool {
    if let Some(id) = fragment.strip_prefix('^') {
        return note
            .blocks
            .iter()
            .any(|block| block.id.as_deref() == Some(id));
    }
    let parts: Vec<_> = fragment
        .split('#')
        .map(|text| text.trim().to_lowercase())
        .collect();
    let mut stack = Vec::<(u8, String)>::new();
    for heading in &note.headings {
        while stack
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            stack.pop();
        }
        stack.push((heading.level, heading.text.to_lowercase()));
        if stack.len() >= parts.len()
            && stack[stack.len() - parts.len()..]
                .iter()
                .map(|(_, text)| text)
                .eq(&parts)
        {
            return true;
        }
    }
    false
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn normalize<'a>(parent: &'a str, target: &'a str) -> Option<String> {
    let mut parts: Vec<_> = parent.split('/').filter(|part| !part.is_empty()).collect();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ if part.contains(':') => return None,
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
