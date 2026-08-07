use crate::{
    execution_ticket::{ExecutionTicketState, TicketScope},
    obsidian::{
        atomic_write_file, ensure_long_term_memory_mutation_allowed_for_runtime,
        register_vault_path_for_runtime, remove_vault_registration_for_runtime,
        resolve_vault_for_runtime, OperationContext, OperationEvent,
    },
    runtime_db::RuntimeDatabase,
};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

const DELETE_CONFIRMATION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING_DELETES: usize = 32;
const MAX_TREE_ENTRIES: u64 = 200_000;
const MAX_GRAPH_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct ObsidianManagementState {
    pending_deletes: Mutex<HashMap<String, PendingEntryDelete>>,
}

#[derive(Clone)]
struct PendingEntryDelete {
    task_id: String,
    trace_id: Option<String>,
    vault_id: String,
    vault_name: String,
    vault_root: PathBuf,
    source_path: PathBuf,
    relative_path: Option<String>,
    snapshot: EntrySnapshot,
    delete_vault: bool,
    execution_ticket: Option<String>,
    effect_digest: String,
    created_at: SystemTime,
}

pub(crate) fn clear_pending_deletes_for_runtime(
    state: &ObsidianManagementState,
) -> Result<usize, String> {
    let mut pending = state
        .pending_deletes
        .lock()
        .map_err(|_| "删除确认状态不可用".to_string())?;
    let count = pending.len();
    pending.clear();
    Ok(count)
}

#[derive(Clone)]
struct EntrySnapshot {
    entry_type: String,
    byte_length: u64,
    entry_count: u64,
    fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDeletePreview {
    approval_id: String,
    vault_id: String,
    vault_name: String,
    relative_path: Option<String>,
    entry_type: String,
    byte_length: u64,
    entry_count: u64,
    fingerprint: String,
    expires_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashManifest {
    operation_id: String,
    vault_id: String,
    vault_name: String,
    original_vault_path: String,
    original_relative_path: Option<String>,
    entry_type: String,
    payload_relative_path: String,
    fingerprint: String,
    byte_length: u64,
    entry_count: u64,
    deleted_at: String,
    state: String,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    recovery_target_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntryDescriptor {
    operation_id: String,
    vault_id: String,
    vault_name: String,
    original_vault_path: String,
    original_relative_path: Option<String>,
    entry_type: String,
    byte_length: u64,
    entry_count: u64,
    deleted_at: String,
    recoverable: bool,
    state: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashPurgeReceipt {
    purge_id: String,
    operation_ids: Vec<String>,
    purged_entries: u64,
    purged_bytes: u64,
    committed_at: String,
}

#[derive(Clone)]
struct PreparedTrashPurge {
    operation_dir: PathBuf,
    manifest: TrashManifest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultMutationReceipt {
    operation_id: String,
    vault_id: String,
    source_path: Option<String>,
    target_path: String,
    entry_type: String,
    checkpoint_path: String,
    committed_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePropertiesResult {
    vault_id: String,
    relative_path: String,
    properties: Value,
    content_hash: String,
    checkpoint_path: String,
    committed_at: String,
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

struct BoundManagementExecution {
    task_id: String,
    trace_id: Option<String>,
    execution_ticket: Option<String>,
}

fn management_effect_digest(
    kind: &str,
    vault_id: &str,
    relative_paths: &[&str],
    effect: Value,
) -> String {
    let payload = serde_json::json!({
        "kind": kind,
        "vaultId": vault_id,
        "relativePaths": relative_paths,
        "effect": effect,
    });
    let bytes = serde_json::to_vec(&payload).expect("management effect payload is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

#[allow(clippy::too_many_arguments)]
fn bind_management_execution(
    ticket_state: Option<&ExecutionTicketState>,
    workspace_scope: &str,
    context: OperationContext,
    capability_ids: &[&str],
    operations: &[&str],
    vault_id: &str,
    relative_paths: &[&str],
    approval_id: &str,
    effect_digest: &str,
) -> Result<BoundManagementExecution, String> {
    let task_id = context
        .task_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Obsidian 管理操作缺少原生任务 ID".to_string())?;
    let trace_id = context.trace_id.filter(|value| !value.trim().is_empty());
    let Some(ticket_state) = ticket_state else {
        return Ok(BoundManagementExecution {
            task_id,
            trace_id,
            execution_ticket: None,
        });
    };
    let execution_ticket = context
        .execution_ticket
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Obsidian 管理操作缺少能力范围执行票据".to_string())?;
    if relative_paths.is_empty() {
        ticket_state.bind_approval(
            &execution_ticket,
            TicketScope {
                workspace_scope,
                task_id: &task_id,
                trace_id: trace_id.as_deref(),
                allowed_capability_ids: capability_ids,
                allowed_operations: operations,
                vault_id,
                relative_path: "",
                require_declared_path: false,
            },
            approval_id,
            effect_digest,
        )?;
    } else {
        for relative_path in relative_paths {
            ticket_state.bind_approval(
                &execution_ticket,
                TicketScope {
                    workspace_scope,
                    task_id: &task_id,
                    trace_id: trace_id.as_deref(),
                    allowed_capability_ids: capability_ids,
                    allowed_operations: operations,
                    vault_id,
                    relative_path,
                    require_declared_path: true,
                },
                approval_id,
                effect_digest,
            )?;
        }
    }
    Ok(BoundManagementExecution {
        task_id,
        trace_id,
        execution_ticket: Some(execution_ticket),
    })
}

fn begin_management_commit(
    ticket_state: Option<&ExecutionTicketState>,
    execution: &BoundManagementExecution,
    workspace_scope: &str,
    approval_id: &str,
    effect_digest: &str,
) -> Result<(), String> {
    if let (Some(state), Some(token)) = (ticket_state, execution.execution_ticket.as_deref()) {
        state.begin_commit(
            token,
            workspace_scope,
            &execution.task_id,
            &[(approval_id, effect_digest)],
        )?;
    }
    Ok(())
}

fn finish_management_commit(
    ticket_state: Option<&ExecutionTicketState>,
    execution_ticket: Option<&str>,
) -> Result<(), String> {
    if let (Some(state), Some(token)) = (ticket_state, execution_ticket) {
        state.complete_commit(token)?;
    }
    Ok(())
}

fn abort_management_commit(
    ticket_state: Option<&ExecutionTicketState>,
    execution_ticket: Option<&str>,
    rolled_back: bool,
) {
    if let (Some(state), Some(token)) = (ticket_state, execution_ticket) {
        if rolled_back {
            state.release_commit(token);
        } else if let Err(error) = state.fail_commit(token) {
            log::warn!("无法封存副作用状态不确定的执行票据：{error}");
        }
    }
}

fn normalized_relative_path(
    value: &str,
    allow_obsidian: bool,
    allow_trash: bool,
) -> Result<(PathBuf, String), String> {
    let value = value.trim().replace('\\', "/");
    let path = Path::new(&value);
    if value.is_empty() || path.is_absolute() || value.contains('\0') {
        return Err("目标必须是 Vault 内的相对路径".to_string());
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("目标路径包含不允许的目录跳转或前缀".to_string());
    }
    let first = path
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .unwrap_or("");
    if first == ".obsidian" && !allow_obsidian {
        return Err(".obsidian 只能通过专用配置接口修改".to_string());
    }
    if first == ".trash" && !allow_trash {
        return Err("Vault 回收目录不能通过普通文件管理接口修改".to_string());
    }
    Ok((path.to_path_buf(), value))
}

fn canonical_root(vault_id: &str) -> Result<(String, PathBuf), String> {
    let (name, root) = resolve_vault_for_runtime(vault_id)?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("Vault 根目录不可访问：{error}"))?;
    Ok((name, root))
}

fn resolve_existing_entry(root: &Path, relative_path: &str) -> Result<(PathBuf, String), String> {
    let (relative, normalized) = normalized_relative_path(relative_path, false, false)?;
    ensure_long_term_memory_mutation_allowed_for_runtime(&normalized)?;
    let target = root.join(relative);
    let metadata =
        fs::symlink_metadata(&target).map_err(|error| format!("目标不存在或不可访问：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("不允许通过 Yunspire 修改符号链接".to_string());
    }
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("目标路径不可访问：{error}"))?;
    if !canonical.starts_with(root) || canonical == root {
        return Err("目标越过 Vault 边界".to_string());
    }
    Ok((canonical, normalized))
}

fn resolve_new_entry(root: &Path, relative_path: &str) -> Result<(PathBuf, String), String> {
    let (relative, normalized) = normalized_relative_path(relative_path, false, false)?;
    ensure_long_term_memory_mutation_allowed_for_runtime(&normalized)?;
    let target = root.join(relative);
    if target.exists() {
        return Err("目标路径已经存在".to_string());
    }
    let mut parent = target.parent().ok_or("目标路径缺少父目录")?;
    while !parent.exists() {
        parent = parent.parent().ok_or("无法定位 Vault 内父目录")?;
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("目标父目录不可访问：{error}"))?;
    if !canonical_parent.starts_with(root) {
        return Err("目标路径越过 Vault 边界".to_string());
    }
    Ok((target, normalized))
}

fn update_snapshot_digest(
    root: &Path,
    path: &Path,
    digest: &mut Sha256,
    byte_length: &mut u64,
    entry_count: &mut u64,
) -> Result<(), String> {
    if *entry_count >= MAX_TREE_ENTRIES {
        return Err("目标目录包含的文件数量超过安全上限".to_string());
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取删除目标元数据：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("删除目标中包含符号链接，已拒绝处理".to_string());
    }
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    digest.update(relative.as_bytes());
    digest.update([0]);
    digest.update(metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified().and_then(|value| {
        value
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(std::io::Error::other)
    }) {
        digest.update(modified.as_secs().to_le_bytes());
        digest.update(modified.subsec_nanos().to_le_bytes());
    }
    *entry_count += 1;
    if metadata.is_file() {
        *byte_length = byte_length.saturating_add(metadata.len());
        let mut file = File::open(path).map_err(|error| format!("无法读取删除目标：{error}"))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("无法计算删除目标指纹：{error}"))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        return Ok(());
    }
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(|error| format!("无法扫描删除目标目录：{error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            update_snapshot_digest(root, &child, digest, byte_length, entry_count)?;
        }
        return Ok(());
    }
    Err("删除目标不是普通文件或目录".to_string())
}

fn entry_snapshot(path: &Path) -> Result<EntrySnapshot, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取目标元数据：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("不允许删除符号链接".to_string());
    }
    let entry_type = if metadata.is_dir() {
        "folder"
    } else if metadata.is_file() {
        "file"
    } else {
        return Err("目标不是普通文件或目录".to_string());
    };
    let root = path.parent().unwrap_or(path);
    let mut digest = Sha256::new();
    let mut byte_length = 0_u64;
    let mut entry_count = 0_u64;
    update_snapshot_digest(root, path, &mut digest, &mut byte_length, &mut entry_count)?;
    Ok(EntrySnapshot {
        entry_type: entry_type.to_string(),
        byte_length,
        entry_count,
        fingerprint: format!("{:x}", digest.finalize()),
    })
}

fn system_trash_root() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(path) = env::var_os("YUNSPIRE_TRASH_DIR") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "windows")]
    {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .map(|path| path.join("AppData").join("Local"))
            })
            .ok_or("无法读取 Windows 本地应用数据目录")?;
        Ok(local_app_data.join("Yunspire").join("Trash"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = env::var_os("HOME").ok_or("无法读取用户主目录")?;
        Ok(PathBuf::from(home).join(".Trash").join("Yunspire"))
    }
}

fn operation_trash_dir(operation_id: &str) -> Result<PathBuf, String> {
    if operation_id.is_empty()
        || operation_id.len() > 120
        || !operation_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("回收记录 ID 无效".to_string());
    }
    Ok(system_trash_root()?.join(operation_id))
}

fn copy_entry(source: &Path, target: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| format!("无法读取跨磁盘回收目标：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("跨磁盘回收不支持符号链接".to_string());
    }
    if metadata.is_file() {
        let parent = target.parent().ok_or("回收目标缺少父目录")?;
        fs::create_dir_all(parent).map_err(|error| format!("无法创建回收目录：{error}"))?;
        let mut source_file =
            File::open(source).map_err(|error| format!("无法打开跨磁盘回收源文件：{error}"))?;
        let mut target_file =
            File::create(target).map_err(|error| format!("无法创建跨磁盘回收文件：{error}"))?;
        std::io::copy(&mut source_file, &mut target_file)
            .map_err(|error| format!("无法复制目标到云枢回收区：{error}"))?;
        target_file
            .sync_all()
            .map_err(|error| format!("无法同步回收文件：{error}"))?;
        drop(target_file);
        fs::set_permissions(target, metadata.permissions())
            .map_err(|error| format!("无法保留回收文件权限：{error}"))?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|error| format!("无法创建回收目录：{error}"))?;
        let mut children = fs::read_dir(source)
            .map_err(|error| format!("无法读取跨磁盘回收目录：{error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            let name = child.file_name().ok_or("回收目录项缺少文件名")?;
            copy_entry(&child, &target.join(name))?;
        }
        return Ok(());
    }
    Err("回收目标不是普通文件或目录".to_string())
}

fn remove_entry(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|error| format!("无法移除原目录：{error}"))
    } else {
        fs::remove_file(path).map_err(|error| format!("无法移除原文件：{error}"))
    }
}

