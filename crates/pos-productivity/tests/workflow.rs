use pos_change::{ChangeEngine, ExclusiveWrite};
use pos_productivity::{
    Action, ClosingDecision, Disposition, EntryStatus, Fields, Interval, Item, Productivity,
    Timestamp, WorkStatus,
};
use std::{fs, path::Path};
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, Productivity) {
    let directory = tempfile::tempdir().unwrap();
    for subdir in ["Life/Goal", "Life/Project", "Life/Plan", "Journal"] {
        fs::create_dir_all(directory.path().join(subdir)).unwrap();
    }
    ChangeEngine::open(directory.path())
        .unwrap()
        .initialize()
        .unwrap();
    let productivity = Productivity::open(directory.path()).unwrap();
    (directory, productivity)
}

fn clock(time: &str) -> Timestamp {
    time.parse().unwrap()
}

fn at() -> Timestamp {
    clock("2025-11-26T10:00:00+08:00")
}

fn submit(service: &Productivity, action: Action, time: Timestamp) -> Uuid {
    let plan = service.preview(action, "fixture", None, time).unwrap();
    service.apply(&plan, &ExclusiveWrite::confirmed()).unwrap();
    plan.request.change_id
}

fn create(service: &Productivity, title: &str, fields: Fields) -> Uuid {
    submit(
        service,
        Action::Create {
            item: Item {
                title: title.into(),
                fields,
            },
            body: "# Kept in Obsidian\n".into(),
        },
        at(),
    )
}

fn task_fields(project: Option<Uuid>, depends_on: Vec<Uuid>) -> Fields {
    Fields::Task {
        status: WorkStatus::Todo,
        project,
        goals: Vec::new(),
        milestone: None,
        depends_on,
        due_at: None,
        initial_estimate_minutes: Some(70),
        estimate_minutes: None,
        category: None,
        importance: None,
        checkpoints: Vec::new(),
        focus_sessions: Vec::new(),
        outcome: None,
        exclude_from_estimates: false,
    }
}

fn note_count(root: &Path) -> usize {
    pos_vault::Vault::open(root)
        .unwrap()
        .scan()
        .unwrap()
        .stats()
        .unwrap()
        .notes
}

