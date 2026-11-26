use crate::model::invalid;
use crate::{
    BoardTask, Catalog, DailyRoutine, Date, DayBoard, Entity, Fields, Interval, PlanSlot, Planning,
    Priority, ProductivityError, RoutineStatus, TimelineItem, Timestamp,
};
use jiff::{SignedDuration, civil::Time, tz::TimeZone};
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct Agenda {
    pub board: DayBoard,
    pub pool: Vec<BoardTask>,
    pub priorities: Vec<Priority>,
}

pub(crate) fn slot_interval(
    date: Date,
    zone: &TimeZone,
    slot: PlanSlot,
) -> Result<Interval, ProductivityError> {
    let start = date
        .to_datetime(slot.start)
        .to_zoned(zone.clone())?
        .timestamp();
    let end = start
        .checked_add(SignedDuration::from_mins(i64::from(slot.minutes)))?
        .min(date.tomorrow()?.to_zoned(zone.clone())?.timestamp());
    Ok(Interval { start, end })
}

pub(crate) fn validate_duration(
    minutes: u32,
    start: Option<Time>,
) -> Result<(), ProductivityError> {
    let start_seconds = start.map_or(0, |t| {
        i32::from(t.hour()) * 3600 + i32::from(t.minute()) * 60 + i32::from(t.second())
    });
    if !(1..=1440).contains(&minutes) || i64::from(start_seconds) + i64::from(minutes) * 60 > 86400
    {
        return Err(invalid("时长至少为一分钟，时间段应在当天内"));
    }
    Ok(())
}

impl Catalog {
    pub fn routines(&self, date: Date, plan: Option<&Entity>) -> BTreeMap<Uuid, DailyRoutine> {
        let mut result = match plan.map(|entity| &entity.item.fields) {
            Some(Fields::DailyPlan {
                layout,
                closing: Some(_),
                ..
            }) => return layout.routines.clone(),
            Some(Fields::DailyPlan { layout, .. }) => layout.routines.clone(),
            _ => BTreeMap::new(),
        };
        for entity in self.entities() {
            if let Fields::Routine {
                from,
                until,
                minutes,
                start,
                ..
            } = entity.item.fields
                && from <= date
                && until.is_none_or(|end| date <= end)
            {
                result.entry(entity.id).or_insert_with(|| DailyRoutine {
                    title: entity.item.title.clone(),
                    minutes,
                    start,
                    status: RoutineStatus::Todo,
                });
            }
        }
        result
    }

    pub(crate) fn agenda_slots(
        &self,
        date: Date,
        zone: &TimeZone,
        plan: Option<&Entity>,
        routines: &BTreeMap<Uuid, DailyRoutine>,
    ) -> Result<Vec<TimelineItem>, ProductivityError> {
        let mut blocks = Vec::new();
        let mut add =
            |id, title: &str, kind, task, slot: PlanSlot| -> Result<(), ProductivityError> {
                blocks.push(TimelineItem {
                    id,
                    title: title.into(),
                    kind,
                    task,
                    interval: slot_interval(date, zone, slot)?,
                });
                Ok(())
            };
        if let Some(Fields::DailyPlan { layout, .. }) = plan.map(|entity| &entity.item.fields) {
            for (&task, &slot) in &layout.slots {
                if let Ok(entity) = self.get(task) {
                    add(task, &entity.item.title, "plan_slot", Some(task), slot)?;
                }
            }
        }
        for (&rule, occurrence) in routines {
            if let Some(start) = occurrence.start
                && occurrence.status != RoutineStatus::Skipped
            {
                add(
                    rule,
                    &occurrence.title,
                    "routine",
                    None,
                    PlanSlot {
                        start,
                        minutes: occurrence.minutes,
                    },
                )?;
            }
        }
        Ok(blocks)
    }

    pub fn agenda(
        &self,
        date: Date,
        timezone: &str,
        now: Timestamp,
    ) -> Result<Agenda, ProductivityError> {
        let board = self.day(date, timezone, now)?;
        let mut pool = Vec::new();
        for task in self.entities().filter(|task| self.task_open(task.id)) {
            pool.push(self.task_row(task.id, false, false, &board.actual, now)?);
        }
        let priorities = Planning::build(
            self,
            &pool,
            board.window,
            &board.scheduled,
            &board.actual,
            now,
            timezone,
        )?
        .priorities;
        Ok(Agenda {
            board,
            pool,
            priorities,
        })
    }
}