fn move_entry(source: &Path, target: &Path) -> Result<(), String> {
    let parent = target.parent().ok_or("目标路径缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建目标目录：{error}"))?;
    match fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_entry(source, target)?;
            if let Err(remove_error) = remove_entry(source) {
                let _ = remove_entry(target);
                return Err(format!(
                    "目标已复制到回收区，但无法移除原位置，已回滚回收副本：{remove_error}"
                ));
            }
            Ok(())
        }
        Err(error) => Err(format!("无法移动目标：{error}")),
    }
}

fn preserve_recovery_manifest(operation_dir: &Path, manifest: &mut TrashManifest, error: &str) {
    manifest.state = "recovery_required".to_string();
    manifest.last_error = Some(error.chars().take(4_000).collect());
    if let Err(manifest_error) = write_json_atomic(&operation_dir.join("manifest.json"), manifest) {
        log::error!(
            "无法持久化回收恢复状态 {}：{manifest_error}",
            operation_dir.display()
        );
    }
}

fn preserve_restore_recovery(
    operation_dir: &Path,
    manifest: &mut TrashManifest,
    target: &Path,
    error: &str,
) {
    manifest.state = "restore_incomplete".to_string();
    manifest.last_error = Some(error.chars().take(4_000).collect());
    manifest.recovery_target_path = target
        .exists()
        .then(|| target.to_string_lossy().into_owned());
    if let Err(manifest_error) = write_json_atomic(&operation_dir.join("manifest.json"), manifest) {
        log::error!(
            "无法持久化恢复中断状态 {}：{manifest_error}",
            operation_dir.display()
        );
    }
}

fn rollback_trashed_entry(pending: &PendingEntryDelete, payload_path: &Path) -> Result<(), String> {
    move_entry(payload_path, &pending.source_path)?;
    if pending.delete_vault {
        register_vault_path_for_runtime(&pending.source_path)?;
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| format!("无法序列化操作记录：{error}"))?;
    atomic_write_file(path, &bytes)
}

fn create_checkpoint(
    app: &AppHandle,
    operation_id: &str,
    name: &str,
    value: &impl Serialize,
) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    let app_data = env::var_os("YUNSPIRE_APP_DATA_DIR")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            app.path()
                .app_data_dir()
                .map_err(|error| format!("无法定位应用数据目录：{error}"))
        })?;
    #[cfg(not(debug_assertions))]
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    create_checkpoint_at(&app_data, operation_id, name, value)
}

fn create_checkpoint_at(
    app_data: &Path,
    operation_id: &str,
    name: &str,
    value: &impl Serialize,
) -> Result<PathBuf, String> {
    let root = app_data.join("checkpoints").join(operation_id);
    fs::create_dir_all(&root).map_err(|error| format!("无法创建操作检查点：{error}"))?;
    let path = root.join(name);
    write_json_atomic(&path, value)?;
    Ok(path)
}

fn append_event(
    database: &RuntimeDatabase,
    task_id: &str,
    trace_id: Option<String>,
    event_type: &str,
    vault_id: &str,
    relative_path: Option<String>,
    detail: String,
) -> Result<String, String> {
    let committed_at = now_string();
    database.append_operation_event(&OperationEvent {
        id: Uuid::new_v4().to_string(),
        task_id: Some(task_id.to_string()),
        trace_id,
        event_type: event_type.to_string(),
        state: "success".to_string(),
        created_at: committed_at.clone(),
        vault_id: Some(vault_id.to_string()),
        relative_path,
        detail,
    })?;
    Ok(committed_at)
}

// The event ledger records task, trace, state transition and detail as separate fields.
#[allow(clippy::too_many_arguments)]
fn append_event_with_state(
    database: &RuntimeDatabase,
    task_id: &str,
    trace_id: Option<String>,
    event_type: &str,
    state: &str,
    vault_id: &str,
    relative_path: Option<String>,
    detail: String,
) -> Result<String, String> {
    let created_at = now_string();
    database.append_operation_event(&OperationEvent {
        id: Uuid::new_v4().to_string(),
        task_id: Some(task_id.to_string()),
        trace_id,
        event_type: event_type.to_string(),
        state: state.to_string(),
        created_at: created_at.clone(),
        vault_id: Some(vault_id.to_string()),
        relative_path,
        detail,
    })?;
    Ok(created_at)
}

fn enqueue_markdown_index_with_trace(
    database: &RuntimeDatabase,
    vault_id: &str,
    root: &Path,
    path: &Path,
    trace_id: &str,
) {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return;
    }
    if let Err(error) = database.enqueue_vault_index_path_with_trace(vault_id, root, path, trace_id)
    {
        log::warn!(
            "Markdown 操作已提交，但无法继承 Trace 入队索引 {}：{error}",
            path.display()
        );
    }
}

#[tauri::command]
pub fn prepare_vault_entry_delete(
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianManagementState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: Option<String>,
    delete_vault: Option<bool>,
    operation_context: OperationContext,
) -> Result<EntryDeletePreview, String> {
    prepare_vault_entry_delete_inner(
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        vault_id,
        relative_path,
        delete_vault,
        operation_context,
    )
}