#[test]
fn daily_agenda_repeats_by_date_and_commits_the_selection_once() {
    use pos_productivity::{PlanSlot, RoutineStatus};
    use std::collections::BTreeMap;
    let (dir, svc) = fixture();
    let date = "2025-11-26".parse().unwrap();
    let next = "2025-11-27".parse().unwrap();
    let start = clock("2025-11-26T09:00:00+08:00");
    let finish = clock("2025-11-26T12:00:00+08:00");
    let rule = create(
        &svc,
        "Read",
        Fields::Routine {
            from: date,
            until: None,
            minutes: 30,
            start: None,
            goal: None,
        },
    );
    let task = create(&svc, "Draft", task_fields(None, vec![]));
    let urgent = create(
        &svc,
        "Review",
        serde_json::from_value(serde_json::json!({
            "type":"task","importance":90,"initial_estimate_minutes":100,"due_at":finish
        }))
        .unwrap(),
    );
    let before = svc
        .catalog()
        .unwrap()
        .agenda(date, "Asia/Shanghai", start)
        .unwrap();
    assert_eq!(before.pool.len(), 2);
    assert_eq!(before.board.routines.len(), 1);
    assert!(before.board.plan.is_none());
    let mut routines = before.board.routines;
    routines.get_mut(&rule).unwrap().start = Some("11:00".parse().unwrap());
    let save = Action::SaveAgenda {
        date,
        base: None,
        tasks: vec![task],
        routines,
        slots: BTreeMap::from([(
            task,
            PlanSlot {
                start: "09:00".parse().unwrap(),
                minutes: 60,
            },
        )]),
        budget_minutes: Some(180),
        work_windows: Some(vec![Interval { start, end: finish }]),
        buffer_minutes: 10,
    };
    let points = svc
        .agenda_draft(save.clone(), "Asia/Shanghai", start)
        .unwrap();
    let point = points.iter().find(|p| p.task == urgent).unwrap();
    assert_eq!(point.available_minutes, Some(80.0));
    assert_eq!(point.quadrant, Some(1));
    assert!(
        points
            .iter()
            .find(|p| p.task == task)
            .unwrap()
            .deadline
            .is_none()
    );
    assert!(svc.catalog().unwrap().plan(date).unwrap().is_none());
    let changes = ChangeEngine::open(dir.path()).unwrap();
    let count = changes.list().unwrap().len();
    submit(&svc, save.clone(), start);
    assert_eq!(changes.list().unwrap().len(), count + 1);
    assert!(svc.preview(save, "fixture", None, start).is_err());
    let board = svc.day_latest(date, "Asia/Shanghai", start).unwrap();
    assert_eq!(board.totals.planned_work_seconds, 90 * 60);
    assert!(
        board
            .tasks
            .iter()
            .find(|r| r.task.id == task)
            .unwrap()
            .planned
    );
    submit(
        &svc,
        Action::SetRoutineStatus {
            date,
            rule,
            status: RoutineStatus::Done,
        },
        start,
    );
    let tomorrow = svc.day_latest(next, "Asia/Shanghai", start).unwrap();
    assert_eq!(tomorrow.routines[&rule].status, RoutineStatus::Todo);
    assert!(tomorrow.routines[&rule].start.is_none());
    assert_eq!(tomorrow.routine_week_done[&rule], 1);
    submit(
        &svc,
        Action::SetRoutineStatus {
            date: next,
            rule,
            status: RoutineStatus::Skipped,
        },
        start,
    );
    assert_eq!(
        svc.day_latest(date, "Asia/Shanghai", start)
            .unwrap()
            .routines[&rule]
            .status,
        RoutineStatus::Done
    );
}

#[test]
fn board_keeps_checkpoints_time_and_original_arrangements_separate() {
    let (_directory, service) = fixture();
    let date = "2025-11-26".parse().unwrap();
    let time = |clock_text: &str| clock(&format!("2025-11-26T{clock_text}:00+08:00"));
    let interval = |start, end| Interval {
        start: time(start),
        end: time(end),
    };
    let task = create(
        &service,
        "Proof",
        serde_json::from_value(serde_json::json!({
            "type":"task", "importance":85, "initial_estimate_minutes":60, "due_at":time("12:00"),
            "checkpoints":[{"title":"Outline"},{"title":"Verify"}]
        }))
        .unwrap(),
    );
    create(
        &service,
        "Plan",
        Fields::DailyPlan {
            layout: Default::default(),
            date,
            tasks: vec![task],
            priority_tasks: vec![task],
            focus_goals: Vec::new(),
            budget_minutes: None,
            buffer_minutes: 10,
            work_windows: Some(vec![interval("09:00", "12:00"), interval("13:00", "14:00")]),
            strategic_minutes: Some(40),
            importance_threshold: None,
            urgency_percent: None,
            closing: None,
        },
    );
    let event = create(
        &service,
        "Meeting",
        Fields::Event {
            interval: interval("10:30", "11:00"),
        },
    );
    submit(
        &service,
        Action::Edit {
            id: event,
            item: Item {
                title: "Meeting".into(),
                fields: Fields::Event {
                    interval: interval("10:45", "11:15"),
                },
            },
        },
        at(),
    );
    create(
        &service,
        "Proof block",
        Fields::TimeBlock {
            task,
            interval: interval("11:15", "11:45"),
        },
    );
    for (start, end) in [("09:00", "09:30"), ("09:20", "09:40")] {
        create(
            &service,
            "Proof log",
            Fields::TimeEntry {
                task,
                intervals: vec![interval(start, end)],
                status: EntryStatus::Finished,
                running_since: None,
            },
        );
    }
    submit(
        &service,
        Action::SetCheckpoint {
            id: task,
            title: "Outline".into(),
            done: true,
        },
        at(),
    );
    let mut item = service.catalog().unwrap().get(task).unwrap().item.clone();
    if let Fields::Task {
        estimate_minutes, ..
    } = &mut item.fields
    {
        *estimate_minutes = Some(90);
    }
    let revision = service
        .preview(
            Action::Edit { id: task, item },
            "fixture",
            Some("harder proof".into()),
            at(),
        )
        .unwrap();
    service
        .apply(&revision, &ExclusiveWrite::confirmed())
        .unwrap();
    let board = service.day(date, "Asia/Shanghai", at()).unwrap();
    assert_eq!(
        board.initial.as_ref().unwrap().scheduled[0].interval,
        interval("10:30", "11:00")
    );
    assert_eq!(board.scheduled[0].interval, interval("10:45", "11:15"));
    assert_eq!(board.totals.actual_seconds, 40 * 60);
    assert_eq!(board.planning.distribution[0].seconds, 40 * 60);
    assert_eq!(board.planning.remaining_work_minutes, Some(140.0));
    assert_eq!(board.planning.priorities[0].available_minutes, Some(80.0));
    assert_eq!(board.planning.priorities[0].pressure, Some(50.0 / 80.0));
    assert_eq!(board.planning.priorities[0].quadrant, Some(2));
    assert_eq!(board.planning.strategic_scheduled_minutes, 30.0);
    assert_eq!(board.estimate_revisions[0].before, Some(60));
    assert_eq!(board.estimate_revisions[0].after, Some(90));
    assert_eq!(
        board.estimate_revisions[0].reason.as_deref(),
        Some("harder proof")
    );
    assert!(
        matches!(&board.tasks[0].task.item.fields, Fields::Task { checkpoints, .. }
        if checkpoints[0].done && !checkpoints[1].done)
    );
}

