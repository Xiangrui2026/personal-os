use crate::emit;
use clap::{Args, Subcommand};
use pos_change::{ChangeEngine, ChangePlan, ChangeRequest, ExclusiveWrite, Operation};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Args)]
pub(crate) struct Actor {
    #[arg(long)]
    actor: String,
    #[arg(long)]
    device: Option<String>,
    #[arg(long)]
    reason: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum ChangeAction {
    /// Create or open the deliberate, local .maggot change store.
    Init,
    /// List completed changes, newest first.
    List,
    /// Show a completed change and its effect.
    Show { id: Uuid },
    /// Show the original stored difference.
    Diff { id: Uuid },
    /// Preview a registered operation using its versioned JSON payload.
    Preview {
        path: String,
        kind: String,
        #[arg(long, default_value_t = 1)]
        version: u32,
        #[arg(long)]
        payload: String,
        #[command(flatten)]
        actor: Actor,
    },
    /// Record external Vault changes against a stored baseline.
    Observe {
        #[arg(long)]
        actor: String,
        #[arg(long)]
        watch: bool,
    },
    /// Preview the conditional inverse of a completed change.
    #[command(visible_alias = "revert")]
    Undo {
        id: Uuid,
        #[command(flatten)]
        actor: Actor,
    },
    /// Apply the reviewed JSON preview exported by a link or undo command.
    Apply {
        preview: PathBuf,
        /// Confirm that other editors and sync writers are paused for the file.
        #[arg(long)]
        exclusive: bool,
    },
    /// Inspect pending executions or settle one after reviewing its evidence.
    Recover {
        #[arg(long)]
        resolve: Option<Uuid>,
        /// Confirm attribution of matching post-replacement bytes.
        #[arg(long, requires = "resolve")]
        accept_written: bool,
        /// Close a conflict while retaining the current note and recovery evidence.
        #[arg(long, requires = "resolve", conflicts_with = "accept_written")]
        keep_current: bool,
        /// Confirm that other editors and sync writers are paused for the file.
        #[arg(long, requires = "resolve")]
        exclusive: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum BackupAction {
    /// Capture a Vault and its Change history into a new directory.
    Create {
        #[arg(long)]
        root: PathBuf,
        destination: PathBuf,
        #[arg(long)]
        exclusive: bool,
    },
    /// Verify bytes, SQLite history, and content objects in an archive.
    Verify { snapshot: PathBuf },
    /// Restore a verified archive into a new directory.
    Restore {
        snapshot: PathBuf,
        destination: PathBuf,
    },
}

#[derive(Subcommand)]
pub(crate) enum LinkAction {
    /// Preview turning one occurrence of plain text into a WikiLink.
    Add {
        path: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        offset: Option<usize>,
        #[command(flatten)]
        actor: Actor,
    },
    /// Preview replacing one exact WikiLink with its visible text.
    Remove {
        path: String,
        #[arg(long)]
        link: String,
        #[arg(long)]
        offset: Option<usize>,
        #[command(flatten)]
        actor: Actor,
    },
}

pub(crate) fn run(root: PathBuf, action: ChangeAction) -> Result<(), Box<dyn Error>> {
    let engine = ChangeEngine::open(root)?;
    match action {
        ChangeAction::Init => emit(&engine.initialize()?),
        ChangeAction::List => emit(&engine.list()?),
        ChangeAction::Show { id } => emit(&engine.show(id)?),
        ChangeAction::Diff { id } => emit(&engine.diff(id)?),
        ChangeAction::Preview {
            path,
            kind,
            version,
            payload,
            actor,
        } => {
            let operation = Operation {
                kind,
                payload_version: version,
                payload: serde_json::from_str(&payload)?,
            };
            preview(&engine, path, operation, actor)
        }
        ChangeAction::Observe {
            actor,
            watch: false,
        } => emit(&engine.observe(&actor)?),
        ChangeAction::Observe { actor, watch: true } => {
            let observer = engine.watch_vault()?;
            loop {
                emit(&engine.observe(&actor)?)?;
                observer.wait_for_change()?;
            }
        }
        ChangeAction::Undo { id, actor } => {
            emit(&engine.preview_revert(id, actor.actor, actor.device, actor.reason)?)
        }
        ChangeAction::Apply { preview, exclusive } => {
            let exclusive = write_access(exclusive)?;
            let content = fs::read_to_string(preview)?;
            let plan: ChangePlan = serde_json::from_str(content.trim_start_matches('\u{feff}'))?;
            emit(&engine.apply(&plan, &exclusive)?)
        }
        ChangeAction::Recover { resolve: None, .. } => emit(&engine.recover()?),
        ChangeAction::Recover {
            resolve: Some(id),
            accept_written,
            keep_current,
            exclusive,
        } => {
            let exclusive = write_access(exclusive)?;
            if keep_current {
                emit(&engine.keep_current(id, &exclusive)?)
            } else {
                emit(&engine.resolve(id, accept_written, &exclusive)?)
            }
        }
    }
}

pub(crate) fn link(root: PathBuf, action: LinkAction) -> Result<(), Box<dyn Error>> {
    let engine = ChangeEngine::open(root)?;
    let (path, operation, actor) = match action {
        LinkAction::Add {
            path,
            text,
            target,
            offset,
            actor,
        } => (path, Operation::link_add(&text, &target, offset), actor),
        LinkAction::Remove {
            path,
            link,
            offset,
            actor,
        } => (path, Operation::link_remove(&link, offset), actor),
    };
    preview(&engine, path, operation, actor)
}

fn preview(
    engine: &ChangeEngine,
    path: String,
    operation: Operation,
    actor: Actor,
) -> Result<(), Box<dyn Error>> {
    emit(&engine.preview(ChangeRequest {
        change_id: Uuid::new_v4(),
        path,
        actor_id: actor.actor,
        device_id: actor.device,
        reason: actor.reason,
        operation,
    })?)
}

pub(crate) fn backup(action: BackupAction) -> Result<(), Box<dyn Error>> {
    match action {
        BackupAction::Create {
            root,
            destination,
            exclusive,
        } => {
            let exclusive = write_access(exclusive)?;
            emit(&pos_change::create_backup(&root, &destination, &exclusive)?)
        }
        BackupAction::Verify { snapshot } => emit(&pos_change::verify_backup(&snapshot)?),
        BackupAction::Restore {
            snapshot,
            destination,
        } => emit(&pos_change::restore_backup(&snapshot, &destination)?),
    }
}

fn write_access(exclusive: bool) -> Result<ExclusiveWrite, Box<dyn Error>> {
    if exclusive {
        Ok(ExclusiveWrite::confirmed())
    } else {
        Err(
            "confirm other writers are paused for the affected note or Vault with --exclusive"
                .into(),
        )
    }
}