fn prepare_vault_entry_delete_inner(
    state: &ObsidianManagementState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    vault_id: String,
    relative_path: Option<String>,
    delete_vault: Option<bool>,
    operation_context: OperationContext,
) -> Result<EntryDeletePreview, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let (vault_name, root) = canonical_root(&vault_id)?;
    let delete_vault = delete_vault.unwrap_or(false);
    let (source_path, normalized_relative) = if delete_vault {
        if relative_path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err("删除整个 Vault 时不能同时指定内部相对路径".to_string());
        }
        database.ensure_vault_write_allowed(&workspace_scope, &vault_id, "")?;
        (root.clone(), None)
    } else {
        let relative_path = relative_path
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "删除文件或文件夹时必须指定 Vault 内相对路径".to_string())?;
        let (path, normalized) = resolve_existing_entry(&root, relative_path)?;
        database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &normalized)?;
        (path, Some(normalized))
    };
    let snapshot = entry_snapshot(&source_path)?;
    let approval_id = Uuid::new_v4().to_string();
    let effect_paths = normalized_relative
        .as_deref()
        .map(|path| vec![path])
        .unwrap_or_default();
    let effect_digest = management_effect_digest(
        "delete",
        &vault_id,
        &effect_paths,
        serde_json::json!({
            "deleteVault": delete_vault,
            "fingerprint": snapshot.fingerprint,
        }),
    );
    let execution = bind_management_execution(
        ticket_state,
        &workspace_scope,
        operation_context,
        &["system:delete", "system:vaults"],
        &["delete"],
        &vault_id,
        &effect_paths,
        &approval_id,
        &effect_digest,
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:delete", "system:vaults"],
        &["delete"],
        Some(&vault_id),
        &["awaiting_approval", "running"],
    )?;
    let mut pending = state
        .pending_deletes
        .lock()
        .map_err(|_| "删除确认状态不可用".to_string())?;
    pending.retain(|_, item| {
        item.created_at
            .elapsed()
            .is_ok_and(|elapsed| elapsed <= DELETE_CONFIRMATION_TTL)
    });
    if pending.len() >= MAX_PENDING_DELETES {
        return Err("待确认删除数量已达到上限".to_string());
    }
    pending.insert(
        approval_id.clone(),
        PendingEntryDelete {
            task_id: execution.task_id,
            trace_id: execution.trace_id,
            vault_id: vault_id.clone(),
            vault_name: vault_name.clone(),
            vault_root: root,
            source_path,
            relative_path: normalized_relative.clone(),
            snapshot: snapshot.clone(),
            delete_vault,
            execution_ticket: execution.execution_ticket,
            effect_digest,
            created_at: SystemTime::now(),
        },
    );
    Ok(EntryDeletePreview {
        approval_id,
        vault_id,
        vault_name,
        relative_path: normalized_relative,
        entry_type: if delete_vault {
            "vault".to_string()
        } else {
            snapshot.entry_type
        },
        byte_length: snapshot.byte_length,
        entry_count: snapshot.entry_count,
        fingerprint: snapshot.fingerprint,
        expires_at: (Utc::now()
            + chrono::Duration::from_std(DELETE_CONFIRMATION_TTL)
                .map_err(|error| format!("无法计算删除确认有效期：{error}"))?)
        .to_rfc3339(),
    })
}

#[tauri::command]
pub fn discard_vault_entry_delete(
    state: State<'_, ObsidianManagementState>,
    approval_id: String,
) -> Result<bool, String> {
    discard_vault_entry_delete_inner(state.inner(), &approval_id)
}

fn discard_vault_entry_delete_inner(
    state: &ObsidianManagementState,
    approval_id: &str,
) -> Result<bool, String> {
    Ok(state
        .pending_deletes
        .lock()
        .map_err(|_| "删除确认状态不可用".to_string())?
        .remove(approval_id.trim())
        .is_some())
}

#[tauri::command]
pub fn commit_vault_entry_delete(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianManagementState>,
    database: State<'_, RuntimeDatabase>,
    approval_id: String,
) -> Result<VaultMutationReceipt, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    let receipt = commit_vault_entry_delete_inner(
        &app_data,
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        &approval_id,
    )?;
    crate::vault_watcher::request_vault_index_refresh(&app);
    Ok(receipt)
}

fn commit_vault_entry_delete_inner(
    app_data: &Path,
    state: &ObsidianManagementState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    approval_id: &str,
) -> Result<VaultMutationReceipt, String> {
    let approval_id = approval_id.trim();
    let pending = state
        .pending_deletes
        .lock()
        .map_err(|_| "删除确认状态不可用".to_string())?
        .get(approval_id)
        .cloned()
        .ok_or_else(|| "删除确认不存在或已经失效".to_string())?;
    if pending
        .created_at
        .elapsed()
        .map_or(true, |elapsed| elapsed > DELETE_CONFIRMATION_TTL)
    {
        return Err("删除确认已过期，请重新定位目标".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &pending.task_id,
        &["system:delete", "system:vaults"],
        &["delete"],
        Some(&pending.vault_id),
        &["running"],
    )?;
    let (_, current_root) = canonical_root(&pending.vault_id)?;
    if current_root != pending.vault_root {
        return Err("Vault 路径在确认期间发生变化".to_string());
    }
    let current_snapshot = entry_snapshot(&pending.source_path)?;
    if current_snapshot.fingerprint != pending.snapshot.fingerprint
        || current_snapshot.entry_count != pending.snapshot.entry_count
        || current_snapshot.byte_length != pending.snapshot.byte_length
    {
        return Err("删除目标在确认期间发生变化，请重新确认".to_string());
    }

    let operation_id = Uuid::new_v4().to_string();
    let operation_dir = operation_trash_dir(&operation_id)?;
    let payload_name = pending
        .source_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("vault-data");
    let payload_relative_path = PathBuf::from("payload").join(payload_name);
    let payload_path = operation_dir.join(&payload_relative_path);
    let mut manifest = TrashManifest {
        operation_id: operation_id.clone(),
        vault_id: pending.vault_id.clone(),
        vault_name: pending.vault_name.clone(),
        original_vault_path: pending.vault_root.to_string_lossy().into_owned(),
        original_relative_path: pending.relative_path.clone(),
        entry_type: if pending.delete_vault {
            "vault".to_string()
        } else {
            pending.snapshot.entry_type.clone()
        },
        payload_relative_path: payload_relative_path.to_string_lossy().replace('\\', "/"),
        fingerprint: pending.snapshot.fingerprint.clone(),
        byte_length: pending.snapshot.byte_length,
        entry_count: pending.snapshot.entry_count,
        deleted_at: now_string(),
        state: "prepared".to_string(),
        last_error: None,
        recovery_target_path: None,
    };
    fs::create_dir_all(&operation_dir).map_err(|error| format!("无法创建云枢回收记录：{error}"))?;
    write_json_atomic(&operation_dir.join("manifest.json"), &manifest)?;
    let checkpoint =
        create_checkpoint_at(app_data, &operation_id, "trash-manifest.json", &manifest)?;
    if ticket_state.is_some() && pending.execution_ticket.is_none() {
        let _ = fs::remove_dir_all(&operation_dir);
        return Err("删除确认缺少能力范围执行票据".to_string());
    }
    let execution = BoundManagementExecution {
        task_id: pending.task_id.clone(),
        trace_id: pending.trace_id.clone(),
        execution_ticket: pending.execution_ticket.clone(),
    };
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        Some(&pending.task_id),
        pending.trace_id.as_deref(),
    )?;
    if let Err(error) = begin_management_commit(
        ticket_state,
        &execution,
        &workspace_scope,
        approval_id,
        &pending.effect_digest,
    ) {
        let _ = fs::remove_dir_all(&operation_dir);
        return Err(error);
    }
    if let Err(error) = move_entry(&pending.source_path, &payload_path) {
        let source_exists = pending.source_path.exists();
        let payload_exists = payload_path.exists();
        if source_exists {
            abort_management_commit(ticket_state, pending.execution_ticket.as_deref(), true);
            let _ = fs::remove_dir_all(&operation_dir);
        } else {
            preserve_recovery_manifest(&operation_dir, &mut manifest, &error);
            abort_management_commit(ticket_state, pending.execution_ticket.as_deref(), false);
        }
        if !source_exists && !payload_exists {
            return Err(format!(
                "删除移动失败且源与回收副本均不可见，请保留检查点 {}：{error}",
                checkpoint.display()
            ));
        }
        return Err(error);
    }
    if pending.delete_vault {
        if let Err(error) = remove_vault_registration_for_runtime(&pending.vault_id) {
            let rollback = rollback_trashed_entry(&pending, &payload_path);
            return match rollback {
                Ok(()) => {
                    abort_management_commit(
                        ticket_state,
                        pending.execution_ticket.as_deref(),
                        true,
                    );
                    let _ = fs::remove_dir_all(&operation_dir);
                    Err(format!("无法更新 Obsidian Vault 注册，删除已回滚：{error}"))
                }
                Err(rollback_error) => {
                    let detail = format!(
                        "无法更新 Obsidian Vault 注册，且回滚失败：{error}；{rollback_error}"
                    );
                    preserve_recovery_manifest(&operation_dir, &mut manifest, &detail);
                    abort_management_commit(
                        ticket_state,
                        pending.execution_ticket.as_deref(),
                        false,
                    );
                    Err(format!(
                        "{detail}；唯一回收副本已保留在 {}",
                        operation_dir.display()
                    ))
                }
            };
        }
    }
    manifest.state = "trashed".to_string();
    if let Err(error) = write_json_atomic(&operation_dir.join("manifest.json"), &manifest) {
        let rollback = rollback_trashed_entry(&pending, &payload_path);
        return match rollback {
            Ok(()) => {
                abort_management_commit(ticket_state, pending.execution_ticket.as_deref(), true);
                let _ = fs::remove_dir_all(&operation_dir);
                Err(format!("无法完成回收清单，删除已回滚：{error}"))
            }
            Err(rollback_error) => {
                let detail = format!("无法完成回收清单，且回滚失败：{error}；{rollback_error}");
                preserve_recovery_manifest(&operation_dir, &mut manifest, &detail);
                abort_management_commit(ticket_state, pending.execution_ticket.as_deref(), false);
                Err(format!(
                    "{detail}；唯一回收副本已保留在 {}",
                    operation_dir.display()
                ))
            }
        };
    }
    let event_type = match manifest.entry_type.as_str() {
        "vault" => "vault.delete",
        "folder" => "vault.folder.delete",
        _ => "vault.file.delete",
    };
    let committed_at = match append_event(
        database,
        &pending.task_id,
        Some(trace_id.clone()),
        event_type,
        &pending.vault_id,
        pending.relative_path.clone(),
        format!(
            "用户确认后已移动到云枢回收区：{}",
            operation_dir.to_string_lossy()
        ),
    ) {
        Ok(value) => value,
        Err(error) => {
            let rollback = rollback_trashed_entry(&pending, &payload_path);
            return match rollback {
                Ok(()) => {
                    abort_management_commit(
                        ticket_state,
                        pending.execution_ticket.as_deref(),
                        true,
                    );
                    let _ = fs::remove_dir_all(&operation_dir);
                    Err(format!("无法记录删除审计，删除已回滚：{error}"))
                }
                Err(rollback_error) => {
                    let detail = format!("无法记录删除审计，且回滚失败：{error}；{rollback_error}");
                    preserve_recovery_manifest(&operation_dir, &mut manifest, &detail);
                    abort_management_commit(
                        ticket_state,
                        pending.execution_ticket.as_deref(),
                        false,
                    );
                    Err(format!(
                        "{detail}；唯一回收副本已保留在 {}",
                        operation_dir.display()
                    ))
                }
            };
        }
    };
    if manifest.entry_type == "file" && !pending.delete_vault {
        enqueue_markdown_index_with_trace(
            database,
            &pending.vault_id,
            &pending.vault_root,
            &pending.source_path,
            &trace_id,
        );
    }
    finish_management_commit(ticket_state, pending.execution_ticket.as_deref())?;
    state
        .pending_deletes
        .lock()
        .map_err(|_| "删除确认状态不可用".to_string())?
        .remove(approval_id);
    Ok(VaultMutationReceipt {
        operation_id,
        vault_id: pending.vault_id,
        source_path: pending.relative_path,
        target_path: operation_dir.to_string_lossy().into_owned(),
        entry_type: manifest.entry_type,
        checkpoint_path: checkpoint.to_string_lossy().into_owned(),
        committed_at,
    })
}

