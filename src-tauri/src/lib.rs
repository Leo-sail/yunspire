mod capture_pipeline;
mod command_bus;
mod connectors;
mod model_config;
mod model_provider;
mod obsidian;
mod obsidian_management;
mod policy;
mod runtime_db;
mod scheduler;
mod task_runtime;
mod updater;
mod vault_watcher;

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver},
    Arc, Mutex,
};
use tauri::{AppHandle, Manager, State};

struct BackgroundVaultIndexTask {
    cancellation: Arc<AtomicBool>,
    completed: Receiver<()>,
}

#[derive(Default)]
struct LocalRuntimeInitializationState {
    initialized: AtomicBool,
    generation: AtomicU64,
    lock: Mutex<()>,
    index_task: Mutex<Option<BackgroundVaultIndexTask>>,
}

const COMMANDS_AVAILABLE_WITHOUT_APPLICATION_AUTHORIZATION: [&str; 2] = [
    "load_application_authorization",
    "update_application_authorization",
];

fn command_available_without_application_authorization(command: &str) -> bool {
    COMMANDS_AVAILABLE_WITHOUT_APPLICATION_AUTHORIZATION.contains(&command)
}

fn initialize_local_runtime(
    app: &AppHandle,
    database: &runtime_db::RuntimeDatabase,
    generation: u64,
) -> Result<Vec<obsidian::VaultDescriptor>, String> {
    match capture_pipeline::cleanup_expired_capture_staging() {
        Ok(report) => {
            let removed = report.removed_upload_parts
                + report.removed_attachments
                + report.removed_claimed_attachments;
            if removed > 0 || report.failed_removals > 0 {
                log::info!(
                    "采集暂存清理完成：分块={}，附件={}，认领附件={}，失败={}",
                    report.removed_upload_parts,
                    report.removed_attachments,
                    report.removed_claimed_attachments,
                    report.failed_removals
                );
            }
        }
        Err(error) => log::warn!("无法清理过期采集暂存文件：{error}"),
    }
    let workspace_scope = database.local_workspace_scope()?;
    if database.should_initialize_default_vaults(&workspace_scope)? {
        obsidian::ensure_default_vaults_for_runtime()?;
        database.mark_default_vaults_initialized(&workspace_scope)?;
    }
    let memory_state = app.state::<obsidian::ObsidianAdapterState>();
    if let Err(error) = obsidian::flush_pending_long_term_memory_events_for_runtime(
        database,
        &workspace_scope,
        memory_state.inner(),
    ) {
        log::warn!("无法重放长期记忆待写入事件：{error}");
    }
    let vaults = obsidian::discover_vaults_for_runtime().unwrap_or_default();
    database.sync_vault_registry(&vaults)?;
    vault_watcher::start_vault_watchers(app, &vaults, generation)?;
    Ok(vaults)
}

fn start_background_vault_indexing(
    app: &AppHandle,
    vaults: Vec<obsidian::VaultDescriptor>,
    generation: u64,
) {
    let state = app.state::<LocalRuntimeInitializationState>();
    let Ok(mut task_slot) = state.index_task.lock() else {
        log::warn!("Vault 索引任务状态不可用");
        return;
    };
    if let Some(previous) = task_slot.take() {
        previous.cancellation.store(true, Ordering::Release);
    }
    let cancellation = Arc::new(AtomicBool::new(false));
    let task_cancellation = Arc::clone(&cancellation);
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let database = app_handle.state::<runtime_db::RuntimeDatabase>();
        for vault in vaults {
            let is_cancelled = || {
                task_cancellation.load(Ordering::Acquire)
                    || !local_runtime_generation_is_active(&app_handle, generation)
            };
            if is_cancelled() {
                break;
            }
            if vault.connection_state == "connected" {
                if let Err(error) =
                    database.rebuild_index_for_vault_with_cancellation(&vault.id, &is_cancelled)
                {
                    if !is_cancelled() {
                        log::warn!("无法重建 Vault {} 的索引：{error}", vault.name);
                    }
                }
            }
        }
        let _ = completed_tx.send(());
    });
    *task_slot = Some(BackgroundVaultIndexTask {
        cancellation,
        completed: completed_rx,
    });
}

pub(crate) fn local_runtime_generation_is_active(app: &AppHandle, generation: u64) -> bool {
    let state = app.state::<LocalRuntimeInitializationState>();
    state.initialized.load(Ordering::Acquire)
        && state.generation.load(Ordering::Acquire) == generation
}

fn activate_local_runtime(
    app: &AppHandle,
    state: &LocalRuntimeInitializationState,
    vaults: Vec<obsidian::VaultDescriptor>,
    generation: u64,
) {
    state.initialized.store(true, Ordering::Release);
    scheduler::start_scheduler(app);
    start_background_vault_indexing(app, vaults, generation);
}