#[test]
fn calibration_requires_comparable_completed_tasks_and_reports_sample_limits() {
    let (_directory, service) = fixture();
    let date = "2025-11-26".parse().unwrap();
    for index in 0..7 {
        let excluded = index == 5;
        let status = if index == 6 { "doing" } else { "done" };
        let task = create(
            &service,
            &format!("Study {index}"),
            serde_json::from_value(serde_json::json!({
                "type":"task", "status":status, "category":"study", "initial_estimate_minutes":30,
                "exclude_from_estimates":excluded
            }))
            .unwrap(),
        );
        create(
            &service,
            &format!("Log {index}"),
            Fields::TimeEntry {
                task,
                intervals: vec![Interval {
                    start: clock("2025-11-25T13:00:00+08:00"),
                    end: clock("2025-11-25T14:00:00+08:00"),
                }],
                status: EntryStatus::Finished,
                running_since: None,
            },
        );
        if index == 6 {
            submit(
                &service,
                Action::ScheduleTask {
                    date,
                    task,
                    priority: false,
                },
                at(),
            );
        }
    }
    let board = service.day(date, "Asia/Shanghai", at()).unwrap();
    let estimate = &board.planning.estimates["study"];
    assert_eq!(estimate.count, 5);
    assert_eq!(estimate.p50, Some(2.0));
    assert_eq!(estimate.p80, None);
    assert_eq!(estimate.p95, None);
    assert_eq!(board.planning.remaining_work_minutes, None);
    assert_eq!(board.planning.priorities[0].quadrant, None);
}