fn read_trash_manifest(operation_id: &str) -> Result<(PathBuf, TrashManifest), String> {
    let operation_dir = operation_trash_dir(operation_id)?;
    let canonical_trash = system_trash_root()?
        .canonicalize()
        .map_err(|error| format!("Yunspire 系统回收区不可访问：{error}"))?;
    let canonical_operation = operation_dir
        .canonicalize()
        .map_err(|error| format!("回收记录不存在：{error}"))?;
    if !canonical_operation.starts_with(&canonical_trash) {
        return Err("回收记录越过 Yunspire 系统回收区".to_string());
    }
    let bytes = fs::read(canonical_operation.join("manifest.json"))
        .map_err(|error| format!("无法读取回收清单：{error}"))?;
    let manifest = serde_json::from_slice::<TrashManifest>(&bytes)
        .map_err(|error| format!("回收清单格式无效：{error}"))?;
    if manifest.operation_id != operation_id
        || !matches!(
            manifest.state.as_str(),
            "prepared" | "trashed" | "recovery_required" | "restore_incomplete"
        )
    {
        return Err("回收清单状态无效".to_string());
    }
    let (payload_relative, _) =
        normalized_relative_path(&manifest.payload_relative_path, false, true)?;
    let payload = canonical_operation.join(payload_relative);
    if payload.exists() {
        let canonical_payload = payload
            .canonicalize()
            .map_err(|error| format!("回收数据路径不可访问：{error}"))?;
        if !canonical_payload.starts_with(&canonical_operation) {
            return Err("回收数据路径越过 Yunspire 系统回收区".to_string());
        }
    }
    Ok((canonical_operation, manifest))
}

#[tauri::command]
pub fn list_yunspire_trash_entries() -> Result<Vec<TrashEntryDescriptor>, String> {
    let root = system_trash_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for operation in fs::read_dir(&root)
        .map_err(|error| format!("无法读取 Yunspire 系统回收区：{error}"))?
        .filter_map(Result::ok)
    {
        let operation_id = operation.file_name().to_string_lossy().into_owned();
        if !operation.file_type().is_ok_and(|kind| kind.is_dir())
            || operation_trash_dir(&operation_id).is_err()
        {
            continue;
        }
        let (operation_dir, manifest) = match read_trash_manifest(&operation_id) {
            Ok(value) => value,
            Err(error) => {
                let deleted_at = operation
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(chrono::DateTime::<Utc>::from)
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(now_string);
                entries.push(TrashEntryDescriptor {
                    operation_id,
                    vault_id: String::new(),
                    vault_name: "损坏的回收记录".to_string(),
                    original_vault_path: String::new(),
                    original_relative_path: None,
                    entry_type: "invalid".to_string(),
                    byte_length: 0,
                    entry_count: 0,
                    deleted_at,
                    recoverable: false,
                    state: "invalid".to_string(),
                    error: Some(error),
                });
                continue;
            }
        };
        let payload = operation_dir.join(&manifest.payload_relative_path);
        let recovery_target_exists = manifest
            .recovery_target_path
            .as_deref()
            .is_some_and(|path| Path::new(path).exists());
        entries.push(TrashEntryDescriptor {
            operation_id,
            vault_id: manifest.vault_id,
            vault_name: manifest.vault_name,
            original_vault_path: manifest.original_vault_path,
            original_relative_path: manifest.original_relative_path,
            entry_type: manifest.entry_type,
            byte_length: manifest.byte_length,
            entry_count: manifest.entry_count,
            deleted_at: manifest.deleted_at,
            recoverable: payload.exists() || recovery_target_exists,
            state: manifest.state,
            error: manifest.last_error,
        });
    }
    entries.sort_by(|left, right| right.deleted_at.cmp(&left.deleted_at));
    Ok(entries)
}

fn prepare_trash_purge(operation_id: &str) -> Result<PreparedTrashPurge, String> {
    let (operation_dir, manifest) = read_trash_manifest(operation_id)?;
    if manifest.state != "trashed" {
        return Err(format!(
            "回收记录 {} 当前状态为 {}，必须先完成恢复或回收修复后才能永久删除",
            manifest.operation_id, manifest.state
        ));
    }
    if manifest
        .recovery_target_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists())
    {
        return Err(format!(
            "回收记录 {} 存在未完成恢复目标，拒绝永久删除",
            manifest.operation_id
        ));
    }
    let payload = operation_dir.join(&manifest.payload_relative_path);
    if !payload.exists() {
        return Err(format!(
            "回收记录 {} 的数据源缺失，无法验证永久删除目标",
            manifest.operation_id
        ));
    }
    let snapshot = entry_snapshot(&payload)?;
    let expected_type_matches = if manifest.entry_type == "vault" {
        snapshot.entry_type == "folder"
    } else {
        snapshot.entry_type == manifest.entry_type
    };
    if !expected_type_matches
        || snapshot.fingerprint != manifest.fingerprint
        || snapshot.byte_length != manifest.byte_length
        || snapshot.entry_count != manifest.entry_count
    {
        return Err(format!(
            "回收记录 {} 的数据在永久删除前发生变化，请先检查回收区",
            manifest.operation_id
        ));
    }
    Ok(PreparedTrashPurge {
        operation_dir,
        manifest,
    })
}

fn bind_trash_purge_execution(
    ticket_state: Option<&ExecutionTicketState>,
    workspace_scope: &str,
    operation_context: OperationContext,
    approval_id: &str,
    effect_digest: &str,
) -> Result<BoundManagementExecution, String> {
    let task_id = operation_context
        .task_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "永久删除回收记录缺少原生任务 ID".to_string())?;
    let trace_id = operation_context
        .trace_id
        .filter(|value| !value.trim().is_empty());
    let Some(ticket_state) = ticket_state else {
        return Ok(BoundManagementExecution {
            task_id,
            trace_id,
            execution_ticket: None,
        });
    };
    let execution_ticket = operation_context
        .execution_ticket
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "永久删除回收记录缺少能力范围执行票据".to_string())?;
    ticket_state.bind_operation_approval(
        &execution_ticket,
        workspace_scope,
        &task_id,
        trace_id.as_deref(),
        &["system:delete"],
        &["delete"],
        approval_id,
        effect_digest,
    )?;
    Ok(BoundManagementExecution {
        task_id,
        trace_id,
        execution_ticket: Some(execution_ticket),
    })
}

