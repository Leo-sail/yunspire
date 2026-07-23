use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    net::IpAddr,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tauri::{path::BaseDirectory, Manager};
use tempfile::{tempdir, NamedTempFile};
use uuid::Uuid;

const MAX_AUTH_SECRET_BYTES: usize = 16 * 1024;
const MAX_UPLOAD_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MODEL_ANALYSIS_IMAGE_TARGET_BYTES: u64 = 3 * 1024 * 1024;
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MODEL_ANALYSIS_IMAGE_DERIVATIVE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const MODEL_ANALYSIS_IMAGE_RESOURCE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTURE_UPLOAD_STAGING_RETENTION: Duration = Duration::ZERO;
const CAPTURE_ATTACHMENT_STAGING_RETENTION: Duration = Duration::ZERO;
const CAPTURE_CLAIM_STAGING_RETENTION: Duration = Duration::ZERO;
const WEB_HELPER_TIMEOUT: Duration = Duration::from_secs(90);
const VIDEO_HELPER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const DOCUMENT_HELPER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const DEFAULT_SPEECH_LOCALE: &str = "zh-CN";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
pub struct CaptureAuthorizationState {
    sessions: Mutex<HashMap<String, CaptureAuthorization>>,
}

#[derive(Default)]
pub struct CaptureTaskState {
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

#[derive(Default)]
pub struct CaptureUploadState {
    uploads: Mutex<HashMap<String, CaptureUpload>>,
}

struct CaptureUpload {
    path: PathBuf,
    name: String,
    relative_path: String,
    finalized: bool,
}

impl CaptureTaskState {
    fn register(&self, task_id: &str) -> Result<Arc<AtomicBool>, String> {
        let mut tasks = self
            .cancellations
            .lock()
            .map_err(|_| "采集任务状态不可用".to_string())?;
        if tasks.contains_key(task_id) {
            return Err("采集任务 ID 已存在".to_string());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        tasks.insert(task_id.to_string(), Arc::clone(&cancellation));
        Ok(cancellation)
    }

    fn finish(&self, task_id: &str) {
        if let Ok(mut tasks) = self.cancellations.lock() {
            tasks.remove(task_id);
        }
    }

    fn cancel_all(&self) -> Result<usize, String> {
        let mut tasks = self
            .cancellations
            .lock()
            .map_err(|_| "采集任务状态不可用".to_string())?;
        let active = std::mem::take(&mut *tasks);
        let count = active.len();
        for cancellation in active.values() {
            cancellation.store(true, Ordering::Release);
        }
        Ok(count)
    }
}

impl CaptureAuthorizationState {
    fn clear(&self) -> Result<usize, String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "授权会话状态不可用".to_string())?;
        let count = sessions.len();
        sessions.clear();
        Ok(count)
    }
}

impl CaptureUploadState {
    fn clear(&self) -> Result<usize, String> {
        let mut uploads = self
            .uploads
            .lock()
            .map_err(|_| "采集分块暂存状态不可用".to_string())?;
        let staged = std::mem::take(&mut *uploads);
        drop(uploads);
        let count = staged.len();
        let mut failures = Vec::new();
        for upload in staged.into_values() {
            if let Err(error) = fs::remove_file(&upload.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    failures.push(format!("{}：{error}", upload.path.display()));
                }
            }
        }
        if failures.is_empty() {
            Ok(count)
        } else {
            Err(format!("无法清理采集分块暂存文件：{}", failures.join("；")))
        }
    }
}

pub(crate) fn suspend_capture_runtime(app: &tauri::AppHandle) -> Result<usize, String> {
    let cancelled = app.state::<CaptureTaskState>().cancel_all()?;
    let authorizations = app.state::<CaptureAuthorizationState>().clear()?;
    let uploads = app.state::<CaptureUploadState>().clear()?;
    Ok(cancelled + authorizations + uploads)
}

struct CaptureAuthorization {
    source_url: String,
    host: String,
    headers: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureAuthorizationReceipt {
    authorization_id: String,
    host: String,
    single_use: bool,
}

#[derive(Serialize)]
struct HelperAuthorization {
    allowed_hosts: Vec<String>,
    headers: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInputFile {
    pub name: String,
    pub relative_path: Option<String>,
    #[serde(default)]
    pub content_base64: Option<String>,
    #[serde(default)]
    pub upload_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureUploadReceipt {
    upload_id: String,
    byte_length: u64,
}

struct PreparedCaptureInputFile {
    name: String,
    relative_path: Option<String>,
    content_base64: Option<String>,
    staged_path: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureExtraction {
    pub source_type: String,
    pub content_hash: String,
    pub result: Value,
}

fn update_capture_hash(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(value) => hasher.update(if *value { b"t" } else { b"f" }),
        Value::Number(value) => {
            hasher.update(b"#");
            hasher.update(value.to_string().as_bytes());
        }
        Value::String(value) => {
            hasher.update(b"s");
            hasher.update(value.len().to_le_bytes());
            hasher.update(value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            for value in values {
                update_capture_hash(hasher, value);
            }
            hasher.update(b"]");
        }
        Value::Object(values) => {
            hasher.update(b"{");
            let mut keys = values
                .keys()
                .filter(|key| !capture_provenance_key(key))
                .collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                hasher.update(key.len().to_le_bytes());
                hasher.update(key.as_bytes());
                if let Some(value) = values.get(key) {
                    update_capture_hash(hasher, value);
                }
            }
            hasher.update(b"}");
        }
    }
}

fn capture_provenance_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "source_url"
            | "sourceurl"
            | "requested_url"
            | "final_url"
            | "source_path"
            | "sourcepath"
            | "relative_path"
            | "relativepath"
            | "file_path"
            | "filepath"
            | "media_path"
            | "mediapath"
            | "staged_attachment_id"
            | "stagedattachmentid"
            | "host"
            | "images"
            | "warnings"
            | "errors"
            | "auth_required"
            | "captured_at"
            | "retrieved_at"
            | "fetched_at"
            | "downloaded_at"
    )
}

fn capture_content_hash(result: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"yunspire.capture.content.v2\0");
    update_capture_hash(&mut hasher, result);
    format!("sha256:{:x}", hasher.finalize())
}

fn capture_extraction(source_type: String, result: Value) -> CaptureExtraction {
    let content_hash = capture_content_hash(&result);
    CaptureExtraction {
        source_type,
        content_hash,
        result,
    }
}

fn normalized_capture_url(value: &str) -> Result<(String, String), String> {
    let mut url =
        reqwest::Url::parse(value.trim()).map_err(|_| "请输入有效的 http 或 https 来源网址")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("授权来源只允许 http 或 https 网址".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("授权来源网址不能包含用户名或密码".to_string());
    }
    let host = url
        .host_str()
        .ok_or("授权来源缺少域名")?
        .trim_end_matches('.')
        .to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Err("禁止为本机或局域网地址创建网络授权".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let blocked = match ip {
            IpAddr::V4(value) => {
                value.is_private()
                    || value.is_loopback()
                    || value.is_link_local()
                    || value.is_broadcast()
                    || value.is_documentation()
                    || value.is_unspecified()
            }
            IpAddr::V6(value) => {
                let first = value.segments()[0];
                value.is_loopback() || value.is_unspecified() || first & 0xfe00 == 0xfc00
            }
        };
        if blocked {
            return Err("禁止为私网、回环、链路本地或保留地址创建网络授权".to_string());
        }
    }
    url.set_fragment(None);
    Ok((url.to_string(), host))
}

