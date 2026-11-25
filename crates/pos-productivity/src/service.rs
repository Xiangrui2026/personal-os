use crate::board::DayBoard;
use crate::catalog::{Catalog, from_note};
use crate::model::*;
use crate::{Date, Timestamp};
use jiff::tz::TimeZone;
use pos_change::{
    ChangeEngine, ChangeError, ChangePlan, ChangeRecord, ChangeRequest, ExclusiveWrite, Operation,
};
use pos_vault::{Vault, parse_document};
use std::path::Path;
use uuid::Uuid;

pub struct Productivity {
    vault: Vault,
    changes: ChangeEngine,
}

impl Productivity {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProductivityError> {
        Ok(Self {
            vault: Vault::open(root.as_ref())?,
            changes: ChangeEngine::open(root)?,
        })
    }

    pub fn catalog(&self) -> Result<Catalog, ProductivityError> {
        Catalog::load(&self.vault)
    }

    pub fn today(zone: &str, at: Timestamp) -> Result<Date, ProductivityError> {
        Ok(at.to_zoned(TimeZone::get(zone)?).date())
    }

    pub fn wall_time(time: &str, timezone: &str) -> Result<Timestamp, ProductivityError> {
        let civil: jiff::civil::DateTime = time.parse()?;
        Ok(TimeZone::get(timezone)?
            .to_ambiguous_zoned(civil)
            .unambiguous()?
            .timestamp())
    }

    pub fn day(
        &self,
        date: Date,
        timezone: &str,
        now: Timestamp,
    ) -> Result<DayBoard, ProductivityError> {
        let catalog = self.catalog()?;
        let mut board = catalog.day(date, timezone, now)?;
        crate::history::attach(&catalog, &mut board, self.changes.list()?);
        let journal = format!("Journal/{date}.md");
        if self.vault.read(&journal).is_ok() {
            board.journal_path = Some(journal);
        }
        Ok(board)
    }

    pub fn preview(
        &self,
        action: Action,
        actor: &str,
        reason: Option<String>,
        at: Timestamp,
    ) -> Result<ChangePlan, ProductivityError> {
        let catalog = self.catalog()?;
        let change_id = Uuid::new_v4();
        let (entity, body) = propose(&catalog, action, change_id, at)?;
        catalog.validate(&entity, body.is_some())?;
        let kind = entity.item.fields.kind();
        let document = Document {
            version: SCHEMA,
            item: entity.item,
        };
        let operation = if let Some(body) = body {
            let data = serde_json::to_string(&document)?;
            let markdown = format!(
                "---\nid: {}\ntype: {kind}\n{PROPERTY}: {data}\n---\n{body}",
                entity.id
            );
            Operation::create_note(&markdown)
        } else {
            Operation::set_property(PROPERTY, serde_json::to_value(document)?)
        };
        Ok(self.changes.preview(ChangeRequest {
            change_id,
            path: entity.path,
            actor_id: actor.into(),
            device_id: None,
            reason,
            operation,
        })?)
    }

    pub fn apply(
        &self,
        plan: &ChangePlan,
        access: &ExclusiveWrite,
    ) -> Result<ChangeRecord, ProductivityError> {
        let operation = &plan.request.operation;
        match operation.kind.as_str() {
            "note.create" => {}
            "property.set"
                if operation.payload.get("key").and_then(|key| key.as_str()) == Some(PROPERTY) => {}
            _ => return Err(invalid("expected a productivity preview")),
        }
        Ok(self.changes.apply_checked(plan, access, |plan, text| {
            self.validate_proposed(plan, text)
                .map_err(|error| ChangeError::Invalid(error.to_string()))
        })?)
    }

    fn validate_proposed(&self, plan: &ChangePlan, text: &str) -> Result<(), ProductivityError> {
        let note = parse_document(&plan.request.path, text)?;
        let entity =
            from_note(&note)?.ok_or_else(|| invalid("the preview has no productivity record"))?;
        if !plan.before_exists
            && (entity.id != plan.request.change_id
                || entity.path != entity.item.fields.path(entity.id))
        {
            return Err(invalid(
                "new productivity records must use the approved ID and path",
            ));
        }
        let catalog = self.catalog()?;
        if plan.before_exists {
            catalog.get(entity.id)?;
        }
        catalog.validate(&entity, !plan.before_exists)
    }
}

