mod assistant_runtime;
mod capture_pipeline;
mod capture_result;
mod command_bus;
mod connectors;
mod content_fingerprint;
mod creation;
mod durable_asset;
mod error;
mod execution_plan;
mod execution_plan_extractor;
mod execution_ticket;
mod knowledge_health;
mod lease_heartbeat;
mod memory;
mod metrics;
mod model_config;
mod model_provider;
mod obsidian;
mod obsidian_management;
mod policy;
mod prompt;
mod runtime_db;
mod scheduler;
mod search_match;
mod skill_lifecycle;
mod task_runtime;
mod trace;
mod updater;
mod vault_batch;
mod vault_watcher;

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver},
    Arc, Mutex,
};
use tauri::{path::BaseDirectory, AppHandle, Manager, State};
use uuid::Uuid;

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

const COMMANDS_AVAILABLE_WITHOUT_APPLICATION_AUTHORIZATION: [&str; 3] = [
    "load_application_authorization",
    "load_third_party_notices",
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
    match durable_asset::reconcile_for_startup(app, database) {
        Ok(report) => {
            if report.recovered_finalizations > 0 || report.source_missing > 0 {
                log::info!(
                    "耐久资产恢复完成：就绪={}，上传中={}，恢复提交={}，源文件缺失={}",
                    report.ready,
                    report.staging,
                    report.recovered_finalizations,
                    report.source_missing
                );
            }
        }
        Err(error) => log::warn!("无法校验耐久资产状态：{error}"),
    }
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
    if let Some(archive_path) = obsidian::archive_legacy_behavior_records_for_runtime()? {
        log::info!(
            "旧 Obsidian 行为记录已移出 Vault 并保留到 {}",
            archive_path.display()
        );
    }
    if let Err(error) =
        obsidian::finalize_pending_long_term_memory_events_for_runtime(database, &workspace_scope)
    {
        log::warn!("无法收口 SQLite 内部长期记忆事件：{error}");
    }
    if let Err(error) = obsidian::recover_vault_batch_manifests_for_runtime(app, database) {
        log::warn!("无法完整恢复中断的跨 Vault 批次：{error}");
    }
    database.recover_vault_index_changes()?;
    if let Err(error) = memory::recover_reflection_jobs(database) {
        log::warn!("无法恢复中断的记忆反思任务：{error}");
    }
    if let Err(error) = assistant_runtime::recover_requests_for_startup(database) {
        log::warn!("无法恢复中断的 AI助手请求：{error}");
    }
    if let Err(error) = creation::runtime::recover_creation_runs_for_startup(database) {
        log::warn!("无法恢复中断的创作 WritingRun：{error}");
    }
    let vaults = obsidian::discover_vaults_for_runtime().unwrap_or_default();
    database.sync_vault_registry(&vaults)?;
    database.purge_unreadable_vault_indexes(&workspace_scope, &vaults)?;
    let readable_vaults = vaults
        .into_iter()
        .filter(|vault| {
            vault.connection_state == "connected"
                && database
                    .ensure_vault_read_allowed(&workspace_scope, &vault.id)
                    .is_ok()
        })
        .collect::<Vec<_>>();
    vault_watcher::start_vault_watchers(app, &readable_vaults, generation)?;
    Ok(readable_vaults)
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
        // 等待旧任务完成（最多5秒）
        let _ = previous
            .completed
            .recv_timeout(std::time::Duration::from_secs(5));
    }
    let vault_count = vaults.len();
    let cancellation = Arc::new(AtomicBool::new(false));
    let task_cancellation = Arc::clone(&cancellation);
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let app_handle = app.clone();

    log::info!("开始后台索引 {} 个 Vault", vault_count);

    tauri::async_runtime::spawn_blocking(move || {
        use std::time::Instant;
        let start = Instant::now();
        const MAX_TOTAL_INDEX_TIME_SECS: u64 = 3600; // 1小时总超时
        const HEARTBEAT_INTERVAL_SECS: u64 = 10;
        let mut last_heartbeat = Instant::now();
        let mut succeeded = 0;
        let mut failed = 0;

        let database = app_handle.state::<runtime_db::RuntimeDatabase>();
        for (idx, vault) in vaults.iter().enumerate() {
            let is_cancelled = || {
                task_cancellation.load(Ordering::Acquire)
                    || !local_runtime_generation_is_active(&app_handle, generation)
            };

            if is_cancelled() {
                log::info!("索引任务被取消，已处理 {}/{}", idx, vaults.len());
                break;
            }

            // 全局超时检查
            if start.elapsed().as_secs() > MAX_TOTAL_INDEX_TIME_SECS {
                log::warn!("索引任务超过1小时全局超时，已处理 {}/{}", idx, vaults.len());
                break;
            }

            // 心跳日志
            if last_heartbeat.elapsed().as_secs() > HEARTBEAT_INTERVAL_SECS {
                log::info!(
                    "索引进度：{}/{} ({:.1}%), 已耗时 {:.1}s",
                    idx,
                    vaults.len(),
                    (idx as f64 / vaults.len() as f64) * 100.0,
                    start.elapsed().as_secs_f64()
                );
                last_heartbeat = Instant::now();
            }

            if vault.connection_state == "connected" {
                match database.rebuild_index_for_vault_with_cancellation(&vault.id, &is_cancelled) {
                    Ok(_) => {
                        succeeded += 1;
                        log::debug!("Vault {} 索引完成", vault.name);
                    }
                    Err(error) => {
                        if !is_cancelled() {
                            failed += 1;
                            log::warn!("Vault {} 索引失败：{}", vault.name, error);
                        }
                    }
                }
            }
        }

        let duration_secs = start.elapsed().as_secs();
        log::info!(
            "后台索引完成：成功 {}, 失败 {}, 耗时 {}s",
            succeeded,
            failed,
            duration_secs
        );

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
    vault_watcher::start_vault_index_worker(app);
    start_background_vault_indexing(app, vaults, generation);

    // 启动 Lease 心跳守护线程
    let worker_id = format!("worker-{}", Uuid::new_v4());
    if let Err(error) = lease_heartbeat::start_lease_heartbeat_if_needed(app, worker_id) {
        log::warn!("无法启动 Lease 心跳守护线程：{}", error);
    }
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
    match app
        .state::<connectors::ExternalConnectorRuntimeState>()
        .cancel_all()
    {
        Ok(cancelled) if cancelled > 0 => {
            log::info!("撤销统一授权时已取消 {cancelled} 个外部连接器请求");
        }
        Ok(_) => {}
        Err(error) => failures.push(error),
    }
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
    let authorization = database.set_application_authorization(false)?;
    suspend_local_runtime_locked(app, state.inner())?;
    Ok(authorization)
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

#[tauri::command]
fn load_third_party_notices(app: AppHandle) -> Result<String, String> {
    let path = app
        .path()
        .resolve("legal/THIRD_PARTY_NOTICES.txt", BaseDirectory::Resource)
        .map_err(|error| format!("无法定位第三方许可清单：{error}"))?;
    std::fs::read_to_string(&path)
        .map_err(|error| format!("无法读取第三方许可清单 {}：{error}", path.display()))
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
        .manage(execution_ticket::ExecutionTicketState::default())
        .manage(connectors::ExternalConnectorRuntimeState::default())
        .manage(capture_pipeline::CaptureAuthorizationState::default())
        .manage(capture_pipeline::CaptureTaskState::default())
        .manage(capture_pipeline::CaptureUploadState::default())
        .manage(durable_asset::DurableAssetState::default())
        .manage(vault_watcher::VaultWatcherState::default())
        .manage(scheduler::SchedulerState::default())
        .manage(lease_heartbeat::LeaseHeartbeatState::default())
        .manage(LocalRuntimeInitializationState::default())
        .invoke_handler({
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                model_config::save_model_provider,
                model_config::load_model_providers,
                model_config::delete_model_provider,
                memory::upsert_memory_record,
                memory::tombstone_memory_record,
                memory::search_memory_records,
                memory::list_memory_records,
                memory::begin_memory_reflection,
                memory::get_memory_reflection,
                memory::list_memory_reflections,
                memory::claim_memory_reflection,
                memory::renew_memory_reflection_lease,
                memory::complete_memory_reflection,
                memory::review_memory_reflection,
                memory::approve_reflection_optimization_candidate,
                memory::fail_memory_reflection,
                memory::cancel_memory_reflection,
                memory::memory_backend_status,
                assistant_runtime::enqueue_assistant_request,
                assistant_runtime::claim_assistant_request,
                assistant_runtime::assemble_assistant_request_context,
                assistant_runtime::finish_assistant_request,
                assistant_runtime::cancel_assistant_runtime_request,
                assistant_runtime::advance_assistant_conversation_revision,
                assistant_runtime::recover_assistant_requests,
                assistant_runtime::update_assistant_execution_step,
                assistant_runtime::get_assistant_execution_plan,
                command_bus::evaluate_application_command,
                command_bus::submit_application_command,
                execution_ticket::retire_execution_ticket,
                connectors::save_external_connector,
                connectors::load_external_connectors,
                connectors::delete_external_connector,
                connectors::prepare_external_delivery,
                connectors::send_external_message,
                creation::list_creation_catalog,
                creation::normalize_creation_document,
                creation::validate_creation_document,
                creation::runtime::begin_creation_run,
                creation::runtime::get_creation_run,
                creation::runtime::read_creation_stream_events_page,
                creation::runtime::append_creation_stream_event,
                creation::runtime::checkpoint_creation_run,
                creation::runtime::record_creation_run_usage,
                creation::runtime::recover_creation_runs,
                creation::runtime::reverify_creation_grounding,
                creation::runtime::accept_creation_candidate,
                creation::runtime::cancel_creation_run,
                creation::runtime::upsert_creation_brand_profile,
                creation::runtime::get_creation_brand_profile,
                creation::runtime::list_creation_brand_profiles,
                creation::runtime::approve_creation_brand_profile,
                creation::runtime::archive_creation_brand_profile,
                creation::runtime::delete_creation_brand_profile,
                creation::runtime::bind_creation_brand_profile,
                creation::runtime::evaluate_creation_brand_profile,
                durable_asset::import_legacy_creation_draft_asset,
                durable_asset::begin_durable_asset_upload,
                durable_asset::append_durable_asset_chunk,
                durable_asset::finish_durable_asset_upload,
                durable_asset::get_durable_asset,
                durable_asset::list_durable_assets,
                durable_asset::list_durable_assets_page,
                durable_asset::read_durable_asset_chunk,
                durable_asset::delete_durable_asset,
                capture_pipeline::create_capture_authorization,
                capture_pipeline::cancel_capture_task,
                capture_pipeline::open_capture_authorization_page,
                capture_pipeline::begin_capture_upload,
                capture_pipeline::append_capture_upload_chunk,
                capture_pipeline::finish_capture_upload,
                capture_pipeline::prepare_capture_image_analysis_input,
                capture_pipeline::discard_capture_attachments,
                capture_pipeline::extract_capture_source,
                capture_pipeline::extract_capture_source_v2,
                creation::mode::get_creation_mode,
                creation::mode::set_creation_mode,
                content_fingerprint::detect_content_duplicate,
                execution_plan::generate_execution_plan,
                execution_plan::submit_execution_decision,
                execution_plan::get_execution_logs,
                knowledge_health::get_knowledge_health_dashboard,
                metrics::get_metrics_report,
                model_provider::analyze_capture_content,
                model_provider::bind_capture_analysis_write_manifest,
                model_provider::issue_direct_write_receipt,
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
                obsidian::open_obsidian_note,
                obsidian::open_vault_note_in_obsidian,
                obsidian::open_obsidian_vault,
                obsidian::open_obsidian_graph,
                obsidian::list_vault_notes,
                obsidian::list_vault_notes_page,
                obsidian::beautify_creation_markdown,
                obsidian::prepare_note_write,
                obsidian::prepare_note_write_from_durable_asset,
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
                obsidian_management::purge_yunspire_trash_entry,
                obsidian_management::empty_yunspire_trash,
                obsidian_management::create_vault_folder,
                obsidian_management::move_vault_entry,
                obsidian_management::read_note_properties,
                obsidian_management::update_note_properties,
                obsidian_management::update_note_tags,
                obsidian_management::update_note_wiki_link,
                obsidian_management::read_obsidian_graph_config,
                obsidian_management::update_obsidian_graph_config,
                runtime_db::load_workspace_snapshot,
                runtime_db::upsert_workspace_messages_page,
                runtime_db::list_workspace_messages_page,
                runtime_db::search_workspace_messages,
                runtime_db::delete_workspace_messages,
                runtime_db::delete_workspace_conversation_messages,
                runtime_db::load_application_authorization,
                load_third_party_notices,
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
                skill_lifecycle::install_skill_from_github,
                skill_lifecycle::save_skill_draft,
                skill_lifecycle::list_user_skills,
                skill_lifecycle::list_routable_skills,
                skill_lifecycle::evaluate_skill_candidate,
                skill_lifecycle::decide_skill_candidate,
                skill_lifecycle::change_skill_activation,
                skill_lifecycle::retire_skill,
                skill_lifecycle::rollback_skill,
                skill_lifecycle::list_skill_versions,
                skill_lifecycle::list_skill_execution_effects,
                skill_lifecycle::record_skill_execution_effect_feedback,
                skill_lifecycle::execute_skill,
                trace::query_runtime_trace,
                trace::validate_runtime_trace,
                runtime_db::read_optimization_evidence,
                runtime_db::create_optimization_candidate,
                runtime_db::evaluate_optimization_candidate,
                runtime_db::get_optimization_candidate,
                runtime_db::load_optimization_profile,
                runtime_db::apply_optimization_candidate,
                runtime_db::rollback_optimization_profile,
                runtime_db::list_optimization_versions,
                runtime_db::get_neural_embedding_index_status,
                runtime_db::rebuild_neural_embedding_index,
                runtime_db::indexed_search,
                runtime_db::sync_runtime_state,
                runtime_db::sync_managed_resources,
                runtime_db::load_managed_resources,
                runtime_db::upsert_report_record,
                runtime_db::delete_report_record,
                runtime_db::list_report_records_page,
                runtime_db::upsert_report_subscription,
                runtime_db::delete_report_subscription,
                runtime_db::list_report_subscriptions_page,
                runtime_db::read_report_source_page,
                runtime_db::upsert_creation_resource,
                runtime_db::list_creation_resources,
                runtime_db::list_creation_resource_revisions,
                runtime_db::restore_creation_resource_revision,
                runtime_db::archive_creation_resource,
                runtime_db::recover_interrupted_runtime_tasks,
                runtime_db::supersede_runtime_task_for_recovery,
                runtime_db::bind_runtime_task_recovery_replacement,
                runtime_db::resolve_runtime_task_recovery,
                runtime_db::upsert_inbound_content_record,
                runtime_db::poll_due_runtime_schedules,
                task_runtime::acknowledge_runtime_schedule_dispatch,
                task_runtime::append_runtime_task_evidence,
                task_runtime::define_runtime_task_plan,
                task_runtime::get_runtime_task,
                task_runtime::get_runtime_task_contract,
                task_runtime::get_runtime_task_step_frontier,
                task_runtime::list_runtime_tasks,
                task_runtime::list_runtime_task_step_receipts,
                task_runtime::claim_runtime_task_plan_steps,
                task_runtime::renew_runtime_task_step_lease,
                task_runtime::renew_runtime_execution_ticket,
                task_runtime::execute_runtime_read_only_capability,
                task_runtime::complete_runtime_task_plan_step,
                task_runtime::fail_runtime_task_plan_step,
                task_runtime::transition_runtime_task,
                lease_heartbeat::get_lease_heartbeat_status,
                vault_watcher::refresh_vault_access_policy,
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
