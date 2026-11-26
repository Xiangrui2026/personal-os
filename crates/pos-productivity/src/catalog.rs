use crate::Date;
use crate::model::*;
use pos_vault::{Note, Vault};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Default)]
pub struct Catalog {
    items: BTreeMap<Uuid, Entity>,
}

impl Catalog {
    pub(crate) fn overlay_plan(&mut self, entity: Entity) {
        self.items.insert(entity.id, entity);
    }
    pub fn load(vault: &Vault) -> Result<Self, ProductivityError> {
        let index = vault.scan()?;
        if let Some(issue) = index.report()?.issues.into_iter().find(|issue| {
            managed_path(&issue.path)
                && issue
                    .path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with("pos-"))
        }) {
            return Err(invalid(format!("{}: {}", issue.path, issue.message)));
        }
        let mut items = BTreeMap::new();
        for note in index
            .list()?
            .into_iter()
            .filter(|note| managed_path(&note.path))
        {
            let note = vault.read(&note.path)?;
            if let Some(entity) = from_note(&note)?
                && items.insert(entity.id, entity).is_some()
            {
                return Err(invalid(format!(
                    "duplicate productivity ID in {}",
                    note.metadata.path
                )));
            }
        }
        Self::from_items(items)
    }

    pub(crate) fn from_items(mut items: BTreeMap<Uuid, Entity>) -> Result<Self, ProductivityError> {
        crate::workflow::roll_up(&mut items);
        let mut sessions = Vec::new();
        for owner in items.values() {
            if let Fields::Task { focus_sessions, .. } = &owner.item.fields {
                for session in focus_sessions {
                    sessions.push(Entity {
                        id: session.id,
                        path: owner.path.clone(),
                        progress: None,
                        item: Item {
                            title: owner.item.title.clone(),
                            fields: Fields::TimeEntry {
                                task: owner.id,
                                intervals: session.intervals.clone(),
                                status: session.status,
                                running_since: session.running_since,
                            },
                        },
                    });
                }
            }
        }
        for entry in sessions {
            if items.insert(entry.id, entry).is_some() {
                return Err(invalid("专注记录 ID 重复"));
            }
        }
        Ok(Self { items })
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.items.values()
    }

    pub fn get(&self, id: Uuid) -> Result<&Entity, ProductivityError> {
        self.items
            .get(&id)
            .ok_or_else(|| invalid(format!("unknown entity {id}")))
    }

    pub fn plan(&self, date: Date) -> Result<Option<&Entity>, ProductivityError> {
        let mut plans = self.entities().filter(|entity|
            matches!(entity.item.fields, Fields::DailyPlan { date: day, .. } if day == date)
        );
        let plan = plans.next();
        if plans.next().is_some() {
            return Err(invalid(format!("multiple daily plans for {date}")));
        }
        Ok(plan)
    }

    pub fn focus(&self) -> Result<Option<&Entity>, ProductivityError> {
        let mut open = self.entities().filter(|entity|
            matches!(entity.item.fields, Fields::TimeEntry { status, .. } if status != EntryStatus::Finished)
        );
        let current = open.next();
        if open.next().is_some() {
            return Err(invalid("multiple open focus sessions"));
        }
        Ok(current)
    }

    pub(crate) fn require(&self, id: Uuid, kind: &str) -> Result<&Entity, ProductivityError> {
        let entity = self.get(id)?;
        if entity.item.fields.kind() != kind {
            return Err(invalid(format!("{id} is not a {kind}")));
        }
        Ok(entity)
    }

    pub(crate) fn all(&self, ids: &[Uuid], kind: &str) -> Result<(), ProductivityError> {
        let mut seen = BTreeSet::new();
        for &id in ids {
            self.require(id, kind)?;
            if !seen.insert(id) {
                return Err(invalid(format!("duplicate {kind} reference {id}")));
            }
        }
        Ok(())
    }

    pub(crate) fn planned_tasks(&self, date: Date, plan: Option<&Entity>) -> BTreeSet<Uuid> {
        let mut tasks = BTreeSet::new();
        if let Some(Entity {
            item:
                Item {
                    fields:
                        Fields::DailyPlan {
                            tasks: arranged, ..
                        },
                    ..
                },
            ..
        }) = plan
        {
            tasks.extend(arranged);
        }
        let decisions = self.latest_decisions(Some(date));
        tasks.extend(decisions.into_iter().filter_map(|(id, (_, decision))| {
            matches!(decision, Disposition::Move { date: target } if *target == date).then_some(id)
        }));
        if let Some(Fields::DailyPlan { layout, .. }) = plan.map(|plan| &plan.item.fields) {
            tasks.retain(|id| !layout.omitted.contains(id));
        }
        tasks
    }

    pub(crate) fn latest_decisions(
        &self,
        before: Option<Date>,
    ) -> BTreeMap<Uuid, (Date, &Disposition)> {
        let mut latest = BTreeMap::new();
        for entity in self.entities() {
            if let Fields::DailyPlan {
                date,
                closing: Some(decisions),
                ..
            } = &entity.item.fields
                && before.is_none_or(|limit| *date < limit)
            {
                for decision in decisions {
                    if latest
                        .get(&decision.task)
                        .is_none_or(|(day, _)| *day < *date)
                    {
                        latest.insert(decision.task, (*date, &decision.disposition));
                    }
                }
            }
        }
        latest
    }

    pub(crate) fn validate(
        &self,
        entity: &Entity,
        creating: bool,
    ) -> Result<(), ProductivityError> {
        if entity.item.title.trim().is_empty() || entity.item.title.contains(['\r', '\n']) {
            return Err(invalid("title must be a nonempty line"));
        }
        let existing = self.items.get(&entity.id);
        if creating && existing.is_some() {
            return Err(invalid("productivity ID already exists"));
        }
        if let Some(old) = existing
            && (old.path != entity.path || old.item.fields.kind() != entity.item.fields.kind())
        {
            return Err(invalid("an edit must keep the entity path and type"));
        }
        match &entity.item.fields {
            Fields::Routine {
                from,
                until,
                minutes,
                start,
                goal,
            } => {
                crate::agenda::validate_duration(*minutes, *start)?;
                if until.is_some_and(|end| end < *from) {
                    return Err(invalid("结束日期早于起始日期"));
                }
                if let Some(id) = goal {
                    self.require(*id, "goal")?;
                }
            }
            Fields::Goal { .. } => {}
            Fields::Milestone { goal, sequence, .. } => {
                self.require(*goal, "goal")?;
                if *sequence == 0 || self.entities().any(|other| other.id != entity.id &&
                    matches!(other.item.fields, Fields::Milestone { goal: g, sequence: n, .. } if g == *goal && n == *sequence)) {
                    return Err(invalid("milestone sequence must be positive and unique within the goal"));
                }
            }
            Fields::Project {
                goals, milestones, ..
            } => {
                self.all(goals, "goal")?;
                self.all(milestones, "milestone")?;
            }
            Fields::Task {
                status,
                project,
                goals,
                milestone,
                depends_on,
                initial_estimate_minutes,
                estimate_minutes,
                importance,
                checkpoints,
                focus_sessions,
                ..
            } => {
                let mut titles = BTreeSet::new();
                if importance.is_some_and(|value| value > 100)
                    || checkpoints.iter().any(|item| {
                        item.title.trim().is_empty()
                            || item.title.trim() != item.title
                            || !titles.insert(&item.title)
                    })
                {
                    return Err(invalid(
                        "importance must be 0–100 and checkpoint titles must be distinct and trimmed",
                    ));
                }
                if let Some(Fields::Task {
                    initial_estimate_minutes: Some(initial),
                    ..
                }) = existing.map(|old| &old.item.fields)
                    && *initial_estimate_minutes != Some(*initial)
                {
                    return Err(invalid(
                        "the initial estimate is fixed after it is first recorded",
                    ));
                }
                if let Some(id) = project {
                    self.require(*id, "project")?;
                }
                if let Some(id) = milestone {
                    self.require(*id, "milestone")?;
                }
                self.all(goals, "goal")?;
                self.all(depends_on, "task")?;
                if matches!(status, WorkStatus::Doing | WorkStatus::Done) {
                    self.require_ready(depends_on)?;
                }
                if *status == WorkStatus::Done && checkpoints.iter().any(|point| !point.done) {
                    return Err(invalid("请先完成剩余检查点"));
                }
                if *status != WorkStatus::Done
                    && matches!(
                        existing.map(|old| &old.item.fields),
                        Some(Fields::Task {
                            status: WorkStatus::Done,
                            ..
                        })
                    )
                    && self.entities().any(|other| {
                        matches!(&other.item.fields, Fields::Task {
                        status: WorkStatus::Doing | WorkStatus::Done, depends_on, ..
                    } if depends_on.contains(&entity.id))
                    })
                {
                    return Err(invalid(
                        "请先暂停或重新打开已经开工的后续任务，再撤回前置任务",
                    ));
                }
                let mut session_ids = BTreeSet::new();
                let mut open_sessions = 0;
                for session in focus_sessions {
                    if session.id == entity.id
                        || !session_ids.insert(session.id)
                        || self
                            .get(session.id)
                            .is_ok_and(|old| old.path != entity.path)
                    {
                        return Err(invalid("专注记录 ID 重复"));
                    }
                    validate_intervals(&session.intervals, session.status, session.running_since)?;
                    if session.status != EntryStatus::Finished {
                        open_sessions += 1;
                        if *status != WorkStatus::Doing
                            || open_sessions > 1
                            || self.focus()?.is_some_and(|active| active.id != session.id)
                        {
                            return Err(invalid("同一时间只能专注于一项进行中的任务"));
                        }
                    }
                }
                let mut stack = depends_on.clone();
                let mut visited = BTreeSet::new();
                while let Some(id) = stack.pop() {
                    if id == entity.id {
                        return Err(invalid("task dependency cycle"));
                    }
                    if visited.insert(id)
                        && let Fields::Task { depends_on, .. } =
                            &self.require(id, "task")?.item.fields
                    {
                        stack.extend(depends_on);
                    }
                }
                if [initial_estimate_minutes, estimate_minutes]
                    .into_iter()
                    .flatten()
                    .any(|minutes| *minutes == 0)
                {
                    return Err(invalid("an estimate must be positive"));
                }
                if !status.is_open() && self.focus()?.is_some_and(|focus|
                    focus.path != entity.path && matches!(focus.item.fields, Fields::TimeEntry { task, .. } if task == entity.id)
                ) {
                    return Err(invalid("finish the open focus session before closing its task"));
                }
            }
            Fields::DailyPlan {
                date,
                layout,
                tasks,
                priority_tasks,
                focus_goals,
                budget_minutes,
                buffer_minutes,
                work_windows,
                strategic_minutes,
                importance_threshold,
                urgency_percent,
                closing,
            } => {
                if self.plan(*date)?.is_some_and(|plan| plan.id != entity.id)
                    || matches!(existing.map(|entity| &entity.item.fields), Some(Fields::DailyPlan { date: old, .. }) if old != date)
                {
                    return Err(invalid(
                        "each date has one daily plan and its date is fixed",
                    ));
                }
                self.all(tasks, "task")?;
                for (id, slot) in &layout.slots {
                    if !tasks.contains(id) {
                        return Err(invalid("定时任务必须在当天计划中"));
                    }
                    crate::agenda::validate_duration(slot.minutes, Some(slot.start))?;
                }
                for occurrence in layout.routines.values() {
                    crate::agenda::validate_duration(occurrence.minutes, occurrence.start)?;
                }
                if tasks.iter().any(|id| layout.omitted.contains(id)) {
                    return Err(invalid("任务同时被选入和移出"));
                }
                self.all(priority_tasks, "task")?;
                self.all(focus_goals, "goal")?;
                if priority_tasks.iter().any(|id| !tasks.contains(id)) {
                    return Err(invalid("priority tasks must be on the daily plan"));
                }
                if budget_minutes.is_some_and(|budget| *buffer_minutes > budget) {
                    return Err(invalid("buffer exceeds the time budget"));
                }
                if let Some(windows) = work_windows {
                    let mut windows = windows.clone();
                    windows.sort_by_key(|window| window.start);
                    for window in &windows {
                        interval_valid(*window)?;
                    }
                    if windows.windows(2).any(|pair| pair[0].end > pair[1].start) {
                        return Err(invalid("work windows must not overlap"));
                    }
                }
                if budget_minutes
                    .zip(*strategic_minutes)
                    .is_some_and(|(budget, strategic)| {
                        strategic > budget.saturating_sub(*buffer_minutes)
                    })
                {
                    return Err(invalid("strategic time exceeds the budget after buffer"));
                }
                if importance_threshold.is_some_and(|value| !(1..100).contains(&value))
                    || urgency_percent.is_some_and(|value| !(1..=1000).contains(&value))
                {
                    return Err(invalid(
                        "priority thresholds must be 1–99 for importance and 1–1000% for urgency",
                    ));
                }
                if let Some(decisions) = closing {
                    let ids: Vec<_> = decisions.iter().map(|decision| decision.task).collect();
                    self.all(&ids, "task")?;
                    let planned = self.planned_tasks(*date, Some(entity));
                    if ids.iter().any(|id| !planned.contains(id)) || decisions.iter().any(|decision|
                        matches!(decision.disposition, Disposition::Move { date: target } if target <= *date)
                    ) {
                        return Err(invalid("closing decisions must reference planned tasks and future dates"));
                    }
                    let was_closed = matches!(
                        existing.map(|entity| &entity.item.fields),
                        Some(Fields::DailyPlan {
                            closing: Some(_),
                            ..
                        })
                    );
                    if !was_closed
                        && planned
                            .iter()
                            .any(|id| self.task_open(*id) && !ids.contains(id))
                    {
                        return Err(invalid(
                            "choose a disposition for every unfinished planned task",
                        ));
                    }
                }
            }
            Fields::Event { interval } => interval_valid(*interval)?,
            Fields::TimeBlock { task, interval } => {
                self.require(*task, "task")?;
                interval_valid(*interval)?;
            }
            Fields::TimeEntry {
                task,
                intervals,
                status,
                running_since,
            } => {
                let task = self.require(*task, "task")?;
                validate_intervals(intervals, *status, *running_since)?;
                if *status != EntryStatus::Finished
                    && let Fields::Task { depends_on, .. } = &task.item.fields
                {
                    self.require_ready(depends_on)?;
                }
                if *status != EntryStatus::Finished
                    && (!self.task_open(task.id)
                        || self.focus()?.is_some_and(|focus| focus.id != entity.id))
                {
                    return Err(invalid(
                        "focus requires an open task and a single active session",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn task_open(&self, id: Uuid) -> bool {
        matches!(self.get(id).map(|item| &item.item.fields), Ok(Fields::Task { status, .. }) if status.is_open())
    }

    fn require_ready(&self, depends_on: &[Uuid]) -> Result<(), ProductivityError> {
        for id in depends_on {
            let task = self.require(*id, "task")?;
            if !matches!(
                task.item.fields,
                Fields::Task {
                    status: WorkStatus::Done,
                    ..
                }
            ) {
                return Err(invalid(format!("前置任务尚未完成：{}", task.item.title)));
            }
        }
        Ok(())
    }
}

fn validate_intervals(
    intervals: &[Interval],
    status: EntryStatus,
    running_since: Option<jiff::Timestamp>,
) -> Result<(), ProductivityError> {
    if (status == EntryStatus::Running) != running_since.is_some() {
        return Err(invalid("a running session needs a start time"));
    }
    for interval in intervals {
        interval_valid(*interval)?;
    }
    if intervals.windows(2).any(|pair| pair[0].end > pair[1].start)
        || intervals
            .last()
            .zip(running_since)
            .is_some_and(|(last, start)| last.end > start)
    {
        return Err(invalid("time-entry intervals must be ordered and disjoint"));
    }
    Ok(())
}

pub(crate) fn from_note(note: &Note) -> Result<Option<Entity>, ProductivityError> {
    let Some(value) = note.frontmatter.get(PROPERTY) else {
        return Ok(None);
    };
    let doc: Document = serde_json::from_value(value.clone())
        .map_err(|error| invalid(format!("{}: {error}", note.metadata.path)))?;
    let id = note
        .metadata
        .id
        .strip_prefix("yaml:")
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| invalid(format!("{} needs a UUID in YAML id", note.metadata.path)))?;
    if doc.version != SCHEMA
        || note
            .frontmatter
            .get("type")
            .and_then(|value| value.as_str())
            != Some(doc.item.fields.kind())
    {
        return Err(invalid(format!(
            "productivity schema or type mismatch in {}",
            note.metadata.path
        )));
    }
    Ok(Some(Entity {
        progress: None,
        id,
        path: note.metadata.path.clone(),
        item: doc.item,
    }))
}

pub(crate) fn managed_path(path: &str) -> bool {
    ["Life/Goal/", "Life/Project/", "Life/Plan/", "Journal/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn interval_valid(interval: Interval) -> Result<(), ProductivityError> {
    if interval.end <= interval.start {
        return Err(invalid("an interval must end after it starts"));
    }
    Ok(())
}