#[tauri::command]
pub fn create_capture_authorization(
    source_url: String,
    auth_kind: String,
    secret: String,
    compliance_acknowledged: bool,
    content_rights_confirmed: bool,
    state: tauri::State<'_, CaptureAuthorizationState>,
) -> Result<CaptureAuthorizationReceipt, String> {
    if !compliance_acknowledged || !content_rights_confirmed {
        return Err("必须先确认平台规则与内容访问权限".to_string());
    }
    let (source_url, host) = normalized_capture_url(&source_url)?;
    let secret = secret.trim();
    if secret.is_empty() || secret.len() > MAX_AUTH_SECRET_BYTES {
        return Err("授权凭据为空或超过 16 KB 安全上限".to_string());
    }
    if secret.contains(['\r', '\n']) {
        return Err("授权凭据不能包含换行符".to_string());
    }
    let mut headers = HashMap::new();
    match auth_kind.trim().to_lowercase().as_str() {
        "cookie" => {
            if !secret.contains('=') {
                return Err("Cookie 格式无效，应为名称=值".to_string());
            }
            headers.insert("Cookie".to_string(), secret.to_string());
        }
        "bearer" => {
            let token = secret.strip_prefix("Bearer ").unwrap_or(secret).trim();
            if token.is_empty() {
                return Err("Bearer 令牌不能为空".to_string());
            }
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        _ => return Err("仅支持临时 Cookie 或官方 Bearer 令牌".to_string()),
    }
    let authorization_id = Uuid::new_v4().to_string();
    state
        .sessions
        .lock()
        .map_err(|_| "授权会话状态不可用".to_string())?
        .insert(
            authorization_id.clone(),
            CaptureAuthorization {
                source_url,
                host: host.clone(),
                headers,
            },
        );
    Ok(CaptureAuthorizationReceipt {
        authorization_id,
        host,
        single_use: true,
    })
}

#[tauri::command]
pub fn cancel_capture_task(
    task_id: String,
    state: tauri::State<'_, CaptureTaskState>,
) -> Result<bool, String> {
    let tasks = state
        .cancellations
        .lock()
        .map_err(|_| "采集任务状态不可用".to_string())?;
    let Some(cancellation) = tasks.get(task_id.trim()) else {
        return Ok(false);
    };
    cancellation.store(true, Ordering::Release);
    Ok(true)
}

#[tauri::command]
pub fn open_capture_authorization_page(source_url: String) -> Result<(), String> {
    let (source_url, _) = normalized_capture_url(&source_url)?;
    open_url_in_default_browser(&source_url)
}

fn open_url_in_default_browser(source_url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("rundll32.exe");
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = Command::new("xdg-open");

    #[cfg(target_os = "macos")]
    command.arg("--").arg(source_url);
    #[cfg(target_os = "windows")]
    command.arg("url.dll,FileProtocolHandler").arg(source_url);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    command.arg(source_url);

    let status = command
        .status()
        .map_err(|error| format!("无法打开平台官方页面：{error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("平台官方页面打开失败".to_string())
    }
}

fn take_capture_authorization(
    authorization_id: Option<&str>,
    source_url: &str,
    state: &CaptureAuthorizationState,
) -> Result<Option<HelperAuthorization>, String> {
    let Some(authorization_id) = authorization_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let authorization = state
        .sessions
        .lock()
        .map_err(|_| "授权会话状态不可用".to_string())?
        .remove(authorization_id.trim())
        .ok_or("本次授权不存在、已使用或已撤销")?;
    let (normalized_source, _) = normalized_capture_url(source_url)?;
    if normalized_source != authorization.source_url {
        return Err("本次授权只允许访问创建授权时的完整来源网址".to_string());
    }
    Ok(Some(HelperAuthorization {
        allowed_hosts: vec![authorization.host],
        headers: authorization.headers,
    }))
}

fn safe_relative_path(value: Option<&str>, fallback: &str) -> Result<PathBuf, String> {
    let raw = value
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(fallback);
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("采集文件路径包含不允许的目录跳转".to_string());
    }
    Ok(path.to_path_buf())
}

fn capture_upload_directory() -> Result<PathBuf, String> {
    let directory = env::temp_dir().join("yunspire-capture-uploads");
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建采集分块暂存目录：{error}"))?;
    Ok(directory)
}

fn capture_attachment_directory() -> Result<PathBuf, String> {
    let directory = env::temp_dir().join("yunspire-capture-attachments");
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建采集附件暂存目录：{error}"))?;
    Ok(directory)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureStagingFileKind {
    UploadPart,
    Attachment,
    ClaimedAttachment,
}

impl CaptureStagingFileKind {
    fn retention(self) -> Duration {
        match self {
            Self::UploadPart => CAPTURE_UPLOAD_STAGING_RETENTION,
            Self::Attachment => CAPTURE_ATTACHMENT_STAGING_RETENTION,
            Self::ClaimedAttachment => CAPTURE_CLAIM_STAGING_RETENTION,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureStagingCleanupReport {
    pub(crate) removed_upload_parts: usize,
    pub(crate) removed_attachments: usize,
    pub(crate) removed_claimed_attachments: usize,
    pub(crate) failed_removals: usize,
}

impl CaptureStagingCleanupReport {
    fn record_removal(&mut self, kind: CaptureStagingFileKind) {
        match kind {
            CaptureStagingFileKind::UploadPart => self.removed_upload_parts += 1,
            CaptureStagingFileKind::Attachment => self.removed_attachments += 1,
            CaptureStagingFileKind::ClaimedAttachment => {
                self.removed_claimed_attachments += 1;
            }
        }
    }

    fn merge(&mut self, other: Self) {
        self.removed_upload_parts += other.removed_upload_parts;
        self.removed_attachments += other.removed_attachments;
        self.removed_claimed_attachments += other.removed_claimed_attachments;
        self.failed_removals += other.failed_removals;
    }
}

fn is_canonical_capture_id(value: &str) -> bool {
    Uuid::parse_str(value)
        .map(|parsed| parsed.hyphenated().to_string() == value)
        .unwrap_or(false)
}

fn capture_staging_file_kind(
    file_name: &str,
    upload_directory: bool,
) -> Option<CaptureStagingFileKind> {
    if upload_directory {
        let token = file_name.strip_suffix(".part")?;
        return is_canonical_capture_id(token).then_some(CaptureStagingFileKind::UploadPart);
    }
    if let Some(token) = file_name.strip_suffix(".asset") {
        return is_canonical_capture_id(token).then_some(CaptureStagingFileKind::Attachment);
    }
    let stem = file_name.strip_suffix(".claimed")?;
    let (attachment_id, claim_id) = stem.split_once('.')?;
    (is_canonical_capture_id(attachment_id) && is_canonical_capture_id(claim_id))
        .then_some(CaptureStagingFileKind::ClaimedAttachment)
}

fn cleanup_expired_capture_staging_directory(
    directory: &Path,
    upload_directory: bool,
    now: SystemTime,
) -> Result<CaptureStagingCleanupReport, String> {
    if !directory.exists() {
        return Ok(CaptureStagingCleanupReport::default());
    }
    let directory_entries = fs::read_dir(directory)
        .map_err(|error| format!("无法读取采集暂存目录 {}：{error}", directory.display()))?;
    let mut report = CaptureStagingCleanupReport::default();
    let mut entries = Vec::new();
    for entry in directory_entries {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => report.failed_removals += 1,
        }
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            report.failed_removals += 1;
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(kind) = capture_staging_file_kind(&file_name, upload_directory) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            report.failed_removals += 1;
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            report.failed_removals += 1;
            continue;
        };
        let expired = now
            .duration_since(modified)
            .is_ok_and(|age| age >= kind.retention());
        if !expired {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => report.record_removal(kind),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => report.failed_removals += 1,
        }
    }
    Ok(report)
}

fn cleanup_expired_capture_staging_in(
    upload_directory: &Path,
    attachment_directory: &Path,
    now: SystemTime,
) -> Result<CaptureStagingCleanupReport, String> {
    let mut report = cleanup_expired_capture_staging_directory(upload_directory, true, now)?;
    report.merge(cleanup_expired_capture_staging_directory(
        attachment_directory,
        false,
        now,
    )?);
    Ok(report)
}

pub(crate) fn cleanup_expired_capture_staging() -> Result<CaptureStagingCleanupReport, String> {
    let uploads = capture_upload_directory()?;
    let attachments = capture_attachment_directory()?;
    cleanup_expired_capture_staging_in(&uploads, &attachments, SystemTime::now())
}

fn valid_upload_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn valid_staged_attachment_id(value: &str) -> bool {
    valid_upload_id(value)
}

fn attachment_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "ts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "aif" | "aiff" => "audio/aiff",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn stage_capture_attachment(path: &Path, root: &Path) -> Result<Option<Value>, String> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法校验附件隔离目录：{error}"))?;
    let canonical_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !canonical_path.starts_with(&canonical_root) || !canonical_path.is_file() {
        return Ok(None);
    }
    let size = fs::metadata(&canonical_path)
        .map_err(|error| format!("无法读取附件元数据：{error}"))?
        .len();
    if size == 0 {
        return Ok(None);
    }

    let directory = capture_attachment_directory()?;
    let staged_attachment_id = Uuid::new_v4().to_string();
    let target = directory.join(format!("{staged_attachment_id}.asset"));
    let mut temporary = NamedTempFile::new_in(&directory)
        .map_err(|error| format!("无法创建附件暂存文件：{error}"))?;
    let source =
        fs::File::open(&canonical_path).map_err(|error| format!("无法打开附件：{error}"))?;
    let mut reader = BufReader::new(source);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut copied = 0u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("无法读取附件：{error}"))?;
        if count == 0 {
            break;
        }
        temporary
            .write_all(&buffer[..count])
            .map_err(|error| format!("无法暂存附件：{error}"))?;
        hasher.update(&buffer[..count]);
        copied = copied.saturating_add(count as u64);
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步附件暂存文件：{error}"))?;
    temporary
        .persist_noclobber(&target)
        .map_err(|error| format!("无法提交附件暂存文件：{}", error.error))?;

    let name = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment.bin")
        .to_string();
    Ok(Some(serde_json::json!({
        "staged_attachment_id": staged_attachment_id,
        "name": name,
        "mime_type": attachment_mime_type(&canonical_path),
        "size": copied,
        "sha256": format!("{:x}", hasher.finalize()),
        "content_role": "untrusted_data",
    })))
}

