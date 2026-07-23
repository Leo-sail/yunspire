use crate::{
    local_runtime_generation_is_active,
    obsidian::{OperationEvent, VaultDescriptor},
    runtime_db::RuntimeDatabase,
};
use chrono::Utc;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

const EVENT_DEDUP_WINDOW: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct VaultWatcherState {
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultChangePayload {
    vault_id: String,
    relative_path: String,
    change_kind: String,
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_hidden_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
}

pub fn start_vault_watchers(
    app: &AppHandle,
    vaults: &[VaultDescriptor],
    generation: u64,
) -> Result<usize, String> {
    let state = app.state::<VaultWatcherState>();
    let mut watchers = state
        .watchers
        .lock()
        .map_err(|_| "Vault watcher 状态不可用".to_string())?;
    watchers.clear();

    for vault in vaults
        .iter()
        .filter(|vault| vault.connection_state == "connected")
    {
        let vault_id = vault.id.clone();
        let root = std::path::PathBuf::from(&vault.path);
        let callback_app = app.clone();
        let callback_root = root.clone();
        let callback_vault_id = vault_id.clone();
        let mut recent_events: HashMap<PathBuf, Instant> = HashMap::new();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else { return };
                let change_kind = format!("{:?}", event.kind);
                for path in event.paths {
                    let is_cancelled =
                        || !local_runtime_generation_is_active(&callback_app, generation);
                    if is_cancelled() {
                        return;
                    }
                    if !is_markdown(&path) || is_hidden_path(&path) {
                        continue;
                    }
                    let observed_at = Instant::now();
                    if recent_events.get(&path).is_some_and(|last_seen| {
                        observed_at.duration_since(*last_seen) < EVENT_DEDUP_WINDOW
                    }) {
                        continue;
                    }
                    recent_events.insert(path.clone(), observed_at);
                    if recent_events.len() > 2048 {
                        recent_events.retain(|_, last_seen| {
                            observed_at.duration_since(*last_seen) < Duration::from_secs(60)
                        });
                    }
                    let database = callback_app.state::<RuntimeDatabase>();
                    if database
                        .index_note_path_with_cancellation(
                            &callback_vault_id,
                            &callback_root,
                            &path,
                            &is_cancelled,
                        )
                        .is_ok()
                    {
                        let relative_path = path
                            .strip_prefix(&callback_root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned();
                        let operation_event = OperationEvent {
                            id: Uuid::new_v4().to_string(),
                            task_id: None,
                            trace_id: None,
                            event_type: "vault.note.index".to_string(),
                            state: "success".to_string(),
                            created_at: Utc::now().to_rfc3339(),
                            vault_id: Some(callback_vault_id.clone()),
                            relative_path: Some(relative_path.clone()),
                            detail: format!("本地文件变化已增量更新索引：{change_kind}"),
                        };
                        let _ = database.append_operation_event(&operation_event);
                        let _ = callback_app.emit(
                            "yunspire://vault-changed",
                            VaultChangePayload {
                                vault_id: callback_vault_id.clone(),
                                relative_path,
                                change_kind: change_kind.clone(),
                            },
                        );
                    }
                }
            })
            .map_err(|error| format!("无法创建 Vault watcher：{error}"))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| format!("无法监听 Vault {}：{error}", root.display()))?;
        watchers.insert(vault_id, watcher);
    }

    Ok(watchers.len())
}

pub fn stop_vault_watchers(app: &AppHandle) -> Result<(), String> {
    app.state::<VaultWatcherState>()
        .watchers
        .lock()
        .map_err(|_| "Vault watcher 状态不可用".to_string())?
        .clear();
    Ok(())
}
