use crate::runtime_db::RuntimeDatabase;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskControlAction {
    Queue,
    Start,
    Pause,
    Resume,
    Cancel,
    Retry,
    Succeed,
    Fail,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTransitionInput {
    pub task_id: String,
    pub action: TaskControlAction,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub checkpoint: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeTask {
    pub id: String,
    pub state: String,
    pub title: String,
    pub trace_id: Option<String>,
    pub progress: u8,
    pub payload: Value,
    pub created_at: String,
    pub updated_at: String,
}

fn target_state(action: &TaskControlAction) -> &'static str {
    match action {
        TaskControlAction::Queue | TaskControlAction::Retry | TaskControlAction::Resume => "queued",
        TaskControlAction::Start => "running",
        TaskControlAction::Pause => "paused",
        TaskControlAction::Cancel => "cancelled",
        TaskControlAction::Succeed => "succeeded",
        TaskControlAction::Fail => "failed",
    }
}

pub(crate) fn valid_task_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match from {
        "created" => matches!(to, "queued" | "cancelled" | "failed"),
        "queued" => matches!(to, "running" | "paused" | "cancelled" | "failed"),
        "running" => matches!(
            to,
            "awaiting_approval" | "paused" | "succeeded" | "failed" | "cancelled"
        ),
        "awaiting_approval" => matches!(to, "queued" | "running" | "cancelled" | "failed"),
        "paused" => matches!(to, "queued" | "cancelled" | "failed"),
        "failed" => matches!(to, "queued" | "cancelled"),
        "succeeded" | "cancelled" => false,
        _ => false,
    }
}

fn default_progress(action: &TaskControlAction, current: u8) -> u8 {
    match action {
        TaskControlAction::Queue | TaskControlAction::Retry | TaskControlAction::Resume => current,
        TaskControlAction::Start => current.max(1),
        TaskControlAction::Pause => current,
        TaskControlAction::Cancel | TaskControlAction::Fail => current,
        TaskControlAction::Succeed => 100,
    }
}

#[tauri::command]
pub fn transition_runtime_task(
    database: State<'_, RuntimeDatabase>,
    input: TaskTransitionInput,
) -> Result<NativeRuntimeTask, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let target = target_state(&input.action);
    let current = database.runtime_task(&workspace_scope, &input.task_id)?;
    let progress = input
        .progress
        .unwrap_or_else(|| default_progress(&input.action, current.progress))
        .min(100);
    database.transition_native_runtime_task(
        &workspace_scope,
        &input.task_id,
        target,
        progress,
        &input.detail,
        input.checkpoint.as_ref(),
    )
}

#[tauri::command]
pub fn get_runtime_task(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
) -> Result<NativeRuntimeTask, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.runtime_task(&workspace_scope, &task_id)
}

#[tauri::command]
pub fn list_runtime_tasks(
    database: State<'_, RuntimeDatabase>,
    state: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<NativeRuntimeTask>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_runtime_tasks(&workspace_scope, state.as_deref(), limit.unwrap_or(200))
}