fn staged_attachment_path(staged_attachment_id: &str) -> Result<PathBuf, String> {
    let token = staged_attachment_id.trim();
    if !valid_staged_attachment_id(token) {
        return Err("采集附件暂存 ID 无效".to_string());
    }
    let directory = capture_attachment_directory()?;
    let path = directory.join(format!("{token}.asset"));
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| format!("无法校验附件暂存目录：{error}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|_| "采集附件暂存文件不存在或已经被使用".to_string())?;
    if !canonical_path.starts_with(canonical_directory) || !canonical_path.is_file() {
        return Err("采集附件暂存路径越过安全边界".to_string());
    }
    Ok(canonical_path)
}

pub(crate) fn claim_staged_capture_attachment(
    staged_attachment_id: &str,
    claim_id: &str,
) -> Result<PathBuf, String> {
    if !valid_staged_attachment_id(claim_id.trim()) {
        return Err("附件写入审批 ID 无效".to_string());
    }
    let source = staged_attachment_path(staged_attachment_id)?;
    let target = capture_attachment_directory()?.join(format!(
        "{}.{}.claimed",
        staged_attachment_id.trim(),
        claim_id.trim()
    ));
    if target.exists() {
        return Err("附件写入审批暂存目标已经存在".to_string());
    }
    fs::rename(&source, &target).map_err(|error| format!("无法认领采集附件：{error}"))?;
    Ok(target)
}

pub(crate) fn remove_claimed_capture_attachment(path: &Path) {
    if let (Ok(directory), Ok(canonical)) = (
        capture_attachment_directory().and_then(|value| {
            value
                .canonicalize()
                .map_err(|error| format!("无法校验附件暂存目录：{error}"))
        }),
        path.canonicalize(),
    ) {
        if canonical.starts_with(directory) && canonical.is_file() {
            let _ = fs::remove_file(canonical);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureImageAnalysisInput {
    data_url: String,
    original_sha256: String,
    analysis_sha256: String,
    original_byte_length: u64,
    analysis_byte_length: u64,
    derived: bool,
    analysis_mime_type: String,
    max_dimension: Option<u32>,
}

fn model_supported_image_signature(mime_type: &str, header: &[u8]) -> bool {
    match mime_type {
        "image/jpeg" | "image/jpg" => header.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => header.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a"),
        "image/webp" => {
            header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP"
        }
        _ => false,
    }
}

fn ensure_original_image_unchanged(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let current_sha256 = stream_sha256(path)?;
    if current_sha256 != expected_sha256 {
        return Err("生成模型分析派生物期间原始图片发生变化，原件写入已阻止".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_image_resource_probe(mut command: Command, label: &str) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动{label}：{error}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法读取{label}状态：{error}"))?
        {
            if !status.success() {
                return Err(format!("{label}失败，图片派生已阻止"));
            }
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .ok_or_else(|| format!("{label}没有返回结果"))?
                .read_to_end(&mut bytes)
                .map_err(|error| format!("无法读取{label}结果：{error}"))?;
            return String::from_utf8(bytes).map_err(|_| format!("{label}返回了无法解析的结果"));
        }
        if started.elapsed() > MODEL_ANALYSIS_IMAGE_RESOURCE_PROBE_TIMEOUT {
            terminate_helper(&mut child);
            return Err(format!("{label}超时，图片派生已阻止"));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "macos")]
fn parse_sips_image_dimensions(output: &str) -> Result<(u64, u64), String> {
    let mut width = None;
    let mut height = None;
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let parsed = || {
            value
                .trim()
                .parse::<u64>()
                .map_err(|_| "本机图片元数据包含无效像素尺寸".to_string())
        };
        match key.trim() {
            "pixelWidth" => width = Some(parsed()?),
            "pixelHeight" => height = Some(parsed()?),
            _ => {}
        }
    }
    match (width, height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => Ok((width, height)),
        _ => Err("本机图片元数据缺少有效像素尺寸，图片派生已阻止".to_string()),
    }
}

#[cfg(target_os = "macos")]
fn sips_image_dimensions(path: &Path) -> Result<(u64, u64), String> {
    let mut command = Command::new("/usr/bin/sips");
    command
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path);
    parse_sips_image_dimensions(&run_image_resource_probe(command, "图片像素探测")?)
}

#[cfg(target_os = "macos")]
fn physical_memory_bytes() -> Result<u64, String> {
    let mut command = Command::new("/usr/sbin/sysctl");
    command.args(["-n", "hw.memsize"]);
    let output = run_image_resource_probe(command, "物理内存探测")?;
    output
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "无法取得有效物理内存容量，图片派生已阻止".to_string())
}

#[cfg(target_os = "macos")]
fn parse_available_memory_bytes(output: &str) -> Result<u64, String> {
    let page_size = output
        .lines()
        .next()
        .and_then(|line| line.split_once("page size of "))
        .and_then(|(_, rest)| rest.split_once(" bytes"))
        .and_then(|(value, _)| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "无法解析本机内存页大小，图片派生已阻止".to_string())?;
    let mut pages = HashMap::new();
    for line in output.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if let Ok(value) = value.trim().trim_end_matches('.').parse::<u64>() {
            pages.insert(key.trim(), value);
        }
    }
    let reclaimable_pages = ["Pages free", "Pages inactive", "Pages speculative"]
        .into_iter()
        .try_fold(0u64, |total, key| {
            let value = pages
                .get(key)
                .copied()
                .ok_or_else(|| format!("本机内存状态缺少 {key}，图片派生已阻止"))?;
            total
                .checked_add(value)
                .ok_or_else(|| "本机可用内存计算溢出，图片派生已阻止".to_string())
        })?;
    reclaimable_pages
        .checked_mul(page_size)
        .filter(|value| *value > 0)
        .ok_or_else(|| "本机可用内存计算失败，图片派生已阻止".to_string())
}

#[cfg(target_os = "macos")]
fn available_memory_bytes() -> Result<u64, String> {
    let command = Command::new("/usr/bin/vm_stat");
    parse_available_memory_bytes(&run_image_resource_probe(command, "可用内存探测")?)
}

#[cfg(target_os = "macos")]
fn parse_available_disk_bytes(output: &str) -> Result<u64, String> {
    let available_kib = output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "无法解析图片派生目录磁盘余量".to_string())?;
    available_kib
        .checked_mul(1024)
        .filter(|value| *value > 0)
        .ok_or_else(|| "图片派生目录磁盘余量计算失败".to_string())
}

#[cfg(target_os = "macos")]
fn available_disk_bytes(path: &Path) -> Result<u64, String> {
    let mut command = Command::new("/bin/df");
    command.args(["-P", "-k"]).arg(path);
    parse_available_disk_bytes(&run_image_resource_probe(command, "磁盘余量探测")?)
}

#[cfg(target_os = "macos")]
fn validate_image_decode_resource_budget(
    width: u64,
    height: u64,
    source_byte_length: u64,
    physical_memory: u64,
    available_memory: u64,
    available_disk: u64,
) -> Result<(), String> {
    let decoded_rgba_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "图片解码尺寸计算溢出，图片派生已阻止".to_string())?;
    let required_working_memory = decoded_rgba_bytes
        .checked_mul(2)
        .and_then(|value| value.checked_add(source_byte_length))
        .ok_or_else(|| "图片解码内存需求计算溢出，图片派生已阻止".to_string())?;
    let memory_budget = available_memory
        .saturating_mul(2)
        .checked_div(3)
        .unwrap_or(0)
        .min(physical_memory.checked_div(3).unwrap_or(0));
    if memory_budget == 0 || required_working_memory > memory_budget {
        return Err(format!(
            "图片预计需要 {required_working_memory} 字节解码内存，超过本机实时可用预算 {memory_budget} 字节，图片派生已阻止"
        ));
    }

    let required_temporary_disk = decoded_rgba_bytes
        .checked_add(source_byte_length)
        .ok_or_else(|| "图片派生磁盘需求计算溢出，图片派生已阻止".to_string())?;
    let disk_budget = available_disk.checked_div(2).unwrap_or(0);
    if disk_budget == 0 || required_temporary_disk > disk_budget {
        return Err(format!(
            "图片预计需要 {required_temporary_disk} 字节临时磁盘空间，超过当前可用预算 {disk_budget} 字节，图片派生已阻止"
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_sips_decode_resource_budget(source: &Path, target: &Path) -> Result<(), String> {
    let source_byte_length = fs::metadata(source)
        .map_err(|error| format!("无法读取图片资源门禁元数据：{error}"))?
        .len();
    let target_directory = target
        .parent()
        .ok_or_else(|| "图片派生目标缺少有效目录".to_string())?;
    let (width, height) = sips_image_dimensions(source)?;
    validate_image_decode_resource_budget(
        width,
        height,
        source_byte_length,
        physical_memory_bytes()?,
        available_memory_bytes()?,
        available_disk_bytes(target_directory)?,
    )
}

#[cfg(target_os = "macos")]
fn run_sips_derivative(
    source: &Path,
    target: &Path,
    max_dimension: u32,
    _windows_adapter: Option<&Path>,
) -> Result<(), String> {
    ensure_sips_decode_resource_budget(source, target)?;
    let mut child = Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg", "-s", "formatOptions", "78", "-Z"])
        .arg(max_dimension.to_string())
        .arg(source)
        .arg("--out")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动本机图片分析派生器：{error}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法读取图片分析派生器状态：{error}"))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err("本机图片分析派生器无法解码该图片".to_string())
            };
        }
        if started.elapsed() > MODEL_ANALYSIS_IMAGE_DERIVATIVE_TIMEOUT {
            terminate_helper(&mut child);
            return Err("生成模型图片分析派生物超时".to_string());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "windows")]
fn run_sips_derivative(
    source: &Path,
    target: &Path,
    max_dimension: u32,
    windows_adapter: Option<&Path>,
) -> Result<(), String> {
    let adapter = windows_adapter
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("YUNSPIRE_WINDOWS_IMAGE_ADAPTER").map(PathBuf::from))
        .ok_or_else(|| {
            "Windows 图片分析派生器未随安装包部署；原图未改动且本次模型图片输入已阻止".to_string()
        })?;
    if !adapter.is_file() {
        return Err(format!(
            "Windows 图片分析派生器不存在：{}",
            adapter.display()
        ));
    }
    let mut child = Command::new(&adapter)
        .arg(source)
        .arg(target)
        .arg(max_dimension.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 Windows 图片分析派生器：{error}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法读取 Windows 图片分析派生器状态：{error}"))?
        {
            let mut stdout = Vec::new();
            if let Some(mut value) = child.stdout.take() {
                value
                    .read_to_end(&mut stdout)
                    .map_err(|error| format!("无法读取 Windows 图片分析派生器输出：{error}"))?;
            }
            let mut stderr = String::new();
            if let Some(mut value) = child.stderr.take() {
                value
                    .read_to_string(&mut stderr)
                    .map_err(|error| format!("无法读取 Windows 图片分析派生器错误：{error}"))?;
            }
            if !status.success() {
                return Err(format!("Windows 图片分析派生器异常退出：{}", stderr.trim()));
            }
            let payload: Value = serde_json::from_slice(&stdout)
                .map_err(|error| format!("Windows 图片分析派生器返回无效 JSON：{error}"))?;
            if payload.get("schema").and_then(Value::as_str)
                != Some("yunspire.windows-image-derivative.v1")
            {
                return Err("Windows 图片分析派生器返回了不兼容的数据结构".to_string());
            }
            let errors = payload
                .get("errors")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !errors.is_empty() {
                let detail = errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!("Windows 图片分析派生失败：{detail}"));
            }
            let output_width = payload
                .get("output_width")
                .and_then(Value::as_u64)
                .ok_or("Windows 图片分析派生器没有返回输出宽度")?;
            let output_height = payload
                .get("output_height")
                .and_then(Value::as_u64)
                .ok_or("Windows 图片分析派生器没有返回输出高度")?;
            if output_width == 0
                || output_height == 0
                || output_width > u64::from(max_dimension)
                || output_height > u64::from(max_dimension)
            {
                return Err("Windows 图片分析派生器返回的图片尺寸越界".to_string());
            }
            let byte_length = fs::metadata(target)
                .map_err(|error| format!("Windows 图片分析派生物不存在：{error}"))?
                .len();
            if payload.get("byte_length").and_then(Value::as_u64) != Some(byte_length) {
                return Err("Windows 图片分析派生物字节长度校验失败".to_string());
            }
            let reported_path = payload
                .get("path")
                .and_then(Value::as_str)
                .ok_or("Windows 图片分析派生器没有返回输出路径")?;
            let reported = fs::canonicalize(reported_path)
                .map_err(|error| format!("无法验证 Windows 图片分析派生物路径：{error}"))?;
            let expected = fs::canonicalize(target)
                .map_err(|error| format!("无法验证 Windows 图片分析派生目标路径：{error}"))?;
            if reported != expected {
                return Err("Windows 图片分析派生器返回了意外输出路径".to_string());
            }
            return Ok(());
        }
        if started.elapsed() > MODEL_ANALYSIS_IMAGE_DERIVATIVE_TIMEOUT {
            terminate_helper(&mut child);
            return Err("生成 Windows 模型图片分析派生物超时".to_string());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_sips_derivative(
    _source: &Path,
    _target: &Path,
    _max_dimension: u32,
    _windows_adapter: Option<&Path>,
) -> Result<(), String> {
    Err(format!(
        "{} 当前没有可用的本机图片分析派生器；原图未改动且本次模型图片输入已阻止",
        env::consts::OS
    ))
}

fn read_verified_direct_image_bytes(
    path: &Path,
    precomputed_sha256: &str,
    expected_sha256: Option<&str>,
) -> Result<(Vec<u8>, String), String> {
    let bytes = fs::read(path).map_err(|error| format!("无法读取模型图片分析输入：{error}"))?;
    let sent_sha256 = format!("{:x}", Sha256::digest(&bytes));
    if sent_sha256 != precomputed_sha256 {
        return Err("读取模型图片分析输入期间原始图片发生变化，本次入库已阻止".to_string());
    }
    if normalize_expected_capture_sha256(expected_sha256)?
        .as_deref()
        .is_some_and(|expected| expected != sent_sha256)
    {
        return Err("模型实际接收图片的哈希与提取结果不一致，本次入库已阻止".to_string());
    }
    Ok((bytes, sent_sha256))
}

fn capture_image_analysis_input_with_adapter(
    path: &Path,
    mime_type: &str,
    expected_sha256: Option<&str>,
    windows_adapter: Option<&Path>,
) -> Result<CaptureImageAnalysisInput, String> {
    let original_byte_length = fs::metadata(path)
        .map_err(|error| format!("无法读取原始图片元数据：{error}"))?
        .len();
    if original_byte_length == 0 {
        return Err("原始图片为空，无法生成模型分析输入".to_string());
    }
    let original_sha256 = stream_sha256(path)?;
    let normalized_expected_sha256 = normalize_expected_capture_sha256(expected_sha256)?;
    if normalized_expected_sha256
        .as_deref()
        .is_some_and(|expected| expected != original_sha256)
    {
        return Err("模型分析前发现原始图片哈希与提取结果不一致".to_string());
    }
    let normalized_mime = mime_type.trim().to_ascii_lowercase();
    if !normalized_mime.starts_with("image/") {
        return Err("模型分析派生器只接受图片附件".to_string());
    }

    if original_byte_length <= MODEL_ANALYSIS_IMAGE_TARGET_BYTES {
        let (bytes, sent_sha256) = read_verified_direct_image_bytes(
            path,
            &original_sha256,
            normalized_expected_sha256.as_deref(),
        )?;
        if model_supported_image_signature(&normalized_mime, &bytes[..bytes.len().min(16)]) {
            return Ok(CaptureImageAnalysisInput {
                data_url: format!(
                    "data:{normalized_mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&bytes)
                ),
                original_sha256: sent_sha256.clone(),
                analysis_sha256: sent_sha256,
                original_byte_length,
                analysis_byte_length: bytes.len() as u64,
                derived: false,
                analysis_mime_type: normalized_mime,
                max_dimension: None,
            });
        }
    }

    let derivative_directory =
        tempdir().map_err(|error| format!("无法创建图片分析派生目录：{error}"))?;
    let derivative_path = derivative_directory.path().join("analysis.jpg");
    let mut selected_dimension = None;
    for max_dimension in [2048_u32, 1600, 1280, 1024, 768, 512] {
        let _ = fs::remove_file(&derivative_path);
        run_sips_derivative(path, &derivative_path, max_dimension, windows_adapter)?;
        ensure_original_image_unchanged(path, &original_sha256)?;
        let byte_length = fs::metadata(&derivative_path)
            .map_err(|error| format!("无法读取图片分析派生物元数据：{error}"))?
            .len();
        if byte_length > 0 && byte_length <= MODEL_ANALYSIS_IMAGE_TARGET_BYTES {
            selected_dimension = Some(max_dimension);
            break;
        }
    }
    let max_dimension = selected_dimension
        .ok_or("图片分析派生物仍超过单次模型边界，原件未改动且本次入库已阻止".to_string())?;
    let bytes =
        fs::read(&derivative_path).map_err(|error| format!("无法读取图片分析派生物：{error}"))?;
    if !model_supported_image_signature("image/jpeg", &bytes[..bytes.len().min(16)]) {
        return Err("图片分析派生物格式校验失败".to_string());
    }
    Ok(CaptureImageAnalysisInput {
        data_url: format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ),
        original_sha256,
        analysis_sha256: format!("{:x}", Sha256::digest(&bytes)),
        original_byte_length,
        analysis_byte_length: bytes.len() as u64,
        derived: true,
        analysis_mime_type: "image/jpeg".to_string(),
        max_dimension: Some(max_dimension),
    })
}

#[cfg(not(target_os = "windows"))]
fn capture_image_analysis_input(
    path: &Path,
    mime_type: &str,
    expected_sha256: Option<&str>,
) -> Result<CaptureImageAnalysisInput, String> {
    capture_image_analysis_input_with_adapter(path, mime_type, expected_sha256, None)
}

#[cfg(target_os = "windows")]
fn windows_image_derivative_adapter(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let relative = Path::new("skills")
        .join("document-content-analysis")
        .join("scripts")
        .join("yunspire_image_windows.exe");
    let bundled = app.path().resolve(&relative, BaseDirectory::Resource).ok();
    if let Some(path) = bundled.as_ref().filter(|path| path.is_file()) {
        return Ok(path.to_path_buf());
    }
    #[cfg(debug_assertions)]
    if let Some(development) = debug_project_file(
        &Path::new("src-tauri")
            .join("target")
            .join("yunspire-native")
            .join("yunspire_image_windows.exe"),
    ) {
        return Ok(development);
    }
    let bundled = bundled
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unavailable>".to_string());
    Err(format!("Windows 图片分析派生器未随安装包部署：{bundled}"))
}

#[tauri::command]
pub fn prepare_capture_image_analysis_input(
    app: tauri::AppHandle,
    staged_attachment_id: String,
    mime_type: String,
    expected_sha256: Option<String>,
) -> Result<CaptureImageAnalysisInput, String> {
    let path = staged_attachment_path(&staged_attachment_id)?;
    #[cfg(target_os = "windows")]
    {
        let adapter = windows_image_derivative_adapter(&app)?;
        return capture_image_analysis_input_with_adapter(
            &path,
            &mime_type,
            expected_sha256.as_deref(),
            Some(&adapter),
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        capture_image_analysis_input(&path, &mime_type, expected_sha256.as_deref())
    }
}

fn discard_staged_capture_attachments_in(
    directory: &Path,
    staged_attachment_ids: &[String],
) -> Result<usize, String> {
    let mut normalized = staged_attachment_ids
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if normalized
        .iter()
        .any(|value| !is_canonical_capture_id(value))
    {
        return Err("采集附件暂存 ID 无效".to_string());
    }
    normalized.sort_unstable();
    normalized.dedup();
    let mut removed = 0usize;
    for token in normalized {
        let path = directory.join(format!("{token}.asset"));
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("无法读取采集附件暂存文件：{error}")),
        };
        if !metadata.file_type().is_file() {
            return Err("采集附件暂存目标不是普通文件".to_string());
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法释放采集附件暂存文件：{error}")),
        }
    }
    Ok(removed)
}

#[tauri::command]
pub fn discard_capture_attachments(staged_attachment_ids: Vec<String>) -> Result<usize, String> {
    let directory = capture_attachment_directory()?;
    discard_staged_capture_attachments_in(&directory, &staged_attachment_ids)
}

#[tauri::command]
pub fn begin_capture_upload(
    file_name: String,
    relative_path: Option<String>,
    state: tauri::State<'_, CaptureUploadState>,
) -> Result<String, String> {
    let name = file_name.trim().chars().take(255).collect::<String>();
    if name.is_empty() {
        return Err("采集文件名不能为空".to_string());
    }
    let safe_path = safe_relative_path(relative_path.as_deref(), &name)?;
    let upload_id = Uuid::new_v4().to_string();
    let path = capture_upload_directory()?.join(format!("{upload_id}.part"));
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("无法创建采集分块暂存文件：{error}"))?;
    state
        .uploads
        .lock()
        .map_err(|_| "采集分块暂存状态不可用".to_string())?
        .insert(
            upload_id.clone(),
            CaptureUpload {
                path,
                name,
                relative_path: safe_path.to_string_lossy().into_owned(),
                finalized: false,
            },
        );
    Ok(upload_id)
}

