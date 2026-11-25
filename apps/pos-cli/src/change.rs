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
        /// Confirm that other editors and sync writers are paused for the file.
        #[arg(long, requires = "resolve")]
        exclusive: bool,
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
            exclusive,
        } => {
            let exclusive = write_access(exclusive)?;
            emit(&engine.resolve(id, accept_written, &exclusive)?)
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
    emit(&engine.preview(ChangeRequest {
        change_id: Uuid::new_v4(),
        path,
        actor_id: actor.actor,
        device_id: actor.device,
        reason: actor.reason,
        operation,
    })?)
}

fn write_access(exclusive: bool) -> Result<ExclusiveWrite, Box<dyn Error>> {
    if exclusive {
        Ok(ExclusiveWrite::confirmed())
    } else {
        Err("confirm exclusive access to the note with --exclusive".into())
    }
}
