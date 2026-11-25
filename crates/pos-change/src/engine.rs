use crate::filesystem::{Layout, regular};
use crate::model::{conflict, hash, invalid};
use crate::operation::validate_link_edit;
use crate::store::Store;
use crate::{
    ChangeError, ChangePlan, ChangeRecord, ChangeRequest, ChangeStoreInfo, Operation,
    OperationRegistry, Patch, RecoveryOutcome, RecoveryStatus,
};
use pos_vault::{NoteSource, Vault};
use serde_json::json;
use similar::TextDiff;
use std::ops::Range;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The caller has paused other editors and sync writers for the affected file.
pub struct ExclusiveWrite(());

impl ExclusiveWrite {
    pub fn confirmed() -> Self {
        Self(())
    }
}

pub struct ChangeEngine {
    pub(crate) vault: Vault,
    pub(crate) root: PathBuf,
    pub(crate) layout: Layout,
    operations: OperationRegistry,
}

struct Prepared {
    plan: ChangePlan,
    target: PathBuf,
    before: String,
    after: String,
}

impl ChangeEngine {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ChangeError> {
        Self::with_operations(root, OperationRegistry::default())
    }

    pub fn with_operations(
        root: impl AsRef<Path>,
        operations: OperationRegistry,
    ) -> Result<Self, ChangeError> {
        let vault = Vault::open(root)?;
        let root = vault.info().root;
        let layout = Layout::new(&root);
        Ok(Self {
            vault,
            root,
            layout,
            operations,
        })
    }

    pub fn initialize(&self) -> Result<ChangeStoreInfo, ChangeError> {
        self.layout.initialize()?;
        let _lock = self.layout.lock()?;
        let store = Store::initialize(&self.layout)?;
        Ok(ChangeStoreInfo {
            vault_id: store.vault_id,
            path: self.layout.database.clone(),
        })
    }

    pub fn list(&self) -> Result<Vec<ChangeRecord>, ChangeError> {
        Store::open(&self.layout, false)?.map_or_else(|| Ok(Vec::new()), |store| store.records())
    }

    pub fn show(&self, id: Uuid) -> Result<ChangeRecord, ChangeError> {
        self.read_store()?
            .record(id)?
            .ok_or(ChangeError::NotFound(id))
    }

    /// Materialize the last recorded version per note in a caller-selected history window.
    pub fn recorded_documents(
        &self,
        records: &[ChangeRecord],
        include: impl Fn(&str) -> bool,
    ) -> Result<Vec<(String, String)>, ChangeError> {
        let mut latest = std::collections::BTreeMap::<&str, &ChangeRecord>::new();
        for record in records {
            let slot = latest.entry(&record.plan.note_id).or_insert(record);
            if record.seq > slot.seq {
                *slot = record;
            }
        }
        if latest.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.read_store()?;
        latest
            .into_values()
            .filter(|record| record.plan.after_exists && include(&record.plan.request.path))
            .map(|record| {
                Ok((
                    record.plan.request.path.clone(),
                    store.blob(&record.plan.after_hash)?,
                ))
            })
            .collect()
    }

    pub fn diff(&self, id: Uuid) -> Result<String, ChangeError> {
        let store = self.read_store()?;
        let record = store.record(id)?.ok_or(ChangeError::NotFound(id))?;
        let before = store.blob(&record.plan.before_hash)?;
        let after = store.blob(&record.plan.after_hash)?;
        let path = &record.plan.request.path;
        Ok(TextDiff::from_lines(&before, &after)
            .unified_diff()
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string())
    }

    pub fn preview(&self, request: ChangeRequest) -> Result<ChangePlan, ChangeError> {
        let store = Store::open(&self.layout, false)?;
        self.prepare(request, store.as_ref())
            .map(|prepared| prepared.plan)
    }

    pub fn preview_revert(
        &self,
        id: Uuid,
        actor_id: String,
        device_id: Option<String>,
        reason: Option<String>,
    ) -> Result<ChangePlan, ChangeError> {
        let store = self.read_store()?;
        let original = store.record(id)?.ok_or(ChangeError::NotFound(id))?;
        let request = ChangeRequest {
            change_id: Uuid::new_v4(),
            path: if original.plan.note_id.starts_with("path:") {
                original.plan.request.path.clone()
            } else {
                self.vault
                    .find_note(&original.plan.note_id)?
                    .ok_or_else(|| conflict("the original note identity is missing"))?
            },
            actor_id,
            device_id,
            reason,
            operation: Operation {
                kind: "change.revert".into(),
                payload_version: 1,
                payload: json!(id),
            },
        };
        self.prepare(request, Some(&store))
            .map(|prepared| prepared.plan)
    }

