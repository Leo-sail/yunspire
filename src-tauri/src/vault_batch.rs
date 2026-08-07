use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use tempfile::NamedTempFile;
use uuid::Uuid;

const MANIFEST_VERSION: u32 = 1;
const MANIFEST_NAME: &str = "batch-manifest.json";
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECOVERY_MANIFESTS: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BatchManifestState {
    Preparing,
    Prepared,
    Committing,
    FilesCommitted,
    Committed,
    RollingBack,
    RolledBack,
    RecoveryFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BatchItemState {
    Pending,
    Writing,
    Written,
    RolledBack,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchAuditRecord {
    pub(crate) id: String,
    pub(crate) task_id: Option<String>,
    pub(crate) trace_id: Option<String>,
    pub(crate) event_type: String,
    pub(crate) created_at: String,
    pub(crate) detail: String,
}

#[derive(Clone, Debug)]
pub(crate) struct BatchManifestEntryInput {
    pub(crate) approval_id: String,
    pub(crate) vault_id: String,
    pub(crate) vault_root: PathBuf,
    pub(crate) relative_path: String,
    pub(crate) previous_hash: Option<String>,
    pub(crate) next_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchManifestItem {
    approval_id: String,
    vault_id: String,
    vault_root: String,
    relative_path: String,
    backup_relative_path: Option<String>,
    previous_hash: Option<String>,
    next_hash: String,
    state: BatchItemState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultBatchManifest {
    version: u32,
    pub(crate) batch_id: String,
    pub(crate) batch_kind: String,
    pub(crate) state: BatchManifestState,
    committed_count: usize,
    items: Vec<BatchManifestItem>,
    pub(crate) audit: BatchAuditRecord,
    created_at: String,
    updated_at: String,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BatchRecoverySummary {
    pub(crate) completed_audits: usize,
    pub(crate) rolled_back_batches: usize,
    pub(crate) failures: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchCommitPoint {
    BeforeWrite,
    AfterWriteBeforeProgress,
    AfterProgress,
}

#[derive(Clone, Copy)]
pub(crate) enum BatchFileSource<'a> {
    Bytes(&'a [u8]),
    Path(&'a Path),
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut source = File::open(path)
        .map_err(|error| format!("无法打开批次源文件 {}：{error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| format!("无法读取批次源文件 {}：{error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

impl BatchFileSource<'_> {
    fn content_hash(self) -> Result<String, String> {
        match self {
            Self::Bytes(content) => Ok(hash_bytes(content)),
            Self::Path(path) => hash_file(path),
        }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("无法同步批次目录 {}：{error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn durable_atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("批次文件缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建批次目录：{error}"))?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|error| format!("无法创建批次临时文件：{error}"))?;
    temporary
        .write_all(content)
        .map_err(|error| format!("无法写入批次临时文件：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步批次临时文件：{error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("无法原子替换批次文件：{}", error.error))?;
    sync_directory(parent)
}

fn durable_atomic_copy(path: &Path, source_path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("批次文件缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建批次目录：{error}"))?;
    let mut source = File::open(source_path)
        .map_err(|error| format!("无法打开批次源文件 {}：{error}", source_path.display()))?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|error| format!("无法创建批次临时文件：{error}"))?;
    std::io::copy(&mut source, &mut temporary)
        .map_err(|error| format!("无法流式写入批次临时文件：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步批次临时文件：{error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("无法原子替换批次文件：{}", error.error))?;
    sync_directory(parent)
}

fn durable_atomic_write_source(path: &Path, source: BatchFileSource<'_>) -> Result<(), String> {
    match source {
        BatchFileSource::Bytes(content) => durable_atomic_write(path, content),
        BatchFileSource::Path(source_path) => durable_atomic_copy(path, source_path),
    }
}

fn persist_manifest(directory: &Path, manifest: &mut VaultBatchManifest) -> Result<(), String> {
    manifest.updated_at = Utc::now().to_rfc3339();
    let payload = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("无法序列化跨 Vault 批次 manifest：{error}"))?;
    if payload.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("跨 Vault 批次 manifest 超过 2 MB 安全上限".to_string());
    }
    durable_atomic_write(&directory.join(MANIFEST_NAME), &payload)
}

fn normalized_relative_path(value: &str) -> Result<PathBuf, String> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains('\0') {
        return Err("批次 manifest 包含无效相对路径".to_string());
    }
    let path = PathBuf::from(&normalized);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("批次 manifest 相对路径越过 Vault 边界".to_string());
    }
    Ok(path)
}

fn resolve_manifest_target(
    item: &BatchManifestItem,
    create_parent: bool,
) -> Result<PathBuf, String> {
    let declared_root = PathBuf::from(&item.vault_root);
    if !declared_root.is_absolute() {
        return Err("批次 manifest Vault 根目录不是绝对路径".to_string());
    }
    let canonical_root = declared_root
        .canonicalize()
        .map_err(|error| format!("无法规范化批次 Vault 根目录：{error}"))?;
    if canonical_root != declared_root {
        return Err("批次 manifest Vault 根目录在提交后发生变化".to_string());
    }
    let relative = normalized_relative_path(&item.relative_path)?;
    let target = canonical_root.join(relative);
    let parent = target.parent().ok_or("批次目标缺少父目录")?;
    if create_parent {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建批次目标目录：{error}"))?;
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("无法规范化批次目标目录：{error}"))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("批次目标目录越过 Vault 边界".to_string());
    }
    Ok(target)
}

fn current_hash(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    hash_file(path).map(Some)
}

pub(crate) fn prepare_batch_manifest(
    app_data: &Path,
    batch_id: String,
    batch_kind: &str,
    event_type: &str,
    task_id: Option<String>,
    trace_id: Option<String>,
    entries: Vec<BatchManifestEntryInput>,
) -> Result<(PathBuf, VaultBatchManifest), String> {
    if entries.is_empty() {
        return Err("跨 Vault 批次 manifest 不能为空".to_string());
    }
    let directory = app_data.join("checkpoints").join(&batch_id);
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建批次检查点：{error}"))?;
    let now = Utc::now().to_rfc3339();
    let items = entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| BatchManifestItem {
            approval_id: entry.approval_id,
            vault_id: entry.vault_id,
            vault_root: entry.vault_root.to_string_lossy().into_owned(),
            relative_path: entry.relative_path,
            backup_relative_path: entry
                .previous_hash
                .as_ref()
                .map(|_| format!("{index}.before")),
            previous_hash: entry.previous_hash,
            next_hash: entry.next_hash,
            state: BatchItemState::Pending,
        })
        .collect::<Vec<_>>();
    let mut manifest = VaultBatchManifest {
        version: MANIFEST_VERSION,
        batch_id: batch_id.clone(),
        batch_kind: batch_kind.to_string(),
        state: BatchManifestState::Preparing,
        committed_count: 0,
        items,
        audit: BatchAuditRecord {
            id: Uuid::new_v4().to_string(),
            task_id,
            trace_id,
            event_type: event_type.to_string(),
            created_at: now.clone(),
            detail: String::new(),
        },
        created_at: now,
        updated_at: String::new(),
        last_error: None,
    };
    manifest.audit.detail = format!(
        "{}批次 {} 已通过可恢复 manifest 提交 {} 个文件",
        batch_kind,
        batch_id,
        manifest.items.len()
    );
    persist_manifest(&directory, &mut manifest)?;

    for item in &manifest.items {
        let target = resolve_manifest_target(item, true)?;
        if current_hash(&target)? != item.previous_hash {
            return Err(format!(
                "创建批次 manifest 时目标版本发生变化：{}",
                item.relative_path
            ));
        }
        if let Some(backup_relative_path) = item.backup_relative_path.as_deref() {
            let backup = directory.join(backup_relative_path);
            durable_atomic_copy(&backup, &target)
                .map_err(|error| format!("无法保存批次检查点：{error}"))?;
            if current_hash(&backup)? != item.previous_hash {
                return Err(format!("批次检查点哈希不一致：{}", item.relative_path));
            }
        }
    }
    sync_directory(&directory)?;
    manifest.state = BatchManifestState::Prepared;
    persist_manifest(&directory, &mut manifest)?;
    Ok((directory, manifest))
}

pub(crate) fn commit_batch_sources(
    directory: &Path,
    manifest: &mut VaultBatchManifest,
    sources: &[BatchFileSource<'_>],
) -> Result<(), String> {
    commit_batch_sources_with_hook(directory, manifest, sources, |_, _| Ok(()))
}

fn commit_batch_sources_with_hook<F>(
    directory: &Path,
    manifest: &mut VaultBatchManifest,
    sources: &[BatchFileSource<'_>],
    mut hook: F,
) -> Result<(), String>
where
    F: FnMut(usize, BatchCommitPoint) -> Result<(), String>,
{
    if manifest.state != BatchManifestState::Prepared || sources.len() != manifest.items.len() {
        return Err("跨 Vault 批次 manifest 状态或内容数量无效".to_string());
    }
    manifest.state = BatchManifestState::Committing;
    persist_manifest(directory, manifest)?;
    for (index, source) in sources.iter().copied().enumerate() {
        manifest.items[index].state = BatchItemState::Writing;
        persist_manifest(directory, manifest)?;
        hook(index, BatchCommitPoint::BeforeWrite)?;
        let target = resolve_manifest_target(&manifest.items[index], true)?;
        if current_hash(&target)? != manifest.items[index].previous_hash {
            return Err(format!(
                "批次提交前目标已被外部修改：{}",
                manifest.items[index].relative_path
            ));
        }
        if source.content_hash()? != manifest.items[index].next_hash {
            return Err(format!(
                "批次内容摘要与 manifest 不一致：{}",
                manifest.items[index].relative_path
            ));
        }
        durable_atomic_write_source(&target, source)?;
        hook(index, BatchCommitPoint::AfterWriteBeforeProgress)?;
        if current_hash(&target)?.as_deref() != Some(&manifest.items[index].next_hash) {
            return Err(format!(
                "批次目标写入后哈希不一致：{}",
                manifest.items[index].relative_path
            ));
        }
        manifest.items[index].state = BatchItemState::Written;
        manifest.committed_count = index + 1;
        persist_manifest(directory, manifest)?;
        hook(index, BatchCommitPoint::AfterProgress)?;
    }
    manifest.state = BatchManifestState::FilesCommitted;
    persist_manifest(directory, manifest)
}

fn mark_recovery_failed(directory: &Path, manifest: &mut VaultBatchManifest, error: &str) {
    manifest.state = BatchManifestState::RecoveryFailed;
    manifest.last_error = Some(error.chars().take(4_000).collect());
    let _ = persist_manifest(directory, manifest);
}

pub(crate) fn rollback_batch_manifest(
    directory: &Path,
    manifest: &mut VaultBatchManifest,
) -> Result<(), String> {
    manifest.state = BatchManifestState::RollingBack;
    manifest.last_error = None;
    persist_manifest(directory, manifest)?;
    for index in (0..manifest.items.len()).rev() {
        let item = manifest.items[index].clone();
        let target = match resolve_manifest_target(&item, true) {
            Ok(target) => target,
            Err(error) => {
                mark_recovery_failed(directory, manifest, &error);
                return Err(error);
            }
        };
        let current = match current_hash(&target) {
            Ok(current) => current,
            Err(error) => {
                mark_recovery_failed(directory, manifest, &error);
                return Err(error);
            }
        };
        if current != item.previous_hash {
            if current.as_deref() != Some(item.next_hash.as_str()) {
                let error = format!("恢复发现目标已被外部修改，未覆盖：{}", item.relative_path);
                mark_recovery_failed(directory, manifest, &error);
                return Err(error);
            }
            if let Some(previous_hash) = item.previous_hash.as_deref() {
                let backup_relative = item
                    .backup_relative_path
                    .as_deref()
                    .ok_or_else(|| "恢复缺少已有文件检查点".to_string())?;
                let backup = directory.join(backup_relative);
                if hash_file(&backup)? != previous_hash {
                    let error = format!("批次恢复检查点哈希不一致：{}", item.relative_path);
                    mark_recovery_failed(directory, manifest, &error);
                    return Err(error);
                }
                if let Err(error) = durable_atomic_copy(&target, &backup) {
                    mark_recovery_failed(directory, manifest, &error);
                    return Err(error);
                }
            } else {
                if let Err(error) = fs::remove_file(&target) {
                    let error = format!("无法移除批次新建文件：{error}");
                    mark_recovery_failed(directory, manifest, &error);
                    return Err(error);
                }
                if let Some(parent) = target.parent() {
                    if let Err(error) = sync_directory(parent) {
                        mark_recovery_failed(directory, manifest, &error);
                        return Err(error);
                    }
                }
            }
        }
        manifest.items[index].state = BatchItemState::RolledBack;
        if let Err(error) = persist_manifest(directory, manifest) {
            mark_recovery_failed(directory, manifest, &error);
            return Err(error);
        }
    }
    manifest.committed_count = 0;
    manifest.state = BatchManifestState::RolledBack;
    persist_manifest(directory, manifest)
}

fn validate_files_committed(manifest: &VaultBatchManifest) -> Result<(), String> {
    for item in &manifest.items {
        let target = resolve_manifest_target(item, false)?;
        if current_hash(&target)?.as_deref() != Some(item.next_hash.as_str()) {
            return Err(format!(
                "待补审计批次的目标内容不匹配：{}",
                item.relative_path
            ));
        }
    }
    Ok(())
}

pub(crate) fn mark_batch_committed(
    directory: &Path,
    manifest: &mut VaultBatchManifest,
) -> Result<(), String> {
    if manifest.state != BatchManifestState::FilesCommitted {
        return Err("只有文件已提交的批次才能标记完成".to_string());
    }
    manifest.state = BatchManifestState::Committed;
    manifest.last_error = None;
    persist_manifest(directory, manifest)
}

fn load_manifest(path: &Path) -> Result<VaultBatchManifest, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取批次 manifest 元数据：{error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("批次 manifest 类型无效或超过安全上限".to_string());
    }
    let manifest = serde_json::from_slice::<VaultBatchManifest>(
        &fs::read(path).map_err(|error| format!("无法读取批次 manifest：{error}"))?,
    )
    .map_err(|error| format!("无法解析批次 manifest：{error}"))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(format!("不支持的批次 manifest 版本：{}", manifest.version));
    }
    let directory_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .ok_or_else(|| "批次 manifest 目录名称无效".to_string())?;
    if directory_name != manifest.batch_id {
        return Err("批次 manifest ID 与目录不一致".to_string());
    }
    Ok(manifest)
}

pub(crate) fn recover_batch_manifests<F>(
    app_data: &Path,
    mut append_audit: F,
) -> BatchRecoverySummary
where
    F: FnMut(&BatchAuditRecord) -> Result<(), String>,
{
    let mut summary = BatchRecoverySummary::default();
    let checkpoints = app_data.join("checkpoints");
    let Ok(entries) = fs::read_dir(&checkpoints) else {
        return summary;
    };
    for entry in entries.take(MAX_RECOVERY_MANIFESTS) {
        let Ok(entry) = entry else {
            continue;
        };
        let directory = entry.path();
        let Ok(metadata) = entry.file_type() else {
            continue;
        };
        if !metadata.is_dir() || metadata.is_symlink() {
            continue;
        }
        let path = directory.join(MANIFEST_NAME);
        if !path.exists() {
            continue;
        }
        let mut manifest = match load_manifest(&path) {
            Ok(manifest) => manifest,
            Err(error) => {
                summary
                    .failures
                    .push(format!("{}：{error}", directory.display()));
                continue;
            }
        };
        match manifest.state {
            BatchManifestState::Committed | BatchManifestState::RolledBack => {}
            BatchManifestState::FilesCommitted => {
                let result = validate_files_committed(&manifest)
                    .and_then(|()| append_audit(&manifest.audit))
                    .and_then(|()| mark_batch_committed(&directory, &mut manifest));
                match result {
                    Ok(()) => summary.completed_audits += 1,
                    Err(error) => summary.failures.push(format!(
                        "批次 {} 无法完成审计恢复：{error}",
                        manifest.batch_id
                    )),
                }
            }
            BatchManifestState::Preparing
            | BatchManifestState::Prepared
            | BatchManifestState::Committing
            | BatchManifestState::RollingBack
            | BatchManifestState::RecoveryFailed => {
                match rollback_batch_manifest(&directory, &mut manifest) {
                    Ok(()) => summary.rolled_back_batches += 1,
                    Err(error) => summary
                        .failures
                        .push(format!("批次 {} 无法自动回滚：{error}", manifest.batch_id)),
                }
            }
        }
    }
    summary
}

pub(crate) fn checkpoint_path(
    directory: &Path,
    manifest: &VaultBatchManifest,
    index: usize,
) -> PathBuf {
    manifest
        .items
        .get(index)
        .and_then(|item| item.backup_relative_path.as_deref())
        .map(|path| directory.join(path))
        .unwrap_or_else(|| directory.join(MANIFEST_NAME))
}
