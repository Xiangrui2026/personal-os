use crate::board::{BoardTask, TimelineItem, clip, entry_intervals, union_seconds};
use crate::catalog::Catalog;
use crate::model::*;
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct Planning {
    pub importance_threshold: u8,
    pub urgency_threshold: f64,
    pub remaining_work_minutes: Option<f64>,
    pub workload_ratio: Option<f64>,
    pub calibrated_remaining_minutes: Option<f64>,
    pub calibrated_tasks: usize,
    pub priorities: Vec<Priority>,
    pub estimates: BTreeMap<String, EstimateSample>,
    pub strategic_target_minutes: Option<u32>,
    pub strategic_scheduled_minutes: f64,
    pub distribution: Vec<TimeShare>,
}

#[derive(Debug, Serialize)]
pub struct Priority {
    pub task: Uuid,
    pub importance: Option<u8>,
    pub deadline: Option<Timestamp>,
    pub deadline_source: Option<&'static str>,
    pub available_minutes: Option<f64>,
    pub pressure: Option<f64>,
    pub slack_minutes: Option<f64>,
    pub quadrant: Option<u8>,
    pub exhausted: bool,
    pub overdue: bool,
    pub p50_remaining_minutes: Option<f64>,
    pub p80_remaining_minutes: Option<f64>,
    pub p95_remaining_minutes: Option<f64>,
    pub p80_slack_minutes: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct EstimateSample {
    pub count: usize,
    pub p50: Option<f64>,
    pub p80: Option<f64>,
    pub p95: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct TimeShare {
    pub category: Option<String>,
    pub overlap: bool,
    pub seconds: i64,
}

const IMPORTANT: u8 = 70;
const URGENT: f64 = 0.7;

impl Planning {
    pub(crate) fn build(
        catalog: &Catalog,
        tasks: &[BoardTask],
        day: Interval,
        scheduled: &[TimelineItem],
        actual: &[TimelineItem],
        now: Timestamp,
        timezone: &str,
    ) -> Result<Self, ProductivityError> {
        let zone = TimeZone::get(timezone)?;
        let plans = WorkCalendar::new(catalog, zone);
        let from = day.start.max(now);
        let (strategic_target_minutes, importance_threshold, urgency_threshold) = match plans
            .plans
            .get(&plans.date(day.start))
            .map(|plan| &plan.item.fields)
        {
            Some(Fields::DailyPlan {
                strategic_minutes,
                importance_threshold,
                urgency_percent,
                ..
            }) => (
                *strategic_minutes,
                importance_threshold.unwrap_or(IMPORTANT),
                urgency_percent.map_or(URGENT, |percent| f64::from(percent) / 100.0),
            ),
            _ => (None, IMPORTANT, URGENT),
        };
        let remaining_work_minutes = if from >= day.end {
            Some(0.0)
        } else {
            plans.available(from, day.end, None)?
        };
        let estimates = estimate_samples(catalog, now);
        let mut demand = 0.0;
        let mut estimated = 0;
        let mut calibrated_remaining = 0.0;
        let mut calibrated_tasks = 0;
        let mut priorities = Vec::new();
        for row in tasks {
            let Fields::Task {
                status,
                importance,
                due_at,
                milestone,
                category,
                initial_estimate_minutes,
                estimate_minutes,
                exclude_from_estimates,
                ..
            } = &row.task.item.fields
            else {
                continue;
            };
            if !status.is_open() {
                continue;
            }
            if let Some(remaining) = row.remaining_minutes {
                demand += remaining;
                estimated += 1;
            }
            let sample = category
                .as_ref()
                .and_then(|category| estimates.get(category))
                .filter(|_| !exclude_from_estimates);
            let adjusted = |factor: Option<f64>| {
                factor
                    .zip(estimate_minutes.or(*initial_estimate_minutes))
                    .map(|(factor, estimate)| {
                        (f64::from(estimate) * factor - row.total_actual_seconds as f64 / 60.0)
                            .max(0.0)
                    })
            };
            let p50_remaining_minutes = adjusted(sample.and_then(|sample| sample.p50));
            let p80_remaining_minutes = adjusted(sample.and_then(|sample| sample.p80));
            let p95_remaining_minutes = adjusted(sample.and_then(|sample| sample.p95));
            if let Some(adjusted) = p50_remaining_minutes {
                calibrated_remaining += adjusted;
                calibrated_tasks += 1;
            } else if let Some(remaining) = row.remaining_minutes {
                calibrated_remaining += remaining;
            }
            let deadline = plans.deadline(row.task.id, *due_at, *milestone, day.start)?;
            let overdue = deadline.is_some_and(|(deadline, _)| deadline <= from);
            let available_minutes = match deadline {
                Some(_) if overdue => Some(0.0),
                Some((deadline, _)) => plans.available(from, deadline, Some(row.task.id))?,
                None => None,
            };
            let comparison = row.remaining_minutes.zip(available_minutes);
            let exhausted = comparison
                .is_some_and(|(remaining, available)| remaining > 0.0 && available == 0.0);
            let pressure = comparison.and_then(|(remaining, available)| {
                (available > 0.0).then(|| remaining / available)
            });
            let urgent = if overdue || exhausted {
                Some(true)
            } else {
                pressure.map(|value| value >= urgency_threshold)
            };
            let quadrant = importance.zip(urgent).map(|(importance, urgent)| {
                match (importance >= importance_threshold, urgent) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, true) => 3,
                    (false, false) => 4,
                }
            });
            priorities.push(Priority {
                task: row.task.id,
                importance: *importance,
                deadline: deadline.map(|value| value.0),
                deadline_source: deadline.map(|value| value.1),
                available_minutes,
                pressure,
                slack_minutes: comparison.map(|(remaining, available)| available - remaining),
                quadrant,
                exhausted,
                overdue,
                p50_remaining_minutes,
                p80_remaining_minutes,
                p95_remaining_minutes,
                p80_slack_minutes: available_minutes
                    .zip(p80_remaining_minutes)
                    .map(|(available, predicted)| available - predicted),
            });
        }
        let strategic: BTreeSet<_> = priorities
            .iter()
            .filter(|point| point.quadrant == Some(2))
            .map(|point| point.task)
            .collect();
        Ok(Self {
            importance_threshold,
            urgency_threshold,
            remaining_work_minutes,
            workload_ratio: remaining_work_minutes
                .filter(|available| *available > 0.0)
                .filter(|_| estimated > 0 || priorities.is_empty())
                .map(|available| demand / available),
            calibrated_remaining_minutes: (calibrated_tasks > 0).then_some(calibrated_remaining),
            calibrated_tasks,
            priorities,
            estimates,
            strategic_target_minutes,
            strategic_scheduled_minutes: union_seconds(
                scheduled
                    .iter()
                    .filter(|slot| slot.task.is_some_and(|id| strategic.contains(&id)))
                    .map(|slot| slot.interval),
            ) as f64
                / 60.0,
            distribution: distribution(catalog, actual),
        })
    }
}