fn initialize_local_runtime_once(
    app: &AppHandle,
    database: &runtime_db::RuntimeDatabase,
) -> Result<(), String> {
    let state = app.state::<LocalRuntimeInitializationState>();
    if state.initialized.load(Ordering::Acquire) {
        return Ok(());
    }
    let _guard = state
        .lock
        .lock()
        .map_err(|_| "云枢本地运行时初始化锁不可用".to_string())?;
    if state.initialized.load(Ordering::Acquire) {
        return Ok(());
    }
    let generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let vaults = initialize_local_runtime(app, database, generation)?;
    activate_local_runtime(app, state.inner(), vaults, generation);
    Ok(())
}

fn suspend_local_runtime_locked(
    app: &AppHandle,
    state: &LocalRuntimeInitializationState,
) -> Result<(), String> {
    state.initialized.store(false, Ordering::Release);
    state.generation.fetch_add(1, Ordering::AcqRel);
    scheduler::pause_scheduler(app);
    let mut failures = Vec::new();
    let index_task = match state.index_task.lock() {
        Ok(mut task) => task.take(),
        Err(_) => {
            failures.push("Vault 索引任务状态不可用".to_string());
            None
        }
    };
    if let Some(task) = index_task.as_ref() {
        task.cancellation.store(true, Ordering::Release);
    }
    if let Err(error) = vault_watcher::stop_vault_watchers(app) {
        failures.push(error);
    }
    if let Some(task) = index_task {
        let _ = task.completed.recv();
    }
    if let Err(error) = model_provider::suspend_model_runtime(app) {
        failures.push(error);
    }
    if let Err(error) = capture_pipeline::suspend_capture_runtime(app) {
        failures.push(error);
    }
    if let Err(error) = obsidian::clear_pending_operations_for_runtime(
        app.state::<obsidian::ObsidianAdapterState>().inner(),
    ) {
        failures.push(error);
    }
    if let Err(error) = obsidian_management::clear_pending_deletes_for_runtime(
        app.state::<obsidian_management::ObsidianManagementState>()
            .inner(),
    ) {
        failures.push(error);
    }
    if !failures.is_empty() {
        log::warn!(
            "撤销统一授权时部分临时运行状态清理失败：{}",
            failures.join("；")
        );
    }
    Ok(())
}

fn persist_grant_after_runtime_preparation<T, F>(
    database: &runtime_db::RuntimeDatabase,
    prepare: F,
) -> Result<(runtime_db::ApplicationAuthorizationState, T), String>
where
    F: FnOnce() -> Result<T, String>,
{
    let prepared = prepare()?;
    let authorization = database.set_application_authorization(true)?;
    Ok((authorization, prepared))
}

fn grant_application_authorization(
    app: &AppHandle,
    database: &runtime_db::RuntimeDatabase,
) -> Result<runtime_db::ApplicationAuthorizationState, String> {
    let state = app.state::<LocalRuntimeInitializationState>();
    let _guard = state
        .lock
        .lock()
        .map_err(|_| "云枢本地运行时初始化锁不可用".to_string())?;
    if state.initialized.load(Ordering::Acquire) {
        return database.set_application_authorization(true);
    }
    let generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let prepared = persist_grant_after_runtime_preparation(database, || {
        initialize_local_runtime(app, database, generation)
    });
    let (authorization, vaults) = match prepared {
        Ok(result) => result,
        Err(error) => {
            let _ = suspend_local_runtime_locked(app, state.inner());
            return Err(error);
        }
    };
    activate_local_runtime(app, state.inner(), vaults, generation);
    Ok(authorization)
}

fn revoke_application_authorization(
    app: &AppHandle,
    database: &runtime_db::RuntimeDatabase,
) -> Result<runtime_db::ApplicationAuthorizationState, String> {
    let state = app.state::<LocalRuntimeInitializationState>();
    let _guard = state
        .lock
        .lock()
        .map_err(|_| "云枢本地运行时初始化锁不可用".to_string())?;
    suspend_local_runtime_locked(app, state.inner())?;
    database.set_application_authorization(false)
}