fn propose(
    catalog: &Catalog,
    action: Action,
    id: Uuid,
    at: Timestamp,
) -> Result<(Entity, Option<String>), ProductivityError> {
    match action {
        Action::Create { item, body } => Ok(new_entity(id, item, body)),
        Action::Edit { id, item } => {
            let mut entity = catalog.get(id)?.clone();
            entity.item = item;
            Ok((entity, None))
        }
        Action::SetTaskStatus { id, status } => {
            let mut entity = catalog.require(id, "task")?.clone();
            if let Fields::Task {
                status: current, ..
            } = &mut entity.item.fields
            {
                *current = status;
            }
            Ok((entity, None))
        }
        Action::SetCheckpoint { id, title, done } => {
            let mut entity = catalog.require(id, "task")?.clone();
            if let Fields::Task { checkpoints, .. } = &mut entity.item.fields {
                let checkpoint = checkpoints
                    .iter_mut()
                    .find(|item| item.title == title)
                    .ok_or_else(|| invalid("checkpoint no longer exists"))?;
                checkpoint.done = done;
            }
            Ok((entity, None))
        }
        Action::ScheduleTask {
            date,
            task,
            priority,
        } => {
            catalog.require(task, "task")?;
            let (mut entity, body) = daily_plan(catalog, id, date)?;
            if let Fields::DailyPlan {
                tasks,
                priority_tasks,
                closing,
                ..
            } = &mut entity.item.fields
            {
                if closing.is_some() {
                    return Err(invalid("the daily plan is closed"));
                }
                if !tasks.contains(&task) {
                    tasks.push(task);
                }
                priority_tasks.retain(|id| *id != task);
                if priority {
                    priority_tasks.push(task);
                }
            }
            Ok((entity, body))
        }
        Action::CloseDay { date, decisions } => {
            let (mut entity, body) = daily_plan(catalog, id, date)?;
            if let Fields::DailyPlan { closing, .. } = &mut entity.item.fields {
                if closing.is_some() {
                    return Err(invalid("the daily plan is already closed"));
                }
                *closing = Some(decisions);
            }
            Ok((entity, body))
        }
        Action::FocusStart { task } => {
            let task_doc = catalog.require(task, "task")?;
            let item = Item {
                title: task_doc.item.title.clone(),
                fields: Fields::TimeEntry {
                    task,
                    intervals: Vec::new(),
                    status: EntryStatus::Running,
                    running_since: Some(at),
                },
            };
            Ok(new_entity(id, item, String::new()))
        }
        action @ (Action::FocusPause | Action::FocusResume | Action::FocusStop) => {
            focus_transition(catalog, action, at)
        }
    }
}

fn new_entity(id: Uuid, item: Item, body: String) -> (Entity, Option<String>) {
    (
        Entity {
            id,
            path: item.fields.path(id),
            item,
        },
        Some(body),
    )
}

fn daily_plan(
    catalog: &Catalog,
    id: Uuid,
    date: Date,
) -> Result<(Entity, Option<String>), ProductivityError> {
    if let Some(plan) = catalog.plan(date)? {
        return Ok((plan.clone(), None));
    }
    Ok(new_entity(
        id,
        Item {
            title: format!("日计划 {date}"),
            fields: Fields::DailyPlan {
                date,
                tasks: Vec::new(),
                priority_tasks: Vec::new(),
                focus_goals: Vec::new(),
                budget_minutes: None,
                buffer_minutes: 0,
                work_windows: None,
                strategic_minutes: None,
                importance_threshold: None,
                urgency_percent: None,
                closing: None,
            },
        },
        String::new(),
    ))
}

fn focus_transition(
    catalog: &Catalog,
    action: Action,
    at: Timestamp,
) -> Result<(Entity, Option<String>), ProductivityError> {
    let mut entry = catalog
        .focus()?
        .ok_or_else(|| invalid("there is no open focus session"))?
        .clone();
    let Fields::TimeEntry {
        intervals,
        status,
        running_since,
        ..
    } = &mut entry.item.fields
    else {
        return Err(invalid("focus is not a time entry"));
    };
    if matches!(action, Action::FocusResume) && *status == EntryStatus::Paused {
        *status = EntryStatus::Running;
        *running_since = Some(at);
        return Ok((entry, None));
    }
    if matches!(action, Action::FocusStop)
        || (matches!(action, Action::FocusPause) && *status == EntryStatus::Running)
    {
        if let Some(start) = running_since.take() {
            if at < start {
                return Err(invalid("focus end is before its start"));
            }
            if at > start {
                intervals.push(Interval { start, end: at });
            }
        }
        *status = if matches!(action, Action::FocusStop) {
            EntryStatus::Finished
        } else {
            EntryStatus::Paused
        };
        return Ok((entry, None));
    }
    Err(invalid("invalid focus transition"))
}