struct WorkCalendar<'a> {
    catalog: &'a Catalog,
    zone: TimeZone,
    plans: BTreeMap<Date, &'a Entity>,
    reservations: Vec<(Option<Uuid>, Interval)>,
    recorded: Vec<Interval>,
}

impl<'a> WorkCalendar<'a> {
    fn new(catalog: &'a Catalog, zone: TimeZone) -> Self {
        let mut plans = BTreeMap::new();
        let mut reservations = Vec::new();
        let mut recorded = Vec::new();
        for item in catalog.entities() {
            match &item.item.fields {
                Fields::DailyPlan { date, .. } => {
                    plans.insert(*date, item);
                }
                Fields::Event { interval } => reservations.push((None, *interval)),
                Fields::TimeBlock { task, interval } => reservations.push((Some(*task), *interval)),
                Fields::TimeEntry {
                    intervals,
                    running_since,
                    ..
                } => {
                    recorded.extend(intervals.iter().copied());
                    if let Some(start) = running_since {
                        recorded.push(Interval {
                            start: *start,
                            end: Timestamp::MAX,
                        });
                    }
                }
                _ => {}
            }
        }
        Self {
            catalog,
            zone,
            plans,
            reservations,
            recorded,
        }
    }

    fn date(&self, time: Timestamp) -> Date {
        time.to_zoned(self.zone.clone()).date()
    }

