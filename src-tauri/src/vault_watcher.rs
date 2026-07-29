use crate::{
    local_runtime_generation_is_active,
    obsidian::{discover_vaults_for_runtime, resolve_vault_for_runtime, VaultDescriptor},
    runtime_db::{RuntimeDatabase, VAULT_INDEX_BATCH_SIZE},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

const INDEX_WORKER_IDLE_DELAY: Duration = Duration::from_millis(250);
const INDEX_WORKER_BUSY_DELAY: Duration = Duration::from_millis(10);
const INDEX_RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct VaultWatcherState {
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
    worker_started: AtomicBool,
    generation: AtomicU64,
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

fn is_hidden_relative_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).map_or(true, |relative| {
        relative.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_none_or(|value| value.starts_with('.'))
        })
    })
}

pub fn start_vault_watchers(
    app: &AppHandle,
    vaults: &[VaultDescriptor],
    generation: u64,
) -> Result<usize, String> {
    let mut next_watchers = HashMap::new();
    for vault in vaults
        .iter()
        .filter(|vault| vault.connection_state == "connected")
    {
        let vault_id = vault.id.clone();
        let root = std::path::PathBuf::from(&vault.path);
        let callback_app = app.clone();
        let callback_root = root.clone();
        let callback_vault_id = vault_id.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else {
                    if let Err(error) = result {
                        log::warn!("Vault watcher 事件读取失败：{error}");
                    }
                    return;
                };
                for path in event.paths {
                    if !local_runtime_generation_is_active(&callback_app, generation) {
                        return;
                    }
                    if !is_markdown(&path) || is_hidden_relative_path(&callback_root, &path) {
                        continue;
                    }
                    let database = callback_app.state::<RuntimeDatabase>();
                    if let Err(error) =
                        database.enqueue_vault_index_path(&callback_vault_id, &callback_root, &path)
                    {
                        log::warn!("无法持久化 Vault 文件变化 {}：{error}", path.display());
                    }
                }
            })
            .map_err(|error| format!("无法创建 Vault watcher：{error}"))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| format!("无法监听 Vault {}：{error}", root.display()))?;
        next_watchers.insert(vault_id, watcher);
    }

    let count = next_watchers.len();
    let state = app.state::<VaultWatcherState>();
    state.generation.store(generation, Ordering::Release);
    *state
        .watchers
        .lock()
        .map_err(|_| "Vault watcher 状态不可用".to_string())? = next_watchers;
    Ok(count)
}

pub fn reconcile_vaults(app: &AppHandle, vaults: &[VaultDescriptor]) -> usize {
    let database = app.state::<RuntimeDatabase>();
    let mut reconciled = 0;
    for vault in vaults
        .iter()
        .filter(|vault| vault.connection_state == "connected")
    {
        match database.reconcile_vault_index(vault) {
            Ok(result) => {
                reconciled += 1;
                log::info!(
                    "Vault {} 索引校准已入队：{} upsert，{} delete",
                    vault.id,
                    result.queued_upserts,
                    result.queued_deletes
                );
            }
            Err(error) => log::warn!("Vault {} 索引校准失败：{error}", vault.id),
        }
    }
    reconciled
}

pub fn refresh_vault_indexing(app: &AppHandle) -> Result<usize, String> {
    let generation = app
        .state::<VaultWatcherState>()
        .generation
        .load(Ordering::Acquire);
    if generation == 0 || !local_runtime_generation_is_active(app, generation) {
        return Err("本地运行时未激活，无法刷新 Vault 索引".to_string());
    }
    let vaults = discover_vaults_for_runtime()?;
    let database = app.state::<RuntimeDatabase>();
    database.sync_vault_registry(&vaults)?;
    start_vault_watchers(app, &vaults, generation)?;
    Ok(reconcile_vaults(app, &vaults))
}

pub fn request_vault_index_refresh(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = refresh_vault_indexing(&app) {
            log::warn!("刷新 Vault watcher 与索引队列失败：{error}");
        }
    });
}

fn process_vault_index_batch(app: &AppHandle) -> Result<usize, String> {
    let generation = app
        .state::<VaultWatcherState>()
        .generation
        .load(Ordering::Acquire);
    if generation == 0 || !local_runtime_generation_is_active(app, generation) {
        return Ok(0);
    }
    let database = app.state::<RuntimeDatabase>();
    let changes = database.claim_vault_index_changes(VAULT_INDEX_BATCH_SIZE)?;
    let claimed_count = changes.len();
    for change in changes {
        let result = resolve_vault_for_runtime(&change.vault_id).and_then(|(_, current_root)| {
            database.apply_claimed_vault_index_change(&change, &current_root)
        });
        match result {
            Ok(Some(applied)) => {
                if let Err(error) = app.emit(
                    "yunspire://vault-changed",
                    VaultChangePayload {
                        vault_id: applied.vault_id,
                        relative_path: applied.relative_path,
                        change_kind: applied.change_kind,
                    },
                ) {
                    log::warn!("Vault 索引已提交，但界面刷新事件发送失败：{error}");
                }
            }
            Ok(None) => {}
            Err(error) => match database.fail_claimed_vault_index_change(&change, &error) {
                Ok(outcome) if outcome.updated && outcome.terminal => log::warn!(
                    "Vault {} 索引任务重试耗尽：{}；{error}",
                    change.vault_id,
                    change.relative_path
                ),
                Ok(_) => {}
                Err(state_error) => log::warn!(
                    "Vault {} 索引任务失败且无法更新重试状态：{state_error}",
                    change.vault_id
                ),
            },
        }
    }
    Ok(claimed_count)
}

pub fn start_vault_index_worker(app: &AppHandle) {
    let state = app.state::<VaultWatcherState>();
    if state
        .worker_started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_reconcile = Instant::now();
        loop {
            let worker_app = app.clone();
            let claimed = match tauri::async_runtime::spawn_blocking(move || {
                process_vault_index_batch(&worker_app)
            })
            .await
            {
                Ok(Ok(count)) => count,
                Ok(Err(error)) => {
                    log::warn!("Vault 索引 worker 批次失败：{error}");
                    0
                }
                Err(error) => {
                    log::warn!("Vault 索引 worker 无法完成后台批次：{error}");
                    0
                }
            };

            if last_reconcile.elapsed() >= INDEX_RECONCILE_INTERVAL {
                let refresh_app = app.clone();
                match tauri::async_runtime::spawn_blocking(move || {
                    refresh_vault_indexing(&refresh_app)
                })
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => log::warn!("周期性 Vault 索引校准失败：{error}"),
                    Err(error) => log::warn!("周期性 Vault 索引校准任务异常：{error}"),
                }
                last_reconcile = Instant::now();
            }

            tokio::time::sleep(if claimed >= VAULT_INDEX_BATCH_SIZE {
                INDEX_WORKER_BUSY_DELAY
            } else {
                INDEX_WORKER_IDLE_DELAY
            })
            .await;
        }
    });
}

pub fn stop_vault_watchers(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<VaultWatcherState>();
    state.generation.store(0, Ordering::Release);
    state
        .watchers
        .lock()
        .map_err(|_| "Vault watcher 状态不可用".to_string())?
        .clear();
    Ok(())
}