fn trash_purge_effect_digest(
    workspace_scope: &str,
    kind: &str,
    entries: &[PreparedTrashPurge],
) -> String {
    let payload = serde_json::json!({
        "kind": kind,
        "workspaceScope": workspace_scope,
        "entries": entries.iter().map(|entry| serde_json::json!({
            "operationId": entry.manifest.operation_id,
            "vaultId": entry.manifest.vault_id,
            "originalVaultPath": entry.manifest.original_vault_path,
            "originalRelativePath": entry.manifest.original_relative_path,
            "entryType": entry.manifest.entry_type,
            "fingerprint": entry.manifest.fingerprint,
            "byteLength": entry.manifest.byte_length,
            "entryCount": entry.manifest.entry_count,
        })).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&payload).expect("trash purge digest is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

fn purge_trash_entries_inner(
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    mut entries: Vec<PreparedTrashPurge>,
    kind: &str,
    confirm_permanent_delete: bool,
    operation_context: OperationContext,
) -> Result<TrashPurgeReceipt, String> {
    if !confirm_permanent_delete {
        return Err("永久删除必须由用户显式确认".to_string());
    }
    entries.sort_by(|left, right| left.manifest.operation_id.cmp(&right.manifest.operation_id));
    if entries
        .windows(2)
        .any(|pair| pair[0].manifest.operation_id == pair[1].manifest.operation_id)
    {
        return Err("永久删除请求包含重复回收记录".to_string());
    }
    if entries.is_empty() {
        return Ok(TrashPurgeReceipt {
            purge_id: format!("purge-{}", Uuid::new_v4()),
            operation_ids: Vec::new(),
            purged_entries: 0,
            purged_bytes: 0,
            committed_at: now_string(),
        });
    }
    let workspace_scope = database.local_workspace_scope()?;
    let approval_id = format!("purge-{}", Uuid::new_v4());
    let effect_digest = trash_purge_effect_digest(&workspace_scope, kind, &entries);
    let execution = bind_trash_purge_execution(
        ticket_state,
        &workspace_scope,
        operation_context,
        &approval_id,
        &effect_digest,
    )?;
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        Some(&execution.task_id),
        execution.trace_id.as_deref(),
    )?;
    let mut vault_ids = entries
        .iter()
        .map(|entry| entry.manifest.vault_id.as_str())
        .collect::<Vec<_>>();
    vault_ids.sort_unstable();
    vault_ids.dedup();
    for vault_id in vault_ids {
        database.ensure_runtime_task_authorized(
            &workspace_scope,
            &execution.task_id,
            &["system:delete"],
            &["delete"],
            Some(vault_id),
            &["awaiting_approval", "running"],
        )?;
    }
    begin_management_commit(
        ticket_state,
        &execution,
        &workspace_scope,
        &approval_id,
        &effect_digest,
    )?;

    // Re-read every manifest and payload fingerprint after the execution ticket is
    // in-flight, before performing the first irreversible filesystem operation.
    for entry in &entries {
        let current = match prepare_trash_purge(&entry.manifest.operation_id) {
            Ok(current) => current,
            Err(error) => {
                abort_management_commit(ticket_state, execution.execution_ticket.as_deref(), true);
                return Err(error);
            }
        };
        if current.manifest.fingerprint != entry.manifest.fingerprint
            || current.manifest.byte_length != entry.manifest.byte_length
            || current.manifest.entry_count != entry.manifest.entry_count
            || current.operation_dir != entry.operation_dir
        {
            abort_management_commit(ticket_state, execution.execution_ticket.as_deref(), true);
            return Err("回收区在永久删除确认期间发生变化".to_string());
        }
    }

    for entry in &entries {
        if let Err(error) = append_event_with_state(
            database,
            &execution.task_id,
            Some(trace_id.clone()),
            "vault.trash.purge",
            "running",
            &entry.manifest.vault_id,
            entry.manifest.original_relative_path.clone(),
            format!(
                "永久删除已确认，准备物理清除回收记录 {}；fingerprint={}；bytes={}；entries={}",
                entry.manifest.operation_id,
                entry.manifest.fingerprint,
                entry.manifest.byte_length,
                entry.manifest.entry_count
            ),
        ) {
            abort_management_commit(ticket_state, execution.execution_ticket.as_deref(), true);
            return Err(format!("无法写入永久删除预审计，尚未删除任何数据：{error}"));
        }
    }

    let mut removed_operation_ids = Vec::with_capacity(entries.len());
    for entry in &entries {
        if let Err(error) = fs::remove_dir_all(&entry.operation_dir) {
            let _ = append_event_with_state(
                database,
                &execution.task_id,
                Some(trace_id.clone()),
                "vault.trash.purge",
                "failed",
                &entry.manifest.vault_id,
                entry.manifest.original_relative_path.clone(),
                format!(
                    "物理清除回收记录 {} 失败；此前已删除={}；error={error}",
                    entry.manifest.operation_id,
                    removed_operation_ids.join(",")
                ),
            );
            abort_management_commit(ticket_state, execution.execution_ticket.as_deref(), false);
            return Err(format!(
                "永久删除未完整完成，执行票据已封存；已删除 [{}]，失败目标 {}：{error}",
                removed_operation_ids.join(","),
                entry.manifest.operation_id
            ));
        }
        removed_operation_ids.push(entry.manifest.operation_id.clone());
    }

    let committed_at = now_string();
    for entry in &entries {
        if let Err(error) = append_event_with_state(
            database,
            &execution.task_id,
            Some(trace_id.clone()),
            "vault.trash.purge",
            "success",
            &entry.manifest.vault_id,
            entry.manifest.original_relative_path.clone(),
            format!(
                "已物理清除云枢回收记录 {}；fingerprint={}；bytes={}；entries={}",
                entry.manifest.operation_id,
                entry.manifest.fingerprint,
                entry.manifest.byte_length,
                entry.manifest.entry_count
            ),
        ) {
            abort_management_commit(ticket_state, execution.execution_ticket.as_deref(), false);
            return Err(format!(
                "回收数据已经永久删除，但无法完成成功审计；执行票据已封存：{error}"
            ));
        }
    }
    finish_management_commit(ticket_state, execution.execution_ticket.as_deref())?;
    Ok(TrashPurgeReceipt {
        purge_id: approval_id,
        operation_ids: removed_operation_ids,
        purged_entries: entries.iter().map(|entry| entry.manifest.entry_count).sum(),
        purged_bytes: entries.iter().map(|entry| entry.manifest.byte_length).sum(),
        committed_at,
    })
}

#[tauri::command]
pub fn purge_yunspire_trash_entry(
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    operation_id: String,
    confirm_permanent_delete: bool,
    operation_context: OperationContext,
) -> Result<TrashPurgeReceipt, String> {
    let entry = prepare_trash_purge(operation_id.trim())?;
    purge_trash_entries_inner(
        database.inner(),
        Some(ticket_state.inner()),
        vec![entry],
        "purge_trash_entry",
        confirm_permanent_delete,
        operation_context,
    )
}

#[tauri::command]
pub fn empty_yunspire_trash(
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    confirm_permanent_delete: bool,
    operation_context: OperationContext,
) -> Result<TrashPurgeReceipt, String> {
    let root = system_trash_root()?;
    if !root.exists() {
        return purge_trash_entries_inner(
            database.inner(),
            Some(ticket_state.inner()),
            Vec::new(),
            "empty_trash",
            confirm_permanent_delete,
            operation_context,
        );
    }
    let mut entries = Vec::new();
    for operation in fs::read_dir(&root)
        .map_err(|error| format!("无法读取 Yunspire 系统回收区：{error}"))?
        .filter_map(Result::ok)
    {
        let operation_id = operation.file_name().to_string_lossy().into_owned();
        if !operation
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir())
            || operation_id.starts_with('.')
        {
            continue;
        }
        entries.push(prepare_trash_purge(&operation_id)?);
    }
    purge_trash_entries_inner(
        database.inner(),
        Some(ticket_state.inner()),
        entries,
        "empty_trash",
        confirm_permanent_delete,
        operation_context,
    )
}

#[tauri::command]
pub fn restore_yunspire_trash_entry(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    operation_id: String,
    target_vault_id: Option<String>,
    target_relative_path: Option<String>,
    operation_context: OperationContext,
) -> Result<VaultMutationReceipt, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    let receipt = restore_yunspire_trash_entry_inner(
        &app_data,
        database.inner(),
        Some(ticket_state.inner()),
        operation_id,
        target_vault_id,
        target_relative_path,
        operation_context,
    )?;
    crate::vault_watcher::request_vault_index_refresh(&app);
    Ok(receipt)
}

fn restore_yunspire_trash_entry_inner(
    app_data: &Path,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    operation_id: String,
    target_vault_id: Option<String>,
    target_relative_path: Option<String>,
    operation_context: OperationContext,
) -> Result<VaultMutationReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let (operation_dir, mut manifest) = read_trash_manifest(operation_id.trim())?;
    let payload = operation_dir.join(&manifest.payload_relative_path);
    if !payload.exists() {
        return Err("云枢回收区中的数据已经不存在，无法恢复".to_string());
    }
    let snapshot = entry_snapshot(&payload)?;
    let expected_entry_type = if manifest.entry_type == "vault" {
        "folder"
    } else {
        manifest.entry_type.as_str()
    };
    if snapshot.entry_type != expected_entry_type
        || snapshot.fingerprint != manifest.fingerprint
        || snapshot.byte_length != manifest.byte_length
        || snapshot.entry_count != manifest.entry_count
    {
        return Err(format!(
            "回收数据完整性校验失败：清单记录 {} 项/{} 字节，当前为 {} 项/{} 字节，拒绝恢复",
            manifest.entry_count, manifest.byte_length, snapshot.entry_count, snapshot.byte_length
        ));
    }
    let (vault_id, target, target_label) = if manifest.entry_type == "vault" {
        if target_vault_id.is_some() || target_relative_path.is_some() {
            return Err("恢复整个 Vault 时不能指定 Vault 内路径".to_string());
        }
        let target = PathBuf::from(&manifest.original_vault_path);
        if !target.is_absolute() || target.exists() {
            return Err("原 Vault 路径无效或已经被占用".to_string());
        }
        let parent = target.parent().ok_or("原 Vault 路径缺少父目录")?;
        if !parent.is_dir() {
            return Err("原 Vault 父目录不存在".to_string());
        }
        (
            manifest.vault_id.clone(),
            target.clone(),
            target.to_string_lossy().into_owned(),
        )
    } else {
        let vault_id = target_vault_id
            .as_deref()
            .unwrap_or(&manifest.vault_id)
            .to_string();
        let (_, root) = canonical_root(&vault_id)?;
        let relative = target_relative_path
            .as_deref()
            .or(manifest.original_relative_path.as_deref())
            .ok_or("回收清单缺少原相对路径")?;
        let (target, normalized) = resolve_new_entry(&root, relative)?;
        database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &normalized)?;
        (vault_id, target, normalized)
    };
    let approval_id = Uuid::new_v4().to_string();
    let effect_paths = (manifest.entry_type != "vault")
        .then_some(target_label.as_str())
        .into_iter()
        .collect::<Vec<_>>();
    let effect_digest = management_effect_digest(
        "restore",
        &vault_id,
        &effect_paths,
        serde_json::json!({
            "trashOperationId": manifest.operation_id,
            "fingerprint": manifest.fingerprint,
            "entryType": manifest.entry_type,
        }),
    );
    let execution = bind_management_execution(
        ticket_state,
        &workspace_scope,
        operation_context,
        &["system:vaults"],
        &["restore"],
        &vault_id,
        &effect_paths,
        &approval_id,
        &effect_digest,
    )?;
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        Some(&execution.task_id),
        execution.trace_id.as_deref(),
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:vaults"],
        &["restore"],
        Some(&vault_id),
        &["running"],
    )?;
    let checkpoint = create_checkpoint_at(
        app_data,
        &manifest.operation_id,
        "restore-manifest.json",
        &manifest,
    )?;
    begin_management_commit(
        ticket_state,
        &execution,
        &workspace_scope,
        &approval_id,
        &effect_digest,
    )?;
    if let Err(error) = move_entry(&payload, &target) {
        let rolled_back = payload.exists() && (!target.exists() || remove_entry(&target).is_ok());
        abort_management_commit(
            ticket_state,
            execution.execution_ticket.as_deref(),
            rolled_back,
        );
        if !rolled_back {
            preserve_restore_recovery(&operation_dir, &mut manifest, &target, &error);
        }
        return Err(error);
    }
    let registered_vault_id = if manifest.entry_type == "vault" {
        match register_vault_path_for_runtime(&target) {
            Ok(id) => id,
            Err(error) => {
                let rollback = move_entry(&target, &payload);
                return match rollback {
                    Ok(()) => {
                        abort_management_commit(
                            ticket_state,
                            execution.execution_ticket.as_deref(),
                            true,
                        );
                        Err(format!("无法重新注册恢复的 Vault，恢复已回滚：{error}"))
                    }
                    Err(rollback_error) => {
                        abort_management_commit(
                            ticket_state,
                            execution.execution_ticket.as_deref(),
                            false,
                        );
                        let detail = format!(
                            "无法重新注册恢复的 Vault，且回滚失败：{error}；{rollback_error}"
                        );
                        preserve_restore_recovery(&operation_dir, &mut manifest, &target, &detail);
                        Err(detail)
                    }
                };
            }
        }
    } else {
        vault_id.clone()
    };
    let committed_at = match append_event(
        database,
        &execution.task_id,
        Some(trace_id.clone()),
        if manifest.entry_type == "vault" {
            "vault.restore"
        } else if manifest.entry_type == "folder" {
            "vault.folder.restore"
        } else {
            "vault.file.restore"
        },
        &registered_vault_id,
        (manifest.entry_type != "vault").then_some(target_label.clone()),
        format!("已从云枢回收区恢复 Yunspire 操作 {}", manifest.operation_id),
    ) {
        Ok(value) => value,
        Err(error) => {
            let registration_rollback = if manifest.entry_type == "vault" {
                remove_vault_registration_for_runtime(&registered_vault_id)
            } else {
                Ok(())
            };
            let rollback = move_entry(&target, &payload);
            return match (registration_rollback, rollback) {
                (Ok(()), Ok(())) => {
                    abort_management_commit(
                        ticket_state,
                        execution.execution_ticket.as_deref(),
                        true,
                    );
                    Err(format!("无法记录恢复审计，恢复已回滚：{error}"))
                }
                (registration_result, payload_result) => {
                    abort_management_commit(
                        ticket_state,
                        execution.execution_ticket.as_deref(),
                        false,
                    );
                    let rollback_errors = [registration_result.err(), payload_result.err()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("；");
                    let detail =
                        format!("无法记录恢复审计，且回滚未完整完成：{error}；{rollback_errors}");
                    preserve_restore_recovery(&operation_dir, &mut manifest, &target, &detail);
                    Err(detail)
                }
            };
        }
    };
    if manifest.entry_type == "file" {
        if let Ok((_, root)) = canonical_root(&registered_vault_id) {
            enqueue_markdown_index_with_trace(
                database,
                &registered_vault_id,
                &root,
                &target,
                &trace_id,
            );
        }
    }
    finish_management_commit(ticket_state, execution.execution_ticket.as_deref())?;
    if let Err(error) = fs::remove_dir_all(&operation_dir) {
        log::warn!("数据已恢复，但无法清理回收记录：{error}");
    }
    Ok(VaultMutationReceipt {
        operation_id: manifest.operation_id,
        vault_id: registered_vault_id,
        source_path: manifest.original_relative_path,
        target_path: target_label,
        entry_type: manifest.entry_type,
        checkpoint_path: checkpoint.to_string_lossy().into_owned(),
        committed_at,
    })
}

