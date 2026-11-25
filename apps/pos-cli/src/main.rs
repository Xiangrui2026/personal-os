mod change;

use change::{BackupAction, ChangeAction, LinkAction};
use clap::{Parser, Subcommand};
use pos_vault::Vault;
use serde::Serialize;
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "Personal OS command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect an Obsidian vault in read-only mode.
    Vault {
        /// Path to the vault root.
        #[arg(long, value_name = "DIRECTORY")]
        root: PathBuf,
        #[command(subcommand)]
        action: VaultAction,
    },
    /// Preview, apply, or inspect versioned changes.
    Change {
        #[arg(long, value_name = "DIRECTORY")]
        root: PathBuf,
        #[command(subcommand)]
        action: ChangeAction,
    },
    /// Preview a single WikiLink edit.
    Link {
        #[arg(long, value_name = "DIRECTORY")]
        root: PathBuf,
        #[command(subcommand)]
        action: LinkAction,
    },
    /// Create, verify, or restore an independent Vault backup.
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
}

#[derive(Subcommand)]
enum VaultAction {
    /// Identify the vault and its Obsidian configuration.
    Info,
    /// Scan notes and return counts and scan issues.
    Scan {
        /// Emit a fresh JSON report after changes in the vault.
        #[arg(long)]
        watch: bool,
    },
    /// List metadata and links for indexed notes.
    List,
    /// Read a note using its vault-relative Markdown path.
    Read {
        path: PathBuf,
    },
    /// Report unreadable notes, duplicate IDs and unresolved links.
    Validate,
    /// Count indexed files, notes, tags and links.
    Stats,
    Backlinks {
        path: String,
    },
    Resolve {
        source: String,
        target: String,
    },
}

fn emit(value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Command::Vault { root, action } => run_vault(root, action),
        Command::Change { root, action } => change::run(root, action),
        Command::Link { root, action } => change::link(root, action),
        Command::Backup { action } => change::backup(action),
    }
}

fn run_vault(root: PathBuf, action: VaultAction) -> Result<(), Box<dyn Error>> {
    let vault = Vault::open(root)?;

    match action {
        VaultAction::Info => emit(&vault.info()),
        VaultAction::Read { path } => emit(&vault.read(path)?),
        VaultAction::Scan { watch: true } => {
            let watcher = vault.watch()?;
            let mut index = vault.scan()?;
            loop {
                emit(&index.report()?)?;
                index.refresh(&vault, &watcher.wait_for_changes()?)?;
            }
        }
        VaultAction::Scan { watch: false } => emit(&vault.scan()?.report()?),
        VaultAction::List => emit(&vault.scan()?.list()?),
        VaultAction::Validate => emit(&vault.scan()?.validate()?),
        VaultAction::Stats => emit(&vault.scan()?.stats()?),
        VaultAction::Backlinks { path } => emit(&vault.scan()?.backlinks(&path)?),
        VaultAction::Resolve { source, target } => {
            let (target, fragment) = target
                .split_once('#')
                .map_or((target.as_str(), None), |(name, fragment)| {
                    (name, Some(fragment.into()))
                });
            emit(&vault.scan()?.resolve(
                &source,
                &pos_vault::WikiLink {
                    target: target.into(),
                    fragment,
                    label: None,
                    embed: false,
                },
            )?)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pos: {error}");
            ExitCode::FAILURE
        }
    }
}
