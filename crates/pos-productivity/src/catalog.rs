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
                ..
            } => {
                if let Some(id) = project {
                    self.require(*id, "project")?;
                }
                if let Some(id) = milestone {
                    self.require(*id, "milestone")?;
                }
                self.all(goals, "goal")?;
                self.all(depends_on, "task")?;
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
                    matches!(focus.item.fields, Fields::TimeEntry { task, .. } if task == entity.id)
                ) {
                    return Err(invalid("finish the open focus session before closing its task"));
                }
            }
            Fields::DailyPlan {
                date,
                tasks,
                priority_tasks,
                focus_goals,
                budget_minutes,
                buffer_minutes,
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
                self.all(priority_tasks, "task")?;
                self.all(focus_goals, "goal")?;
                if priority_tasks.iter().any(|id| !tasks.contains(id)) {
                    return Err(invalid("priority tasks must be on the daily plan"));
                }
                if budget_minutes.is_some_and(|budget| *buffer_minutes > budget) {
                    return Err(invalid("buffer exceeds the time budget"));
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
                if (*status == EntryStatus::Running) != running_since.is_some() {
                    return Err(invalid("a running session needs a start time"));
                }
                if *status != EntryStatus::Finished
                    && (!self.task_open(task.id)
                        || self.focus()?.is_some_and(|focus| focus.id != entity.id))
                {
                    return Err(invalid(
                        "focus requires an open task and a single active session",
                    ));
                }
                for interval in intervals {
                    interval_valid(*interval)?;
                }
                if intervals.windows(2).any(|pair| pair[0].end > pair[1].start)
                    || intervals
                        .last()
                        .zip(*running_since)
                        .is_some_and(|(last, start)| last.end > start)
                {
                    return Err(invalid("time-entry intervals must be ordered and disjoint"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn task_open(&self, id: Uuid) -> bool {
        matches!(self.get(id).map(|item| &item.item.fields), Ok(Fields::Task { status, .. }) if status.is_open())
    }
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
        id,
        path: note.metadata.path.clone(),
        item: doc.item,
    }))
}

fn managed_path(path: &str) -> bool {
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