#[tauri::command]
pub fn create_vault_folder(
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    operation_context: OperationContext,
) -> Result<VaultMutationReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let (_, root) = canonical_root(&vault_id)?;
    let (target, normalized) = resolve_new_entry(&root, &relative_path)?;
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &normalized)?;
    let operation_id = Uuid::new_v4().to_string();
    let effect_digest = management_effect_digest(
        "folder.create",
        &vault_id,
        &[&normalized],
        serde_json::json!({ "operationId": operation_id }),
    );
    let execution = bind_management_execution(
        Some(ticket_state.inner()),
        &workspace_scope,
        operation_context,
        &["system:vaults"],
        &["create"],
        &vault_id,
        &[&normalized],
        &operation_id,
        &effect_digest,
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:vaults"],
        &["create"],
        Some(&vault_id),
        &["running"],
    )?;
    begin_management_commit(
        Some(ticket_state.inner()),
        &execution,
        &workspace_scope,
        &operation_id,
        &effect_digest,
    )?;
    if let Err(error) = fs::create_dir_all(&target) {
        let rolled_back = !target.exists() || remove_entry(&target).is_ok();
        abort_management_commit(
            Some(ticket_state.inner()),
            execution.execution_ticket.as_deref(),
            rolled_back,
        );
        return Err(format!("无法创建 Vault 文件夹：{error}"));
    }
    let committed_at = match append_event(
        &database,
        &execution.task_id,
        execution.trace_id.clone(),
        "vault.folder.create",
        &vault_id,
        Some(normalized.clone()),
        "已创建 Vault 文件夹".to_string(),
    ) {
        Ok(value) => value,
        Err(error) => {
            let rollback = remove_entry(&target);
            abort_management_commit(
                Some(ticket_state.inner()),
                execution.execution_ticket.as_deref(),
                rollback.is_ok(),
            );
            return match rollback {
                Ok(()) => Err(format!("无法记录文件夹创建审计，创建已回滚：{error}")),
                Err(rollback_error) => Err(format!(
                    "无法记录文件夹创建审计，且回滚失败：{error}；{rollback_error}"
                )),
            };
        }
    };
    finish_management_commit(
        Some(ticket_state.inner()),
        execution.execution_ticket.as_deref(),
    )?;
    Ok(VaultMutationReceipt {
        operation_id,
        vault_id,
        source_path: None,
        target_path: normalized,
        entry_type: "folder".to_string(),
        checkpoint_path: String::new(),
        committed_at,
    })
}

#[tauri::command]
pub fn move_vault_entry(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    source_relative_path: String,
    target_relative_path: String,
    operation_context: OperationContext,
) -> Result<VaultMutationReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let (_, root) = canonical_root(&vault_id)?;
    let (source, source_normalized) = resolve_existing_entry(&root, &source_relative_path)?;
    let (target, target_normalized) = resolve_new_entry(&root, &target_relative_path)?;
    if target.starts_with(&source) {
        return Err("不能把文件夹移动到自身内部".to_string());
    }
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &source_normalized)?;
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &target_normalized)?;
    let operation_id = Uuid::new_v4().to_string();
    let snapshot = entry_snapshot(&source)?;
    let effect_digest = management_effect_digest(
        "entry.move",
        &vault_id,
        &[&source_normalized, &target_normalized],
        serde_json::json!({
            "entryType": snapshot.entry_type,
            "fingerprint": snapshot.fingerprint,
        }),
    );
    let execution = bind_management_execution(
        Some(ticket_state.inner()),
        &workspace_scope,
        operation_context,
        &["system:vaults"],
        &["move", "rename"],
        &vault_id,
        &[&source_normalized, &target_normalized],
        &operation_id,
        &effect_digest,
    )?;
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        Some(&execution.task_id),
        execution.trace_id.as_deref(),
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:vaults"],
        &["move", "rename"],
        Some(&vault_id),
        &["running"],
    )?;
    let checkpoint = create_checkpoint(
        &app,
        &operation_id,
        "move.json",
        &serde_json::json!({
            "vaultId": vault_id,
            "sourcePath": source_normalized,
            "targetPath": target_normalized,
            "fingerprint": snapshot.fingerprint,
        }),
    )?;
    begin_management_commit(
        Some(ticket_state.inner()),
        &execution,
        &workspace_scope,
        &operation_id,
        &effect_digest,
    )?;
    if let Err(error) = move_entry(&source, &target) {
        let rolled_back = if source.exists() {
            !target.exists() || remove_entry(&target).is_ok()
        } else if target.exists() {
            move_entry(&target, &source).is_ok()
        } else {
            false
        };
        abort_management_commit(
            Some(ticket_state.inner()),
            execution.execution_ticket.as_deref(),
            rolled_back,
        );
        return Err(error);
    }
    let committed_at = match append_event(
        &database,
        &execution.task_id,
        Some(trace_id.clone()),
        if snapshot.entry_type == "folder" {
            "vault.folder.move"
        } else {
            "vault.file.move"
        },
        &vault_id,
        Some(target_normalized.clone()),
        format!("已从 {source_normalized} 移动到 {target_normalized}"),
    ) {
        Ok(value) => value,
        Err(error) => {
            let rollback = move_entry(&target, &source);
            abort_management_commit(
                Some(ticket_state.inner()),
                execution.execution_ticket.as_deref(),
                rollback.is_ok(),
            );
            return match rollback {
                Ok(()) => Err(format!("无法记录移动审计，移动已回滚：{error}")),
                Err(rollback_error) => Err(format!(
                    "无法记录移动审计，且回滚失败：{error}；{rollback_error}"
                )),
            };
        }
    };
    if snapshot.entry_type == "file" {
        enqueue_markdown_index_with_trace(&database, &vault_id, &root, &source, &trace_id);
        enqueue_markdown_index_with_trace(&database, &vault_id, &root, &target, &trace_id);
    }
    finish_management_commit(
        Some(ticket_state.inner()),
        execution.execution_ticket.as_deref(),
    )?;
    let receipt = VaultMutationReceipt {
        operation_id,
        vault_id,
        source_path: Some(source_normalized),
        target_path: target_normalized,
        entry_type: snapshot.entry_type,
        checkpoint_path: checkpoint.to_string_lossy().into_owned(),
        committed_at,
    };
    crate::vault_watcher::request_vault_index_refresh(&app);
    Ok(receipt)
}