#[tauri::command]
pub fn append_capture_upload_chunk(
    upload_id: String,
    chunk_base64: String,
    state: tauri::State<'_, CaptureUploadState>,
) -> Result<u64, String> {
    if !valid_upload_id(upload_id.trim()) {
        return Err("采集分块暂存 ID 无效".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(chunk_base64.as_bytes())
        .map_err(|_| "采集分块不是有效的 base64 数据".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_CHUNK_BYTES {
        return Err("采集分块必须大于 0 且不超过 4 MB".to_string());
    }
    let mut uploads = state
        .uploads
        .lock()
        .map_err(|_| "采集分块暂存状态不可用".to_string())?;
    let upload = uploads
        .get_mut(upload_id.trim())
        .ok_or_else(|| "采集分块暂存会话不存在".to_string())?;
    if upload.finalized {
        return Err("采集分块暂存会话已经结束".to_string());
    }
    let mut target = fs::OpenOptions::new()
        .append(true)
        .open(&upload.path)
        .map_err(|error| format!("无法打开采集分块暂存文件：{error}"))?;
    target
        .write_all(&bytes)
        .map_err(|error| format!("无法写入采集分块：{error}"))?;
    target
        .flush()
        .map_err(|error| format!("无法刷新采集分块：{error}"))?;
    fs::metadata(&upload.path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("无法读取采集分块暂存进度：{error}"))
}

#[tauri::command]
pub fn finish_capture_upload(
    upload_id: String,
    state: tauri::State<'_, CaptureUploadState>,
) -> Result<CaptureUploadReceipt, String> {
    if !valid_upload_id(upload_id.trim()) {
        return Err("采集分块暂存 ID 无效".to_string());
    }
    let mut uploads = state
        .uploads
        .lock()
        .map_err(|_| "采集分块暂存状态不可用".to_string())?;
    let upload = uploads
        .get_mut(upload_id.trim())
        .ok_or_else(|| "采集分块暂存会话不存在".to_string())?;
    upload.finalized = true;
    let byte_length = fs::metadata(&upload.path)
        .map_err(|error| format!("无法读取采集分块暂存文件：{error}"))?
        .len();
    Ok(CaptureUploadReceipt {
        upload_id: upload_id.trim().to_string(),
        byte_length,
    })
}

fn prepare_capture_input_files(
    files: Vec<CaptureInputFile>,
    state: &CaptureUploadState,
) -> Result<Vec<PreparedCaptureInputFile>, String> {
    let mut uploads = state
        .uploads
        .lock()
        .map_err(|_| "采集分块暂存状态不可用".to_string())?;
    files
        .into_iter()
        .map(|file| {
            if let Some(upload_id) = file
                .upload_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                let upload = uploads
                    .remove(upload_id.trim())
                    .ok_or_else(|| format!("文件 {} 的分块暂存会话不存在", file.name))?;
                if !upload.finalized {
                    let _ = fs::remove_file(&upload.path);
                    return Err(format!("文件 {} 的分块上传尚未结束", file.name));
                }
                return Ok(PreparedCaptureInputFile {
                    name: upload.name,
                    relative_path: Some(upload.relative_path),
                    content_base64: None,
                    staged_path: Some(upload.path),
                });
            }
            if file.content_base64.as_deref().is_none_or(str::is_empty) {
                return Err(format!("文件 {} 没有可读取的内容", file.name));
            }
            Ok(PreparedCaptureInputFile {
                name: file.name,
                relative_path: file.relative_path,
                content_base64: file.content_base64,
                staged_path: None,
            })
        })
        .collect()
}

fn stream_sha256(path: &Path) -> Result<String, String> {
    let source = fs::File::open(path).map_err(|error| format!("无法读取采集文件：{error}"))?;
    let mut reader = BufReader::new(source);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("无法读取采集文件：{error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn normalize_expected_capture_sha256(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = value
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("sha256:"))
        .map(|_| &value[7..])
        .unwrap_or(value);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("预期图片哈希必须是完整的 SHA-256".to_string());
    }
    Ok(Some(value.to_ascii_lowercase()))
}

#[cfg(debug_assertions)]
fn debug_project_file(relative: &Path) -> Option<PathBuf> {
    let current = env::current_dir().ok()?;
    for root in current.ancestors().take(4) {
        let candidate = root.join(relative);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn helper_script(app: &tauri::AppHandle, kind: &str) -> Result<PathBuf, String> {
    let relative = match kind {
        "url" => [
            "skills",
            "web-content-analysis",
            "scripts",
            "extract_web.py",
        ],
        "video" => [
            "skills",
            "video-content-analysis",
            "scripts",
            "extract_video.py",
        ],
        "file" | "folder" | "text" => [
            "skills",
            "document-content-analysis",
            "scripts",
            "extract_document.py",
        ],
        _ => return Err("不支持的采集来源类型".to_string()),
    };
    let relative_path = relative.iter().copied().collect::<PathBuf>();
    let bundled = app
        .path()
        .resolve(&relative_path, BaseDirectory::Resource)
        .ok();
    if let Some(path) = bundled.as_ref().filter(|path| path.is_file()) {
        return Ok(path.to_path_buf());
    }
    #[cfg(debug_assertions)]
    if let Some(development) = debug_project_file(&relative_path) {
        return Ok(development);
    }
    let bundled = bundled
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unavailable>".to_string());
    Err(format!("采集技能脚本未随应用部署：{bundled}"))
}

fn python_executable(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Ok(configured) = env::var("YUNSPIRE_PYTHON") {
        if !configured.trim().is_empty() {
            let configured = PathBuf::from(configured);
            if configured.is_file() {
                return Ok(configured);
            }
            return Err(format!(
                "YUNSPIRE_PYTHON 指向的运行时不存在：{}",
                configured.display()
            ));
        }
    }
    #[cfg(target_os = "windows")]
    {
        let relative = Path::new("runtime").join("python").join("python.exe");
        let bundled = app.path().resolve(&relative, BaseDirectory::Resource).ok();
        if let Some(path) = bundled.as_ref().filter(|path| path.is_file()) {
            return Ok(path.to_path_buf());
        }
        #[cfg(debug_assertions)]
        if let Some(development) = debug_project_file(
            &Path::new("src-tauri")
                .join("target")
                .join("yunspire-runtime")
                .join("python")
                .join("python.exe"),
        ) {
            return Ok(development);
        }
        #[cfg(debug_assertions)]
        {
            return Ok(PathBuf::from("python"));
        }
        #[cfg(not(debug_assertions))]
        let bundled = bundled
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<unavailable>".to_string());
        #[cfg(not(debug_assertions))]
        Err(format!(
            "云枢 Windows Python 运行时未随安装包部署：{bundled}"
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        if Path::new("/usr/bin/python3").exists() {
            return Ok(PathBuf::from("/usr/bin/python3"));
        }
        Ok(PathBuf::from("python3"))
    }
}

fn configure_python_path(command: &mut Command) {
    command.env("PYTHONDONTWRITEBYTECODE", "1");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        command
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = env::var("HOME") {
        let user_site = Path::new(&home).join("Library/Python/3.9/lib/python/site-packages");
        if user_site.exists() {
            let joined = match env::var_os("PYTHONPATH") {
                Some(existing) if !existing.is_empty() => {
                    let mut paths = vec![user_site];
                    paths.extend(env::split_paths(&existing));
                    env::join_paths(paths).ok()
                }
                _ => env::join_paths([user_site]).ok(),
            };
            if let Some(value) = joined {
                command.env("PYTHONPATH", value);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_job {
    use std::{ffi::c_void, mem::size_of, os::windows::io::AsRawHandle, process::Child, ptr};

    type Handle = *mut c_void;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct IoCounters {
        ReadOperationCount: u64,
        WriteOperationCount: u64,
        OtherOperationCount: u64,
        ReadTransferCount: u64,
        WriteTransferCount: u64,
        OtherTransferCount: u64,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct BasicLimitInformation {
        PerProcessUserTimeLimit: i64,
        PerJobUserTimeLimit: i64,
        LimitFlags: u32,
        MinimumWorkingSetSize: usize,
        MaximumWorkingSetSize: usize,
        ActiveProcessLimit: u32,
        Affinity: usize,
        PriorityClass: u32,
        SchedulingClass: u32,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct ExtendedLimitInformation {
        BasicLimitInformation: BasicLimitInformation,
        IoInfo: IoCounters,
        ProcessMemoryLimit: usize,
        JobMemoryLimit: usize,
        PeakProcessMemoryUsed: usize,
        PeakJobMemoryUsed: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            information_class: i32,
            information: *const c_void,
            information_length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    pub(super) struct KillOnCloseJob(Handle);

    impl KillOnCloseJob {
        pub(super) fn assign(child: &Child) -> Result<Self, String> {
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "无法创建 Windows 采集进程作业：错误 {}",
                    unsafe { GetLastError() }
                ));
            }
            let information = ExtendedLimitInformation {
                BasicLimitInformation: BasicLimitInformation {
                    PerProcessUserTimeLimit: 0,
                    PerJobUserTimeLimit: 0,
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    MinimumWorkingSetSize: 0,
                    MaximumWorkingSetSize: 0,
                    ActiveProcessLimit: 0,
                    Affinity: 0,
                    PriorityClass: 0,
                    SchedulingClass: 0,
                },
                IoInfo: IoCounters {
                    ReadOperationCount: 0,
                    WriteOperationCount: 0,
                    OtherOperationCount: 0,
                    ReadTransferCount: 0,
                    WriteTransferCount: 0,
                    OtherTransferCount: 0,
                },
                ProcessMemoryLimit: 0,
                JobMemoryLimit: 0,
                PeakProcessMemoryUsed: 0,
                PeakJobMemoryUsed: 0,
            };
            let configured = unsafe {
                SetInformationJobObject(
                    handle,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&information as *const ExtendedLimitInformation).cast(),
                    size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if configured == 0 {
                let error = unsafe { GetLastError() };
                unsafe { CloseHandle(handle) };
                return Err(format!("无法配置 Windows 采集进程作业：错误 {error}"));
            }
            let assigned =
                unsafe { AssignProcessToJobObject(handle, child.as_raw_handle().cast::<c_void>()) };
            if assigned == 0 {
                let error = unsafe { GetLastError() };
                unsafe { CloseHandle(handle) };
                return Err(format!("无法绑定 Windows 采集进程树：错误 {error}"));
            }
            Ok(Self(handle))
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
                self.0 = ptr::null_mut();
            }
        }
    }
}

fn normalize_speech_locale(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 63 {
        return Err("语音识别语言必须是有效的 BCP-47 标签".to_string());
    }
    let parts = value.split('-').collect::<Vec<_>>();
    let language = parts[0];
    if !(2..=8).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return Err("语音识别语言必须以 2 至 8 位字母语言代码开头".to_string());
    }
    let mut normalized = Vec::with_capacity(parts.len());
    normalized.push(language.to_ascii_lowercase());
    for part in parts.into_iter().skip(1) {
        if part.is_empty()
            || part.len() > 8
            || !part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err("语音识别语言包含无效的 BCP-47 子标签".to_string());
        }
        if part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            let mut characters = part.chars();
            let first = characters.next().unwrap_or_default().to_ascii_uppercase();
            normalized.push(format!(
                "{first}{}",
                characters.as_str().to_ascii_lowercase()
            ));
        } else if (part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()))
            || (part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            normalized.push(part.to_ascii_uppercase());
        } else {
            normalized.push(part.to_ascii_lowercase());
        }
    }
    Ok(normalized.join("-"))
}

fn normalize_system_speech_locale(value: &str) -> Option<String> {
    let without_modifier = value.split('@').next().unwrap_or(value);
    let without_encoding = without_modifier
        .split('.')
        .next()
        .unwrap_or(without_modifier);
    let normalized_separators = without_encoding.replace('_', "-");
    if matches!(normalized_separators.as_str(), "C" | "POSIX") {
        return None;
    }
    normalize_speech_locale(&normalized_separators).ok()
}

#[cfg(target_os = "windows")]
fn windows_user_speech_locale() -> Option<String> {
    const LOCALE_NAME_MAX_LENGTH: usize = 85;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultLocaleName(locale_name: *mut u16, locale_name_count: i32) -> i32;
    }

    let mut locale_name = [0_u16; LOCALE_NAME_MAX_LENGTH];
    let length = unsafe {
        GetUserDefaultLocaleName(locale_name.as_mut_ptr(), LOCALE_NAME_MAX_LENGTH as i32)
    };
    if length <= 1 {
        return None;
    }
    String::from_utf16(&locale_name[..length as usize - 1])
        .ok()
        .and_then(|value| normalize_system_speech_locale(&value))
}

#[cfg(not(target_os = "windows"))]
fn windows_user_speech_locale() -> Option<String> {
    None
}

fn preferred_speech_locale(requested: Option<&str>) -> Result<String, String> {
    if let Some(requested) = requested.filter(|value| !value.trim().is_empty()) {
        return normalize_speech_locale(requested);
    }
    if let Ok(configured) = env::var("YUNSPIRE_SPEECH_LOCALE") {
        if !configured.trim().is_empty() {
            return normalize_speech_locale(&configured)
                .map_err(|error| format!("云枢语音语言偏好无效：{error}"));
        }
    }
    if let Some(locale) = windows_user_speech_locale() {
        return Ok(locale);
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = env::var(name) {
            if let Some(locale) = normalize_system_speech_locale(&value) {
                return Ok(locale);
            }
        }
    }
    Ok(DEFAULT_SPEECH_LOCALE.to_string())
}

fn video_helper_args(source: String, output_dir: Option<String>, locale: &str) -> Vec<String> {
    let mut args = vec![source];
    if let Some(output_dir) = output_dir {
        args.extend(["--output-dir".to_string(), output_dir]);
    }
    args.extend(["--locale".to_string(), locale.to_string()]);
    args
}

fn terminate_helper(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("/bin/kill")
            .args(["-TERM", process_group.as_str()])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_helper_error_tail(file: &mut fs::File) -> String {
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = length.saturating_sub(64 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity((length - start) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn run_helper(
    python: &Path,
    script: &Path,
    args: &[String],
    authorization: Option<&HelperAuthorization>,
    cancellation: Option<&AtomicBool>,
    timeout: Duration,
) -> Result<Value, String> {
    let mut stdout_file =
        NamedTempFile::new().map_err(|error| format!("无法创建采集技能输出暂存文件：{error}"))?;
    let mut stderr_file =
        NamedTempFile::new().map_err(|error| format!("无法创建采集技能错误暂存文件：{error}"))?;
    let progress_file =
        NamedTempFile::new().map_err(|error| format!("无法创建采集技能进度暂存文件：{error}"))?;
    let mut command = Command::new(python);
    configure_python_path(&mut command);
    command.env("YUNSPIRE_PROGRESS_FILE", progress_file.path());
    #[cfg(target_os = "windows")]
    {
        let script_directory = script
            .parent()
            .ok_or_else(|| "采集技能脚本缺少父目录".to_string())?;
        command
            .arg("-c")
            .arg("import runpy,sys;script=sys.argv[1];sys.path.insert(0,sys.argv[2]);sys.argv=[script,*sys.argv[3:]];runpy.run_path(script,run_name='__main__')")
            .arg(script)
            .arg(script_directory);
    }
    #[cfg(not(target_os = "windows"))]
    command.arg(script);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if authorization.is_some() {
        command.arg("--request-headers-stdin").stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::from(stdout_file.reopen().map_err(|error| {
            format!("无法打开采集技能输出暂存文件：{error}")
        })?))
        .stderr(Stdio::from(stderr_file.reopen().map_err(|error| {
            format!("无法打开采集技能错误暂存文件：{error}")
        })?))
        .spawn()
        .map_err(|error| format!("无法启动采集技能运行时：{error}"))?;
    #[cfg(target_os = "windows")]
    let helper_job = match windows_job::KillOnCloseJob::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            terminate_helper(&mut child);
            return Err(error);
        }
    };
    if let Some(authorization) = authorization {
        let payload = serde_json::to_vec(authorization)
            .map_err(|error| format!("无法准备一次性授权：{error}"))?;
        child
            .stdin
            .take()
            .ok_or("采集技能授权通道不可用")?
            .write_all(&payload)
            .map_err(|error| format!("无法传递一次性授权：{error}"))?;
    }
    let mut last_activity = Instant::now();
    let mut stdout_length = 0;
    let mut stderr_length = 0;
    let mut progress_length = 0;
    let termination_reason;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法等待采集技能完成：{error}"))?
        {
            if !status.success() {
                let detail = read_helper_error_tail(stderr_file.as_file_mut());
                return Err(if detail.is_empty() {
                    "采集技能执行失败".to_string()
                } else {
                    detail.chars().take(800).collect()
                });
            }
            stdout_file
                .as_file_mut()
                .seek(SeekFrom::Start(0))
                .map_err(|error| format!("无法读取采集技能输出：{error}"))?;
            return serde_json::from_reader(BufReader::new(stdout_file.as_file_mut()))
                .map_err(|error| format!("采集技能返回无效 JSON：{error}"));
        }
        let current_stdout_length = stdout_file
            .as_file()
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(stdout_length);
        let current_stderr_length = stderr_file
            .as_file()
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(stderr_length);
        let current_progress_length = progress_file
            .as_file()
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(progress_length);
        if current_stdout_length > stdout_length
            || current_stderr_length > stderr_length
            || current_progress_length > progress_length
        {
            last_activity = Instant::now();
            stdout_length = current_stdout_length;
            stderr_length = current_stderr_length;
            progress_length = current_progress_length;
        }
        if cancellation.is_some_and(|value| value.load(Ordering::Acquire)) {
            termination_reason = "采集任务已取消".to_string();
            break;
        }
        if last_activity.elapsed() >= timeout {
            termination_reason = format!(
                "采集技能在 {} 秒内没有产生可验证进度，已停止运行",
                timeout.as_secs()
            );
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    #[cfg(target_os = "windows")]
    drop(helper_job);
    terminate_helper(&mut child);
    Err(termination_reason)
}

fn stage_result_attachments(result: &mut Value, root: &Path) -> Result<(), String> {
    let Some(attachments) = result
        .as_object_mut()
        .and_then(|object| object.get_mut("attachments"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments.drain(..) {
        let mut object = attachment.as_object().cloned().unwrap_or_default();
        let original_name = object.get("name").cloned();
        let original_mime_type = object.get("mime_type").cloned();
        let local_path = object
            .remove("local_attachment_path")
            .and_then(|value| value.as_str().map(str::to_string));
        object.remove("data_base64");
        if let Some(local_path) = local_path {
            if let Some(descriptor) = stage_capture_attachment(Path::new(&local_path), root)? {
                if let Some(values) = descriptor.as_object() {
                    for (key, value) in values {
                        object.insert(key.clone(), value.clone());
                    }
                }
                if let Some(name) = original_name {
                    object.insert("name".to_string(), name);
                }
                if let Some(mime_type) = original_mime_type {
                    object.insert("mime_type".to_string(), mime_type);
                }
                staged.push(Value::Object(object));
            }
        } else if object.get("staged_attachment_id").is_some() {
            staged.push(Value::Object(object));
        }
    }
    *attachments = staged;
    Ok(())
}

fn enrich_video_result(mut result: Value, root: &Path) -> Result<Value, String> {
    let object = result
        .as_object_mut()
        .ok_or_else(|| "视频采集技能返回的结果不是对象".to_string())?;
    let mut attachments = object
        .remove("attachments")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    if let Some(media_path) = object.get("media_path").and_then(Value::as_str) {
        if let Some(attachment) = stage_capture_attachment(Path::new(media_path), root)? {
            attachments.push(attachment);
        } else {
            object
                .entry("warnings")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(warnings) = object.get_mut("warnings").and_then(Value::as_array_mut) {
                warnings.push(Value::String(
                    "原视频不存在、为空或越过隔离目录，未进入待审批写入".to_string(),
                ));
            }
        }
    }
    let frames = object
        .get("frames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for frame in frames.iter().filter_map(Value::as_str) {
        if let Some(attachment) = stage_capture_attachment(Path::new(frame), root)? {
            attachments.push(attachment);
        }
    }
    object.insert("attachments".to_string(), Value::Array(attachments));
    object.remove("media_path");
    Ok(result)
}

fn result_has_media_candidates(result: &Value) -> bool {
    result
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("media_candidate_count"))
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0)
}

fn result_has_local_media(result: &Value) -> bool {
    result
        .get("attachments")
        .and_then(Value::as_array)
        .is_some_and(|attachments| {
            attachments.iter().any(|attachment| {
                attachment
                    .get("mime_type")
                    .and_then(Value::as_str)
                    .is_some_and(|mime| mime.starts_with("video/") || mime.starts_with("audio/"))
            })
        })
}

fn result_has_web_content(result: &Value) -> bool {
    let blocked = result
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            errors
                .iter()
                .filter_map(Value::as_str)
                .any(|error| error == "web_content_blocked")
        });
    if blocked {
        return false;
    }
    result
        .get("content_markdown")
        .and_then(Value::as_str)
        .is_some_and(|content| {
            content
                .lines()
                .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
                .any(|line| !line.trim().is_empty())
        })
}

fn append_warning(result: &mut Value, warning: String) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let warnings = object
        .entry("warnings")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(warnings) = warnings.as_array_mut() {
        warnings.push(Value::String(warning));
    }
}

fn append_error(result: &mut Value, error: String) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let errors = object
        .entry("errors")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(errors) = errors.as_array_mut() {
        errors.push(Value::String(error));
    }
}

fn media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_lowercase().as_str(),
                "mp4"
                    | "mov"
                    | "m4v"
                    | "webm"
                    | "m3u8"
                    | "mp3"
                    | "m4a"
                    | "aac"
                    | "wav"
                    | "aif"
                    | "aiff"
                    | "caf"
                    | "flac"
                    | "ogg"
                    | "ts"
            )
        })
}

