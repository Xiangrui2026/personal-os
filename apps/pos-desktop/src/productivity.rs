use pos_change::{ChangePlan, ChangeRecord, ExclusiveWrite};
use pos_productivity::{
    Action, Date, DayBoard, Entity, Productivity, ProductivityError, RoadmapGoal, Timestamp,
};
use std::path::PathBuf;

#[tauri::command]
pub(crate) fn startup_vault() -> Option<String> {
    std::env::var("POS_VAULT_ROOT")
        .ok()
        .filter(|path| !path.trim().is_empty())
}

async fn read<T: Send + 'static>(
    root: PathBuf,
    operation: impl FnOnce(Productivity) -> Result<T, ProductivityError> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || operation(Productivity::open(root)?))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn day(
    root: PathBuf,
    date: Option<Date>,
    latest: Option<bool>,
    timezone: String,
) -> Result<DayBoard, String> {
    read(root, move |engine| {
        let now = Timestamp::now();
        let date = match date {
            Some(date) => date,
            None => Productivity::today(&timezone, now)?,
        };
        if latest.unwrap_or(false) {
            engine.day_latest(date, &timezone, now)
        } else {
            engine.day(date, &timezone, now)
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn roadmap(root: PathBuf) -> Result<Vec<RoadmapGoal>, String> {
    read(root, |engine| Ok(engine.catalog()?.roadmap())).await
}

#[tauri::command]
pub(crate) async fn entities(root: PathBuf) -> Result<Vec<Entity>, String> {
    read(root, |engine| {
        Ok(engine.catalog()?.entities().cloned().collect())
    })
    .await
}

#[tauri::command]
pub(crate) fn wall_time(time: String, timezone: String) -> Result<Timestamp, String> {
    Productivity::wall_time(&time, &timezone).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn preview(
    root: PathBuf,
    action: Action,
    reason: Option<String>,
) -> Result<ChangePlan, String> {
    read(root, move |engine| {
        engine.preview(action, "desktop", reason, Timestamp::now())
    })
    .await
}

#[tauri::command]
pub(crate) async fn apply(
    root: PathBuf,
    plan: ChangePlan,
    exclusive: bool,
) -> Result<ChangeRecord, String> {
    if !exclusive {
        return Err("先确认目标文件的写入范围和独占窗口".into());
    }
    read(root, move |engine| {
        engine.apply(&plan, &ExclusiveWrite::confirmed())
    })
    .await
}
