//! Durable, restart-safe storage for large local documents and binary assets.
//!
//! Payload bytes live in the application data directory instead of SQLite or
//! workspace snapshots. SQLite only stores small descriptors and the current
//! upload state. All IPC reads and writes are chunked; there is deliberately no
//! aggregate size limit for an asset or for a creation document.

use base64::Engine;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(debug_assertions)]
use std::env;
use std::{
    collections::HashMap,
    fs,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::runtime_db::RuntimeDatabase;

/// IPC is intentionally bounded per chunk. The complete resource is not.
pub(crate) const MAX_ASSET_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENCODED_CHUNK_BYTES: usize = MAX_ASSET_CHUNK_BYTES.div_ceil(3) * 4 + 16;
const MAX_ASSET_METADATA_BYTES: usize = 64 * 1024;
const MAX_READ_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_READ_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct DurableAssetState {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl DurableAssetState {
    fn asset_lock(&self, asset_id: &str) -> Result<Arc<Mutex<()>>, String> {
        let mut locks = self
            .locks
            .lock()
            .map_err(|_| "耐久资产运行时锁不可用".to_string())?;
        Ok(Arc::clone(
            locks
                .entry(asset_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        ))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginDurableAssetInput {
    #[serde(default)]
    pub asset_id: Option<String>,
    pub owner_type: String,
    pub owner_id: String,
    #[serde(default = "default_asset_role")]
    pub role: String,
    pub file_name: String,
    pub mime_type: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportLegacyCreationDraftAssetInput {
    pub attachment_id: String,
    pub owner_id: String,
    #[serde(default = "default_inline_image_role")]
    pub role: String,
    pub file_name: String,
    pub mime_type: String,
    #[serde(default)]
    pub metadata: Value,
}

fn default_asset_role() -> String {
    "source".to_string()
}

fn default_inline_image_role() -> String {
    "inline_image".to_string()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAssetDescriptor {
    pub asset_id: String,
    pub staged_id: Option<String>,
    pub owner_type: String,
    pub owner_id: String,
    pub role: String,
    pub file_name: String,
    pub mime_type: String,
    pub state: String,
    pub relative_path: Option<String>,
    pub byte_length: u64,
    pub sha256: Option<String>,
    pub expected_sha256: Option<String>,
    pub metadata: Value,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finalized_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAssetChunk {
    asset_id: String,
    offset: u64,
    next_offset: u64,
    byte_length: u64,
    content_base64: String,
    eof: bool,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAssetPage {
    items: Vec<DurableAssetDescriptor>,
    next_cursor_updated_at: Option<String>,
    next_cursor_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAssetReconcileReport {
    pub(crate) ready: usize,
    pub(crate) staging: usize,
    pub(crate) recovered_finalizations: usize,
    pub(crate) source_missing: usize,
}

struct AssetRow {
    descriptor: DurableAssetDescriptor,
    storage_relative_path: String,
}

pub(crate) fn migrate_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS durable_assets (
               workspace_scope TEXT NOT NULL,
               asset_id TEXT NOT NULL,
               staged_id TEXT,
               owner_type TEXT NOT NULL,
               owner_id TEXT NOT NULL,
               role TEXT NOT NULL,
               file_name TEXT NOT NULL,
               mime_type TEXT NOT NULL,
               state TEXT NOT NULL CHECK(state IN (
                 'staging', 'ready', 'failed', 'source_missing', 'deleted'
               )),
               storage_relative_path TEXT NOT NULL,
               byte_length INTEGER NOT NULL DEFAULT 0 CHECK(byte_length >= 0),
               sha256 TEXT,
               expected_sha256 TEXT,
               metadata_json TEXT NOT NULL DEFAULT '{}',
               last_error TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               finalized_at TEXT,
               PRIMARY KEY(workspace_scope, asset_id),
               UNIQUE(workspace_scope, staged_id),
               FOREIGN KEY(workspace_scope)
                 REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_durable_assets_owner
               ON durable_assets(workspace_scope, owner_type, owner_id, role, updated_at DESC);
             CREATE INDEX IF NOT EXISTS idx_durable_assets_state
               ON durable_assets(workspace_scope, state, updated_at);",
        )
        .map_err(|error| format!("无法创建耐久资产表：{error}"))
}

fn app_data_root(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    let app_data = env::var_os("YUNSPIRE_APP_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or(
            app.path()
                .app_data_dir()
                .map_err(|error| format!("无法定位应用数据目录：{error}"))?,
        );
    #[cfg(not(debug_assertions))]
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    Ok(app_data)
}

fn asset_store_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app_data_root(app)?.join("asset-store");
    ensure_store_directories(&root)?;
    Ok(root)
}

fn ensure_store_directories(root: &Path) -> Result<(), String> {
    for child in ["staging", "objects", "trash"] {
        fs::create_dir_all(root.join(child)).map_err(|error| {
            format!(
                "无法创建耐久资产目录 {}：{error}",
                root.join(child).display()
            )
        })?;
    }
    Ok(())
}

fn valid_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn normalize_owner_id(value: &str) -> Result<String, String> {
    let normalized = value.trim();
    if normalized.is_empty()
        || normalized.chars().count() > 240
        || normalized.chars().any(char::is_control)
    {
        return Err("耐久资产 ownerId 无效".to_string());
    }
    Ok(normalized.to_string())
}

fn normalize_file_name(value: &str) -> Result<String, String> {
    let normalized = value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(255)
        .collect::<String>();
    if normalized.is_empty() {
        return Err("耐久资产文件名不能为空".to_string());
    }
    Ok(normalized)
}

fn normalize_mime_type(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 127
        || !normalized.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '/' | '-' | '+' | '.' | ';' | '=')
        })
    {
        return Err("耐久资产 MIME 类型无效".to_string());
    }
    Ok(normalized)
}

fn normalize_sha256(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = value
        .strip_prefix("sha256:")
        .or_else(|| value.strip_prefix("SHA256:"))
        .unwrap_or(value)
        .to_ascii_lowercase();
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("耐久资产 SHA-256 格式无效".to_string());
    }
    Ok(Some(value))
}

fn display_sha256(value: Option<String>) -> Option<String> {
    value.map(|value| format!("sha256:{value}"))
}

fn metadata_json(value: &Value) -> Result<String, String> {
    let serialized = serde_json::to_string(value)
        .map_err(|error| format!("无法序列化耐久资产元数据：{error}"))?;
    if serialized.len() > MAX_ASSET_METADATA_BYTES {
        return Err("耐久资产元数据超过单条 64 KB 安全上限".to_string());
    }
    Ok(serialized)
}

fn staging_relative_path(staged_id: &str) -> String {
    format!("staging/{staged_id}.part")
}

fn object_relative_path(asset_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(asset_id.as_bytes()));
    format!("objects/{}/{}.blob", &digest[..2], asset_id)
}

fn checked_store_path(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("耐久资产路径越过存储边界".to_string());
    }
    Ok(root.join(relative))
}

fn row_to_asset(row: &Row<'_>) -> rusqlite::Result<AssetRow> {
    let _workspace_scope: String = row.get(0)?;
    let metadata_json: String = row.get(13)?;
    let metadata = serde_json::from_str(&metadata_json).unwrap_or(Value::Null);
    let sha256: Option<String> = row.get(11)?;
    let expected_sha256: Option<String> = row.get(12)?;
    Ok(AssetRow {
        descriptor: DurableAssetDescriptor {
            asset_id: row.get(1)?,
            staged_id: row.get(2)?,
            owner_type: row.get(3)?,
            owner_id: row.get(4)?,
            role: row.get(5)?,
            file_name: row.get(6)?,
            mime_type: row.get(7)?,
            state: row.get(8)?,
            relative_path: None,
            byte_length: row.get::<_, i64>(10)?.max(0) as u64,
            sha256: display_sha256(sha256),
            expected_sha256: display_sha256(expected_sha256),
            metadata,
            last_error: row.get(14)?,
            created_at: row.get(15)?,
            updated_at: row.get(16)?,
            finalized_at: row.get(17)?,
        },
        storage_relative_path: row.get(9)?,
    })
}

const ASSET_SELECT: &str =
    "SELECT workspace_scope, asset_id, staged_id, owner_type, owner_id, role,
            file_name, mime_type, state, storage_relative_path, byte_length,
            sha256, expected_sha256, metadata_json, last_error, created_at,
            updated_at, finalized_at
       FROM durable_assets";

fn load_asset_row(
    connection: &Connection,
    workspace_scope: &str,
    asset_id: &str,
) -> Result<Option<AssetRow>, String> {
    connection
        .query_row(
            &format!("{ASSET_SELECT} WHERE workspace_scope=?1 AND asset_id=?2"),
            params![workspace_scope, asset_id],
            row_to_asset,
        )
        .optional()
        .map_err(|error| format!("无法读取耐久资产：{error}"))
}

fn descriptor_with_relative_path(mut row: AssetRow) -> DurableAssetDescriptor {
    row.descriptor.relative_path =
        (!row.storage_relative_path.is_empty()).then_some(row.storage_relative_path);
    row.descriptor
}

fn begin_upload_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: BeginDurableAssetInput,
) -> Result<DurableAssetDescriptor, String> {
    ensure_store_directories(root)?;
    let asset_id = input
        .asset_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("asset-{}", Uuid::new_v4()));
    if !valid_identifier(&asset_id, 128) {
        return Err("耐久资产 assetId 无效".to_string());
    }
    let owner_type = input.owner_type.trim().to_string();
    if !valid_identifier(&owner_type, 64) {
        return Err("耐久资产 ownerType 无效".to_string());
    }
    let owner_id = normalize_owner_id(&input.owner_id)?;
    let role = input.role.trim().to_string();
    if !valid_identifier(&role, 64) {
        return Err("耐久资产 role 无效".to_string());
    }
    let file_name = normalize_file_name(&input.file_name)?;
    let mime_type = normalize_mime_type(&input.mime_type)?;
    let expected_sha256 = normalize_sha256(input.expected_sha256.as_deref())?;
    let metadata_json = metadata_json(&input.metadata)?;
    let staged_id = format!("staged-{}", Uuid::new_v4());
    let relative_path = staging_relative_path(&staged_id);
    let path = checked_store_path(root, &relative_path)?;
    let created_at = Utc::now().to_rfc3339();

    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    if let Some(existing) = load_asset_row(&connection, workspace_scope, &asset_id)? {
        let descriptor = descriptor_with_relative_path(existing);
        if descriptor.owner_type == owner_type
            && descriptor.owner_id == owner_id
            && descriptor.role == role
            && descriptor.file_name == file_name
            && descriptor.mime_type == mime_type
            && matches!(descriptor.state.as_str(), "staging" | "ready")
        {
            return Ok(descriptor);
        }
        return Err("耐久资产 assetId 已被其他资源占用".to_string());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建耐久资产暂存目录：{error}"))?;
    }
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("无法创建耐久资产暂存文件：{error}"))?;

    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始耐久资产登记事务：{error}"))?;
    let inserted = transaction.execute(
        "INSERT INTO durable_assets
         (workspace_scope, asset_id, staged_id, owner_type, owner_id, role,
          file_name, mime_type, state, storage_relative_path, byte_length,
          sha256, expected_sha256, metadata_json, last_error, created_at,
          updated_at, finalized_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'staging', ?9, 0,
                 NULL, ?10, ?11, NULL, ?12, ?12, NULL)",
        params![
            workspace_scope,
            asset_id,
            staged_id,
            owner_type,
            owner_id,
            role,
            file_name,
            mime_type,
            relative_path,
            expected_sha256,
            metadata_json,
            created_at,
        ],
    );
    if let Err(error) = inserted {
        let _ = fs::remove_file(&path);
        return Err(format!("无法登记耐久资产：{error}"));
    }
    if let Err(error) = transaction.commit() {
        let _ = fs::remove_file(&path);
        return Err(format!("无法提交耐久资产登记：{error}"));
    }
    let row = load_asset_row(&connection, workspace_scope, &asset_id)?
        .ok_or_else(|| "耐久资产登记后无法读取".to_string())?;
    Ok(descriptor_with_relative_path(row))
}