    pub fn apply(
        &self,
        approved: &ChangePlan,
        exclusive: &ExclusiveWrite,
    ) -> Result<ChangeRecord, ChangeError> {
        self.apply_checked(approved, exclusive, |_, _| Ok(()))
    }

    pub fn apply_checked(
        &self,
        approved: &ChangePlan,
        _exclusive: &ExclusiveWrite,
        validate: impl FnOnce(&ChangePlan, &str) -> Result<(), ChangeError>,
    ) -> Result<ChangeRecord, ChangeError> {
        if approved.vault_root != self.root {
            return Err(conflict("the preview belongs to a different vault path"));
        }
        let (_lock, mut store) = self.write_store()?;
        if approved.vault_id != Some(store.vault_id) {
            return Err(conflict(
                "initialize the vault and generate a preview for this change store",
            ));
        }
        let id = approved.request.change_id;
        if let Some((state, existing)) = store.execution(id)? {
            if &existing != approved {
                return Err(conflict("this change ID belongs to a different preview"));
            }
            return match state.as_str() {
                "completed" => store.record(id)?.ok_or(ChangeError::NotFound(id)),
                "pending" => Err(ChangeError::Pending(id)),
                _ => Err(conflict(
                    "this execution was closed; generate a new preview",
                )),
            };
        }
        let Prepared {
            plan,
            target,
            before,
            after,
        } = self.prepare(approved.request.clone(), Some(&store))?;
        if &plan != approved {
            return Err(conflict(
                "the note or change history differs from the approved preview",
            ));
        }
        if let Some(pending) = store.pending_for(&plan.request.path)? {
            return Err(ChangeError::Pending(pending));
        }
        validate(&plan, &after)?;
        if plan.before_exists {
            self.record_unseen_preimage(&mut store, &plan)?;
        }
        store.prepare(&plan, before.as_bytes(), after.as_bytes())?;
        let mutation = match (plan.before_exists, plan.after_exists) {
            (true, true) => self
                .layout
                .replace(id, &target, &plan.before_hash, after.as_bytes()),
            (false, true) => self.layout.create(id, &target, after.as_bytes()),
            (true, false) => self.layout.delete(id, &target, &plan.before_hash),
            (false, false) => Err(invalid("change has no file to modify")),
        };
        mutation.map_err(|source| ChangeError::Interrupted {
            id,
            source: Box::new(source),
        })?;
        let file_key = self
            .result_file_key(&plan)
            .map_err(|source| ChangeError::Interrupted {
                id,
                source: Box::new(source),
            })?;
        let record = store.finish(&plan, file_key.as_deref()).map_err(|source| {
            ChangeError::Interrupted {
                id,
                source: Box::new(source),
            }
        })?;
        self.layout.cleanup(id);
        Ok(record)
    }

    pub fn recover(&self) -> Result<Vec<RecoveryOutcome>, ChangeError> {
        let Some(store) = Store::open(&self.layout, false)? else {
            return Ok(Vec::new());
        };
        store
            .pending()?
            .iter()
            .map(|plan| self.inspect_recovery(plan))
            .collect()
    }

    pub fn resolve(
        &self,
        id: Uuid,
        accept_written: bool,
        _exclusive: &ExclusiveWrite,
    ) -> Result<RecoveryOutcome, ChangeError> {
        let (_lock, mut store) = self.write_store()?;
        let (state, plan) = store.execution(id)?.ok_or(ChangeError::NotFound(id))?;
        let mut outcome = self.inspect_recovery(&plan)?;
        match state.as_str() {
            "completed" => outcome.status = RecoveryStatus::Recovered,
            "not_applied" => {
                outcome.status = if store.conflict_closed(id)? {
                    RecoveryStatus::ClosedConflict
                } else {
                    RecoveryStatus::NotApplied
                }
            }
            "pending" => match outcome.status {
                RecoveryStatus::NotApplied => {
                    store.mark_not_applied(id)?;
                    self.layout.cleanup(id);
                }
                RecoveryStatus::AwaitingConfirmation if accept_written => {
                    store.blob(&plan.before_hash)?;
                    store.blob(&plan.after_hash)?;
                    let file_key = self.result_file_key(&plan)?;
                    store.finish(&plan, file_key.as_deref())?;
                    self.layout.cleanup(id);
                    outcome.status = RecoveryStatus::Recovered;
                }
                _ => {}
            },
            _ => {
                return Err(ChangeError::Corrupt(format!(
                    "unknown execution state {state}"
                )));
            }
        }
        Ok(outcome)
    }

