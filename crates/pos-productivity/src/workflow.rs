use crate::{
    BoardTask, Catalog, Entity, Fields, Interval, Priority, TaskProgress, TimelineItem, Timestamp,
    WorkStatus,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) fn roll_up(items: &mut BTreeMap<Uuid, Entity>) {
    let mut progress = BTreeMap::<Uuid, TaskProgress>::new();
    for entity in items.values() {
        let Fields::Task {
            status,
            project,
            goals,
            milestone,
            checkpoints,
            ..
        } = &entity.item.fields
        else {
            continue;
        };
        if *status == WorkStatus::Cancelled {
            continue;
        }
        let mut parents: BTreeSet<_> = goals.iter().copied().collect();
        if let Some(project) = project {
            parents.insert(*project);
            if let Some(Fields::Project {
                goals, milestones, ..
            }) = items.get(project).map(|entity| &entity.item.fields)
            {
                parents.extend(goals);
                for stage in milestones {
                    if let Some(Fields::Milestone { goal, .. }) =
                        items.get(stage).map(|item| &item.item.fields)
                    {
                        parents.insert(*goal);
                    }
                }
            }
        }
        if let Some(stage) = milestone {
            parents.insert(*stage);
            if let Some(Fields::Milestone { goal, .. }) =
                items.get(stage).map(|item| &item.item.fields)
            {
                parents.insert(*goal);
            }
        }
        for id in parents {
            let summary = progress.entry(id).or_default();
            summary.tasks += 1;
            summary.completed += usize::from(*status == WorkStatus::Done);
            summary.checkpoints += checkpoints.len();
            summary.checked += checkpoints.iter().filter(|point| point.done).count();
        }
    }
    for (id, summary) in progress {
        if let Some(item) = items.get_mut(&id) {
            item.progress = Some(summary);
        }
    }
}

pub(crate) fn next_task(
    catalog: &Catalog,
    rows: &[BoardTask],
    inbox: &[Entity],
    priorities: &[Priority],
    scheduled: &[TimelineItem],
    now: Timestamp,
    day: Interval,
) -> Option<Uuid> {
    let current_blocks: BTreeSet<_> = scheduled
        .iter()
        .filter(|slot| slot.interval.start <= now && now < slot.interval.end)
        .filter_map(|slot| slot.task)
        .collect();
    let priority: BTreeMap<_, _> = priorities.iter().map(|point| (point.task, point)).collect();
    rows.iter()
        .filter(|row| catalog.task_open(row.task.id) && row.blockers.is_empty())
        .min_by_key(|row| {
            let point = priority.get(&row.task.id);
            (
                !current_blocks.contains(&row.task.id),
                !matches!(
                    row.task.item.fields,
                    Fields::Task {
                        status: WorkStatus::Doing,
                        ..
                    }
                ),
                !row.priority,
                !point.is_some_and(|point| point.overdue || point.quadrant == Some(1)),
                point
                    .and_then(|point| point.deadline)
                    .unwrap_or(Timestamp::MAX),
                &row.task.item.title,
                row.task.id,
            )
        })
        .map(|row| row.task.id)
        .or_else(|| {
            if now < day.start || now >= day.end {
                return None;
            }
            inbox
                .iter()
                .filter(|item| {
                    matches!(&item.item.fields, Fields::Task { depends_on, .. }
            if depends_on.iter().all(|id| matches!(catalog.get(*id).map(|item| &item.item.fields),
                Ok(Fields::Task { status: WorkStatus::Done, .. }))))
                })
                .min_by_key(|item| match &item.item.fields {
                    Fields::Task {
                        due_at, importance, ..
                    } => (
                        due_at.unwrap_or(Timestamp::MAX),
                        std::cmp::Reverse(importance.unwrap_or(0)),
                        &item.item.title,
                    ),
                    _ => unreachable!(),
                })
                .map(|item| item.id)
        })
}