fn read_existing_range(path: &Path, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    let mut file =
        File::open(path).map_err(|error| format!("无法读取耐久资产暂存文件：{error}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("无法定位耐久资产暂存文件：{error}"))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("无法校验重复耐久资产分块：{error}"))?;
    Ok(bytes)
}

// Keep the asset/staging identity and offset explicit at this integrity boundary.
#[allow(clippy::too_many_arguments)]
fn append_chunk_in(
    root: &Path,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    workspace_scope: &str,
    asset_id: &str,
    staged_id: &str,
    offset: u64,
    chunk_base64: &str,
) -> Result<DurableAssetDescriptor, String> {
    if chunk_base64.is_empty() || chunk_base64.len() > MAX_ENCODED_CHUNK_BYTES {
        return Err("耐久资产分块必须大于 0 且解码后不超过 4 MB".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(chunk_base64.as_bytes())
        .map_err(|_| "耐久资产分块不是有效的 Base64".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_ASSET_CHUNK_BYTES {
        return Err("耐久资产分块必须大于 0 且不超过 4 MB".to_string());
    }
    let lock = state.asset_lock(asset_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "耐久资产写入锁不可用".to_string())?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let row = load_asset_row(&connection, workspace_scope, asset_id)?
        .ok_or_else(|| "耐久资产上传会话不存在".to_string())?;
    if row.descriptor.state != "staging"
        || row.descriptor.staged_id.as_deref() != Some(staged_id.trim())
    {
        return Err("耐久资产上传会话状态或 stagedId 不匹配".to_string());
    }
    let path = checked_store_path(root, &row.storage_relative_path)?;
    let actual_length = match fs::metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.len(),
        Ok(_) => return Err("source_missing: 耐久资产暂存目标不是普通文件".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            connection
                .execute(
                    "UPDATE durable_assets SET state='source_missing',
                     last_error='source_missing: staging file is absent', updated_at=?3
                     WHERE workspace_scope=?1 AND asset_id=?2",
                    params![workspace_scope, asset_id, Utc::now().to_rfc3339()],
                )
                .map_err(|db_error| format!("无法记录 source_missing：{db_error}"))?;
            return Err("source_missing: 耐久资产暂存文件不存在".to_string());
        }
        Err(error) => return Err(format!("无法读取耐久资产暂存进度：{error}")),
    };
    if actual_length != row.descriptor.byte_length {
        return Err(format!(
            "耐久资产暂存进度不一致：文件为 {actual_length} 字节，记录为 {} 字节",
            row.descriptor.byte_length
        ));
    }
    if offset < actual_length {
        let end = offset.saturating_add(bytes.len() as u64);
        if end <= actual_length && read_existing_range(&path, offset, bytes.len())? == bytes {
            return Ok(descriptor_with_relative_path(row));
        }
        return Err(format!(
            "耐久资产分块偏移已被其他内容占用；expectedOffset={actual_length}"
        ));
    }
    if offset != actual_length {
        return Err(format!(
            "耐久资产分块偏移不连续；expectedOffset={actual_length}"
        ));
    }
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|error| format!("无法打开耐久资产暂存文件：{error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("无法写入耐久资产分块：{error}"))?;
    file.sync_data()
        .map_err(|error| format!("无法同步耐久资产分块：{error}"))?;
    let next_length = actual_length.saturating_add(bytes.len() as u64);
    let updated_at = Utc::now().to_rfc3339();
    if let Err(error) = connection.execute(
        "UPDATE durable_assets SET byte_length=?3, updated_at=?4, last_error=NULL
         WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
        params![workspace_scope, asset_id, next_length as i64, updated_at],
    ) {
        let _ = file.set_len(actual_length);
        let _ = file.sync_data();
        return Err(format!("无法保存耐久资产分块进度：{error}"));
    }
    let row = load_asset_row(&connection, workspace_scope, asset_id)?
        .ok_or_else(|| "耐久资产分块写入后无法读取".to_string())?;
    Ok(descriptor_with_relative_path(row))
}

fn stream_sha256(path: &Path) -> Result<(String, u64), String> {
    let file = File::open(path).map_err(|error| format!("无法打开耐久资产：{error}"))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut byte_length = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("无法读取耐久资产：{error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        byte_length = byte_length.saturating_add(count as u64);
    }
    Ok((format!("{:x}", hasher.finalize()), byte_length))
}

fn finish_upload_in(
    root: &Path,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    workspace_scope: &str,
    asset_id: &str,
    staged_id: &str,
) -> Result<DurableAssetDescriptor, String> {
    let lock = state.asset_lock(asset_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "耐久资产写入锁不可用".to_string())?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let row = load_asset_row(&connection, workspace_scope, asset_id)?
        .ok_or_else(|| "耐久资产上传会话不存在".to_string())?;
    if row.descriptor.state == "ready" {
        return Ok(descriptor_with_relative_path(row));
    }
    if row.descriptor.state != "staging"
        || row.descriptor.staged_id.as_deref() != Some(staged_id.trim())
    {
        return Err("耐久资产上传会话状态或 stagedId 不匹配".to_string());
    }
    let staging_path = checked_store_path(root, &row.storage_relative_path)?;
    if !staging_path.is_file() {
        connection
            .execute(
                "UPDATE durable_assets SET state='source_missing',
                 last_error='source_missing: staging file is absent', updated_at=?3
                 WHERE workspace_scope=?1 AND asset_id=?2",
                params![workspace_scope, asset_id, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法记录 source_missing：{error}"))?;
        return Err("source_missing: 耐久资产暂存文件不存在".to_string());
    }
    let (sha256, byte_length) = stream_sha256(&staging_path)?;
    if byte_length != row.descriptor.byte_length {
        return Err("耐久资产最终字节数与分块账本不一致".to_string());
    }
    let expected = normalize_sha256(row.descriptor.expected_sha256.as_deref())?;
    if expected
        .as_deref()
        .is_some_and(|expected| expected != sha256)
    {
        let error = format!(
            "耐久资产 SHA-256 不一致：expected={}, actual={sha256}",
            expected.as_deref().unwrap_or_default()
        );
        connection
            .execute(
                "UPDATE durable_assets SET state='failed', last_error=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND asset_id=?2",
                params![workspace_scope, asset_id, error, Utc::now().to_rfc3339()],
            )
            .map_err(|db_error| format!("无法记录耐久资产哈希失败：{db_error}"))?;
        return Err(error);
    }
    let relative_path = object_relative_path(asset_id);
    let final_path = checked_store_path(root, &relative_path)?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建耐久资产对象目录：{error}"))?;
    }
    if final_path.exists() {
        let (existing_sha256, existing_length) = stream_sha256(&final_path)?;
        if existing_sha256 != sha256 || existing_length != byte_length {
            return Err("耐久资产最终对象路径已被不同内容占用".to_string());
        }
        let _ = fs::remove_file(&staging_path);
    } else {
        fs::rename(&staging_path, &final_path)
            .map_err(|error| format!("无法原子提交耐久资产：{error}"))?;
    }
    File::open(&final_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("无法同步耐久资产最终文件：{error}"))?;
    let finalized_at = Utc::now().to_rfc3339();
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始耐久资产提交事务：{error}"))?;
    let updated = match transaction.execute(
        "UPDATE durable_assets
             SET staged_id=NULL, state='ready', storage_relative_path=?3,
                 byte_length=?4, sha256=?5, last_error=NULL, updated_at=?6,
                 finalized_at=?6
             WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
        params![
            workspace_scope,
            asset_id,
            relative_path,
            byte_length as i64,
            sha256,
            finalized_at,
        ],
    ) {
        Ok(updated) => updated,
        Err(error) => {
            let _ = fs::rename(&final_path, &staging_path);
            return Err(format!("无法写入耐久资产完成状态：{error}"));
        }
    };
    if updated != 1 {
        let _ = fs::rename(&final_path, &staging_path);
        return Err("耐久资产提交状态发生并发变化".to_string());
    }
    if let Err(error) = transaction.commit() {
        let _ = fs::rename(&final_path, &staging_path);
        return Err(format!("无法提交耐久资产状态：{error}"));
    }
    let row = load_asset_row(&connection, workspace_scope, asset_id)?
        .ok_or_else(|| "耐久资产完成后无法读取".to_string())?;
    Ok(descriptor_with_relative_path(row))
}

fn mark_source_missing(
    connection: &Connection,
    workspace_scope: &str,
    asset_id: &str,
    detail: &str,
) -> Result<(), String> {
    connection
        .execute(
            "UPDATE durable_assets SET state='source_missing', last_error=?3,
             updated_at=?4 WHERE workspace_scope=?1 AND asset_id=?2 AND state!='deleted'",
            params![workspace_scope, asset_id, detail, Utc::now().to_rfc3339()],
        )
        .map(|_| ())
        .map_err(|error| format!("无法记录耐久资产 source_missing：{error}"))
}

fn get_asset_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    asset_id: &str,
) -> Result<DurableAssetDescriptor, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let row = load_asset_row(&connection, workspace_scope, asset_id)?
        .ok_or_else(|| "耐久资产不存在".to_string())?;
    if matches!(row.descriptor.state.as_str(), "ready" | "staging") {
        let path = checked_store_path(root, &row.storage_relative_path)?;
        if !path.is_file() {
            mark_source_missing(
                &connection,
                workspace_scope,
                asset_id,
                "source_missing: durable asset file is absent",
            )?;
            let mut descriptor = descriptor_with_relative_path(row);
            descriptor.state = "source_missing".to_string();
            descriptor.last_error =
                Some("source_missing: durable asset file is absent".to_string());
            return Ok(descriptor);
        }
    }
    Ok(descriptor_with_relative_path(row))
}

fn list_assets_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    owner_type: Option<&str>,
    owner_id: Option<&str>,
) -> Result<Vec<DurableAssetDescriptor>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut sql = ASSET_SELECT.to_string();
    match (owner_type, owner_id) {
        (Some(_), Some(_)) => sql.push_str(
            " WHERE workspace_scope=?1 AND owner_type=?2 AND owner_id=?3 AND state!='deleted'
              ORDER BY updated_at DESC",
        ),
        (Some(_), None) => sql.push_str(
            " WHERE workspace_scope=?1 AND owner_type=?2 AND state!='deleted'
              ORDER BY updated_at DESC",
        ),
        (None, None) => {
            sql.push_str(" WHERE workspace_scope=?1 AND state!='deleted' ORDER BY updated_at DESC")
        }
        (None, Some(_)) => return Err("按 ownerId 查询时必须同时提供 ownerType".to_string()),
    }
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| format!("无法准备耐久资产列表：{error}"))?;
    let rows = match (owner_type, owner_id) {
        (Some(owner_type), Some(owner_id)) => {
            statement.query_map(params![workspace_scope, owner_type, owner_id], row_to_asset)
        }
        (Some(owner_type), None) => {
            statement.query_map(params![workspace_scope, owner_type], row_to_asset)
        }
        (None, None) => statement.query_map(params![workspace_scope], row_to_asset),
        (None, Some(_)) => unreachable!(),
    }
    .map_err(|error| format!("无法读取耐久资产列表：{error}"))?;
    let mut assets = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析耐久资产列表：{error}"))?;
    for row in &mut assets {
        if matches!(row.descriptor.state.as_str(), "ready" | "staging") {
            let path = checked_store_path(root, &row.storage_relative_path)?;
            if !path.is_file() {
                mark_source_missing(
                    &connection,
                    workspace_scope,
                    &row.descriptor.asset_id,
                    "source_missing: durable asset file is absent",
                )?;
                row.descriptor.state = "source_missing".to_string();
                row.descriptor.last_error =
                    Some("source_missing: durable asset file is absent".to_string());
            }
        }
    }
    Ok(assets
        .into_iter()
        .map(descriptor_with_relative_path)
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn list_assets_page_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    owner_type: Option<&str>,
    owner_id: Option<&str>,
    state: Option<&str>,
    cursor_updated_at: Option<&str>,
    cursor_id: Option<&str>,
    limit: usize,
) -> Result<DurableAssetPage, String> {
    if owner_id.is_some() && owner_type.is_none() {
        return Err("按 ownerId 查询时必须同时提供 ownerType".to_string());
    }
    if cursor_updated_at.is_some() != cursor_id.is_some() {
        return Err("耐久资产分页游标必须同时包含 cursorUpdatedAt 和 cursorId".to_string());
    }
    if state.is_some_and(|state| {
        !matches!(
            state,
            "staging" | "ready" | "failed" | "source_missing" | "deleted"
        )
    }) {
        return Err("耐久资产 state 无效".to_string());
    }
    let page_size = limit.clamp(1, 512);
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(&format!(
            "{ASSET_SELECT}
             WHERE workspace_scope=?1
               AND (?2 IS NULL OR owner_type=?2)
               AND (?3 IS NULL OR owner_id=?3)
               AND ((?4 IS NULL AND state!='deleted') OR state=?4)
               AND (?5 IS NULL OR updated_at<?5 OR (updated_at=?5 AND asset_id<?6))
             ORDER BY updated_at DESC, asset_id DESC
             LIMIT ?7"
        ))
        .map_err(|error| format!("无法准备耐久资产分页列表：{error}"))?;
    let rows = statement
        .query_map(
            params![
                workspace_scope,
                owner_type,
                owner_id,
                state,
                cursor_updated_at,
                cursor_id,
                (page_size + 1) as i64
            ],
            row_to_asset,
        )
        .map_err(|error| format!("无法读取耐久资产分页列表：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析耐久资产分页列表：{error}"))?;
    let has_more = rows.len() > page_size;
    let mut visible = rows.into_iter().take(page_size).collect::<Vec<_>>();
    for row in &mut visible {
        if matches!(row.descriptor.state.as_str(), "ready" | "staging") {
            let path = checked_store_path(root, &row.storage_relative_path)?;
            if !path.is_file() {
                mark_source_missing(
                    &connection,
                    workspace_scope,
                    &row.descriptor.asset_id,
                    "source_missing: durable asset file is absent",
                )?;
                row.descriptor.state = "source_missing".to_string();
                row.descriptor.last_error =
                    Some("source_missing: durable asset file is absent".to_string());
            }
        }
    }
    let next_cursor = has_more
        .then(|| {
            visible.last().map(|row| {
                (
                    row.descriptor.updated_at.clone(),
                    row.descriptor.asset_id.clone(),
                )
            })
        })
        .flatten();
    Ok(DurableAssetPage {
        items: visible
            .into_iter()
            .map(descriptor_with_relative_path)
            .collect(),
        next_cursor_updated_at: next_cursor.as_ref().map(|cursor| cursor.0.clone()),
        next_cursor_id: next_cursor.map(|cursor| cursor.1),
    })
}

fn read_chunk_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    asset_id: &str,
    offset: u64,
    requested_length: Option<usize>,
) -> Result<DurableAssetChunk, String> {
    let descriptor = get_asset_in(root, database, workspace_scope, asset_id)?;
    if descriptor.state != "ready" {
        if descriptor.state == "source_missing" {
            return Err("source_missing: 耐久资产文件不存在".to_string());
        }
        return Err(format!("耐久资产当前不可读取：{}", descriptor.state));
    }
    if offset > descriptor.byte_length {
        return Err("耐久资产读取偏移超过文件长度".to_string());
    }
    let length = requested_length
        .unwrap_or(DEFAULT_READ_CHUNK_BYTES)
        .clamp(1, MAX_READ_CHUNK_BYTES);
    let relative_path = descriptor
        .relative_path
        .as_deref()
        .ok_or_else(|| "source_missing: 耐久资产没有存储路径".to_string())?;
    let path = checked_store_path(root, relative_path)?;
    let mut file = File::open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "source_missing: 耐久资产文件不存在".to_string()
        } else {
            format!("无法打开耐久资产：{error}")
        }
    })?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("无法定位耐久资产读取位置：{error}"))?;
    let remaining = descriptor.byte_length.saturating_sub(offset);
    let count = remaining.min(length as u64) as usize;
    let mut bytes = vec![0; count];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("无法读取耐久资产分块：{error}"))?;
    let next_offset = offset.saturating_add(bytes.len() as u64);
    Ok(DurableAssetChunk {
        asset_id: asset_id.to_string(),
        offset,
        next_offset,
        byte_length: descriptor.byte_length,
        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        eof: next_offset >= descriptor.byte_length,
        sha256: descriptor.sha256,
    })
}