    fn result_file_key(&self, plan: &ChangePlan) -> Result<Option<String>, ChangeError> {
        plan.after_exists
            .then(|| self.vault.file_key(Path::new(&plan.request.path)))
            .transpose()
            .map_err(ChangeError::from)
    }

    pub fn keep_current(
        &self,
        id: Uuid,
        _exclusive: &ExclusiveWrite,
    ) -> Result<RecoveryOutcome, ChangeError> {
        let (_lock, mut store) = self.write_store()?;
        let (state, plan) = store.execution(id)?.ok_or(ChangeError::NotFound(id))?;
        let mut outcome = self.inspect_recovery(&plan)?;
        if state == "not_applied" && store.conflict_closed(id)? {
            outcome.status = RecoveryStatus::ClosedConflict;
            return Ok(outcome);
        }
        if state != "pending"
            || !matches!(
                outcome.status,
                RecoveryStatus::Conflict | RecoveryStatus::AwaitingConfirmation
            )
        {
            return Err(conflict(
                "keep-current requires a pending execution with uncertain results",
            ));
        }
        let bytes = if outcome.current_hash.is_some() {
            let path = self
                .vault
                .resolve_note_path(Path::new(&plan.request.path), false)?;
            Some(std::fs::read(path)?)
        } else {
            None
        };
        if bytes.as_deref().map(hash) != outcome.current_hash {
            return Err(conflict("the note changed while closing the conflict"));
        }
        store.close_conflict(
            id,
            bytes.as_deref(),
            outcome
                .detail
                .as_deref()
                .unwrap_or("unresolved replacement"),
        )?;
        outcome.status = RecoveryStatus::ClosedConflict;
        Ok(outcome)
    }

    fn inspect_recovery(&self, plan: &ChangePlan) -> Result<RecoveryOutcome, ChangeError> {
        let id = plan.request.change_id;
        let mut outcome = RecoveryOutcome {
            change_id: id,
            path: plan.request.path.clone(),
            status: RecoveryStatus::Conflict,
            current_hash: None,
            expected_before: plan.before_hash.clone(),
            expected_after: plan.after_hash.clone(),
            detail: None,
        };
        if plan.vault_root != self.root {
            outcome.detail =
                Some("the pending execution was prepared at a different vault path".into());
            return Ok(outcome);
        }
        let path = match self
            .vault
            .resolve_note_path(Path::new(&plan.request.path), true)
        {
            Ok(path) => path,
            Err(error) => {
                outcome.detail = Some(error.to_string());
                return Ok(outcome);
            }
        };
        let source = if regular(&path, false)? {
            match self.vault.read_source(&plan.request.path) {
                Ok(source) => Some(source),
                Err(error) => {
                    outcome.current_hash = Some(hash(&std::fs::read(path)?));
                    outcome.detail = Some(error.to_string());
                    return Ok(outcome);
                }
            }
        } else {
            None
        };
        let current = source.as_ref().map(|source| hash(source.text.as_bytes()));
        let matches =
            |exists: bool, expected: &str| current.as_deref() == exists.then_some(expected);
        let backup = self.layout.backup_hash(id)?;
        if backup
            .as_ref()
            .is_some_and(|digest| digest != &plan.before_hash)
        {
            outcome.detail =
                Some("staging contains a different preimage; preserve it for inspection".into());
        } else if source.as_ref().is_some_and(|source| {
            (plan.note_id.starts_with("yaml:") && source.metadata.id != plan.note_id)
                || source.metadata.path != plan.request.path
        }) {
            outcome.detail = Some("the note identity changed".into());
        } else if matches(plan.before_exists, &plan.before_hash) {
            outcome.status = RecoveryStatus::NotApplied;
        } else if matches(plan.after_exists, &plan.after_hash) {
            outcome.status = RecoveryStatus::AwaitingConfirmation;
            outcome.detail = Some(
                if backup.is_some() {
                    "staged preimage matches; confirm attribution before completing history"
                } else {
                    "expected bytes are present; replacement has no corroborating staged preimage"
                }
                .into(),
            );
        } else {
            outcome.detail = Some("the note differs from both recorded versions".into());
        }
        outcome.current_hash = current;
        Ok(outcome)
    }

