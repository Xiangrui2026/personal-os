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
    pub fn agenda_draft(
        &self,
        action: Action,
        timezone: &str,
        at: Timestamp,
    ) -> Result<Vec<crate::Priority>, ProductivityError> {
        let Action::SaveAgenda { date, .. } = action else {
            return Err(invalid("需要日计划草稿"));
        };
        let mut catalog = self.catalog()?;
        let (entity, body) = propose(&catalog, action, Uuid::new_v4(), at)?;
        catalog.validate(&entity, body.is_some())?;
        catalog.overlay_plan(entity);
        Ok(catalog.agenda(date, timezone, at)?.priorities)
    }
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
        let end = date
            .tomorrow()?
            .to_zoned(TimeZone::get(timezone)?)?
            .timestamp();
        if now >= end {
            return self.day_as_of(date, timezone, end);
        }
        self.day_latest(date, timezone, now)
    }

    pub fn day_as_of(
        &self,
        date: Date,
        timezone: &str,
        cutoff: Timestamp,
    ) -> Result<DayBoard, ProductivityError> {
        let mut records = self.changes.list()?;
        records.retain(|record| record.executed_unix_ms < cutoff.as_millisecond());
        let mut items = std::collections::BTreeMap::new();
        for (path, text) in self
            .changes
            .recorded_documents(&records, crate::catalog::managed_path)?
        {
            if let Some(entity) = from_note(&parse_document(&path, &text)?)? {
                items.insert(entity.id, entity);
            }
        }
        let catalog = Catalog::from_items(items)?;
        let mut board = catalog.project_day(date, timezone, cutoff, true)?;
        board.snapshot_entities = Some(catalog.entities().cloned().collect());
        crate::history::attach(&catalog, &mut board, records);
        Ok(board)
    }

    pub fn day_latest(
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
        let (mut entity, body) = propose(&catalog, action, change_id, at)?;
        reconcile_task(catalog.get(entity.id).ok(), &mut entity.item.fields, at)?;
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
        let plan = self.changes.preview(ChangeRequest {
            change_id,
            path: entity.path,
            actor_id: actor.into(),
            device_id: None,
            reason,
            operation,
        })?;
        if plan.before_exists {
            let previous = parse_document(
                &plan.request.path,
                &format!("---\n{}\n---\n", plan.patch.before.trim_end()),
            )?;
            let value = previous
                .frontmatter
                .get(PROPERTY)
                .ok_or_else(|| invalid("任务数据已变化，请刷新重试"))?;
            let doc: Document = serde_json::from_value(value.clone())?;
            if doc.item != catalog.get(entity.id)?.item {
                return Err(invalid("生成预览期间数据已变化，请刷新重试"));
            }
        }
        Ok(plan)
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
        Action::SaveAgenda {
            date,
            base,
            tasks: chosen,
            slots,
            routines,
            budget_minutes: budget,
            work_windows: windows,
            buffer_minutes: buffer,
        } => {
            if catalog.plan(date)?.map(|plan| &plan.item) != base.as_ref() {
                return Err(invalid("当天计划已更新，请重新载入后合并选择"));
            }
            let (mut entity, body) = daily_plan(catalog, id, date)?;
            let prior = catalog.planned_tasks(date, Some(&entity));
            let current = catalog.routines(date, catalog.plan(date)?);
            if current.keys().ne(routines.keys())
                || current.iter().any(|(id, old)| {
                    let draft = &routines[id];
                    old.title != draft.title || old.minutes != draft.minutes
                })
            {
                return Err(invalid("每日固定任务规则已更新，请重新载入"));
            }
            if let Fields::DailyPlan {
                layout,
                tasks,
                priority_tasks,
                budget_minutes,
                work_windows,
                buffer_minutes,
                closing,
                ..
            } = &mut entity.item.fields
            {
                if closing.is_some() {
                    return Err(invalid("当天计划已结案"));
                }
                layout
                    .omitted
                    .extend(prior.into_iter().filter(|id| !chosen.contains(id)));
                layout.omitted.retain(|id| !chosen.contains(id));
                layout.omitted.sort();
                layout.omitted.dedup();
                layout.slots = slots;
                layout.routines = routines;
                priority_tasks.retain(|id| chosen.contains(id));
                *tasks = chosen;
                *budget_minutes = budget;
                *work_windows = windows;
                *buffer_minutes = buffer;
            }
            Ok((entity, body))
        }
        Action::SetRoutineStatus { date, rule, status } => {
            let (mut entity, body) = daily_plan(catalog, id, date)?;
            let mut occurrences = catalog.routines(date, Some(&entity));
            occurrences
                .get_mut(&rule)
                .ok_or_else(|| invalid("当天没有此固定任务"))?
                .status = status;
            if let Fields::DailyPlan {
                layout, closing, ..
            } = &mut entity.item.fields
            {
                if closing.is_some() {
                    return Err(invalid("当天计划已结案"));
                }
                layout.routines = occurrences;
            }
            Ok((entity, body))
        }
        Action::ReestimateRemaining { id, minutes } => {
            if minutes == 0 || !catalog.task_open(id) {
                return Err(invalid("请为未完成任务填写正数分钟"));
            }
            let spent = crate::board::union_seconds(catalog.entities().filter(|item|
                matches!(item.item.fields, Fields::TimeEntry { task, .. } if task == id))
                .flat_map(|item| crate::board::entry_intervals(item, at)));
            let total = u32::try_from((spent + 59) / 60)
                .ok()
                .and_then(|spent| spent.checked_add(minutes))
                .ok_or_else(|| invalid("估时超出可记录范围"))?;
            let mut entity = catalog.require(id, "task")?.clone();
            if let Fields::Task {
                estimate_minutes, ..
            } = &mut entity.item.fields
            {
                *estimate_minutes = Some(total);
            }
            Ok((entity, None))
        }
        Action::Create { item, body } => Ok(new_entity(id, item, body)),
        Action::Edit { id, mut item } => {
            let mut entity = catalog.get(id)?.clone();
            if let (
                Fields::DailyPlan { layout: old, .. },
                Fields::DailyPlan { layout, tasks, .. },
            ) = (&entity.item.fields, &mut item.fields)
            {
                layout.clone_from(old);
                layout.slots.retain(|id, _| tasks.contains(id));
                layout.omitted.retain(|id| !tasks.contains(id));
            }
            if let (
                Fields::Task {
                    focus_sessions: saved,
                    ..
                },
                Fields::Task { focus_sessions, .. },
            ) = (&entity.item.fields, &mut item.fields)
            {
                focus_sessions.clone_from(saved);
            }
            if let Fields::TimeEntry { task, .. } = entity.item.fields
                && entity.path == catalog.get(task)?.path
            {
                if item.title != entity.item.title {
                    return Err(invalid("专注记录沿用任务标题"));
                }
                if !matches!(item.fields, Fields::TimeEntry { task: owner, .. } if owner == task) {
                    return Err(invalid("专注记录必须保留原任务"));
                }
                entity.item = item;
                return save_session(catalog, entity);
            }
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
            if let Fields::Task {
                checkpoints,
                status,
                ..
            } = &mut entity.item.fields
            {
                if *status == WorkStatus::Cancelled {
                    return Err(invalid("请先恢复已取消的任务"));
                }
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
                layout,
                closing,
                ..
            } = &mut entity.item.fields
            {
                if closing.is_some() {
                    return Err(invalid("the daily plan is closed"));
                }
                layout.omitted.retain(|id| *id != task);
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
            let occurrences = catalog.routines(date, Some(&entity));
            if let Fields::DailyPlan {
                layout, closing, ..
            } = &mut entity.item.fields
            {
                if closing.is_some() {
                    return Err(invalid("the daily plan is already closed"));
                }
                *closing = Some(decisions);
                layout.routines = occurrences;
            }
            Ok((entity, body))
        }
        Action::FocusStart { task } => {
            let mut entity = catalog.require(task, "task")?.clone();
            if let Fields::Task {
                status,
                focus_sessions,
                ..
            } = &mut entity.item.fields
            {
                if !status.is_open() {
                    return Err(invalid("只有未完成的任务可以开始专注"));
                }
                *status = WorkStatus::Doing;
                focus_sessions.push(FocusSession {
                    id,
                    intervals: Vec::new(),
                    status: EntryStatus::Running,
                    running_since: Some(at),
                });
            }
            Ok((entity, None))
        }
        action @ (Action::FocusPause | Action::FocusResume | Action::FocusStop) => {
            focus_transition(catalog, action, at)
        }
    }
}