fn delete_asset_in(
    root: &Path,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    workspace_scope: &str,
    asset_id: &str,
) -> Result<bool, String> {
    let lock = state.asset_lock(asset_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "耐久资产写入锁不可用".to_string())?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let Some(row) = load_asset_row(&connection, workspace_scope, asset_id)? else {
        return Ok(false);
    };
    if row.descriptor.state == "deleted" {
        return Ok(false);
    }
    let path = checked_store_path(root, &row.storage_relative_path)?;
    let trash_relative = format!("trash/{}-{}.deleted", asset_id, Uuid::new_v4());
    let trash_path = checked_store_path(root, &trash_relative)?;
    let moved = if path.exists() {
        fs::rename(&path, &trash_path)
            .map_err(|error| format!("无法隔离待删除耐久资产：{error}"))?;
        true
    } else {
        false
    };
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始耐久资产删除事务：{error}"))?;
    let result = transaction.execute(
        "UPDATE durable_assets SET state='deleted', staged_id=NULL,
         storage_relative_path='', last_error=NULL, updated_at=?3
         WHERE workspace_scope=?1 AND asset_id=?2 AND state!='deleted'",
        params![workspace_scope, asset_id, Utc::now().to_rfc3339()],
    );
    let updated = match result {
        Ok(updated) => updated,
        Err(error) => {
            if moved {
                let _ = fs::rename(&trash_path, &path);
            }
            return Err(format!("无法标记耐久资产已删除：{error}"));
        }
    };
    if let Err(error) = transaction.commit() {
        if moved {
            let _ = fs::rename(&trash_path, &path);
        }
        return Err(format!("无法提交耐久资产删除：{error}"));
    }
    if moved {
        if let Err(error) = fs::remove_file(&trash_path) {
            log::warn!(
                "耐久资产 {} 已从账本删除，但隔离文件 {} 等待后续清理：{}",
                asset_id,
                trash_path.display(),
                error
            );
        }
    }
    Ok(updated == 1)
}

