use crate::board::{DayBoard, InitialPlan, clip, timeline};
use crate::catalog::{Catalog, from_note};
use crate::model::*;
use jiff::Timestamp;
use pos_change::ChangeRecord;
use pos_vault::parse_document;
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct EstimateRevision {
    pub task: Uuid,
    pub time: Timestamp,
    pub before: Option<u32>,
    pub after: Option<u32>,
    pub reason: Option<String>,
}

pub(crate) fn attach(catalog: &Catalog, board: &mut DayBoard, mut records: Vec<ChangeRecord>) {
    let tasks: BTreeMap<_, _> = board
        .tasks
        .iter()
        .map(|row| (row.task.path.clone(), row.task.id))
        .collect();
    let mut originals = BTreeMap::new();
    records.sort_by_key(|record| record.seq);
    for record in records {
        let plan = record.plan;
        let path = &plan.request.path;
        let operation = &plan.request.operation;
        if !plan.before_exists
            && plan.after_exists
            && operation.kind == "note.create"
            && let Ok(note) = parse_document(path, &plan.patch.after)
            && let Ok(Some(entity)) = from_note(&note)
        {
            originals.entry(entity.id).or_insert((record.seq, entity));
        }
        if operation.kind == "property.set"
            && operation.payload.get("key").and_then(|key| key.as_str()) == Some(PROPERTY)
            && let Some(&task) = tasks.get(path)
            && let Ok(time) = Timestamp::from_millisecond(record.executed_unix_ms)
            && let Some(before) = task_estimate(path, &plan.patch.before)
            && let Some(after) = task_estimate(path, &plan.patch.after)
            && before != after
        {
            board.estimate_revisions.push(EstimateRevision {
                task,
                time,
                before,
                after,
                reason: plan.request.reason,
            });
        }
    }
    board.estimate_revisions.reverse();
    let mut initial = InitialPlan::default();
    let mut originals_in_order = originals.values().collect::<Vec<_>>();
    originals_in_order.sort_by_key(|(seq, _)| *seq);
    for (_, entity) in originals_in_order {
        match &entity.item.fields {
            Fields::DailyPlan {
                date,
                tasks,
                layout,
                ..
            } if *date == board.date && initial.task_titles.is_none() => {
                if let Ok(zone) = jiff::tz::TimeZone::get(&board.timezone) {
                    for (id, slot) in &layout.slots {
                        if let Ok(interval) = crate::agenda::slot_interval(*date, &zone, *slot)
                            && let Some(task) = originals
                                .get(id)
                                .map(|(_, entity)| entity)
                                .or_else(|| catalog.get(*id).ok())
                        {
                            initial.scheduled.push(timeline(
                                task,
                                "plan_slot",
                                Some(*id),
                                interval,
                            ));
                        }
                    }
                    for (&id, routine) in &layout.routines {
                        if let Some(start) = routine.start
                            && routine.status != RoutineStatus::Skipped
                            && let Ok(interval) = crate::agenda::slot_interval(
                                *date,
                                &zone,
                                PlanSlot {
                                    start,
                                    minutes: routine.minutes,
                                },
                            )
                        {
                            initial.scheduled.push(crate::TimelineItem {
                                id,
                                title: routine.title.clone(),
                                kind: "routine",
                                task: None,
                                interval,
                            });
                        }
                    }
                }
                initial.task_titles = Some(
                    tasks
                        .iter()
                        .map(|id| {
                            originals
                                .get(id)
                                .map(|(_, task)| task)
                                .or_else(|| catalog.get(*id).ok())
                                .map_or_else(|| id.to_string(), |task| task.item.title.clone())
                        })
                        .collect(),
                );
            }
            Fields::Event { interval } => {
                if let Some(interval) = clip(*interval, board.window) {
                    initial
                        .scheduled
                        .push(timeline(entity, "event", None, interval));
                }
            }
            Fields::TimeBlock { interval, task } => {
                if let Some(interval) = clip(*interval, board.window) {
                    initial
                        .scheduled
                        .push(timeline(entity, "time_block", Some(*task), interval));
                }
            }
            _ => {}
        }
    }
    if initial.task_titles.is_some() || !initial.scheduled.is_empty() {
        initial
            .scheduled
            .sort_by_key(|item| (item.interval.start, item.id));
        board.initial = Some(initial);
    }
}

fn task_estimate(path: &str, property: &str) -> Option<Option<u32>> {
    let note = parse_document(path, &format!("---\n{}\n---\n", property.trim_end())).ok()?;
    let doc: Document = serde_json::from_value(note.frontmatter.get(PROPERTY)?.clone()).ok()?;
    if let Fields::Task {
        estimate_minutes,
        initial_estimate_minutes,
        ..
    } = doc.item.fields
    {
        Some(estimate_minutes.or(initial_estimate_minutes))
    } else {
        None
    }
}