#[tauri::command]
fn update_application_authorization(
    app: AppHandle,
    database: State<'_, runtime_db::RuntimeDatabase>,
    granted: bool,
) -> Result<runtime_db::ApplicationAuthorizationState, String> {
    if granted {
        grant_application_authorization(&app, database.inner())
    } else {
        revoke_application_authorization(&app, database.inner())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(obsidian::ObsidianAdapterState::default())
        .manage(obsidian_management::ObsidianManagementState::default())
        .manage(model_provider::ModelAnalysisState::default())
        .manage(model_provider::ModelIntentState::default())
        .manage(model_provider::ModelRequestState::default())
        .manage(capture_pipeline::CaptureAuthorizationState::default())
        .manage(capture_pipeline::CaptureTaskState::default())
        .manage(capture_pipeline::CaptureUploadState::default())
        .manage(vault_watcher::VaultWatcherState::default())
        .manage(scheduler::SchedulerState::default())
        .manage(LocalRuntimeInitializationState::default())
        .invoke_handler({
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                model_config::save_model_provider,
                model_config::load_model_providers,
                model_config::delete_model_provider,
                command_bus::evaluate_application_command,
                command_bus::submit_application_command,
                connectors::save_external_connector,
                connectors::load_external_connectors,
                connectors::delete_external_connector,
                connectors::send_external_message,
                capture_pipeline::create_capture_authorization,
                capture_pipeline::cancel_capture_task,
                capture_pipeline::open_capture_authorization_page,
                capture_pipeline::begin_capture_upload,
                capture_pipeline::append_capture_upload_chunk,
                capture_pipeline::finish_capture_upload,
                capture_pipeline::prepare_capture_image_analysis_input,
                capture_pipeline::discard_capture_attachments,
                capture_pipeline::extract_capture_source,
                model_provider::analyze_capture_content,
                model_provider::discard_capture_analysis_receipt,
                model_provider::chat_with_assistant,
                model_provider::cancel_assistant_request,
                model_provider::consume_assistant_decision,
                model_provider::generate_assistant_image,
                model_provider::fetch_provider_models,
                obsidian::discover_obsidian_vaults,
                obsidian::set_local_vault_selection,
                obsidian::list_vault_folders,
                obsidian::search_vault_notes,
                obsidian::read_vault_note,
                obsidian::list_vault_notes,
                obsidian::save_creation_draft_asset,
                obsidian::load_creation_draft_asset,
                obsidian::beautify_creation_markdown,
                obsidian::prepare_note_write,
                obsidian::commit_note_write,
                obsidian::discard_note_write,
                obsidian::prepare_asset_write,
                obsidian::prepare_capture_vault_writes,
                obsidian::discard_asset_write,
                obsidian::commit_capture_batch,
                obsidian::list_operation_events,
                obsidian::append_long_term_memory_event,
                obsidian_management::prepare_vault_entry_delete,
                obsidian_management::discard_vault_entry_delete,
                obsidian_management::commit_vault_entry_delete,
                obsidian_management::list_yunspire_trash_entries,
                obsidian_management::restore_yunspire_trash_entry,
                obsidian_management::create_vault_folder,
                obsidian_management::move_vault_entry,
                obsidian_management::read_note_properties,
                obsidian_management::update_note_properties,
                obsidian_management::update_note_tags,
                obsidian_management::update_note_wiki_link,
                obsidian_management::read_obsidian_graph_config,
                obsidian_management::update_obsidian_graph_config,
                runtime_db::load_workspace_snapshot,
                runtime_db::load_application_authorization,
                update_application_authorization,
                runtime_db::save_workspace_snapshot,
                runtime_db::database_health,
                runtime_db::backup_local_database,
                runtime_db::list_database_backups,
                runtime_db::preflight_database_restore,
                runtime_db::restore_local_database,
                runtime_db::query_long_term_memory,
                runtime_db::govern_long_term_memory,
                runtime_db::export_long_term_memory,
                runtime_db::long_term_memory_metrics,
                runtime_db::read_optimization_evidence,
                runtime_db::create_optimization_candidate,
                runtime_db::evaluate_optimization_candidate,
                runtime_db::load_optimization_profile,
                runtime_db::apply_optimization_candidate,
                runtime_db::rollback_optimization_profile,
                runtime_db::list_optimization_versions,
                runtime_db::indexed_search,
                runtime_db::sync_runtime_state,
                runtime_db::sync_managed_resources,
                runtime_db::load_managed_resources,
                runtime_db::recover_interrupted_runtime_tasks,
                runtime_db::resolve_runtime_task_recovery,
                runtime_db::upsert_inbound_content_record,
                runtime_db::poll_due_runtime_schedules,
                task_runtime::get_runtime_task,
                task_runtime::list_runtime_tasks,
                task_runtime::transition_runtime_task,
                updater::check_for_updates,
                updater::prepare_update_installation,
                updater::list_update_backups,
                updater::rollback_update_backup,
            ];
            move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
                let command = invoke.message.command().to_string();
                let runtime_initialized = invoke
                    .message
                    .webview()
                    .state::<LocalRuntimeInitializationState>()
                    .initialized
                    .load(Ordering::Acquire);
                if !runtime_initialized
                    && !command_available_without_application_authorization(&command)
                {
                    invoke
                        .resolver
                        .reject("云枢当前处于受限模式；请先在“设置 > 权限”中完成统一授权。");
                    return true;
                }
                handler(invoke)
            }
        })
        .setup(|app| {
            let database =
                runtime_db::RuntimeDatabase::open(app.handle()).map_err(std::io::Error::other)?;
            app.manage(database);
            let database = app.state::<runtime_db::RuntimeDatabase>();
            if database
                .application_authorization()
                .map_err(std::io::Error::other)?
                .is_granted()
            {
                initialize_local_runtime_once(app.handle(), database.inner())
                    .map_err(std::io::Error::other)?;
            }
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