fn reconcile_in(
    root: &Path,
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<DurableAssetReconcileReport, String> {
    ensure_store_directories(root)?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(&format!(
            "{ASSET_SELECT} WHERE workspace_scope=?1 AND state!='deleted' ORDER BY created_at"
        ))
        .map_err(|error| format!("无法准备耐久资产恢复扫描：{error}"))?;
    let rows = statement
        .query_map(params![workspace_scope], row_to_asset)
        .map_err(|error| format!("无法扫描耐久资产恢复状态：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析耐久资产恢复状态：{error}"))?;
    drop(statement);
    let mut report = DurableAssetReconcileReport {
        ready: 0,
        staging: 0,
        recovered_finalizations: 0,
        source_missing: 0,
    };
    for row in rows {
        let descriptor = &row.descriptor;
        match descriptor.state.as_str() {
            "ready" => {
                let path = checked_store_path(root, &row.storage_relative_path)?;
                if path.is_file() {
                    report.ready += 1;
                } else {
                    mark_source_missing(
                        &connection,
                        workspace_scope,
                        &descriptor.asset_id,
                        "source_missing: durable asset file is absent during startup reconciliation",
                    )?;
                    report.source_missing += 1;
                }
            }
            "staging" => {
                let staging_path = checked_store_path(root, &row.storage_relative_path)?;
                let final_relative = object_relative_path(&descriptor.asset_id);
                let final_path = checked_store_path(root, &final_relative)?;
                if staging_path.is_file() {
                    let actual = fs::metadata(&staging_path)
                        .map_err(|error| format!("无法读取耐久资产恢复进度：{error}"))?
                        .len();
                    connection
                        .execute(
                            "UPDATE durable_assets SET byte_length=?3, updated_at=?4
                             WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
                            params![
                                workspace_scope,
                                descriptor.asset_id,
                                actual as i64,
                                Utc::now().to_rfc3339(),
                            ],
                        )
                        .map_err(|error| format!("无法修复耐久资产上传进度：{error}"))?;
                    report.staging += 1;
                } else if final_path.is_file() {
                    let (sha256, byte_length) = stream_sha256(&final_path)?;
                    let expected = normalize_sha256(descriptor.expected_sha256.as_deref())?;
                    if expected
                        .as_deref()
                        .is_some_and(|expected| expected != sha256)
                    {
                        connection
                            .execute(
                                "UPDATE durable_assets SET state='failed', last_error=?3,
                                 updated_at=?4 WHERE workspace_scope=?1 AND asset_id=?2",
                                params![
                                    workspace_scope,
                                    descriptor.asset_id,
                                    "recovered final asset failed expected SHA-256 verification",
                                    Utc::now().to_rfc3339(),
                                ],
                            )
                            .map_err(|error| format!("无法记录恢复哈希失败：{error}"))?;
                    } else {
                        let now = Utc::now().to_rfc3339();
                        connection
                            .execute(
                                "UPDATE durable_assets SET staged_id=NULL, state='ready',
                                 storage_relative_path=?3, byte_length=?4, sha256=?5,
                                 last_error=NULL, updated_at=?6, finalized_at=?6
                                 WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
                                params![
                                    workspace_scope,
                                    descriptor.asset_id,
                                    final_relative,
                                    byte_length as i64,
                                    sha256,
                                    now,
                                ],
                            )
                            .map_err(|error| format!("无法恢复耐久资产最终状态：{error}"))?;
                        report.recovered_finalizations += 1;
                        report.ready += 1;
                    }
                } else {
                    mark_source_missing(
                        &connection,
                        workspace_scope,
                        &descriptor.asset_id,
                        "source_missing: staging and final durable asset files are absent",
                    )?;
                    report.source_missing += 1;
                }
            }
            "source_missing" => {
                report.source_missing += 1;
            }
            _ => {}
        }
    }
    Ok(report)
}

pub(crate) fn reconcile_for_startup(
    app: &AppHandle,
    database: &RuntimeDatabase,
) -> Result<DurableAssetReconcileReport, String> {
    let workspace_scope = database.local_workspace_scope()?;
    reconcile_in(&asset_store_root(app)?, database, &workspace_scope)
}

pub(crate) fn resolve_ready_asset_path(
    app: &AppHandle,
    database: &RuntimeDatabase,
    asset_id: &str,
) -> Result<(DurableAssetDescriptor, PathBuf), String> {
    let workspace_scope = database.local_workspace_scope()?;
    let root = asset_store_root(app)?;
    let descriptor = get_asset_in(&root, database, &workspace_scope, asset_id)?;
    if descriptor.state != "ready" {
        if descriptor.state == "source_missing" {
            return Err("source_missing: 耐久资产文件不存在".to_string());
        }
        return Err(format!("耐久资产尚未就绪：{}", descriptor.state));
    }
    let path = checked_store_path(
        &root,
        descriptor
            .relative_path
            .as_deref()
            .ok_or_else(|| "source_missing: 耐久资产没有存储路径".to_string())?,
    )?;
    Ok((descriptor, path))
}

pub(crate) fn delete_for_runtime(
    app: &AppHandle,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    asset_id: &str,
) -> Result<bool, String> {
    let workspace_scope = database.local_workspace_scope()?;
    delete_asset_in(
        &asset_store_root(app)?,
        database,
        state,
        &workspace_scope,
        asset_id,
    )
}

fn legacy_creation_draft_path(legacy_root: &Path, attachment_id: &str) -> Result<PathBuf, String> {
    let attachment_id = attachment_id.trim();
    if !valid_identifier(attachment_id, 100) {
        return Err("旧版创作附件 ID 无效".to_string());
    }
    let root = legacy_root.join("draft-assets");
    let path = root.join(format!("{attachment_id}.asset"));
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "source_missing: 旧版创作草稿附件不存在".to_string()
        } else {
            format!("无法读取旧版创作草稿附件：{error}")
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err("旧版创作草稿附件必须是普通文件".to_string());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("无法校验旧版创作草稿目录：{error}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("无法校验旧版创作草稿附件：{error}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("旧版创作草稿附件越过应用数据边界".to_string());
    }
    Ok(canonical_path)
}

fn import_legacy_creation_draft_asset_in(
    store_root: &Path,
    legacy_root: &Path,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    workspace_scope: &str,
    input: ImportLegacyCreationDraftAssetInput,
) -> Result<DurableAssetDescriptor, String> {
    if !input
        .mime_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/")
    {
        return Err("旧版创作草稿迁移只接受图片".to_string());
    }
    let attachment_id = input.attachment_id.trim().to_string();
    let requested_owner_id = normalize_owner_id(&input.owner_id)?;
    let requested_role = input.role.trim().to_string();
    match get_asset_in(store_root, database, workspace_scope, &attachment_id) {
        Ok(descriptor) if descriptor.state == "ready" => {
            if descriptor.owner_type == "creation_asset"
                && descriptor.owner_id == requested_owner_id
                && descriptor.role == requested_role
            {
                return Ok(descriptor);
            }
            return Err("旧版创作附件 ID 已绑定到其他耐久资产所有者".to_string());
        }
        Ok(descriptor) if descriptor.state != "staging" => {
            return Err(format!(
                "旧版创作草稿迁移目标当前不可写：{}",
                descriptor.state
            ));
        }
        Ok(_) => {}
        Err(error) if error == "耐久资产不存在" => {}
        Err(error) => return Err(error),
    }
    let source_path = legacy_creation_draft_path(legacy_root, &attachment_id)?;
    let descriptor = begin_upload_in(
        store_root,
        database,
        workspace_scope,
        BeginDurableAssetInput {
            asset_id: Some(attachment_id),
            owner_type: "creation_asset".to_string(),
            owner_id: requested_owner_id,
            role: requested_role,
            file_name: input.file_name,
            mime_type: input.mime_type,
            expected_sha256: None,
            metadata: input.metadata,
        },
    )?;
    if descriptor.state == "ready" {
        return Ok(descriptor);
    }
    let staged_id = descriptor
        .staged_id
        .clone()
        .ok_or_else(|| "旧版创作草稿迁移缺少 stagedId".to_string())?;
    let staging_path = checked_store_path(
        store_root,
        descriptor
            .relative_path
            .as_deref()
            .ok_or_else(|| "旧版创作草稿迁移缺少暂存路径".to_string())?,
    )?;
    let lock = state.asset_lock(&descriptor.asset_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "耐久资产写入锁不可用".to_string())?;
    let copy_result = (|| -> Result<(u64, String), String> {
        let source = File::open(&source_path)
            .map_err(|error| format!("无法打开旧版创作草稿附件：{error}"))?;
        let mut reader = BufReader::new(source);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&staging_path)
            .map_err(|error| format!("无法打开耐久资产迁移暂存文件：{error}"))?;
        let mut hasher = Sha256::new();
        let mut byte_length = 0u64;
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| format!("无法读取旧版创作草稿附件：{error}"))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|error| format!("无法写入耐久资产迁移暂存文件：{error}"))?;
            hasher.update(&buffer[..count]);
            byte_length = byte_length.saturating_add(count as u64);
        }
        if byte_length == 0 {
            return Err("旧版创作草稿附件为空".to_string());
        }
        output
            .sync_all()
            .map_err(|error| format!("无法同步耐久资产迁移暂存文件：{error}"))?;
        Ok((byte_length, format!("{:x}", hasher.finalize())))
    })();
    let (byte_length, sha256) = match copy_result {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&staging_path);
            if let Ok(connection) = database.connection.lock() {
                let _ = connection.execute(
                    "UPDATE durable_assets SET state='failed', last_error=?3, updated_at=?4
                     WHERE workspace_scope=?1 AND asset_id=?2",
                    params![
                        workspace_scope,
                        descriptor.asset_id,
                        error,
                        Utc::now().to_rfc3339()
                    ],
                );
            }
            return Err(error);
        }
    };
    {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE durable_assets SET byte_length=?3, expected_sha256=?4,
                 updated_at=?5, last_error=NULL
                 WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
                params![
                    workspace_scope,
                    descriptor.asset_id,
                    byte_length as i64,
                    sha256,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|error| format!("无法保存旧版创作草稿迁移进度：{error}"))?;
    }
    drop(_guard);
    finish_upload_in(
        store_root,
        database,
        state,
        workspace_scope,
        &descriptor.asset_id,
        &staged_id,
    )
}