fn reconcile_task(
    previous: Option<&Entity>,
    fields: &mut Fields,
    at: Timestamp,
) -> Result<(), ProductivityError> {
    let Fields::Task {
        status,
        checkpoints,
        focus_sessions,
        ..
    } = fields
    else {
        return Ok(());
    };
    let old = previous.and_then(|entity| match &entity.item.fields {
        Fields::Task {
            checkpoints,
            status,
            ..
        } => Some((checkpoints, *status)),
        _ => None,
    });
    if *status != WorkStatus::Cancelled && !checkpoints.is_empty() {
        if checkpoints.iter().all(|point| point.done) {
            *status = WorkStatus::Done;
        } else if old
            .is_some_and(|(points, prior)| points != checkpoints && prior == WorkStatus::Done)
            || (*status == WorkStatus::Todo
                && checkpoints.iter().any(|point| point.done)
                && old.is_none_or(|(points, _)| points != checkpoints))
        {
            *status = WorkStatus::Doing;
        }
    }
    if !status.is_open() {
        for session in focus_sessions
            .iter_mut()
            .filter(|session| session.status != EntryStatus::Finished)
        {
            finish_interval(&mut session.intervals, &mut session.running_since, at)?;
            session.status = EntryStatus::Finished;
        }
    }
    Ok(())
}

