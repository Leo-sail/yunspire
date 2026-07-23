use crate::{obsidian, runtime_db::RuntimeDatabase};
use chrono::Utc;
use reqwest::{header::USER_AGENT, Client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

const RELEASE_API: &str = "https://api.github.com/repos/Leo-sail/yunspire/releases/latest";
const UPDATE_TIMEOUT_SECONDS: u64 = 20;

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    name: Option<String>,
    published_at: Option<String>,
    prerelease: bool,
    draft: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    current_version: String,
    latest_version: String,
    update_available: bool,
    release_name: String,
    release_url: String,
    published_at: Option<String>,
    checked_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultUpdateBackup {
    vault_id: String,
    vault_name: String,
    source_path: String,
    snapshot_path: String,
    file_count: u64,
    byte_length: u64,
    skipped_symlinks: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBackupManifest {
    id: String,
    reason: String,
    app_version: String,
    created_at: String,
    database_backup_path: String,
    vaults: Vec<VaultUpdateBackup>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRollbackResult {
    restored_backup_id: String,
    safety_backup_id: String,
    restored_vaults: usize,
    restored_database_from: String,
    completed_at: String,
}

fn normalized_version(value: &str) -> Option<Vec<u64>> {
    let value = value.trim().trim_start_matches(['v', 'V']);
    let core = value.split(['-', '+']).next()?;
    let parts = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!parts.is_empty()).then_some(parts)
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let (Some(mut latest), Some(mut current)) =
        (normalized_version(latest), normalized_version(current))
    else {
        return false;
    };
    let length = latest.len().max(current.len());
    latest.resize(length, 0);
    current.resize(length, 0);
    latest > current
}

fn update_backup_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("update-backups"))
        .map_err(|error| format!("无法定位更新保护目录：{error}"))
}

fn valid_backup_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn update_backup_directory(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    if !valid_backup_id(id) {
        return Err("更新保护点 ID 无效".to_string());
    }
    Ok(update_backup_root(app)?.join(id))
}

fn safe_snapshot_child(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn copy_snapshot_tree(
    source_root: &Path,
    source: &Path,
    destination_root: &Path,
    file_count: &mut u64,
    byte_length: &mut u64,
    skipped_symlinks: &mut u64,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("无法读取更新快照来源 {}：{error}", source.display()))?;
    if metadata.file_type().is_symlink() {
        *skipped_symlinks = skipped_symlinks.saturating_add(1);
        return Ok(());
    }
    let relative = source
        .strip_prefix(source_root)
        .map_err(|_| "更新快照来源越过 Vault 边界".to_string())?;
    if !relative.as_os_str().is_empty() && !safe_snapshot_child(relative) {
        return Err("更新快照包含不安全相对路径".to_string());
    }
    let destination = destination_root.join(relative);
    if metadata.is_dir() {
        fs::create_dir_all(&destination)
            .map_err(|error| format!("无法创建更新快照目录：{error}"))?;
        for entry in
            fs::read_dir(source).map_err(|error| format!("无法读取 Vault 快照目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取 Vault 快照项：{error}"))?;
            copy_snapshot_tree(
                source_root,
                &entry.path(),
                destination_root,
                file_count,
                byte_length,
                skipped_symlinks,
            )?;
        }
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建更新快照父目录：{error}"))?;
        }
        fs::copy(source, &destination)
            .map_err(|error| format!("无法复制 Vault 更新快照：{error}"))?;
        *file_count = file_count.saturating_add(1);
        *byte_length = byte_length.saturating_add(metadata.len());
    }
    Ok(())
}

fn write_manifest(directory: &Path, manifest: &UpdateBackupManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("无法序列化更新保护清单：{error}"))?;
    let temporary = directory.join("manifest.json.tmp");
    let target = directory.join("manifest.json");
    fs::write(&temporary, bytes).map_err(|error| format!("无法写入更新保护清单：{error}"))?;
    fs::rename(&temporary, &target).map_err(|error| format!("无法原子提交更新保护清单：{error}"))
}