// Provider identity, durable ownership, MIME and metadata are independently audited.
#[allow(clippy::too_many_arguments)]
pub(crate) fn store_generated_image_base64(
    app: &AppHandle,
    database: &RuntimeDatabase,
    state: &DurableAssetState,
    owner_id: &str,
    file_name: &str,
    mime_type: &str,
    encoded: &str,
    metadata: Value,
) -> Result<DurableAssetDescriptor, String> {
    use base64::read::DecoderReader;
    use std::io::Cursor;

    let workspace_scope = database.local_workspace_scope()?;
    let root = asset_store_root(app)?;
    let descriptor = begin_upload_in(
        &root,
        database,
        &workspace_scope,
        BeginDurableAssetInput {
            asset_id: None,
            owner_type: "assistant_image".to_string(),
            owner_id: owner_id.to_string(),
            role: "generated".to_string(),
            file_name: file_name.to_string(),
            mime_type: mime_type.to_string(),
            expected_sha256: None,
            metadata,
        },
    )?;
    let staged_id = descriptor
        .staged_id
        .clone()
        .ok_or_else(|| "生成图片耐久资产缺少 stagedId".to_string())?;
    let path = checked_store_path(
        &root,
        descriptor
            .relative_path
            .as_deref()
            .ok_or_else(|| "生成图片耐久资产缺少暂存路径".to_string())?,
    )?;
    let write_result = (|| -> Result<u64, String> {
        let mut decoder = DecoderReader::new(
            Cursor::new(encoded.as_bytes()),
            &base64::engine::general_purpose::STANDARD,
        );
        let mut output = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|error| format!("无法打开生成图片耐久暂存文件：{error}"))?;
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut byte_length = 0u64;
        loop {
            let count = decoder
                .read(&mut buffer)
                .map_err(|_| "图像模型返回的 Base64 无效".to_string())?;
            if count == 0 {
                break;
            }
            byte_length = byte_length.saturating_add(count as u64);
            output
                .write_all(&buffer[..count])
                .map_err(|error| format!("无法写入生成图片耐久资产：{error}"))?;
        }
        if byte_length == 0 {
            return Err("图像模型返回了空图片".to_string());
        }
        output
            .sync_all()
            .map_err(|error| format!("无法同步生成图片耐久资产：{error}"))?;
        Ok(byte_length)
    })();
    let byte_length = match write_result {
        Ok(byte_length) => byte_length,
        Err(error) => {
            let _ = fs::remove_file(&path);
            if let Ok(connection) = database.connection.lock() {
                let _ = connection.execute(
                    "UPDATE durable_assets SET state='failed', last_error=?3, updated_at=?4
                     WHERE workspace_scope=?1 AND asset_id=?2",
                    params![
                        workspace_scope,
                        descriptor.asset_id,
                        error,
                        Utc::now().to_rfc3339()
                    ],
                );
            }
            return Err(error);
        }
    };
    {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE durable_assets SET byte_length=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND asset_id=?2 AND state='staging'",
                params![
                    workspace_scope,
                    descriptor.asset_id,
                    byte_length as i64,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|error| format!("无法保存生成图片耐久进度：{error}"))?;
    }
    finish_upload_in(
        &root,
        database,
        state,
        &workspace_scope,
        &descriptor.asset_id,
        &staged_id,
    )
}