fn finish_interval(
    intervals: &mut Vec<Interval>,
    running: &mut Option<Timestamp>,
    at: Timestamp,
) -> Result<(), ProductivityError> {
    if let Some(start) = running.take() {
        if at < start {
            return Err(invalid("focus end is before its start"));
        }
        if at > start {
            intervals.push(Interval { start, end: at });
        }
    }
    Ok(())
}

fn save_session(
    catalog: &Catalog,
    entry: Entity,
) -> Result<(Entity, Option<String>), ProductivityError> {
    let Fields::TimeEntry {
        task,
        status,
        running_since,
        ref intervals,
    } = entry.item.fields
    else {
        return Err(invalid("expected time entry"));
    };
    let mut owner = catalog.require(task, "task")?.clone();
    if entry.path != owner.path {
        return Ok((entry, None));
    }
    if let Fields::Task { focus_sessions, .. } = &mut owner.item.fields {
        let session = focus_sessions
            .iter_mut()
            .find(|session| session.id == entry.id)
            .ok_or_else(|| invalid("专注记录已变更"))?;
        session.status = status;
        session.running_since = running_since;
        session.intervals.clone_from(intervals);
    }
    Ok((owner, None))
}

fn new_entity(id: Uuid, item: Item, body: String) -> (Entity, Option<String>) {
    (
        Entity {
            progress: None,
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
                layout: PlanLayout::default(),
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
        return save_session(catalog, entry);
    }
    if matches!(action, Action::FocusStop)
        || (matches!(action, Action::FocusPause) && *status == EntryStatus::Running)
    {
        finish_interval(intervals, running_since, at)?;
        *status = if matches!(action, Action::FocusStop) {
            EntryStatus::Finished
        } else {
            EntryStatus::Paused
        };
        return save_session(catalog, entry);
    }
    Err(invalid("invalid focus transition"))
}