    fn midnight(&self, date: Date) -> Result<Timestamp, ProductivityError> {
        Ok(date.to_zoned(self.zone.clone())?.timestamp())
    }

    fn available(
        &self,
        start: Timestamp,
        end: Timestamp,
        task: Option<Uuid>,
    ) -> Result<Option<f64>, ProductivityError> {
        let mut total = 0.0;
        let mut date = self.date(start);
        let mut midnight = self.midnight(date)?;
        while midnight < end {
            let next = date.tomorrow()?;
            let next_midnight = self.midnight(next)?;
            let Some(Fields::DailyPlan {
                work_windows: Some(windows),
                budget_minutes,
                buffer_minutes,
                ..
            }) = self.plans.get(&date).map(|entity| &entity.item.fields)
            else {
                return Ok(None);
            };
            let day = Interval {
                start: midnight,
                end: next_midnight,
            };
            let requested = Interval {
                start: start.max(midnight),
                end: end.min(next_midnight),
            };
            let stops = self
                .reservations
                .iter()
                .filter(|(owner, _)| owner.is_none() || task.is_some_and(|id| *owner != Some(id)))
                .map(|(_, interval)| *interval)
                .collect::<Vec<_>>();
            let windows = windows
                .iter()
                .filter_map(|window| clip(*window, day))
                .collect::<Vec<_>>();
            let available =
                covered(&windows, requested) - reserved(&windows, requested, stops.iter().copied());
            let past = Interval {
                start: midnight,
                end: requested.start,
            };
            let consumed = reserved(
                &windows,
                past,
                self.recorded.iter().copied().chain(stops.iter().copied()),
            );
            let future = Interval {
                start: requested.start,
                end: day.end,
            };
            let capacity = budget_minutes.map_or(available, |budget| {
                available.min(
                    i64::from(budget) * 60
                        - consumed
                        - reserved(&windows, future, stops.iter().copied()),
                )
            });
            total += (capacity - i64::from(*buffer_minutes) * 60).max(0) as f64 / 60.0;
            date = next;
            midnight = next_midnight;
        }
        Ok(Some(total))
    }

    fn deadline(
        &self,
        task: Uuid,
        due_at: Option<Timestamp>,
        milestone: Option<Uuid>,
        day: Timestamp,
    ) -> Result<Option<(Timestamp, &'static str)>, ProductivityError> {
        if let Some(deadline) = due_at {
            return Ok(Some((deadline, "task")));
        }
        let stage = milestone
            .and_then(|id| self.catalog.get(id).ok())
            .and_then(|entity| match entity.item.fields {
                Fields::Milestone { due_at, .. } => due_at,
                _ => None,
            })
            .map(|deadline| (deadline, "milestone"));
        let downstream = self
            .catalog
            .entities()
            .filter_map(|item| match &item.item.fields {
                Fields::Task {
                    status,
                    depends_on,
                    due_at,
                    ..
                } if status.is_open() && depends_on.contains(&task) => *due_at,
                _ => None,
            })
            .min()
            .map(|deadline| (deadline, "dependency"));
        let scheduled = self
            .plans
            .range(self.date(day)..)
            .find_map(|(date, plan)| match &plan.item.fields {
                Fields::DailyPlan {
                    tasks,
                    closing: None,
                    ..
                } if tasks.contains(&task) => Some(*date),
                _ => None,
            })
            .map(|date| self.midnight(date.tomorrow()?))
            .transpose()?
            .map(|deadline| (deadline, "day"));
        let block = self
            .reservations
            .iter()
            .filter(|(id, interval)| *id == Some(task) && interval.end > day)
            .map(|(_, interval)| interval.end)
            .min()
            .map(|deadline| (deadline, "block"));
        Ok([stage, downstream, scheduled, block]
            .into_iter()
            .flatten()
            .min_by_key(|(deadline, _)| *deadline))
    }
}