#[tauri::command]
pub fn import_legacy_creation_draft_asset(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    state: State<'_, DurableAssetState>,
    input: ImportLegacyCreationDraftAssetInput,
) -> Result<DurableAssetDescriptor, String> {
    let workspace_scope = database.local_workspace_scope()?;
    import_legacy_creation_draft_asset_in(
        &asset_store_root(&app)?,
        &app_data_root(&app)?,
        database.inner(),
        state.inner(),
        &workspace_scope,
        input,
    )
}

#[tauri::command]
pub fn begin_durable_asset_upload(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    input: BeginDurableAssetInput,
) -> Result<DurableAssetDescriptor, String> {
    let workspace_scope = database.local_workspace_scope()?;
    begin_upload_in(
        &asset_store_root(&app)?,
        database.inner(),
        &workspace_scope,
        input,
    )
}

#[tauri::command]
pub fn append_durable_asset_chunk(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    state: State<'_, DurableAssetState>,
    asset_id: String,
    staged_id: String,
    offset: u64,
    chunk_base64: String,
) -> Result<DurableAssetDescriptor, String> {
    let workspace_scope = database.local_workspace_scope()?;
    append_chunk_in(
        &asset_store_root(&app)?,
        database.inner(),
        state.inner(),
        &workspace_scope,
        asset_id.trim(),
        staged_id.trim(),
        offset,
        &chunk_base64,
    )
}

