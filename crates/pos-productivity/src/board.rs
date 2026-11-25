use crate::catalog::Catalog;
use crate::model::*;
use crate::{Date, Timestamp};
use jiff::tz::TimeZone;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct BoardTask {
    pub task: Entity,
    pub priority: bool,
    pub planned: bool,
    pub blockers: Vec<Uuid>,
    pub actual_seconds: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineItem {
    pub id: Uuid,
    pub title: String,
    pub kind: &'static str,
    pub task: Option<Uuid>,
    #[serde(flatten)]
    pub interval: Interval,
}

#[derive(Debug, Serialize)]
pub struct DayTotals {
    pub completed: usize,
    pub tasks: usize,
    pub planned_work_seconds: i64,
    pub fixed_event_seconds: i64,
    pub actual_seconds: i64,
    pub available_work_minutes: Option<u32>,
    pub remaining_estimate_minutes: u64,
    pub unestimated_tasks: usize,
}

#[derive(Debug, Serialize)]
pub struct DayBoard {
    pub date: Date,
    pub timezone: String,
    pub plan: Option<Entity>,
    pub tasks: Vec<BoardTask>,
    pub inbox: Vec<Entity>,
    pub focus_goals: Vec<RoadmapGoal>,
    pub scheduled: Vec<TimelineItem>,
    pub actual: Vec<TimelineItem>,
    pub overlaps: Vec<[Uuid; 2]>,
    pub focus: Option<Entity>,
    pub focus_elapsed_seconds: i64,
    pub totals: DayTotals,
    pub journal_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RoadmapGoal {
    pub goal: Entity,
    pub milestones: Vec<Entity>,
    pub current_milestone: Option<Uuid>,
    pub projects: Vec<Entity>,
}

impl Catalog {
    pub fn roadmap(&self) -> Vec<RoadmapGoal> {
        self.entities().filter(|entity| matches!(entity.item.fields, Fields::Goal { .. })).map(|goal| {
            let mut milestones: Vec<_> = self.entities().filter(|entity|
                matches!(entity.item.fields, Fields::Milestone { goal: id, .. } if id == goal.id)
            ).cloned().collect();
            milestones.sort_by_key(|entity| match entity.item.fields {
                Fields::Milestone { sequence, .. } => sequence,
                _ => 0,
            });
            let current_milestone = milestones.iter().find(|item|
                matches!(item.item.fields, Fields::Milestone { status: WorkStatus::Doing, .. })
            ).or_else(|| milestones.iter().find(|item|
                matches!(item.item.fields, Fields::Milestone { status: WorkStatus::Todo, .. })
            )).map(|item| item.id);
            let projects = self.entities().filter(|entity| matches!(
                &entity.item.fields, Fields::Project { goals, milestones: stages, .. }
                    if goals.contains(&goal.id) || stages.iter().any(|id| milestones.iter().any(|stage| stage.id == *id))
            )).cloned().collect();
            RoadmapGoal { goal: goal.clone(), milestones, current_milestone, projects }
        }).collect()
    }

    pub fn day(
        &self,
        date: Date,
        timezone: &str,
        now: Timestamp,
    ) -> Result<DayBoard, ProductivityError> {
        let zone = TimeZone::get(timezone)?;
        let first = date.to_zoned(zone.clone())?.timestamp();
        let last = date.tomorrow()?.to_zoned(zone)?.timestamp();
        let window = Interval {
            start: first,
            end: last,
        };
        let plan = self.plan(date)?;
        let mut tasks = self.planned_tasks(date, plan);
        let planned_tasks = tasks.clone();
        let (priorities, chosen_goals, budget, buffer) = match plan.map(|plan| &plan.item.fields) {
            Some(Fields::DailyPlan {
                priority_tasks,
                focus_goals,
                budget_minutes,
                buffer_minutes,
                ..
            }) => (
                priority_tasks.as_slice(),
                focus_goals.as_slice(),
                *budget_minutes,
                *buffer_minutes,
            ),
            _ => (&[][..], &[][..], None, 0),
        };
        let mut scheduled = Vec::new();
        let mut actual = Vec::new();
        for entity in self.entities() {
            match &entity.item.fields {
                Fields::Event { interval } => {
                    if let Some(interval) = clip(*interval, window) {
                        scheduled.push(timeline(entity, "event", None, interval));
                    }
                }
                Fields::TimeBlock { task, interval } => {
                    if let Some(interval) = clip(*interval, window) {
                        tasks.insert(*task);
                        scheduled.push(timeline(entity, "time_block", Some(*task), interval));
                    }
                }
                Fields::Task {
                    due_at: Some(due), ..
                } if first <= *due && *due < last => {
                    tasks.insert(entity.id);
                }
                Fields::TimeEntry { task, .. } => {
                    for interval in
                        entry_intervals(entity, now).filter_map(|interval| clip(interval, window))
                    {
                        actual.push(timeline(entity, "time_entry", Some(*task), interval));
                    }
                }
                _ => {}
            }
        }
        scheduled.sort_by_key(|item| (item.interval.start, item.id));
        actual.sort_by_key(|item| (item.interval.start, item.id));
        let mut totals = DayTotals {
            completed: 0,
            tasks: 0,
            planned_work_seconds: union_seconds(
                scheduled
                    .iter()
                    .filter(|item| item.kind == "time_block")
                    .map(|item| item.interval),
            ),
            fixed_event_seconds: union_seconds(
                scheduled
                    .iter()
                    .filter(|item| item.kind == "event")
                    .map(|item| item.interval),
            ),
            actual_seconds: union_seconds(actual.iter().map(|item| item.interval)),
            available_work_minutes: None,
            remaining_estimate_minutes: 0,
            unestimated_tasks: 0,
        };
        totals.available_work_minutes = budget.map(|budget| {
            budget
                .saturating_sub(buffer)
                .saturating_sub((totals.fixed_event_seconds / 60) as u32)
        });
        let mut board_tasks = Vec::new();
        for id in tasks {
            let entity = self.require(id, "task")?;
            let Fields::Task {
                status,
                depends_on,
                estimate_minutes,
                initial_estimate_minutes,
                ..
            } = &entity.item.fields
            else {
                return Err(invalid("planned entity must be a task"));
            };
            let actual_seconds = union_seconds(
                actual
                    .iter()
                    .filter(|item| item.task == Some(id))
                    .map(|item| item.interval),
            );
            if *status == WorkStatus::Done {
                totals.completed += 1;
            }
            if *status != WorkStatus::Cancelled {
                totals.tasks += 1;
            }
            if status.is_open() {
                if let Some(estimate) = estimate_minutes.or(*initial_estimate_minutes) {
                    let recorded = union_seconds(self.entities().filter(|item|
                        matches!(item.item.fields, Fields::TimeEntry { task, .. } if task == id)
                    ).flat_map(|entry| entry_intervals(entry, now)));
                    totals.remaining_estimate_minutes += u64::from(estimate)
                        .saturating_sub(u64::try_from(recorded / 60).unwrap_or(0));
                } else {
                    totals.unestimated_tasks += 1;
                }
            }
            let mut blockers = Vec::new();
            for dependency in depends_on {
                if !matches!(
                    self.require(*dependency, "task")?.item.fields,
                    Fields::Task {
                        status: WorkStatus::Done,
                        ..
                    }
                ) {
                    blockers.push(*dependency);
                }
            }
            board_tasks.push(BoardTask {
                task: entity.clone(),
                priority: priorities.contains(&id),
                planned: planned_tasks.contains(&id),
                blockers,
                actual_seconds,
            });
        }
        board_tasks.sort_by_key(|row| {
            (
                !row.priority,
                row.task.item.title.to_lowercase(),
                row.task.id,
            )
        });

        let latest = self.latest_decisions(None);
        let mut arranged = BTreeSet::new();
        for entity in self.entities() {
            match &entity.item.fields {
                Fields::DailyPlan {
                    tasks,
                    closing: None,
                    ..
                } => arranged.extend(tasks.iter().copied()),
                Fields::TimeBlock { task, interval } if interval.end > first => {
                    arranged.insert(*task);
                }
                _ => {}
            }
        }
        let inbox = self
            .entities()
            .filter(|entity| {
                self.task_open(entity.id)
                    && !arranged.contains(&entity.id)
                    && latest
                        .get(&entity.id)
                        .is_none_or(|(_, decision)| matches!(decision, Disposition::Inbox))
                    && !board_tasks.iter().any(|row| row.task.id == entity.id)
            })
            .cloned()
            .collect();

        let focus = self.focus()?.cloned();
        let focus_elapsed_seconds = focus
            .as_ref()
            .map_or(0, |entry| union_seconds(entry_intervals(entry, now)));
        let focus_goals = self
            .roadmap()
            .into_iter()
            .filter(|item| {
                if chosen_goals.is_empty() {
                    matches!(
                        item.goal.item.fields,
                        Fields::Goal {
                            status: WorkStatus::Doing
                        }
                    )
                } else {
                    chosen_goals.contains(&item.goal.id)
                }
            })
            .collect();
        let mut overlaps = Vec::new();
        for (index, left) in scheduled.iter().enumerate() {
            for right in &scheduled[index + 1..] {
                if right.interval.start >= left.interval.end {
                    break;
                }
                overlaps.push([left.id, right.id]);
            }
        }
        Ok(DayBoard {
            date,
            timezone: timezone.into(),
            plan: plan.cloned(),
            tasks: board_tasks,
            inbox,
            focus_goals,
            scheduled,
            actual,
            overlaps,
            focus,
            focus_elapsed_seconds,
            totals,
            journal_path: None,
        })
    }
}

fn timeline(
    entity: &Entity,
    kind: &'static str,
    task: Option<Uuid>,
    interval: Interval,
) -> TimelineItem {
    TimelineItem {
        id: entity.id,
        title: entity.item.title.clone(),
        kind,
        task,
        interval,
    }
}

fn entry_intervals(entity: &Entity, now: Timestamp) -> impl Iterator<Item = Interval> + '_ {
    let (intervals, running) = match &entity.item.fields {
        Fields::TimeEntry {
            intervals,
            running_since,
            ..
        } => (intervals.as_slice(), *running_since),
        _ => (&[][..], None),
    };
    intervals
        .iter()
        .map(move |interval| Interval {
            start: interval.start,
            end: interval.end.min(now),
        })
        .filter(|interval| interval.start < interval.end)
        .chain(
            running
                .filter(|start| *start < now)
                .map(|start| Interval { start, end: now }),
        )
}

fn clip(interval: Interval, bounds: Interval) -> Option<Interval> {
    let start = interval.start.max(bounds.start);
    let end = interval.end.min(bounds.end);
    (start < end).then_some(Interval { start, end })
}

fn union_seconds(intervals: impl Iterator<Item = Interval>) -> i64 {
    let mut intervals: Vec<_> = intervals.collect();
    intervals.sort_by_key(|interval| interval.start);
    let mut merged = BTreeMap::new();
    let mut active: Option<Interval> = None;
    for interval in intervals {
        match &mut active {
            Some(current) if interval.start <= current.end => {
                current.end = current.end.max(interval.end)
            }
            _ => {
                if let Some(current) = active.replace(interval) {
                    merged.insert(current.start, current.end);
                }
            }
        }
    }
    if let Some(current) = active {
        merged.insert(current.start, current.end);
    }
    merged
        .into_iter()
        .map(|(start, end)| end.duration_since(start).as_secs())
        .sum()
}
