use crate::emit;
use clap::Subcommand;
use pos_change::{ChangePlan, ExclusiveWrite};
use pos_productivity::{Action, DEFAULT_ZONE, Date, Productivity, Timestamp};
use std::{
    error::Error,
    fs,
    io::{self, Read},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Subcommand)]
pub(crate) enum ProductivityAction {
    /// List managed entities, optionally by document type.
    List {
        #[arg(long)]
        kind: Option<String>,
    },
    Show {
        id: Uuid,
    },
    Roadmap,
    /// Show a daily projection. The default date is today in the selected zone.
    Day {
        /// Use today's documents, including entries backfilled after this date.
        #[arg(long)]
        latest: bool,
        #[arg(long)]
        date: Option<Date>,
        #[arg(long, default_value = DEFAULT_ZONE)]
        timezone: String,
    },
    /// Preview a business operation from a JSON file or standard input.
    Preview {
        request: PathBuf,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Apply the reviewed preview through Change.
    Apply {
        preview: PathBuf,
        #[arg(long)]
        exclusive: bool,
    },
}

pub(crate) fn run(root: PathBuf, action: ProductivityAction) -> Result<(), Box<dyn Error>> {
    let engine = Productivity::open(root)?;
    match action {
        ProductivityAction::List { kind } => {
            let catalog = engine.catalog()?;
            emit(
                &catalog
                    .entities()
                    .filter(|entity| {
                        kind.as_deref()
                            .is_none_or(|kind| entity.item.fields.kind() == kind)
                    })
                    .collect::<Vec<_>>(),
            )
        }
        ProductivityAction::Show { id } => emit(engine.catalog()?.get(id)?),
        ProductivityAction::Roadmap => emit(&engine.catalog()?.roadmap()),
        ProductivityAction::Day {
            date,
            timezone,
            latest,
        } => {
            let now = Timestamp::now();
            let date = match date {
                Some(date) => date,
                None => Productivity::today(&timezone, now)?,
            };
            emit(&if latest {
                engine.day_latest(date, &timezone, now)?
            } else {
                engine.day(date, &timezone, now)?
            })
        }
        ProductivityAction::Preview {
            request,
            actor,
            reason,
        } => {
            let request = read_json::<Action>(request)?;
            emit(&engine.preview(request, &actor, reason, Timestamp::now())?)
        }
        ProductivityAction::Apply { preview, exclusive } => {
            if !exclusive {
                return Err(
                    "confirm other writers are paused for the affected note with --exclusive"
                        .into(),
                );
            }
            let plan = read_json::<ChangePlan>(preview)?;
            emit(&engine.apply(&plan, &ExclusiveWrite::confirmed())?)
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: PathBuf) -> Result<T, Box<dyn Error>> {
    let text = if path.as_os_str() == "-" {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        text
    } else {
        fs::read_to_string(path)?
    };
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
}
