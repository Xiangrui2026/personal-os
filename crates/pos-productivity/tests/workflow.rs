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
    let board = service.day(date, "Asia/Shanghai", at()).unwrap();
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
    let focus = submit(&service, Action::FocusStart { task }, start);
    assert!(service.apply(&stale, &ExclusiveWrite::confirmed()).is_err());
    assert!(!directory.path().join(&stale.request.path).exists());
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
        .day("2025-11-25".parse().unwrap(), "Asia/Shanghai", at())
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
