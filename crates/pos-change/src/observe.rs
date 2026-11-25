use crate::model::{hash, invalid};
use crate::store::Store;
use crate::{ChangeEngine, ChangeError, ChangePlan, ChangeRecord, ChangeRequest, Operation, Patch};
use pos_vault::{IssueKind, NoteSource, VaultIssue, VaultWatch};
use rusqlite::params;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct ObservationReport {
    pub changes: Vec<ChangeRecord>,
    pub baselines: usize,
    pub issues: Vec<VaultIssue>,
}

struct Baseline {
    path: String,
    key: String,
    digest: String,
}

impl ChangeEngine {
    pub(crate) fn record_unseen_preimage(
        &self,
        store: &mut Store,
        plan: &ChangePlan,
    ) -> Result<(), ChangeError> {
        let previous = load_baselines(store)?;
        let Some(old) = previous.get(&plan.note_id) else {
            return Ok(());
        };
        if old.digest == plan.before_hash && old.path == plan.request.path {
            return Ok(());
        }
        let source = self.vault.read_source(&plan.request.path)?;
        if source.metadata.id != plan.note_id || hash(source.text.as_bytes()) != plan.before_hash {
            return Err(crate::model::conflict(
                "the note changed during execution preparation",
            ));
        }
        let vault_id = store.vault_id;
        let tx = store.db.transaction()?;
        self.record_observation(
            &tx,
            vault_id,
            "external",
            &plan.note_id,
            Some(old),
            Some(&source),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn watch_vault(&self) -> Result<VaultWatch, ChangeError> {
        Ok(self.vault.watch()?)
    }

    pub fn observe(&self, actor: &str) -> Result<ObservationReport, ChangeError> {
        if actor.trim().is_empty() {
            return Err(invalid("an observation actor is required"));
        }
        let (_lock, mut store) = self.write_store()?;
        let index = self.vault.scan()?;
        let mut issues = index.report()?.issues;
        let mut notes = Vec::new();
        for metadata in index.list()? {
            match self.vault.read_source(&metadata.path) {
                Ok(note) => notes.push(note),
                Err(error) => issues.push(VaultIssue {
                    kind: IssueKind::UnreadableFile,
                    path: metadata.path,
                    message: error.to_string(),
                }),
            }
        }
        let previous = load_baselines(&store)?;
        let pending = store.pending()?;
        let mut seen = HashSet::new();
        let mut duplicated = HashSet::new();
        for note in &notes {
            if !seen.insert(note.metadata.id.clone()) {
                duplicated.insert(note.metadata.id.clone());
            }
        }
        let mut report = ObservationReport {
            changes: Vec::new(),
            baselines: 0,
            issues,
        };
        let vault_id = store.vault_id;
        let tx = store.db.transaction()?;
        let ready: bool =
            tx.query_row("SELECT EXISTS (SELECT 1 FROM observation_state)", [], |r| {
                r.get(0)
            })?;
        let mut accounted = HashSet::new();
        let visible_paths: HashSet<_> = notes
            .iter()
            .map(|note| note.metadata.path.clone())
            .collect();
        for note in notes {
            let path = &note.metadata.path;
            if duplicated.contains(&note.metadata.id) {
                report.issues.push(VaultIssue {
                    kind: IssueKind::DuplicateId,
                    path: path.clone(),
                    message: "ambiguous observation identity".into(),
                });
                continue;
            }
            let mut note_id = note.metadata.id.clone();
            if !note_id.starts_with("yaml:") && !previous.contains_key(&note_id) {
                // An editor may save by replacing the file at the same path.
                let candidates: Vec<_> = previous
                    .iter()
                    .filter(|(id, old)| {
                        !id.starts_with("yaml:") && old.path == *path && !seen.contains(*id)
                    })
                    .map(|(id, _)| id)
                    .collect();
                if let [id] = candidates.as_slice() {
                    note_id = (*id).clone();
                }
            }
            accounted.insert(note_id.clone());
            if pending
                .iter()
                .any(|plan| plan.request.path == *path || plan.note_id == note_id)
            {
                continue;
            }
            let digest = hash(note.text.as_bytes());
            let old = previous.get(&note_id);
            if old.is_some_and(|old| {
                old.path == *path && old.digest == digest && old.key == note.file_key
            }) {
                continue;
            }
            if (!ready && old.is_none())
                || old.is_some_and(|old| old.path == *path && old.digest == digest)
            {
                tx.execute(
                    "INSERT OR IGNORE INTO content_blobs VALUES (?1, ?2)",
                    params![digest, note.text.as_bytes()],
                )?;
                Store::baseline(&tx, &note_id, path, &note.file_key, &digest)?;
                report.baselines += 1;
            } else {
                report.changes.push(self.record_observation(
                    &tx,
                    vault_id,
                    actor,
                    &note_id,
                    old,
                    Some(&note),
                )?);
            }
        }
        if report.issues.is_empty() {
            for (id, old) in &previous {
                if !accounted.contains(id)
                    && (visible_paths.contains(&old.path) || !self.root.join(&old.path).exists())
                    && !pending
                        .iter()
                        .any(|plan| plan.request.path == old.path || plan.note_id == *id)
                {
                    report.changes.push(self.record_observation(
                        &tx,
                        vault_id,
                        actor,
                        id,
                        Some(old),
                        None,
                    )?);
                }
            }
            tx.execute("INSERT OR IGNORE INTO observation_state VALUES (1)", [])?;
        }
        tx.commit()?;
        Ok(report)
    }

    fn record_observation(
        &self,
        tx: &rusqlite::Transaction<'_>,
        vault_id: Uuid,
        actor: &str,
        note_id: &str,
        old: Option<&Baseline>,
        current: Option<&NoteSource>,
    ) -> Result<ChangeRecord, ChangeError> {
        let before = old
            .map(|old| Store::blob_in(tx, &old.digest))
            .transpose()?
            .unwrap_or_default();
        let after = current.map_or("", |note| note.text.as_str());
        let path = current
            .map(|note| note.metadata.path.as_str())
            .or_else(|| old.map(|old| old.path.as_str()))
            .ok_or_else(|| invalid("observation has no note"))?;
        let plan = ChangePlan {
            vault_root: self.root.clone(),
            vault_id: Some(vault_id),
            request: ChangeRequest {
                change_id: Uuid::new_v4(),
                path: path.into(),
                actor_id: actor.into(),
                device_id: None,
                reason: None,
                operation: Operation {
                    kind: "external.observe".into(),
                    payload_version: 1,
                    payload: json!({ "previous_path": old.map(|old| &old.path) }),
                },
            },
            note_id: note_id.into(),
            patch: Patch {
                start: 0,
                before: before.clone(),
                after: after.into(),
                prefix: String::new(),
                suffix: String::new(),
            },
            before_hash: hash(before.as_bytes()),
            after_hash: hash(after.as_bytes()),
            before_exists: old.is_some(),
            after_exists: current.is_some(),
            depends_on: Vec::new(),
            reverts: None,
            reversible: false,
        };
        Store::prepare_in(tx, &plan, before.as_bytes(), after.as_bytes())?;
        Store::finish_in(
            tx,
            vault_id,
            &plan,
            current.map(|note| note.file_key.as_str()),
        )
    }
}

fn load_baselines(store: &Store) -> Result<BTreeMap<String, Baseline>, ChangeError> {
    let mut query = store
        .db
        .prepare("SELECT note_id, path, file_key, hash FROM observed_notes ORDER BY note_id")?;
    Ok(query
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Baseline {
                    path: row.get(1)?,
                    key: row.get(2)?,
                    digest: row.get(3)?,
                },
            ))
        })?
        .collect::<Result<_, _>>()?)
}