fn covered(windows: &[Interval], bounds: Interval) -> i64 {
    union_seconds(windows.iter().filter_map(|window| clip(*window, bounds)))
}

fn reserved(windows: &[Interval], bounds: Interval, stops: impl Iterator<Item = Interval>) -> i64 {
    union_seconds(
        stops
            .filter_map(|stop| clip(stop, bounds))
            .flat_map(|stop| windows.iter().filter_map(move |window| clip(stop, *window))),
    )
}

fn estimate_samples(catalog: &Catalog, now: Timestamp) -> BTreeMap<String, EstimateSample> {
    let mut records = BTreeMap::<Uuid, Vec<Interval>>::new();
    for entity in catalog.entities() {
        if let Fields::TimeEntry {
            task,
            status: EntryStatus::Finished,
            ..
        } = entity.item.fields
        {
            records
                .entry(task)
                .or_default()
                .extend(entry_intervals(entity, now));
        }
    }
    let mut ratios = BTreeMap::<String, Vec<f64>>::new();
    for task in catalog.entities() {
        if let Fields::Task {
            status: WorkStatus::Done,
            category: Some(category),
            initial_estimate_minutes: Some(estimate),
            exclude_from_estimates: false,
            ..
        } = &task.item.fields
            && *estimate > 0
            && let Some(intervals) = records.get(&task.id)
        {
            let seconds = union_seconds(intervals.iter().copied());
            if seconds > 0 {
                ratios
                    .entry(category.clone())
                    .or_default()
                    .push(seconds as f64 / (f64::from(*estimate) * 60.0));
            }
        }
    }
    ratios
        .into_iter()
        .map(|(category, mut ratios)| {
            ratios.sort_by(f64::total_cmp);
            let percentile = |rank: f64, minimum| {
                (ratios.len() >= minimum)
                    .then(|| ratios[(rank * ratios.len() as f64).ceil() as usize - 1])
            };
            let p50 = (ratios.len() >= 5).then(|| {
                let middle = ratios.len() / 2;
                (ratios[(ratios.len() - 1) / 2] + ratios[middle]) / 2.0
            });
            (
                category,
                EstimateSample {
                    count: ratios.len(),
                    p50,
                    p80: percentile(0.80, 10),
                    p95: percentile(0.95, 20),
                },
            )
        })
        .collect()
}

fn distribution(catalog: &Catalog, actual: &[TimelineItem]) -> Vec<TimeShare> {
    let bounds: BTreeSet<_> = actual
        .iter()
        .flat_map(|item| [item.interval.start, item.interval.end])
        .collect();
    let mut shares = BTreeMap::new();
    for (start, end) in bounds.iter().zip(bounds.iter().skip(1)) {
        let tasks: BTreeSet<_> = actual
            .iter()
            .filter(|item| item.interval.start <= *start && item.interval.end >= *end)
            .filter_map(|item| item.task)
            .collect();
        if tasks.is_empty() {
            continue;
        }
        let overlap = tasks.len() > 1;
        let category = if overlap {
            None
        } else {
            tasks
                .first()
                .and_then(|id| catalog.get(*id).ok())
                .and_then(|task| match &task.item.fields {
                    Fields::Task { category, .. } => category.clone(),
                    _ => None,
                })
        };
        *shares.entry((overlap, category)).or_default() += end.duration_since(*start).as_secs();
    }
    let mut shares = shares
        .into_iter()
        .map(|((overlap, category), seconds)| TimeShare {
            category,
            overlap,
            seconds,
        })
        .collect::<Vec<_>>();
    shares.sort_by_key(|share| std::cmp::Reverse(share.seconds));
    shares
}