fn read_manifest(directory: &Path) -> Result<UpdateBackupManifest, String> {
    let bytes = fs::read(directory.join("manifest.json"))
        .map_err(|error| format!("无法读取更新保护清单：{error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("更新保护清单格式无效：{error}"))
}

fn prepare_backup(
    app: &AppHandle,
    database: &RuntimeDatabase,
    reason: &str,
) -> Result<UpdateBackupManifest, String> {
    let id = format!(
        "{}-{}",
        Utc::now().format("%Y%m%d-%H%M%S-%3f"),
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let directory = update_backup_directory(app, &id)?;
    let vault_root = directory.join("vaults");
    fs::create_dir_all(&vault_root).map_err(|error| format!("无法创建更新保护目录：{error}"))?;
    let database_backup = database.backup_for_runtime()?;
    let mut vault_backups = Vec::new();
    for (index, vault) in obsidian::discover_vaults_for_runtime()?
        .into_iter()
        .filter(|vault| vault.connection_state == "connected")
        .enumerate()
    {
        let source = PathBuf::from(&vault.path)
            .canonicalize()
            .map_err(|error| format!("无法规范化 Vault {}：{error}", vault.name))?;
        let key = format!(
            "{index:03}-{}",
            &format!("{:x}", Sha256::digest(vault.id.as_bytes()))[..12]
        );
        let snapshot = vault_root.join(key);
        let mut file_count = 0;
        let mut byte_length = 0;
        let mut skipped_symlinks = 0;
        copy_snapshot_tree(
            &source,
            &source,
            &snapshot,
            &mut file_count,
            &mut byte_length,
            &mut skipped_symlinks,
        )?;
        vault_backups.push(VaultUpdateBackup {
            vault_id: vault.id,
            vault_name: vault.name,
            source_path: source.to_string_lossy().into_owned(),
            snapshot_path: snapshot.to_string_lossy().into_owned(),
            file_count,
            byte_length,
            skipped_symlinks,
        });
    }
    let manifest = UpdateBackupManifest {
        id,
        reason: reason.chars().take(120).collect(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: Utc::now().to_rfc3339(),
        database_backup_path: database_backup.path,
        vaults: vault_backups,
    };
    write_manifest(&directory, &manifest)?;
    Ok(manifest)
}

#[tauri::command]
pub async fn check_for_updates() -> Result<UpdateCheckResult, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(UPDATE_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("无法创建更新检查客户端：{error}"))?;
    let response = client
        .get(RELEASE_API)
        .header(USER_AGENT, "Yunspire-Desktop")
        .send()
        .await
        .map_err(|error| format!("无法连接更新服务：{error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        let current = env!("CARGO_PKG_VERSION").to_string();
        return Ok(UpdateCheckResult {
            current_version: current.clone(),
            latest_version: current,
            update_available: false,
            release_name: "当前没有公开的稳定 Release".to_string(),
            release_url: "https://github.com/Leo-sail/yunspire/releases".to_string(),
            published_at: None,
            checked_at: Utc::now().to_rfc3339(),
        });
    }
    if !response.status().is_success() {
        return Err(format!("更新服务返回 HTTP {}", response.status()));
    }
    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|error| format!("更新服务返回无效数据：{error}"))?;
    if release.draft || release.prerelease {
        return Err("最新 Release 不是稳定正式版本".to_string());
    }
    let current = env!("CARGO_PKG_VERSION").to_string();
    let latest = release.tag_name.trim_start_matches(['v', 'V']).to_string();
    Ok(UpdateCheckResult {
        update_available: version_is_newer(&latest, &current),
        current_version: current,
        latest_version: latest,
        release_name: release.name.unwrap_or(release.tag_name),
        release_url: release.html_url,
        published_at: release.published_at,
        checked_at: Utc::now().to_rfc3339(),
    })
}

#[tauri::command]
pub fn prepare_update_installation(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
) -> Result<UpdateBackupManifest, String> {
    prepare_backup(&app, database.inner(), "更新安装前保护点")
}

#[tauri::command]
pub fn list_update_backups(app: AppHandle) -> Result<Vec<UpdateBackupManifest>, String> {
    let root = update_backup_root(&app)?;
    fs::create_dir_all(&root).map_err(|error| format!("无法读取更新保护目录：{error}"))?;
    let mut manifests = fs::read_dir(root)
        .map_err(|error| format!("无法枚举更新保护点：{error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| read_manifest(&entry.path()).ok())
        .collect::<Vec<_>>();
    manifests.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(manifests)
}

#[tauri::command]
pub fn rollback_update_backup(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    backup_id: String,
) -> Result<UpdateRollbackResult, String> {
    let directory = update_backup_directory(&app, backup_id.trim())?;
    let manifest = read_manifest(&directory)?;
    if manifest.id != backup_id.trim() {
        return Err("更新保护清单与请求 ID 不一致".to_string());
    }
    for vault in &manifest.vaults {
        let (_, current_path) = obsidian::resolve_vault_for_runtime(&vault.vault_id)?;
        let expected = PathBuf::from(&vault.source_path)
            .canonicalize()
            .map_err(|error| format!("无法检查原 Vault 路径：{error}"))?;
        if current_path != expected {
            return Err(format!("Vault {} 的路径已变化，拒绝回滚", vault.vault_name));
        }
        let snapshot = PathBuf::from(&vault.snapshot_path)
            .canonicalize()
            .map_err(|error| format!("无法读取 Vault 保护快照：{error}"))?;
        if !snapshot.starts_with(directory.join("vaults")) {
            return Err("Vault 保护快照越过更新保护目录".to_string());
        }
    }
    let safety = prepare_backup(&app, database.inner(), "执行本地回滚前安全保护点")?;
    for vault in &manifest.vaults {
        let source = PathBuf::from(&vault.snapshot_path);
        let target = PathBuf::from(&vault.source_path);
        let mut file_count = 0;
        let mut byte_length = 0;
        let mut skipped_symlinks = 0;
        copy_snapshot_tree(
            &source,
            &source,
            &target,
            &mut file_count,
            &mut byte_length,
            &mut skipped_symlinks,
        )?;
    }
    let database_restore = database.restore_for_runtime(&manifest.database_backup_path)?;
    Ok(UpdateRollbackResult {
        restored_backup_id: manifest.id,
        safety_backup_id: safety.id,
        restored_vaults: manifest.vaults.len(),
        restored_database_from: database_restore.restored_from,
        completed_at: Utc::now().to_rfc3339(),
    })
}