fn collect_media_paths(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| format!("无法读取隔离目录：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取隔离目录项：{error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_media_paths(&path, output)?;
        } else if path.is_file() && media_path(&path) {
            output.push(path);
        }
    }
    output.sort();
    Ok(())
}

fn extend_result_array(object: &mut serde_json::Map<String, Value>, key: &str, values: Vec<Value>) {
    if values.is_empty() {
        return;
    }
    let target = object
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(array) = target.as_array_mut() {
        array.extend(values);
    }
}

fn merge_media_result(base: &mut Value, media: &Value, label: &str) {
    let Some(base_object) = base.as_object_mut() else {
        return;
    };
    let Some(media_object) = media.as_object() else {
        return;
    };
    let transcript = media_object
        .get("transcript")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !transcript.is_empty() {
        let content = base_object
            .entry("content_markdown".to_string())
            .or_insert_with(|| Value::String(String::new()));
        let existing = content.as_str().unwrap_or_default();
        let separator = if existing.trim().is_empty() {
            ""
        } else {
            "\n\n"
        };
        *content = Value::String(format!(
            "{existing}{separator}## 音视频转录：{label}\n\n{transcript}"
        ));
    }
    if let Some(attachments) = media_object.get("attachments").and_then(Value::as_array) {
        extend_result_array(base_object, "attachments", attachments.clone());
    }
    for key in ["warnings", "errors"] {
        let values = media_object
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                Value::String(format!(
                    "音视频 {label}：{}",
                    value.as_str().unwrap_or("处理失败")
                ))
            })
            .collect();
        extend_result_array(base_object, key, values);
    }
    let summary = serde_json::json!({
        "title": media_object.get("title").cloned().unwrap_or(Value::String(label.to_string())),
        "status": media_object.get("status").cloned().unwrap_or(Value::String("unknown".to_string())),
        "transcript": transcript,
        "metadata": media_object.get("metadata").cloned().unwrap_or(Value::Object(serde_json::Map::new())),
    });
    extend_result_array(base_object, "media_results", vec![summary]);
}