#[test]
fn roadmap_day_and_carry_over_reference_one_task() {
    let (directory, service) = fixture();
    let goal = create(
        &service,
        "Research",
        Fields::Goal {
            status: WorkStatus::Doing,
        },
    );
    let milestone = create(
        &service,
        "Phase 2",
        Fields::Milestone {
            goal,
            sequence: 2,
            status: WorkStatus::Doing,
            exit_criteria: vec!["Draft".into()],
            due_at: None,
        },
    );
    let project = create(
        &service,
        "Study",
        Fields::Project {
            goals: vec![goal],
            milestones: vec![milestone],
            status: WorkStatus::Doing,
        },
    );
    let task = create(&service, "Read", task_fields(Some(project), Vec::new()));
    let date = "2025-11-25".parse().unwrap();
    let next = "2025-11-26".parse().unwrap();
    submit(
        &service,
        Action::ScheduleTask {
            date,
            task,
            priority: true,
        },
        at(),
    );
    let board = service.day_latest(date, "Asia/Shanghai", at()).unwrap();
    assert_eq!(board.tasks[0].task.id, task);
    assert!(board.tasks[0].priority);
    assert_eq!(board.focus_goals[0].current_milestone, Some(milestone));
    assert_eq!(board.focus_goals[0].projects[0].id, project);
    assert!(
        service
            .preview(
                Action::CloseDay {
                    date,
                    decisions: Vec::new()
                },
                "fixture",
                None,
                at()
            )
            .is_err()
    );
    submit(
        &service,
        Action::CloseDay {
            date,
            decisions: vec![ClosingDecision {
                task,
                disposition: Disposition::Move { date: next },
            }],
        },
        at(),
    );
    let moved = service.day(next, "Asia/Shanghai", at()).unwrap();
    assert_eq!(moved.tasks[0].task.id, task);
    assert_eq!(
        service
            .catalog()
            .unwrap()
            .entities()
            .filter(|entity| matches!(entity.item.fields, Fields::Task { .. }))
            .count(),
        1
    );
    assert_eq!(note_count(directory.path()), 5);
}

#[test]
fn focus_is_persistent_and_a_stale_start_cannot_create_a_second_session() {
    let (directory, service) = fixture();
    let task = create(&service, "Read", task_fields(None, Vec::new()));
    let other_task = create(&service, "Write", task_fields(None, Vec::new()));
    let start = clock("2025-11-25T09:00:00+08:00");
    let pause = clock("2025-11-25T09:15:00+08:00");
    let resume = clock("2025-11-25T09:30:00+08:00");
    let end = clock("2025-11-25T09:40:00+08:00");
    let stale = service
        .preview(
            Action::FocusStart { task: other_task },
            "fixture",
            None,
            start,
        )
        .unwrap();
    let stale_target = directory.path().join(&stale.request.path);
    let before = fs::read(&stale_target).unwrap();
    let focus = submit(&service, Action::FocusStart { task }, start);
    assert!(service.apply(&stale, &ExclusiveWrite::confirmed()).is_err());
    assert_eq!(fs::read(stale_target).unwrap(), before);
    submit(&service, Action::FocusPause, pause);
    let reopened = Productivity::open(directory.path()).unwrap();
    assert_eq!(
        reopened.catalog().unwrap().focus().unwrap().unwrap().id,
        focus
    );
    submit(&reopened, Action::FocusResume, resume);
    submit(&reopened, Action::FocusStop, end);
    let catalog = reopened.catalog().unwrap();
    assert!(catalog.focus().unwrap().is_none());
    assert!(
        matches!(&catalog.get(focus).unwrap().item.fields, Fields::TimeEntry { status: EntryStatus::Finished, intervals, .. } if intervals.len() == 2)
    );
    let board = reopened
        .day("2025-11-25".parse().unwrap(), "Asia/Shanghai", end)
        .unwrap();
    assert_eq!(board.totals.actual_seconds, 1_500);
}

#[test]
fn day_clips_intervals_and_counts_overlapping_actual_time_once() {
    let (_directory, service) = fixture();
    let task = create(&service, "Read", task_fields(None, Vec::new()));
    let start = clock("2025-11-25T23:30:00+08:00");
    let end = clock("2025-11-26T00:30:00+08:00");
    let middle = clock("2025-11-25T23:45:00+08:00");
    create(
        &service,
        "Read block",
        Fields::TimeBlock {
            task,
            interval: Interval { start, end },
        },
    );
    create(
        &service,
        "Event",
        Fields::Event {
            interval: Interval { start: middle, end },
        },
    );
    for start in [start, middle] {
        create(
            &service,
            "Read log",
            Fields::TimeEntry {
                task,
                intervals: vec![Interval { start, end }],
                status: EntryStatus::Finished,
                running_since: None,
            },
        );
    }
    let board = service
        .day_latest("2025-11-25".parse().unwrap(), "Asia/Shanghai", at())
        .unwrap();
    assert_eq!(board.totals.planned_work_seconds, 1_800);
    assert_eq!(board.totals.fixed_event_seconds, 900);
    assert_eq!(board.totals.actual_seconds, 1_800);
    assert_eq!(board.totals.remaining_estimate_minutes, 10);
    assert_eq!(board.overlaps.len(), 1);
}