#[tauri::command]
pub fn finish_durable_asset_upload(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    state: State<'_, DurableAssetState>,
    asset_id: String,
    staged_id: String,
) -> Result<DurableAssetDescriptor, String> {
    let workspace_scope = database.local_workspace_scope()?;
    finish_upload_in(
        &asset_store_root(&app)?,
        database.inner(),
        state.inner(),
        &workspace_scope,
        asset_id.trim(),
        staged_id.trim(),
    )
}

#[tauri::command]
pub fn get_durable_asset(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    asset_id: String,
) -> Result<DurableAssetDescriptor, String> {
    let workspace_scope = database.local_workspace_scope()?;
    get_asset_in(
        &asset_store_root(&app)?,
        database.inner(),
        &workspace_scope,
        asset_id.trim(),
    )
}

#[tauri::command]
pub fn list_durable_assets(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    owner_type: Option<String>,
    owner_id: Option<String>,
) -> Result<Vec<DurableAssetDescriptor>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let owner_type = owner_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let owner_id = owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if owner_type.is_some_and(|value| !valid_identifier(value, 64)) {
        return Err("耐久资产 ownerType 无效".to_string());
    }
    list_assets_in(
        &asset_store_root(&app)?,
        database.inner(),
        &workspace_scope,
        owner_type,
        owner_id,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn list_durable_assets_page(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    owner_type: Option<String>,
    owner_id: Option<String>,
    cursor_updated_at: Option<String>,
    cursor_id: Option<String>,
    limit: Option<usize>,
) -> Result<DurableAssetPage, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let owner_type = owner_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let owner_id = owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if owner_type.is_some_and(|value| !valid_identifier(value, 64)) {
        return Err("耐久资产 ownerType 无效".to_string());
    }
    list_assets_page_in(
        &asset_store_root(&app)?,
        database.inner(),
        &workspace_scope,
        owner_type,
        owner_id,
        None,
        cursor_updated_at.as_deref(),
        cursor_id.as_deref(),
        limit.unwrap_or(128),
    )
}

#[tauri::command]
pub fn read_durable_asset_chunk(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    asset_id: String,
    offset: u64,
    length: Option<usize>,
) -> Result<DurableAssetChunk, String> {
    let workspace_scope = database.local_workspace_scope()?;
    read_chunk_in(
        &asset_store_root(&app)?,
        database.inner(),
        &workspace_scope,
        asset_id.trim(),
        offset,
        length,
    )
}

#[tauri::command]
pub fn delete_durable_asset(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    state: State<'_, DurableAssetState>,
    asset_id: String,
) -> Result<bool, String> {
    let workspace_scope = database.local_workspace_scope()?;
    delete_asset_in(
        &asset_store_root(&app)?,
        database.inner(),
        state.inner(),
        &workspace_scope,
        asset_id.trim(),
    )
}