fn try_video_url(
    python: &Path,
    source: &str,
    video_script: &Path,
    locale: &str,
    authorization: Option<&HelperAuthorization>,
    cancellation: &AtomicBool,
) -> Result<Option<CaptureExtraction>, String> {
    let probe_args = video_helper_args(source.to_string(), None, locale);
    let probe = match run_helper(
        python,
        video_script,
        &probe_args,
        authorization,
        Some(cancellation),
        WEB_HELPER_TIMEOUT,
    ) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if probe
        .get("auth_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(Some(capture_extraction("url".to_string(), probe)));
    }
    if !result_has_media_candidates(&probe) {
        return Ok(None);
    }
    let directory = tempdir().map_err(|error| format!("无法创建隔离视频目录：{error}"))?;
    let output_dir = directory.path().to_string_lossy().into_owned();
    let full_args = video_helper_args(source.to_string(), Some(output_dir), locale);
    let full = run_helper(
        python,
        video_script,
        &full_args,
        authorization,
        Some(cancellation),
        VIDEO_HELPER_TIMEOUT,
    )
    .and_then(|value| enrich_video_result(value, directory.path()));
    match full {
        Ok(mut video) if result_has_local_media(&video) => {
            append_warning(
                &mut video,
                "页面公开元数据表明该来源为视频，已自动切换到本地音视频处理".to_string(),
            );
            Ok(Some(capture_extraction("video".to_string(), video)))
        }
        Ok(_) => Err("视频来源没有生成可验证的本地音视频、转录或画面资料".to_string()),
        Err(error) => Err(format!("视频来源本地处理失败：{error}")),
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn extract_capture_source(
    app: tauri::AppHandle,
    source_type: String,
    source: String,
    files: Vec<CaptureInputFile>,
    authorization_id: Option<String>,
    authorization_state: tauri::State<'_, CaptureAuthorizationState>,
    task_id: Option<String>,
    speech_locale: Option<String>,
    task_state: tauri::State<'_, CaptureTaskState>,
    upload_state: tauri::State<'_, CaptureUploadState>,
) -> Result<CaptureExtraction, String> {
    let source_type = source_type.trim().to_lowercase();
    let locale = preferred_speech_locale(speech_locale.as_deref())?;
    let python = python_executable(&app)?;
    let script = helper_script(&app, &source_type)?;
    let video_script = helper_script(&app, "video")?;
    if authorization_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && !matches!(source_type.as_str(), "url" | "video")
    {
        return Err("网络授权只能用于网址或视频链接采集".to_string());
    }
    let authorization = if matches!(source_type.as_str(), "url" | "video") {
        take_capture_authorization(authorization_id.as_deref(), &source, &authorization_state)?
    } else {
        None
    };
    let task_id = task_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if task_id.len() > 80
        || !task_id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_')
    {
        return Err("采集任务 ID 格式无效".to_string());
    }
    let cancellation = task_state.register(&task_id)?;
    let files = prepare_capture_input_files(files, upload_state.inner())?;
    let task_state_ref = task_state.inner();
    let result = tauri::async_runtime::spawn_blocking(move || {
        extract_capture_source_blocking(CaptureExtractionJob {
            source_type,
            source,
            files,
            script,
            video_script,
            python,
            locale,
            authorization,
            cancellation,
        })
    })
    .await
    .map_err(|error| format!("采集任务线程异常：{error}"));
    task_state_ref.finish(&task_id);
    result?
}

struct CaptureExtractionJob {
    source_type: String,
    source: String,
    files: Vec<PreparedCaptureInputFile>,
    script: PathBuf,
    video_script: PathBuf,
    python: PathBuf,
    locale: String,
    authorization: Option<HelperAuthorization>,
    cancellation: Arc<AtomicBool>,
}

fn extract_capture_source_blocking(job: CaptureExtractionJob) -> Result<CaptureExtraction, String> {
    let CaptureExtractionJob {
        source_type,
        source,
        files,
        script,
        video_script,
        python,
        locale,
        authorization,
        cancellation,
    } = job;
    if source_type == "url" {
        if source.trim().is_empty() {
            return Err("采集来源不能为空".to_string());
        }
        // 网页图片由技能流式写入隔离目录，随后统一进入原生附件暂存，避免把整张图片
        // 堆在助手进程内存或 JSON stdout 中；目录在本次提取完成后随临时目录销毁。
        let directory = tempdir().map_err(|error| format!("无法创建隔离网页附件目录：{error}"))?;
        let attachment_output = directory.path().join(".yunspire-web-attachments");
        fs::create_dir_all(&attachment_output)
            .map_err(|error| format!("无法创建网页附件输出目录：{error}"))?;
        let mut result = run_helper(
            &python,
            &script,
            &[
                source.clone(),
                "--attachment-output-dir".to_string(),
                attachment_output.to_string_lossy().into_owned(),
            ],
            authorization.as_ref(),
            Some(cancellation.as_ref()),
            WEB_HELPER_TIMEOUT,
        )?;
        if result
            .get("auth_required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(capture_extraction(source_type, result));
        }
        if let Some(video) = try_video_url(
            &python,
            &source,
            &video_script,
            &locale,
            authorization.as_ref(),
            cancellation.as_ref(),
        )? {
            if result_has_web_content(&result) {
                let label = video
                    .result
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("网页内嵌媒体");
                merge_media_result(&mut result, &video.result, label);
                append_warning(
                    &mut result,
                    "网页包含公开音视频，已在保留正文和图片的基础上追加本地媒体分析".to_string(),
                );
            } else {
                return Ok(video);
            }
        }
        stage_result_attachments(&mut result, directory.path())?;
        return Ok(capture_extraction(source_type, result));
    }
    if source_type == "video" {
        if source.trim().is_empty() {
            return Err("采集来源不能为空".to_string());
        }
        let directory = tempdir().map_err(|error| format!("无法创建隔离视频目录：{error}"))?;
        let output_dir = directory.path().to_string_lossy().into_owned();
        let helper_args = video_helper_args(source, Some(output_dir.clone()), &locale);
        let result = run_helper(
            &python,
            &script,
            &helper_args,
            authorization.as_ref(),
            Some(cancellation.as_ref()),
            VIDEO_HELPER_TIMEOUT,
        )?;
        let result = enrich_video_result(result, directory.path())?;
        return Ok(capture_extraction(source_type, result));
    }
    if files.is_empty() && source.trim().is_empty() {
        return Err("没有收到本地文件内容".to_string());
    }

    let directory = tempdir().map_err(|error| format!("无法创建隔离采集目录：{error}"))?;
    let root = directory.path().to_path_buf();
    let mut seen_file_hashes = HashSet::new();
    let mut duplicate_file_count = 0usize;
    if !source.trim().is_empty() && source_type == "text" {
        let path = root.join("粘贴内容.md");
        fs::write(&path, source.as_bytes())
            .map_err(|error| format!("无法保存文本内容：{error}"))?;
    }
    for file in files {
        if cancellation.load(Ordering::Acquire) {
            return Err("采集任务已取消".to_string());
        }
        let content_hash = if let Some(staged_path) = file.staged_path.as_deref() {
            stream_sha256(staged_path)?
        } else {
            let encoded = file
                .content_base64
                .as_deref()
                .ok_or_else(|| format!("文件 {} 的内容不存在", file.name))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| format!("文件 {} 的内容编码无效", file.name))?;
            format!("{:x}", Sha256::digest(bytes))
        };
        if !seen_file_hashes.insert(content_hash) {
            duplicate_file_count = duplicate_file_count.saturating_add(1);
            if let Some(staged_path) = file.staged_path.as_deref() {
                let _ = fs::remove_file(staged_path);
            }
            continue;
        }
        let relative = safe_relative_path(file.relative_path.as_deref(), &file.name)?;
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("无法创建隔离目录：{error}"))?;
        }
        if let Some(staged_path) = file.staged_path.as_deref() {
            fs::copy(staged_path, &path)
                .map_err(|error| format!("无法把分块采集文件复制到隔离目录：{error}"))?;
            let _ = fs::remove_file(staged_path);
        } else {
            let encoded = file
                .content_base64
                .as_deref()
                .ok_or_else(|| format!("文件 {} 的内容不存在", file.name))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| format!("文件 {} 的内容编码无效", file.name))?;
            fs::write(path, bytes).map_err(|error| format!("无法写入隔离文件：{error}"))?;
        }
    }
    let attachment_output = root.join(".yunspire-extracted-attachments");
    fs::create_dir_all(&attachment_output)
        .map_err(|error| format!("无法创建文档附件隔离目录：{error}"))?;
    let mut result = run_helper(
        &python,
        &script,
        &[
            root.to_string_lossy().into_owned(),
            "--attachment-output-dir".to_string(),
            attachment_output.to_string_lossy().into_owned(),
        ],
        None,
        Some(cancellation.as_ref()),
        DOCUMENT_HELPER_TIMEOUT,
    )?;
    stage_result_attachments(&mut result, &root)?;
    if duplicate_file_count > 0 {
        append_warning(
            &mut result,
            format!("已跳过 {duplicate_file_count} 个内容哈希完全相同的重复文件"),
        );
    }
    let mut media_files = Vec::new();
    collect_media_paths(&root, &mut media_files)?;
    if !media_files.is_empty() {
        for (index, media_file) in media_files.into_iter().enumerate() {
            if cancellation.load(Ordering::Acquire) {
                return Err("采集任务已取消".to_string());
            }
            let media_output = root.join(".yunspire-media-v2").join(index.to_string());
            fs::create_dir_all(&media_output)
                .map_err(|error| format!("无法创建音视频处理目录：{error}"))?;
            let helper_args = video_helper_args(
                media_file.to_string_lossy().into_owned(),
                Some(media_output.to_string_lossy().into_owned()),
                &locale,
            );
            let media_result = run_helper(
                &python,
                &video_script,
                &helper_args,
                None,
                Some(cancellation.as_ref()),
                VIDEO_HELPER_TIMEOUT,
            )
            .and_then(|value| enrich_video_result(value, &root));
            match media_result {
                Ok(value) => {
                    let label = media_file
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("本地媒体");
                    merge_media_result(&mut result, &value, label);
                }
                Err(error) => append_error(
                    &mut result,
                    format!("本地媒体 {} 未完成处理：{error}", media_file.display()),
                ),
            }
        }
    }
    Ok(capture_extraction(source_type, result))
}