    fn prepare(
        &self,
        mut request: ChangeRequest,
        store: Option<&Store>,
    ) -> Result<Prepared, ChangeError> {
        if request.actor_id.trim().is_empty() || request.change_id.is_nil() {
            return Err(invalid("an actor and a non-nil change ID are required"));
        }
        if request.operation.kind == "note.create" {
            return self.prepare_create(request, store);
        }
        let source = self.vault.read_source(&request.path)?;
        request.path = source.metadata.path.clone();
        if self.vault.find_note(&source.metadata.id)?.as_deref() != Some(request.path.as_str()) {
            return Err(conflict(
                "the note identity changed while preparing the operation",
            ));
        }
        let before_hash = hash(source.text.as_bytes());
        let (patch, depends_on, reverts, reversible, after_exists) =
            if request.operation.kind == "change.revert" {
                let store = store.ok_or(ChangeError::NotInitialized)?;
                let id = serde_json::from_value(request.operation.payload.clone())?;
                if request.operation.payload_version != 1 || id == request.change_id {
                    return Err(invalid("invalid revert payload"));
                }
                let original = store.record(id)?.ok_or(ChangeError::NotFound(id))?;
                let patch = revert_patch(store, &original, &source, &before_hash)?;
                (
                    patch,
                    vec![id],
                    Some(id),
                    false,
                    original.plan.before_exists,
                )
            } else {
                let edit = self.operations.prepare(&request.operation, &source)?;
                let patch = Patch::new(&source.text, edit.range.clone(), edit.replacement)?;
                let mut ranges = edit.reads;
                if ranges
                    .iter()
                    .any(|range| range.is_empty() || source.text.get(range.clone()).is_none())
                {
                    return Err(invalid("operation reported an invalid read range"));
                }
                ranges.push(edit.range);
                let depends_on = dependencies(store, &source, &before_hash, &ranges)?;
                (patch, depends_on, None, edit.reversible, true)
            };
        let edited = patch.apply(&source.text)?;
        if after_exists {
            source.validate_edit(&edited)?;
        }
        let plan = ChangePlan {
            vault_root: self.root.clone(),
            vault_id: store.map(|store| store.vault_id),
            request,
            note_id: source.metadata.id.clone(),
            patch,
            before_hash,
            after_hash: hash(edited.as_bytes()),
            before_exists: true,
            after_exists,
            depends_on,
            reverts,
            reversible,
        };
        Ok(Prepared {
            plan,
            target: source.absolute_path,
            before: source.text,
            after: edited,
        })
    }

    fn prepare_create(
        &self,
        mut request: ChangeRequest,
        store: Option<&Store>,
    ) -> Result<Prepared, ChangeError> {
        if request.operation.payload_version != 1 {
            return Err(invalid("unsupported note.create payload version"));
        }
        let text: String = serde_json::from_value(request.operation.payload.clone())?;
        let (target, metadata) = self
            .vault
            .validate_new_note(Path::new(&request.path), &text)?;
        if regular(&target, false)? {
            return Err(conflict("the target note already exists"));
        }
        if metadata.id.starts_with("yaml:") && self.vault.find_note(&metadata.id)?.is_some() {
            return Err(conflict("the new note's YAML ID already exists"));
        }
        request.path = metadata.path;
        let note_id = if metadata.id.starts_with("yaml:") {
            metadata.id
        } else {
            format!("note:{}", request.change_id)
        };
        let plan = ChangePlan {
            vault_root: self.root.clone(),
            vault_id: store.map(|store| store.vault_id),
            request,
            note_id,
            patch: Patch {
                start: 0,
                before: String::new(),
                after: text.clone(),
                prefix: String::new(),
                suffix: String::new(),
            },
            before_hash: hash(&[]),
            after_hash: hash(text.as_bytes()),
            before_exists: false,
            after_exists: true,
            depends_on: Vec::new(),
            reverts: None,
            reversible: true,
        };
        Ok(Prepared {
            plan,
            target,
            before: String::new(),
            after: text,
        })
    }

    pub(crate) fn read_store(&self) -> Result<Store, ChangeError> {
        Store::open(&self.layout, false)?.ok_or(ChangeError::NotInitialized)
    }

    pub(crate) fn write_store(&self) -> Result<(std::fs::File, Store), ChangeError> {
        if !self.layout.has_database()? {
            return Err(ChangeError::NotInitialized);
        }
        let lock = self.layout.lock()?;
        let store = Store::open(&self.layout, true)?.ok_or(ChangeError::NotInitialized)?;
        Ok((lock, store))
    }
}