#[test]
fn validates_dependencies_preserves_body_and_updates_through_change() {
    let (directory, service) = fixture();
    let first = create(&service, "First", task_fields(None, Vec::new()));
    let next = create(&service, "Second", task_fields(None, vec![first]));
    let item = Item {
        title: "First".into(),
        fields: task_fields(None, vec![next]),
    };
    assert!(
        service
            .preview(Action::Edit { id: first, item }, "fixture", None, at())
            .is_err()
    );
    let plan = service
        .preview(
            Action::SetTaskStatus {
                id: first,
                status: WorkStatus::Done,
            },
            "fixture",
            None,
            at(),
        )
        .unwrap();
    let filepath = directory.path().join(&plan.request.path);
    assert!(
        fs::read_to_string(&filepath)
            .unwrap()
            .contains("\"status\":\"todo\"")
    );
    service.apply(&plan, &ExclusiveWrite::confirmed()).unwrap();
    assert!(
        fs::read_to_string(&filepath)
            .unwrap()
            .ends_with("# Kept in Obsidian\n")
    );
    let changes = ChangeEngine::open(directory.path()).unwrap();
    assert_eq!(changes.list().unwrap().len(), 3);
    let undo = changes
        .preview_revert(plan.request.change_id, "fixture".into(), None, None)
        .unwrap();
    changes.apply(&undo, &ExclusiveWrite::confirmed()).unwrap();
    assert!(matches!(
        service.catalog().unwrap().get(first).unwrap().item.fields,
        Fields::Task {
            status: WorkStatus::Todo,
            ..
        }
    ));
}