fn read_note(root: &Path, relative_path: &str) -> Result<(PathBuf, String, String), String> {
    let (target, normalized) = resolve_existing_entry(root, relative_path)?;
    if !target.is_file()
        || !target
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("md"))
    {
        return Err("Properties、标签和 Wiki Link 只能修改 Markdown 笔记".to_string());
    }
    let metadata = fs::metadata(&target).map_err(|error| format!("无法读取笔记元数据：{error}"))?;
    let requested_capacity = usize::try_from(metadata.len())
        .map_err(|_| "笔记大小超过当前平台可寻址内存".to_string())?;
    let mut content = String::new();
    content
        .try_reserve_exact(requested_capacity)
        .map_err(|_| "当前可用内存不足以处理该笔记，请释放内存后重试".to_string())?;
    File::open(&target)
        .map_err(|error| format!("无法打开 Markdown 笔记：{error}"))?
        .read_to_string(&mut content)
        .map_err(|error| format!("无法读取 UTF-8 Markdown 笔记：{error}"))?;
    Ok((target, normalized, content))
}

fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn property_key(line: &str) -> Option<String> {
    if line.starts_with(' ') || line.starts_with('\t') || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.trim_matches('"').trim_matches('\'').to_string())
}

fn frontmatter_parts(content: &str) -> (Vec<String>, String) {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    if lines.next() != Some("---") {
        return (Vec::new(), normalized);
    }
    let mut frontmatter = Vec::new();
    let mut body = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed && line == "---" {
            closed = true;
            continue;
        }
        if closed {
            body.push(line.to_string());
        } else {
            frontmatter.push(line.to_string());
        }
    }
    if !closed {
        return (Vec::new(), normalized);
    }
    (frontmatter, body.join("\n"))
}

fn validate_property_key(key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty()
        || key.chars().count() > 128
        || key
            .chars()
            .any(|character| character.is_control() || matches!(character, ':' | '#' | '[' | ']'))
    {
        return Err(format!("Properties 键无效：{key}"));
    }
    Ok(key.to_string())
}

fn yaml_property_line(key: &str, value: &Value) -> Result<String, String> {
    let value = serde_json::to_string(value)
        .map_err(|error| format!("无法序列化 Properties 值：{error}"))?;
    Ok(format!("{key}: {value}"))
}

fn mutate_frontmatter(
    content: &str,
    updates: &Map<String, Value>,
    remove_keys: &BTreeSet<String>,
) -> Result<String, String> {
    let (frontmatter, body) = frontmatter_parts(content);
    let mut output = Vec::new();
    let mut emitted = BTreeSet::new();
    let mut index = 0;
    while index < frontmatter.len() {
        let Some(key) = property_key(&frontmatter[index]) else {
            output.push(frontmatter[index].clone());
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < frontmatter.len() && property_key(&frontmatter[end]).is_none() {
            end += 1;
        }
        if let Some(value) = updates.get(&key) {
            output.push(yaml_property_line(&key, value)?);
            emitted.insert(key.clone());
        } else if !remove_keys.contains(&key) {
            output.extend(frontmatter[index..end].iter().cloned());
        }
        index = end;
    }
    for (key, value) in updates {
        if !emitted.contains(key) && !remove_keys.contains(key) {
            output.push(yaml_property_line(key, value)?);
        }
    }
    while output.last().is_some_and(|line| line.trim().is_empty()) {
        output.pop();
    }
    let body = body.trim_start_matches('\n');
    Ok(format!("---\n{}\n---\n\n{}", output.join("\n"), body))
}

fn parse_property_values(frontmatter: &[String]) -> BTreeMap<String, Value> {
    let mut values = BTreeMap::new();
    let mut index = 0;
    while index < frontmatter.len() {
        let Some(key) = property_key(&frontmatter[index]) else {
            index += 1;
            continue;
        };
        let inline = frontmatter[index]
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or("");
        let mut end = index + 1;
        while end < frontmatter.len() && property_key(&frontmatter[end]).is_none() {
            end += 1;
        }
        let value = if !inline.is_empty() {
            serde_json::from_str(inline).unwrap_or_else(|_| Value::String(inline.to_string()))
        } else {
            let items = frontmatter[index + 1..end]
                .iter()
                .filter_map(|line| line.trim().strip_prefix("- "))
                .map(|value| Value::String(value.trim_matches('"').trim_matches('\'').to_string()))
                .collect::<Vec<_>>();
            if items.is_empty() {
                Value::Null
            } else {
                Value::Array(items)
            }
        };
        values.insert(key, value);
        index = end;
    }
    values
}

#[allow(clippy::too_many_arguments)]
fn write_note_mutation(
    app: &AppHandle,
    ticket_state: &ExecutionTicketState,
    database: &RuntimeDatabase,
    vault_id: &str,
    relative_path: &str,
    expected_hash: Option<&str>,
    operation_context: OperationContext,
    event_type: &str,
    next_content: String,
) -> Result<NotePropertiesResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let (_, root) = canonical_root(vault_id)?;
    let (target, normalized, current) = read_note(&root, relative_path)?;
    database.ensure_vault_write_allowed(&workspace_scope, vault_id, &normalized)?;
    let current_hash = content_hash(&current);
    if expected_hash.is_some_and(|expected| expected != current_hash) {
        return Err("笔记已被 Obsidian 或其他程序修改，请重新读取后再提交".to_string());
    }
    let operation_id = Uuid::new_v4().to_string();
    let effect_digest = management_effect_digest(
        "note.update",
        vault_id,
        &[&normalized],
        serde_json::json!({
            "eventType": event_type,
            "previousHash": current_hash,
            "nextHash": content_hash(&next_content),
        }),
    );
    let execution = bind_management_execution(
        Some(ticket_state),
        &workspace_scope,
        operation_context,
        &["system:vaults"],
        &["update"],
        vault_id,
        &[&normalized],
        &operation_id,
        &effect_digest,
    )?;
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        Some(&execution.task_id),
        execution.trace_id.as_deref(),
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:vaults"],
        &["update"],
        Some(vault_id),
        &["running"],
    )?;
    let checkpoint_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?
        .join("checkpoints")
        .join(&operation_id);
    fs::create_dir_all(&checkpoint_root)
        .map_err(|error| format!("无法创建笔记修改检查点：{error}"))?;
    let checkpoint = checkpoint_root.join("before.md");
    atomic_write_file(&checkpoint, current.as_bytes())?;
    begin_management_commit(
        Some(ticket_state),
        &execution,
        &workspace_scope,
        &operation_id,
        &effect_digest,
    )?;
    if let Err(error) = atomic_write_file(&target, next_content.as_bytes()) {
        let rollback = atomic_write_file(&target, current.as_bytes());
        abort_management_commit(
            Some(ticket_state),
            execution.execution_ticket.as_deref(),
            rollback.is_ok(),
        );
        return match rollback {
            Ok(()) => Err(format!("无法写入笔记，原内容已恢复：{error}")),
            Err(rollback_error) => Err(format!(
                "无法写入笔记，且原内容恢复失败：{error}；{rollback_error}"
            )),
        };
    }
    let committed_at = match append_event(
        database,
        &execution.task_id,
        Some(trace_id.clone()),
        event_type,
        vault_id,
        Some(normalized.clone()),
        format!("已更新 {normalized}，写入前检查点已创建"),
    ) {
        Ok(value) => value,
        Err(error) => {
            let rollback = atomic_write_file(&target, current.as_bytes());
            abort_management_commit(
                Some(ticket_state),
                execution.execution_ticket.as_deref(),
                rollback.is_ok(),
            );
            return match rollback {
                Ok(()) => Err(format!("无法记录笔记修改审计，修改已回滚：{error}")),
                Err(rollback_error) => Err(format!(
                    "无法记录笔记修改审计，且回滚失败：{error}；{rollback_error}"
                )),
            };
        }
    };
    enqueue_markdown_index_with_trace(database, vault_id, &root, &target, &trace_id);
    finish_management_commit(Some(ticket_state), execution.execution_ticket.as_deref())?;
    let (frontmatter, _) = frontmatter_parts(&next_content);
    Ok(NotePropertiesResult {
        vault_id: vault_id.to_string(),
        relative_path: normalized,
        properties: Value::Object(parse_property_values(&frontmatter).into_iter().collect()),
        content_hash: content_hash(&next_content),
        checkpoint_path: checkpoint.to_string_lossy().into_owned(),
        committed_at,
    })
}

