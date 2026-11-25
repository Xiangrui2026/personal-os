use crate::filesystem::Layout;
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
    vault: Vault,
    root: PathBuf,
    layout: Layout,
    operations: OperationRegistry,
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
            .map(|(plan, _, _)| plan)
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
            path: original.plan.request.path,
            actor_id,
            device_id,
            reason,
            operation: Operation {
                kind: "change.revert".into(),
                payload_version: 1,
                payload: json!(id),
            },
        };
        self.prepare(request, Some(&store)).map(|(plan, _, _)| plan)
    }

    pub fn apply(
        &self,
        approved: &ChangePlan,
        _exclusive: &ExclusiveWrite,
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
        let (plan, source, edited) = self.prepare(approved.request.clone(), Some(&store))?;
        if &plan != approved {
            return Err(conflict(
                "the note or change history differs from the approved preview",
            ));
        }
        if let Some(pending) = store.pending_for(&plan.request.path)? {
            return Err(ChangeError::Pending(pending));
        }
        store.prepare(&plan, source.text.as_bytes(), edited.as_bytes())?;
        self.layout
            .replace(
                id,
                &source.absolute_path,
                &plan.before_hash,
                edited.as_bytes(),
            )
            .map_err(|source| ChangeError::Interrupted {
                id,
                source: Box::new(source),
            })?;
        let record = store
            .finish(&plan)
            .map_err(|source| ChangeError::Interrupted {
                id,
                source: Box::new(source),
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
            "not_applied" => outcome.status = RecoveryStatus::NotApplied,
            "pending" => match outcome.status {
                RecoveryStatus::NotApplied => {
                    store.mark_not_applied(id)?;
                    self.layout.cleanup(id);
                }
                RecoveryStatus::AwaitingConfirmation if accept_written => {
                    store.blob(&plan.before_hash)?;
                    store.blob(&plan.after_hash)?;
                    store.finish(&plan)?;
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
        let source = match self.vault.read_source(&plan.request.path) {
            Ok(source) => source,
            Err(error) => {
                outcome.detail = Some(error.to_string());
                return Ok(outcome);
            }
        };
        let current = hash(source.text.as_bytes());
        let backup = self.layout.backup_hash(id)?;
        if backup
            .as_ref()
            .is_some_and(|digest| digest != &plan.before_hash)
        {
            outcome.detail =
                Some("staging contains a different preimage; preserve it for inspection".into());
        } else if source.metadata.id != plan.note_id || source.metadata.path != plan.request.path {
            outcome.detail = Some("the note identity changed".into());
        } else if current == plan.before_hash {
            outcome.status = RecoveryStatus::NotApplied;
        } else if current == plan.after_hash {
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
        outcome.current_hash = Some(current);
        Ok(outcome)
    }

    fn prepare(
        &self,
        mut request: ChangeRequest,
        store: Option<&Store>,
    ) -> Result<(ChangePlan, NoteSource, String), ChangeError> {
        if request.actor_id.trim().is_empty() || request.change_id.is_nil() {
            return Err(invalid("an actor and a non-nil change ID are required"));
        }
        let source = self.vault.read_source(&request.path)?;
        request.path = source.metadata.path.clone();
        let before_hash = hash(source.text.as_bytes());
        let (patch, depends_on, reverts, reversible) = if request.operation.kind == "change.revert"
        {
            let store = store.ok_or(ChangeError::NotInitialized)?;
            let id = serde_json::from_value(request.operation.payload.clone())?;
            if request.operation.payload_version != 1 || id == request.change_id {
                return Err(invalid("invalid revert payload"));
            }
            let original = store.record(id)?.ok_or(ChangeError::NotFound(id))?.plan;
            let patch = revert_patch(store, &original, &source, &before_hash)?;
            (patch, vec![id], Some(id), false)
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
            (patch, depends_on, None, edit.reversible)
        };
        let edited = patch.apply(&source.text)?;
        source.validate_edit(&edited)?;
        let plan = ChangePlan {
            vault_root: self.root.clone(),
            vault_id: store.map(|store| store.vault_id),
            request,
            note_id: source.metadata.id.clone(),
            patch,
            before_hash,
            after_hash: hash(edited.as_bytes()),
            depends_on,
            reverts,
            reversible,
        };
        Ok((plan, source, edited))
    }

    fn read_store(&self) -> Result<Store, ChangeError> {
        Store::open(&self.layout, false)?.ok_or(ChangeError::NotInitialized)
    }

    fn write_store(&self) -> Result<(std::fs::File, Store), ChangeError> {
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
    original: &ChangePlan,
    source: &NoteSource,
    before_hash: &str,
) -> Result<Patch, ChangeError> {
    if !original.reversible || original.reverts.is_some() {
        return Err(conflict("this change has no available inverse"));
    }
    if original.note_id != source.metadata.id || original.request.path != source.metadata.path {
        return Err(conflict("the original note path or identity differs"));
    }
    store.check_revert(original.request.change_id)?;
    let original_patch = &original.patch;
    let range = if before_hash == original.after_hash {
        original_patch.start..original_patch.start + original_patch.after.len()
    } else {
        let matches = original_patch.locations(&source.text);
        let [range] = matches.as_slice() else {
            return Err(conflict(
                "the original effect is absent or cannot be uniquely relocated",
            ));
        };
        range.clone()
    };
    let replacement = &original_patch.before;
    match original.request.operation.kind.as_str() {
        "link.add" => validate_link_edit(&source.text, &range, replacement, false)?,
        "link.remove" => validate_link_edit(&source.text, &range, replacement, true)?,
        _ => {}
    }
    Patch::new(&source.text, range, replacement.clone())
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
    for prior in store.active_for(&source.metadata.path)? {
        if prior.note_id != source.metadata.id {
            continue;
        }
        let overlaps = |candidate: Range<usize>| {
            ranges
                .iter()
                .any(|range| range.start < candidate.end && candidate.start < range.end)
        };
        let patch = &prior.patch;
        let depends = if prior.after_hash == before_hash {
            overlaps(patch.start..patch.start + patch.after.len())
        } else {
            let locations = patch.locations(&source.text);
            if locations.is_empty() {
                patch.matches(&source.text).any(overlaps)
            } else {
                locations.into_iter().any(overlaps)
            }
        };
        if depends {
            dependencies.push(prior.request.change_id);
        }
    }
    Ok(dependencies)
}