#[test]
fn focus_checkpoints_unlock_the_successor_and_roll_up_in_one_change() {
    let (directory, service) = fixture();
    let date = Productivity::today("Asia/Shanghai", at()).unwrap();
    let goal = create(
        &service,
        "Publish",
        Fields::Goal {
            status: WorkStatus::Doing,
        },
    );
    let milestone = create(
        &service,
        "Draft",
        serde_json::from_value(serde_json::json!({
            "type":"milestone","goal":goal,"sequence":1,"exit_criteria":["Reviewed"]
        }))
        .unwrap(),
    );
    let project = create(
        &service,
        "Paper",
        serde_json::from_value(serde_json::json!({
            "type":"project","goals":[goal]
        }))
        .unwrap(),
    );
    let first = create(&service, "Outline", serde_json::from_value(serde_json::json!({
        "type":"task","goals":[goal],"milestone":milestone,"project":project,
        "checkpoints":[{"title":"Structure"},{"title":"References"}], "initial_estimate_minutes":5
    })).unwrap());
    let next = create(&service, "Review", task_fields(None, vec![first]));
    for task in [first, next] {
        submit(
            &service,
            Action::ScheduleTask {
                date,
                task,
                priority: task == next,
            },
            at(),
        );
    }
    for action in [
        Action::FocusStart { task: next },
        Action::SetTaskStatus {
            id: next,
            status: WorkStatus::Done,
        },
        Action::SetTaskStatus {
            id: first,
            status: WorkStatus::Done,
        },
    ] {
        assert!(service.preview(action, "test", None, at()).is_err());
    }
    let checkpoint = |title: &str, done| Action::SetCheckpoint {
        id: first,
        title: title.into(),
        done,
    };
    submit(&service, Action::FocusStart { task: first }, at());
    submit(&service, checkpoint("Structure", true), at());
    let end = clock("2025-11-26T10:06:00+08:00");
    let in_progress = service.day(date, "Asia/Shanghai", end).unwrap();
    let row = in_progress
        .tasks
        .iter()
        .find(|row| row.task.id == first)
        .unwrap();
    assert!(matches!(
        row.task.item.fields,
        Fields::Task {
            status: WorkStatus::Doing,
            ..
        }
    ));
    assert!(row.needs_reestimate);
    assert_eq!(row.remaining_minutes, None);
    assert_eq!(in_progress.planning.workload_ratio, None);
    assert_eq!(in_progress.next_task, Some(first));
    submit(
        &service,
        Action::ReestimateRemaining {
            id: first,
            minutes: 10,
        },
        end,
    );
    let revised = service.day(date, "Asia/Shanghai", end).unwrap();
    let row = revised
        .tasks
        .iter()
        .find(|row| row.task.id == first)
        .unwrap();
    assert_eq!(row.remaining_minutes, Some(10.0));
    assert!(!row.needs_reestimate);
    assert!(matches!(
        row.task.item.fields,
        Fields::Task {
            initial_estimate_minutes: Some(5),
            estimate_minutes: Some(16),
            ..
        }
    ));
    let changes = ChangeEngine::open(directory.path()).unwrap();
    let count = changes.list().unwrap().len();
    submit(&service, checkpoint("References", true), end);
    assert_eq!(changes.list().unwrap().len(), count + 1);
    let board = service.day(date, "Asia/Shanghai", end).unwrap();
    assert_eq!(board.next_task, Some(next));
    assert!(board.focus.is_none());
    assert_eq!(board.totals.actual_seconds, 360);
    assert_eq!(board.totals.completed, 1);
    for parent in [project, milestone, goal] {
        let catalog = service.catalog().unwrap();
        let progress = catalog.get(parent).unwrap().progress.as_ref().unwrap();
        assert_eq!(
            (
                progress.tasks,
                progress.completed,
                progress.checkpoints,
                progress.checked
            ),
            (1, 1, 2, 2)
        );
    }
    let approved = service
        .preview(Action::FocusStart { task: next }, "test", None, end)
        .unwrap();
    submit(&service, checkpoint("References", false), end);
    assert!(
        service
            .apply(&approved, &ExclusiveWrite::confirmed())
            .is_err()
    );
    assert!(service.catalog().unwrap().focus().unwrap().is_none());
}

#[test]
fn historical_projection_uses_only_versions_recorded_before_cutoff() {
    let (directory, service) = fixture();
    let date = Productivity::today("Asia/Shanghai", at()).unwrap();
    let task = create(
        &service,
        "Before",
        serde_json::from_value(serde_json::json!({
            "type":"task","checkpoints":[{"title":"First"},{"title":"Last"}]
        }))
        .unwrap(),
    );
    submit(
        &service,
        Action::ScheduleTask {
            date,
            task,
            priority: true,
        },
        at(),
    );
    submit(
        &service,
        Action::SetCheckpoint {
            id: task,
            title: "First".into(),
            done: true,
        },
        at(),
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    let completion = submit(
        &service,
        Action::SetCheckpoint {
            id: task,
            title: "Last".into(),
            done: true,
        },
        at(),
    );
    let cutoff = ChangeEngine::open(directory.path())
        .unwrap()
        .show(completion)
        .unwrap()
        .executed_unix_ms;
    let snapshot = service
        .day_as_of(
            date,
            "Asia/Shanghai",
            Timestamp::from_millisecond(cutoff).unwrap(),
        )
        .unwrap();
    assert_eq!(snapshot.tasks.len(), 1);
    assert!(
        matches!(&snapshot.tasks[0].task.item.fields, Fields::Task { status: WorkStatus::Doing,checkpoints,.. }
        if checkpoints.iter().filter(|p| p.done).count() == 1)
    );
    assert_eq!(snapshot.totals.completed, 0);
    assert_eq!(
        service
            .day_latest(date, "Asia/Shanghai", at())
            .unwrap()
            .totals
            .completed,
        1
    );
}