#[tauri::command]
pub fn read_note_properties(vault_id: String, relative_path: String) -> Result<Value, String> {
    let (_, root) = canonical_root(&vault_id)?;
    let (_, _, content) = read_note(&root, &relative_path)?;
    let (frontmatter, _) = frontmatter_parts(&content);
    Ok(Value::Object(
        parse_property_values(&frontmatter).into_iter().collect(),
    ))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_note_properties(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    properties: Value,
    remove_keys: Vec<String>,
    expected_hash: Option<String>,
    operation_context: OperationContext,
) -> Result<NotePropertiesResult, String> {
    let object = properties
        .as_object()
        .ok_or_else(|| "Properties 更新必须是 JSON 对象".to_string())?;
    let mut updates = Map::new();
    for (key, value) in object {
        let key = validate_property_key(key)?;
        if value.is_null() {
            continue;
        }
        updates.insert(key, value.clone());
    }
    let remove_keys = remove_keys
        .into_iter()
        .map(|key| validate_property_key(&key))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let (_, root) = canonical_root(&vault_id)?;
    let (_, _, content) = read_note(&root, &relative_path)?;
    let current_hash = content_hash(&content);
    if expected_hash
        .as_deref()
        .is_some_and(|expected| expected != current_hash)
    {
        return Err("笔记已被 Obsidian 或其他程序修改，请重新读取后再提交".to_string());
    }
    let next = mutate_frontmatter(&content, &updates, &remove_keys)?;
    write_note_mutation(
        &app,
        ticket_state.inner(),
        &database,
        &vault_id,
        &relative_path,
        Some(&current_hash),
        operation_context,
        "vault.note.properties.update",
        next,
    )
}

fn normalize_tag(value: &str) -> Result<String, String> {
    let value = value
        .trim()
        .trim_start_matches('#')
        .trim_matches('/')
        .to_string();
    if value.is_empty()
        || value.chars().count() > 120
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(format!("标签无效：{value}"));
    }
    Ok(value)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_note_tags(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    add: Vec<String>,
    remove: Vec<String>,
    operation_context: OperationContext,
) -> Result<NotePropertiesResult, String> {
    let (_, root) = canonical_root(&vault_id)?;
    let (_, _, content) = read_note(&root, &relative_path)?;
    let current_hash = content_hash(&content);
    let (frontmatter, _) = frontmatter_parts(&content);
    let current = parse_property_values(&frontmatter)
        .remove("tags")
        .and_then(|value| match value {
            Value::Array(values) => Some(values),
            Value::String(value) => Some(vec![Value::String(value)]),
            _ => None,
        })
        .unwrap_or_default();
    let mut tags = current
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .map(|value| normalize_tag(&value))
        .collect::<Result<BTreeSet<_>, _>>()?;
    for tag in add {
        tags.insert(normalize_tag(&tag)?);
    }
    for tag in remove {
        tags.remove(&normalize_tag(&tag)?);
    }
    let mut updates = Map::new();
    updates.insert(
        "tags".to_string(),
        Value::Array(tags.into_iter().map(Value::String).collect()),
    );
    let next = mutate_frontmatter(&content, &updates, &BTreeSet::new())?;
    write_note_mutation(
        &app,
        ticket_state.inner(),
        &database,
        &vault_id,
        &relative_path,
        Some(&current_hash),
        operation_context,
        "vault.note.tags.update",
        next,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_note_wiki_link(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    target: String,
    alias: Option<String>,
    action: String,
    operation_context: OperationContext,
) -> Result<NotePropertiesResult, String> {
    let target = target.trim();
    if target.is_empty()
        || target.chars().count() > 300
        || target
            .chars()
            .any(|character| character.is_control() || matches!(character, '[' | ']'))
    {
        return Err("Wiki Link 目标无效".to_string());
    }
    let alias = alias
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 200
                || value
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '[' | ']' | '|'))
            {
                Err("Wiki Link 别名无效".to_string())
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()?;
    let (_, root) = canonical_root(&vault_id)?;
    let (_, _, content) = read_note(&root, &relative_path)?;
    let current_hash = content_hash(&content);
    let pattern = Regex::new(&format!(
        r"!?\[\[{}(?:\|[^\]]+)?\]\]",
        regex::escape(target)
    ))
    .map_err(|error| format!("无法构建 Wiki Link 匹配器：{error}"))?;
    let next = match action.trim() {
        "add" => {
            if pattern.is_match(&content) {
                content
            } else {
                let link = alias.as_deref().map_or_else(
                    || format!("[[{target}]]"),
                    |alias| format!("[[{target}|{alias}]]"),
                );
                format!("{}\n\n## 关联\n\n- {}\n", content.trim_end(), link)
            }
        }
        "remove" => pattern.replace_all(&content, "").into_owned(),
        _ => return Err("Wiki Link 操作必须是 add 或 remove".to_string()),
    };
    write_note_mutation(
        &app,
        ticket_state.inner(),
        &database,
        &vault_id,
        &relative_path,
        Some(&current_hash),
        operation_context,
        "vault.note.wikilink.update",
        next,
    )
}

fn graph_config_path(vault_id: &str) -> Result<(PathBuf, PathBuf), String> {
    let (_, root) = canonical_root(vault_id)?;
    let obsidian_dir = root.join(".obsidian");
    let metadata = fs::symlink_metadata(&obsidian_dir)
        .map_err(|error| format!("Obsidian 配置目录不可访问：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Obsidian 配置目录必须是 Vault 内的普通目录".to_string());
    }
    let canonical_dir = obsidian_dir
        .canonicalize()
        .map_err(|error| format!("Obsidian 配置目录不可访问：{error}"))?;
    if !canonical_dir.starts_with(&root) {
        return Err("Obsidian 配置目录越过 Vault 边界".to_string());
    }
    let path = canonical_dir.join("graph.json");
    if path.exists()
        && fs::symlink_metadata(&path)
            .map_err(|error| format!("Obsidian Graph 配置不可访问：{error}"))?
            .file_type()
            .is_symlink()
    {
        return Err("不允许通过云枢修改符号链接形式的 Graph 配置".to_string());
    }
    Ok((path, root))
}

fn merge_json(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else if let Some(existing) = target.get_mut(key) {
                merge_json(existing, value);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    } else {
        *target = patch.clone();
    }
}

#[tauri::command]
pub fn read_obsidian_graph_config(vault_id: String) -> Result<Value, String> {
    let (path, _) = graph_config_path(&vault_id)?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("Obsidian Graph 配置格式无效：{error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(format!("无法读取 Obsidian Graph 配置：{error}")),
    }
}

#[tauri::command]
pub fn update_obsidian_graph_config(
    app: AppHandle,
    ticket_state: State<'_, ExecutionTicketState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    patch: Value,
    replace: Option<bool>,
    operation_context: OperationContext,
) -> Result<VaultMutationReceipt, String> {
    if !patch.is_object() {
        return Err("Obsidian Graph 配置必须是 JSON 对象".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let (path, _) = graph_config_path(&vault_id)?;
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, ".obsidian/graph.json")?;
    let current_exists = path.is_file();
    let current = read_obsidian_graph_config(vault_id.clone())?;
    let replace = replace.unwrap_or(false);
    let mut next = if replace {
        patch.clone()
    } else {
        current.clone()
    };
    if !replace {
        merge_json(&mut next, &patch);
    }
    let bytes = serde_json::to_vec_pretty(&next)
        .map_err(|error| format!("无法序列化 Obsidian Graph 配置：{error}"))?;
    if bytes.len() > MAX_GRAPH_CONFIG_BYTES {
        return Err("Obsidian Graph 配置超过 1 MB 安全上限".to_string());
    }
    let operation_id = Uuid::new_v4().to_string();
    let graph_relative_path = ".obsidian/graph.json";
    let current_bytes = serde_json::to_vec(&current)
        .map_err(|error| format!("无法序列化当前 Obsidian Graph 配置：{error}"))?;
    let effect_digest = management_effect_digest(
        "graph.config.update",
        &vault_id,
        &[graph_relative_path],
        serde_json::json!({
            "replace": replace,
            "previousHash": format!("{:x}", Sha256::digest(&current_bytes)),
            "nextHash": format!("{:x}", Sha256::digest(&bytes)),
        }),
    );
    let execution = bind_management_execution(
        Some(ticket_state.inner()),
        &workspace_scope,
        operation_context,
        &["system:vaults"],
        &["update"],
        &vault_id,
        &[graph_relative_path],
        &operation_id,
        &effect_digest,
    )?;
    database.ensure_runtime_task_authorized(
        &workspace_scope,
        &execution.task_id,
        &["system:vaults"],
        &["update"],
        Some(&vault_id),
        &["running"],
    )?;
    let checkpoint_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?
        .join("checkpoints")
        .join(&operation_id);
    fs::create_dir_all(&checkpoint_root)
        .map_err(|error| format!("无法创建 Graph 配置检查点：{error}"))?;
    let checkpoint = checkpoint_root.join("graph-before.json");
    write_json_atomic(&checkpoint, &current)?;
    begin_management_commit(
        Some(ticket_state.inner()),
        &execution,
        &workspace_scope,
        &operation_id,
        &effect_digest,
    )?;
    if let Err(error) = atomic_write_file(&path, &bytes) {
        let rollback = if current_exists {
            serde_json::to_vec_pretty(&current)
                .map_err(|serialize_error| {
                    format!("无法序列化 Graph 配置回滚数据：{serialize_error}")
                })
                .and_then(|previous| atomic_write_file(&path, &previous))
        } else if path.exists() {
            fs::remove_file(&path)
                .map_err(|remove_error| format!("无法移除未完成的 Graph 配置：{remove_error}"))
        } else {
            Ok(())
        };
        abort_management_commit(
            Some(ticket_state.inner()),
            execution.execution_ticket.as_deref(),
            rollback.is_ok(),
        );
        return match rollback {
            Ok(()) => Err(format!("无法写入 Graph 配置，原状态已恢复：{error}")),
            Err(rollback_error) => Err(format!(
                "无法写入 Graph 配置，且原状态恢复失败：{error}；{rollback_error}"
            )),
        };
    }
    let committed_at = match append_event(
        &database,
        &execution.task_id,
        execution.trace_id.clone(),
        "vault.graph.config.update",
        &vault_id,
        Some(".obsidian/graph.json".to_string()),
        "已更新 Obsidian Graph 配置并创建检查点".to_string(),
    ) {
        Ok(value) => value,
        Err(error) => {
            let rollback = if current_exists {
                serde_json::to_vec_pretty(&current)
                    .map_err(|serialize_error| {
                        format!("无法序列化 Graph 配置回滚数据：{serialize_error}")
                    })
                    .and_then(|previous| atomic_write_file(&path, &previous))
            } else {
                fs::remove_file(&path)
                    .map_err(|remove_error| format!("无法移除未提交的 Graph 配置：{remove_error}"))
            };
            abort_management_commit(
                Some(ticket_state.inner()),
                execution.execution_ticket.as_deref(),
                rollback.is_ok(),
            );
            return match rollback {
                Ok(()) => Err(format!("无法记录 Graph 配置审计，修改已回滚：{error}")),
                Err(rollback_error) => Err(format!(
                    "无法记录 Graph 配置审计，且回滚失败：{error}；{rollback_error}"
                )),
            };
        }
    };
    finish_management_commit(
        Some(ticket_state.inner()),
        execution.execution_ticket.as_deref(),
    )?;
    Ok(VaultMutationReceipt {
        operation_id,
        vault_id,
        source_path: Some(".obsidian/graph.json".to_string()),
        target_path: ".obsidian/graph.json".to_string(),
        entry_type: "graph_config".to_string(),
        checkpoint_path: checkpoint.to_string_lossy().into_owned(),
        committed_at,
    })
}
