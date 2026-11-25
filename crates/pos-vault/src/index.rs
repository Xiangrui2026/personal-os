use crate::vault::{is_note, path_key};
use crate::{IssueKind, NoteMetadata, Vault, VaultError, VaultIssue};
use ignore::gitignore::Gitignore;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashSet;

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
}

impl VaultIndex {
    pub(crate) fn build(vault: &Vault, ignore: &Gitignore) -> Result<Self, VaultError> {
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
                match vault.read_note(relative, ignore) {
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
        Ok(Self { db, issues })
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
                if link.target.is_empty() {
                    continue; // Local heading or block reference.
                }
                let paths = self.resolve_link(&note.path, &link.target)?;
                let (kind, message) = match paths.as_slice() {
                    [] => (
                        IssueKind::UnresolvedLink,
                        format!("no file matches [[{}]]", link.target),
                    ),
                    [_] => continue,
                    _ => (
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
