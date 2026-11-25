use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const DEFAULT_ZONE: &str = "Asia/Shanghai";
pub(crate) const PROPERTY: &str = "pos_productivity";
pub(crate) const SCHEMA: u32 = 1;

#[derive(Debug, Error)]
pub enum ProductivityError {
    #[error(transparent)]
    Vault(#[from] pos_vault::VaultError),
    #[error(transparent)]
    Change(#[from] pos_change::ChangeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    DateTime(#[from] jiff::Error),
    #[error("{0}")]
    Invalid(String),
}

pub(crate) fn invalid(message: impl Into<String>) -> ProductivityError {
    ProductivityError::Invalid(message.into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    #[default]
    Todo,
    Doing,
    Done,
    Cancelled,
}

impl WorkStatus {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Todo | Self::Doing)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    #[default]
    Finished,
    Running,
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    pub start: Timestamp,
    pub end: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Disposition {
    Move { date: Date },
    Inbox,
    Drop,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClosingDecision {
    pub task: Uuid,
    #[serde(flatten)]
    pub disposition: Disposition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fields {
    Goal {
        #[serde(default)]
        status: WorkStatus,
    },
    Milestone {
        goal: Uuid,
        sequence: u32,
        #[serde(default)]
        status: WorkStatus,
        #[serde(default)]
        exit_criteria: Vec<String>,
    },
    Project {
        #[serde(default)]
        goals: Vec<Uuid>,
        #[serde(default)]
        milestones: Vec<Uuid>,
        #[serde(default)]
        status: WorkStatus,
    },
    Task {
        #[serde(default)]
        status: WorkStatus,
        #[serde(default)]
        project: Option<Uuid>,
        #[serde(default)]
        goals: Vec<Uuid>,
        #[serde(default)]
        milestone: Option<Uuid>,
        #[serde(default)]
        depends_on: Vec<Uuid>,
        #[serde(default)]
        due_at: Option<Timestamp>,
        #[serde(default)]
        initial_estimate_minutes: Option<u32>,
        #[serde(default)]
        estimate_minutes: Option<u32>,
        #[serde(default)]
        category: Option<String>,
    },
    DailyPlan {
        date: Date,
        #[serde(default)]
        tasks: Vec<Uuid>,
        #[serde(default)]
        priority_tasks: Vec<Uuid>,
        #[serde(default)]
        focus_goals: Vec<Uuid>,
        #[serde(default)]
        budget_minutes: Option<u32>,
        #[serde(default)]
        buffer_minutes: u32,
        #[serde(default)]
        closing: Option<Vec<ClosingDecision>>,
    },
    Event {
        interval: Interval,
    },
    TimeBlock {
        task: Uuid,
        interval: Interval,
    },
    TimeEntry {
        task: Uuid,
        #[serde(default)]
        intervals: Vec<Interval>,
        #[serde(default)]
        status: EntryStatus,
        #[serde(default)]
        running_since: Option<Timestamp>,
    },
}

impl Fields {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Goal { .. } => "goal",
            Self::Milestone { .. } => "milestone",
            Self::Project { .. } => "project",
            Self::Task { .. } => "task",
            Self::DailyPlan { .. } => "daily_plan",
            Self::Event { .. } => "event",
            Self::TimeBlock { .. } => "time_block",
            Self::TimeEntry { .. } => "time_entry",
        }
    }

    pub(crate) fn directory(&self) -> &'static str {
        match self {
            Self::Goal { .. } | Self::Milestone { .. } => "Life/Goal",
            Self::Project { .. } | Self::Task { .. } => "Life/Project",
            Self::DailyPlan { .. } | Self::Event { .. } | Self::TimeBlock { .. } => "Life/Plan",
            Self::TimeEntry { .. } => "Journal",
        }
    }

    pub(crate) fn path(&self, id: Uuid) -> String {
        if let Self::DailyPlan { date, .. } = self {
            format!("{}/pos-daily-plan-{date}.md", self.directory())
        } else {
            format!(
                "{}/pos-{}-{id}.md",
                self.directory(),
                self.kind().replace('_', "-")
            )
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Document {
    pub version: u32,
    #[serde(flatten)]
    pub item: Item,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entity {
    pub id: Uuid,
    pub path: String,
    #[serde(flatten)]
    pub item: Item,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Create {
        item: Item,
        #[serde(default)]
        body: String,
    },
    Edit {
        id: Uuid,
        item: Item,
    },
    SetTaskStatus {
        id: Uuid,
        status: WorkStatus,
    },
    ScheduleTask {
        date: Date,
        task: Uuid,
        #[serde(default)]
        priority: bool,
    },
    CloseDay {
        date: Date,
        decisions: Vec<ClosingDecision>,
    },
    FocusStart {
        task: Uuid,
    },
    FocusPause,
    FocusResume,
    FocusStop,
}