fn revert_patch(
    store: &Store,
    original: &ChangeRecord,
    source: &NoteSource,
    before_hash: &str,
) -> Result<Patch, ChangeError> {
    let original_plan = &original.plan;
    if !original_plan.reversible || original_plan.reverts.is_some() {
        return Err(conflict("this change has no available inverse"));
    }
    if original_plan.note_id != source.metadata.id
        && original_plan.note_id.strip_prefix("path:") != Some(source.metadata.path.as_str())
    {
        return Err(conflict("the original note path or identity differs"));
    }
    store.check_revert(original_plan.request.change_id)?;
    let original_patch = &original_plan.patch;
    let range = project_effect(store, original, &source.metadata.id, before_hash)?;
    if !original_plan.before_exists {
        if range != (0..source.text.len()) || before_hash != original_plan.after_hash {
            return Err(conflict("the created note has since been edited"));
        }
        return Ok(Patch {
            start: 0,
            before: source.text.clone(),
            after: String::new(),
            prefix: String::new(),
            suffix: String::new(),
        });
    }
    let replacement = &original_patch.before;
    match original_plan.request.operation.kind.as_str() {
        "link.add" => validate_link_edit(&source.text, &range, replacement, false)?,
        "link.remove" => validate_link_edit(&source.text, &range, replacement, true)?,
        _ => {}
    }
    Patch::new(&source.text, range, replacement.clone())
}

fn project_effect(
    store: &Store,
    original: &ChangeRecord,
    note_id: &str,
    current_hash: &str,
) -> Result<Range<usize>, ChangeError> {
    let patch = &original.plan.patch;
    let mut range = patch.start..patch.start + patch.after.len();
    let mut expected = original.plan.after_hash.clone();
    let mut replaced = None;
    for later in store.history_after(original.seq, &original.plan.note_id, note_id)? {
        let plan = later.plan;
        if plan.before_hash != expected {
            return Err(conflict(
                "untracked edits prevent proving the original occurrence",
            ));
        }
        if plan.request.operation.kind == "external.observe" {
            if plan.before_exists && plan.after_exists && plan.before_hash == plan.after_hash {
                continue; // A recorded rename preserves the byte address.
            }
            return Err(conflict(
                "external edits prevent proving the original occurrence",
            ));
        }
        expected = plan.after_hash;
        if let Some((id, offset, length)) = replaced {
            if plan.reverts == Some(id) {
                let start = plan.patch.start + offset;
                range = start..start + length;
                replaced = None;
            }
            continue;
        }
        let start = plan.patch.start;
        let end = start + plan.patch.before.len();
        if end <= range.start {
            let new_start = range.start - plan.patch.before.len() + plan.patch.after.len();
            range = new_start..new_start + range.len();
        } else if start < range.end {
            if start > range.start || end < range.end {
                return Err(conflict(
                    "another change partially replaced the original occurrence",
                ));
            }
            replaced = Some((plan.request.change_id, range.start - start, range.len()));
        }
    }
    if replaced.is_some() || expected != current_hash {
        return Err(conflict(
            "the original occurrence cannot be proven in the current note",
        ));
    }
    Ok(range)
}

fn dependencies(
    store: Option<&Store>,
    source: &NoteSource,
    before_hash: &str,
    ranges: &[Range<usize>],
) -> Result<Vec<Uuid>, ChangeError> {
    let Some(store) = store else {
        return Ok(Vec::new());
    };
    let mut dependencies = Vec::new();
    for record in store.active_for(&source.metadata.path, &source.metadata.id)? {
        let prior = &record.plan;
        if prior.note_id != source.metadata.id
            && prior.note_id.strip_prefix("path:") != Some(&source.metadata.path)
        {
            continue;
        }
        let overlaps = |candidate: Range<usize>| {
            ranges
                .iter()
                .any(|range| range.start < candidate.end && candidate.start < range.end)
        };
        let patch = &prior.patch;
        let depends = !prior.before_exists
            || match project_effect(store, &record, &source.metadata.id, before_hash) {
                Ok(range) => overlaps(range),
                Err(ChangeError::Conflict(_)) => patch.matches(&source.text).any(overlaps),
                Err(error) => return Err(error),
            };
        if depends {
            dependencies.push(prior.request.change_id);
        }
    }
    Ok(dependencies)
}
