use crate::{
    capture_pipeline::{claim_staged_capture_attachment, remove_claimed_capture_attachment},
    durable_asset::resolve_ready_asset_path,
    execution_ticket::{ExecutionTicketState, TicketScope},
    model_provider::ModelAnalysisState,
    runtime_db::RuntimeDatabase,
    vault_batch::{self, BatchFileSource, BatchManifestEntryInput},
};
use base64::Engine;
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use similar::TextDiff;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env,
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tauri::{AppHandle, Manager, State};
use tempfile::NamedTempFile;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

pub use crate::task_runtime::OperationContext;

const DEFAULT_SEARCH_LIMIT: usize = 50;
const MAX_SEARCH_LIMIT: usize = 200;
const MAX_PENDING_WRITES: usize = 32;
const WRITE_APPROVAL_TTL: Duration = Duration::from_secs(15 * 60);
const FULL_NOTE_DIFF_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LONG_TERM_MEMORY_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_LONG_TERM_MEMORY_METADATA_BYTES: usize = 256 * 1024;
const VAULT_WRITE_CAPABILITIES: &[&str] = &[
    "system:capture",
    "system:create",
    "system:inbox",
    "system:knowledge_maintenance",
    "system:reports",
    "system:vaults",
];
const VAULT_WRITE_OPERATIONS: &[&str] = &["run", "create", "update", "generate"];

#[derive(Default)]
pub struct ObsidianAdapterState {
    pending_writes: Mutex<HashMap<String, PendingWrite>>,
    pending_assets: Mutex<HashMap<String, PendingAssetWrite>>,
}

#[derive(Clone)]
enum PendingNoteSource {
    Text(String),
    Durable(PathBuf),
}

impl PendingNoteSource {
    fn content_hash(&self) -> Result<String, String> {
        match self {
            Self::Text(content) => Ok(hash_bytes(content.as_bytes())),
            Self::Durable(path) => hash_file_streaming(path),
        }
    }

    fn byte_length(&self) -> Result<u64, String> {
        match self {
            Self::Text(content) => Ok(content.len() as u64),
            Self::Durable(path) => fs::metadata(path)
                .map(|metadata| metadata.len())
                .map_err(|error| format!("无法读取待写入耐久正文元数据：{error}")),
        }
    }

    fn line_count(&self) -> Result<u64, String> {
        match self {
            Self::Text(content) => Ok(text_line_count(content.as_bytes())),
            Self::Durable(path) => validate_utf8_file_and_count_lines(path),
        }
    }

    fn read_to_string(&self) -> Result<String, String> {
        match self {
            Self::Text(content) => Ok(content.clone()),
            Self::Durable(path) => fs::read_to_string(path)
                .map_err(|error| format!("无法读取待写入耐久 Markdown：{error}")),
        }
    }

    fn batch_source(&self) -> BatchFileSource<'_> {
        match self {
            Self::Text(content) => BatchFileSource::Bytes(content.as_bytes()),
            Self::Durable(path) => BatchFileSource::Path(path),
        }
    }
}

#[derive(Clone)]
struct PendingWrite {
    task_id: Option<String>,
    trace_id: Option<String>,
    vault_id: String,
    vault_path: PathBuf,
    relative_path: String,
    target_path: PathBuf,
    source: PendingNoteSource,
    content_hash: String,
    expected_hash: Option<String>,
    expected_absent: bool,
    previous_hash: Option<String>,
    analysis_receipt: String,
    write_manifest_digest: Option<String>,
    execution_ticket: Option<String>,
    effect_digest: String,
    created_at: SystemTime,
}

const NOTE_WRITE_MANIFEST_VERSION: &str = "yunspire.note-write-manifest.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NoteWriteManifestEntry {
    vault_id: String,
    relative_path: String,
    previous: String,
    next_content_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NoteWriteManifest<'a> {
    version: &'static str,
    writes: &'a [NoteWriteManifestEntry],
}

#[derive(Clone)]
enum PendingAssetSource {
    Bytes(Vec<u8>),
    Staged(PathBuf),
    Durable(PathBuf),
}

#[derive(Clone)]
struct PendingAssetWrite {
    task_id: Option<String>,
    trace_id: Option<String>,
    vault_id: String,
    vault_path: PathBuf,
    relative_path: String,
    target_path: PathBuf,
    source: PendingAssetSource,
    content_hash: String,
    previous_hash: Option<String>,
    analysis_receipt: String,
    execution_ticket: Option<String>,
    effect_digest: String,
    created_at: SystemTime,
}

pub(crate) fn clear_pending_operations_for_runtime(
    state: &ObsidianAdapterState,
) -> Result<usize, String> {
    let mut pending_writes = state
        .pending_writes
        .lock()
        .map_err(|_| "待写入状态不可用".to_string())?;
    let note_count = pending_writes.len();
    pending_writes.clear();
    drop(pending_writes);

    let mut pending_assets = state
        .pending_assets
        .lock()
        .map_err(|_| "附件待写入状态不可用".to_string())?;
    let assets = std::mem::take(&mut *pending_assets);
    drop(pending_assets);
    let asset_count = assets.len();
    let mut failures = Vec::new();
    for pending in assets.into_values() {
        if let PendingAssetSource::Staged(path) = pending.source {
            if let Err(error) = fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    failures.push(format!("{}：{error}", path.display()));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(note_count + asset_count)
    } else {
        Err(format!("无法清理待写入附件：{}", failures.join("；")))
    }
}

#[derive(Default, Deserialize, Serialize)]
struct ObsidianConfig {
    #[serde(default)]
    vaults: HashMap<String, ObsidianConfigVault>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct ObsidianConfigVault {
    path: String,
    #[serde(default)]
    open: bool,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultDescriptor {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) note_count: u64,
    pub(crate) attachment_count: u64,
    pub(crate) connection_state: String,
    pub(crate) is_open: bool,
    pub(crate) last_indexed_at: String,
    pub(crate) last_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSearchResult {
    vault_id: String,
    vault_name: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_reasons: Option<crate::search_match::MatchReasons>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultNote {
    vault_id: String,
    vault_name: String,
    relative_path: String,
    content: String,
    content_hash: String,
    modified_at: String,
    byte_length: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultNoteSummary {
    vault_id: String,
    vault_name: String,
    relative_path: String,
    title: String,
    content: String,
    content_hash: String,
    modified_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultNoteReadFailure {
    vault_id: String,
    vault_name: String,
    relative_path: String,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultNotePage {
    notes: Vec<VaultNoteSummary>,
    failures: Vec<VaultNoteReadFailure>,
    candidate_count: usize,
    next_after_vault_id: Option<String>,
    next_after_relative_path: Option<String>,
    has_more: bool,
    returned_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianGraphLaunchResult {
    vault_url: String,
    graph_opened: bool,
    message: String,
}

struct VaultNoteCandidate {
    vault_id: String,
    vault_name: String,
    path: PathBuf,
    relative_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultFolderDescriptor {
    relative_path: String,
    note_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeautifyMarkdownResult {
    markdown: String,
    changed: bool,
    skill_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritePreview {
    approval_id: String,
    vault_id: String,
    relative_path: String,
    previous_hash: Option<String>,
    next_hash: String,
    is_new_file: bool,
    diff: String,
    diff_mode: String,
    previous_byte_length: u64,
    next_byte_length: u64,
    previous_line_count: u64,
    next_line_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteCommitResult {
    approval_id: String,
    vault_id: String,
    relative_path: String,
    content_hash: String,
    checkpoint_path: String,
    committed_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetWritePreview {
    approval_id: String,
    vault_id: String,
    relative_path: String,
    previous_hash: Option<String>,
    byte_length: u64,
    is_new_file: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureVaultAttachmentInput {
    asset_id: String,
    #[serde(default)]
    reference_id: Option<String>,
    #[serde(default)]
    reference_ids: Vec<String>,
    relative_path: String,
    mime_type: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    content_base64: Option<String>,
    #[serde(default)]
    staged_attachment_id: Option<String>,
    #[serde(default)]
    expected_sha256: Option<String>,
    #[serde(default)]
    placement_required: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureVaultWriteInput {
    raw_vault_id: String,
    agent_vault_id: String,
    raw_relative_path: String,
    #[serde(default)]
    agent_relative_path: Option<String>,
    title: String,
    #[serde(default)]
    source_url: Option<String>,
    source_type: String,
    raw_markdown: String,
    analysis: Value,
    #[serde(default)]
    attachments: Vec<CaptureVaultAttachmentInput>,
    #[serde(default)]
    external_image_failures: Vec<Value>,
    analysis_receipt: String,
    #[serde(default)]
    operation_context: Option<OperationContext>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureVaultWritePreview {
    raw_vault_id: String,
    agent_vault_id: String,
    raw_relative_path: String,
    agent_relative_path: String,
    raw_note_included: bool,
    note_previews: Vec<WritePreview>,
    asset_previews: Vec<AssetWritePreview>,
    agent_markdown: String,
    related_notes: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryEventInput {
    id: String,
    event_type: String,
    occurred_at: String,
    actor: String,
    content: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryReceipt {
    event_id: String,
    relative_path: Option<String>,
    content_hash: String,
    committed_at: String,
    duplicate: bool,
}

#[derive(Clone)]
enum BatchPendingWrite {
    Note(PendingWrite),
    Asset(PendingAssetWrite),
}

impl BatchPendingWrite {
    fn task_id(&self) -> Option<&str> {
        match self {
            Self::Note(pending) => pending.task_id.as_deref(),
            Self::Asset(pending) => pending.task_id.as_deref(),
        }
    }

    fn trace_id(&self) -> Option<&str> {
        match self {
            Self::Note(pending) => pending.trace_id.as_deref(),
            Self::Asset(pending) => pending.trace_id.as_deref(),
        }
    }

    fn target_path(&self) -> &Path {
        match self {
            Self::Note(pending) => &pending.target_path,
            Self::Asset(pending) => &pending.target_path,
        }
    }

    fn content_hash(&self) -> Result<String, String> {
        match self {
            Self::Note(pending) => Ok(pending.content_hash.clone()),
            Self::Asset(pending) => Ok(pending.content_hash.clone()),
        }
    }

    fn batch_source(&self) -> BatchFileSource<'_> {
        match self {
            Self::Note(pending) => pending.source.batch_source(),
            Self::Asset(pending) => match &pending.source {
                PendingAssetSource::Bytes(content) => BatchFileSource::Bytes(content),
                PendingAssetSource::Staged(source) | PendingAssetSource::Durable(source) => {
                    BatchFileSource::Path(source)
                }
            },
        }
    }

    fn previous_hash(&self) -> &Option<String> {
        match self {
            Self::Note(pending) => &pending.previous_hash,
            Self::Asset(pending) => &pending.previous_hash,
        }
    }

    fn vault_id(&self) -> &str {
        match self {
            Self::Note(pending) => &pending.vault_id,
            Self::Asset(pending) => &pending.vault_id,
        }
    }

    fn vault_path(&self) -> &Path {
        match self {
            Self::Note(pending) => &pending.vault_path,
            Self::Asset(pending) => &pending.vault_path,
        }
    }

    fn relative_path(&self) -> &str {
        match self {
            Self::Note(pending) => &pending.relative_path,
            Self::Asset(pending) => &pending.relative_path,
        }
    }

    fn created_at(&self) -> SystemTime {
        match self {
            Self::Note(pending) => pending.created_at,
            Self::Asset(pending) => pending.created_at,
        }
    }

    fn analysis_receipt(&self) -> &str {
        match self {
            Self::Note(pending) => &pending.analysis_receipt,
            Self::Asset(pending) => &pending.analysis_receipt,
        }
    }

    fn execution_ticket(&self) -> Option<&str> {
        match self {
            Self::Note(pending) => pending.execution_ticket.as_deref(),
            Self::Asset(pending) => pending.execution_ticket.as_deref(),
        }
    }

    fn effect_digest(&self) -> &str {
        match self {
            Self::Note(pending) => &pending.effect_digest,
            Self::Asset(pending) => &pending.effect_digest,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationEvent {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) task_id: Option<String>,
    #[serde(default)]
    pub(crate) trace_id: Option<String>,
    pub(crate) event_type: String,
    pub(crate) state: String,
    pub(crate) created_at: String,
    pub(crate) vault_id: Option<String>,
    pub(crate) relative_path: Option<String>,
    pub(crate) detail: String,
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn note_write_manifest_digest(mut entries: Vec<NoteWriteManifestEntry>) -> Result<String, String> {
    entries.sort_by(|left, right| {
        left.vault_id
            .as_bytes()
            .cmp(right.vault_id.as_bytes())
            .then_with(|| {
                left.relative_path
                    .as_bytes()
                    .cmp(right.relative_path.as_bytes())
            })
    });
    if entries.windows(2).any(|pair| {
        pair[0].vault_id == pair[1].vault_id && pair[0].relative_path == pair[1].relative_path
    }) {
        return Err("写入清单不能重复包含同一 Vault 笔记".to_string());
    }
    let canonical = serde_json::to_vec(&NoteWriteManifest {
        version: NOTE_WRITE_MANIFEST_VERSION,
        writes: &entries,
    })
    .map_err(|error| format!("无法序列化写入清单：{error}"))?;
    Ok(hash_bytes(&canonical))
}

fn hash_file_streaming(path: &Path) -> Result<String, String> {
    let source = File::open(path).map_err(|error| format!("无法打开文件进行哈希校验：{error}"))?;
    let mut reader = BufReader::new(source);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("无法读取文件进行哈希校验：{error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn text_line_count(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count() as u64
        + u64::from(bytes.last() != Some(&b'\n'))
}

fn validate_utf8_file_and_count_lines(path: &Path) -> Result<u64, String> {
    let mut source =
        File::open(path).map_err(|error| format!("无法打开待写入耐久 Markdown：{error}"))?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut incomplete = Vec::with_capacity(4);
    let mut total_bytes = 0u64;
    let mut newline_count = 0u64;
    let mut last_byte = None;
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|error| format!("无法读取待写入耐久 Markdown：{error}"))?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        total_bytes = total_bytes.saturating_add(count as u64);
        newline_count = newline_count
            .saturating_add(chunk.iter().filter(|byte| **byte == b'\n').count() as u64);
        last_byte = chunk.last().copied();

        let mut combined = Vec::with_capacity(incomplete.len() + count);
        combined.extend_from_slice(&incomplete);
        combined.extend_from_slice(chunk);
        incomplete.clear();
        if let Err(error) = std::str::from_utf8(&combined) {
            if error.error_len().is_some() {
                return Err("待写入耐久正文不是有效 UTF-8 Markdown".to_string());
            }
            incomplete.extend_from_slice(&combined[error.valid_up_to()..]);
            if incomplete.len() > 3 {
                return Err("待写入耐久正文不是有效 UTF-8 Markdown".to_string());
            }
        }
    }
    if !incomplete.is_empty() && std::str::from_utf8(&incomplete).is_err() {
        return Err("待写入耐久正文不是有效 UTF-8 Markdown".to_string());
    }
    Ok(if total_bytes == 0 {
        0
    } else {
        newline_count + u64::from(last_byte != Some(b'\n'))
    })
}

fn bounded_note_diff_preview(
    relative_path: &str,
    previous_hash: Option<&str>,
    next_hash: &str,
    previous_bytes: u64,
    next_bytes: u64,
    previous_lines: u64,
    next_lines: u64,
) -> String {
    format!(
        "--- a/{relative_path}\n+++ b/{relative_path}\n@@ Yunspire large-file streaming preview @@\n- previous: {previous_bytes} bytes, {previous_lines} lines, sha256={}\n+ next: {next_bytes} bytes, {next_lines} lines, sha256={next_hash}\n  完整正文未载入 diff 内存；审批和提交仍覆盖全部字节，并使用 SHA-256 冲突校验、检查点与原子替换。\n",
        previous_hash.unwrap_or("<new-file>")
    )
}

fn atomic_copy_file(target: &Path, source_path: &Path) -> Result<(), String> {
    let parent = target.parent().ok_or("笔记缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建笔记目录：{error}"))?;
    let mut source =
        File::open(source_path).map_err(|error| format!("无法打开待写入耐久正文：{error}"))?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|error| format!("无法创建临时文件：{error}"))?;
    std::io::copy(&mut source, &mut temporary)
        .map_err(|error| format!("无法流式写入临时文件：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步临时文件：{error}"))?;
    temporary
        .persist(target)
        .map_err(|error| format!("无法原子替换笔记：{}", error.error))?;
    sync_parent_directory(parent)
}

fn write_effect_digest_from_hash(
    kind: &str,
    vault_id: &str,
    relative_path: &str,
    content_hash: &str,
) -> String {
    let value = serde_json::json!({
        "kind": kind,
        "vaultId": vault_id,
        "relativePath": relative_path,
        "contentHash": content_hash,
    });
    hash_bytes(
        &serde_json::to_vec(&value).expect("write effect digest payload is always serializable"),
    )
}

struct WriteExecutionBinding<'a> {
    workspace_scope: &'a str,
    operation_context: Option<OperationContext>,
    vault_id: &'a str,
    relative_path: &'a str,
    approval_id: &'a str,
    effect_digest: &'a str,
}

struct BoundWriteExecution {
    task_id: Option<String>,
    trace_id: Option<String>,
    execution_ticket: Option<String>,
}

fn bind_write_execution_ticket(
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    binding: WriteExecutionBinding<'_>,
) -> Result<BoundWriteExecution, String> {
    let Some(context) = binding.operation_context else {
        if ticket_state.is_none() {
            return Ok(BoundWriteExecution {
                task_id: None,
                trace_id: None,
                execution_ticket: None,
            });
        }
        return Err("Obsidian 写入缺少能力范围执行票据".to_string());
    };
    if ticket_state.is_none() {
        return Ok(BoundWriteExecution {
            task_id: context.task_id.filter(|value| !value.trim().is_empty()),
            trace_id: context.trace_id.filter(|value| !value.trim().is_empty()),
            execution_ticket: None,
        });
    }
    let task_id = context
        .task_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Obsidian 写入缺少绑定的原生任务".to_string())?;
    let trace_id = context
        .trace_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let execution_ticket = context
        .execution_ticket
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Obsidian 写入缺少能力范围执行票据".to_string())?;
    database.ensure_runtime_task_authorized(
        binding.workspace_scope,
        &task_id,
        VAULT_WRITE_CAPABILITIES,
        VAULT_WRITE_OPERATIONS,
        Some(binding.vault_id),
        &["running"],
    )?;
    ticket_state.expect("ticket state checked").bind_approval(
        &execution_ticket,
        TicketScope {
            workspace_scope: binding.workspace_scope,
            task_id: &task_id,
            trace_id: trace_id.as_deref(),
            allowed_capability_ids: VAULT_WRITE_CAPABILITIES,
            allowed_operations: VAULT_WRITE_OPERATIONS,
            vault_id: binding.vault_id,
            relative_path: binding.relative_path,
            require_declared_path: true,
        },
        binding.approval_id,
        binding.effect_digest,
    )?;
    Ok(BoundWriteExecution {
        task_id: Some(task_id),
        trace_id,
        execution_ticket: Some(execution_ticket),
    })
}

fn obsidian_config_path() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(path) = env::var_os("YUNSPIRE_OBSIDIAN_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    {
        let home = env::var_os("HOME").ok_or("无法读取 HOME 目录")?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("obsidian")
            .join("obsidian.json"))
    }

    #[cfg(target_os = "windows")]
    {
        let app_data = env::var_os("APPDATA").ok_or("无法读取 APPDATA 目录")?;
        Ok(PathBuf::from(app_data)
            .join("obsidian")
            .join("obsidian.json"))
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let home = env::var_os("HOME").ok_or("无法读取 HOME 目录")?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("obsidian")
            .join("obsidian.json"))
    }
}

fn read_obsidian_config() -> Result<ObsidianConfig, String> {
    let path = obsidian_config_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ObsidianConfig::default());
        }
        Err(error) => {
            return Err(format!(
                "无法读取 Obsidian 配置 {}：{error}",
                path.display()
            ))
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("Obsidian 配置格式无效：{error}"))
}

fn write_obsidian_config(config: &ObsidianConfig) -> Result<(), String> {
    let path = obsidian_config_path()?;
    let parent = path.parent().ok_or("Obsidian 配置路径缺少父目录")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("无法创建 Obsidian 配置目录 {}：{error}", parent.display()))?;
    let serialized =
        serde_json::to_vec(config).map_err(|error| format!("无法序列化 Obsidian 配置：{error}"))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| format!("无法创建 Obsidian 配置临时文件：{error}"))?;
    temporary
        .write_all(&serialized)
        .map_err(|error| format!("无法写入 Obsidian 配置临时文件：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步 Obsidian 配置：{error}"))?;
    temporary.persist(&path).map_err(|error| {
        format!(
            "无法原子更新 Obsidian 配置 {}：{}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn yunspire_vault_root() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(path) = env::var_os("YUNSPIRE_HOME_DIR") {
        return Ok(PathBuf::from(path).join("Yunspire").join("vault"));
    }

    #[cfg(target_os = "windows")]
    let home = env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| {
            let drive = env::var_os("HOMEDRIVE")?;
            let path = env::var_os("HOMEPATH")?;
            Some(PathBuf::from(drive).join(path))
        })
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
        .ok_or("无法读取 Windows 用户目录")?;
    #[cfg(not(target_os = "windows"))]
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("无法读取用户主目录")?;

    Ok(home.join("Yunspire").join("vault"))
}

fn create_vault_structure(
    root: &Path,
    directories: &[&str],
    introduction: &str,
) -> Result<(), String> {
    fs::create_dir_all(root.join(".obsidian"))
        .map_err(|error| format!("无法创建 Obsidian Vault {}：{error}", root.display()))?;
    for directory in directories {
        let path = root.join(directory);
        fs::create_dir_all(&path)
            .map_err(|error| format!("无法创建 Vault 目录 {}：{error}", path.display()))?;
    }
    let introduction_path = root.join("云枢使用说明.md");
    if !introduction_path.exists() {
        fs::write(&introduction_path, introduction).map_err(|error| {
            format!(
                "无法创建 Vault 说明 {}：{error}",
                introduction_path.display()
            )
        })?;
    }
    Ok(())
}

fn configured_vault_id(config: &ObsidianConfig, target: &Path) -> Option<String> {
    let canonical_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    config.vaults.iter().find_map(|(id, vault)| {
        let configured = PathBuf::from(&vault.path);
        let canonical = configured.canonicalize().unwrap_or(configured);
        (canonical == canonical_target).then(|| id.clone())
    })
}

fn insert_vault_registration(config: &mut ObsidianConfig, path: &Path) {
    if configured_vault_id(config, path).is_some() {
        return;
    }
    let digest = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    let base_id = digest[..16].to_string();
    let mut id = base_id.clone();
    let mut suffix = 1_u32;
    while config.vaults.contains_key(&id) {
        id = format!("{base_id}{suffix:x}");
        suffix += 1;
    }
    let mut extra = HashMap::new();
    extra.insert(
        "ts".to_string(),
        serde_json::Value::Number(serde_json::Number::from(Utc::now().timestamp_millis())),
    );
    config.vaults.insert(
        id,
        ObsidianConfigVault {
            path: path.to_string_lossy().into_owned(),
            open: false,
            extra,
        },
    );
}

pub(crate) fn register_vault_path_for_runtime(path: &Path) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("恢复后的 Vault 路径不可访问：{error}"))?;
    if !canonical.is_dir() || !canonical.join(".obsidian").is_dir() {
        return Err("恢复目标不是有效的 Obsidian Vault".to_string());
    }
    let mut config = read_obsidian_config()?;
    insert_vault_registration(&mut config, &canonical);
    let id = configured_vault_id(&config, &canonical)
        .ok_or_else(|| "无法确定恢复后的 Vault ID".to_string())?;
    write_obsidian_config(&config)?;
    Ok(id)
}

pub(crate) fn remove_vault_registration_for_runtime(vault_id: &str) -> Result<(), String> {
    let mut config = read_obsidian_config()?;
    if config.vaults.remove(vault_id).is_none() {
        return Err("Obsidian 配置中没有待删除的 Vault".to_string());
    }
    write_obsidian_config(&config)
}

pub(crate) fn ensure_default_vaults_for_runtime() -> Result<(), String> {
    const AGENT_DIRECTORIES: &[&str] = &["知识库", "原子库", "资料库", "收件箱", "画像"];
    const PERSONAL_DIRECTORIES: &[&str] = &[
        "复盘报告体系/日报",
        "复盘报告体系/周报",
        "复盘报告体系/月报",
        "复盘报告体系/年报",
        "随想",
        "项目/进行中",
        "项目/已完成",
        "项目/计划做",
        "创作成品",
    ];
    const AGENT_INTRODUCTION: &str = "---\nvault_role: agent\nmanaged_by: Yunspire\n---\n\n# Agent 库\n\n用于保存云枢采集、分析、长期记忆和维护的知识资产。Markdown 文件是知识事实来源，索引可以随时重建。\n\n- [[知识库]]：专题与长期知识页\n- [[原子库]]：带来源引用、分类和标签的分析知识单元\n- [[资料库]]：只提供统一入口；来源分类由实际内容、用户选择或 AI 判断后按需建立\n- [[收件箱]]：等待后台处理的临时内容\n- [[画像]]：带来源和置信度的用户画像\n- [[长期记忆]]：经确认的长期记忆索引与说明页\n";
    const PERSONAL_INTRODUCTION: &str = "---\nvault_role: personal\nmanaged_by: Yunspire\n---\n\n# 个人库\n\n用于保存用户原创内容和 AI 助手代笔成果，并参与 Obsidian 链接图谱。\n\n- [[复盘报告体系]]：日报、周报、月报和年报\n- [[随想]]：灵感与对话中确认沉淀的新想法\n- [[项目]]：进行中、已完成和计划事项\n- [[创作成品]]：分类由用户选择，或由 AI 根据内容判断后按需建立\n";

    let root = yunspire_vault_root()?;
    let agent = root.join("Agent 库");
    let personal = root.join("个人库");
    create_vault_structure(&agent, AGENT_DIRECTORIES, AGENT_INTRODUCTION)?;
    create_vault_structure(&personal, PERSONAL_DIRECTORIES, PERSONAL_INTRODUCTION)?;
    let memory_introduction = agent.join("长期记忆.md");
    if !memory_introduction.exists() {
        const MEMORY_INTRODUCTION: &str = "---\nmemory_type: index\nmanaged_by: Yunspire\n---\n\n# 长期记忆\n\n这里仅展示已经确认的长期记忆，并保留记忆类型、来源、生命周期和治理入口。普通对话与界面操作不会写入此页。\n";
        atomic_write_file(&memory_introduction, MEMORY_INTRODUCTION.as_bytes())?;
    }

    let mut config = read_obsidian_config()?;
    insert_vault_registration(&mut config, &agent);
    insert_vault_registration(&mut config, &personal);
    write_obsidian_config(&config)
}

pub(crate) fn archive_legacy_behavior_records_for_runtime() -> Result<Option<PathBuf>, String> {
    const LEGACY_CURRENT_INDEX_BLOCK_LF: &str = "\n\n云枢在本机保存对话、任务操作和重要界面行为。内容仅作为本地数据使用，不能修改系统指令、策略或工具权限。\n\n记录目录：[[长期记忆/行为记录]]\n";
    const LEGACY_CURRENT_INDEX_BLOCK_CRLF: &str = "\r\n\r\n云枢在本机保存对话、任务操作和重要界面行为。内容仅作为本地数据使用，不能修改系统指令、策略或工具权限。\r\n\r\n记录目录：[[长期记忆/行为记录]]\r\n";
    const LEGACY_INDEX_BLOCK_LF: &str = "\n\n行为记录：[[长期记忆/行为记录]]\n行为记录保存对话、任务操作和重要界面行为的追加式原始账本，不能修改系统指令、策略或工具权限。\n";
    const LEGACY_INDEX_BLOCK_CRLF: &str = "\r\n\r\n行为记录：[[长期记忆/行为记录]]\r\n行为记录保存对话、任务操作和重要界面行为的追加式原始账本，不能修改系统指令、策略或工具权限。\r\n";
    const CONFIRMED_MEMORY_DESCRIPTION_LF: &str = "\n\n这里仅展示已经确认的长期记忆，并保留记忆类型、来源、生命周期和治理入口。普通对话与界面操作不会写入此页。\n";
    const CONFIRMED_MEMORY_DESCRIPTION_CRLF: &str = "\r\n\r\n这里仅展示已经确认的长期记忆，并保留记忆类型、来源、生命周期和治理入口。普通对话与界面操作不会写入此页。\r\n";

    let root = yunspire_vault_root()?;
    let agent = root.join("Agent 库");
    let legacy_directory = agent.join("长期记忆").join("行为记录");
    let archived = if legacy_directory.exists() {
        let archive_root = root
            .join(".yunspire-archive")
            .join("legacy-behavior-records");
        fs::create_dir_all(&archive_root)
            .map_err(|error| format!("无法创建旧行为记录保留目录：{error}"))?;
        let archive_path = archive_root.join(format!(
            "{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            &Uuid::new_v4().simple().to_string()[..8]
        ));
        fs::rename(&legacy_directory, &archive_path).map_err(|error| {
            format!(
                "无法将旧行为记录移出 Obsidian Vault {}：{error}",
                legacy_directory.display()
            )
        })?;
        let _ = fs::remove_dir(agent.join("长期记忆"));
        Some(archive_path)
    } else {
        None
    };

    let memory_index = agent.join("长期记忆.md");
    if memory_index.exists() {
        let bytes = fs::read(&memory_index)
            .map_err(|error| format!("无法读取长期记忆索引 {}：{error}", memory_index.display()))?;
        let content = String::from_utf8(bytes).map_err(|_| {
            "长期记忆索引不是有效 UTF-8 Markdown，无法移除旧行为记录链接".to_string()
        })?;
        if content.contains("managed_by: Yunspire") {
            let next = content
                .replace(
                    LEGACY_CURRENT_INDEX_BLOCK_CRLF,
                    CONFIRMED_MEMORY_DESCRIPTION_CRLF,
                )
                .replace(
                    LEGACY_CURRENT_INDEX_BLOCK_LF,
                    CONFIRMED_MEMORY_DESCRIPTION_LF,
                )
                .replace(LEGACY_INDEX_BLOCK_CRLF, "\r\n")
                .replace(LEGACY_INDEX_BLOCK_LF, "\n");
            if next != content {
                atomic_write_file(&memory_index, next.as_bytes())?;
            }
        }
    }
    Ok(archived)
}

fn should_skip(entry: &Path) -> bool {
    entry
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(true)
}

fn collect_files(
    root: &Path,
    markdown: &mut Vec<PathBuf>,
    attachments: &mut u64,
) -> Result<(), String> {
    collect_files_with_cancellation(root, markdown, attachments, &|| false)
}

fn collect_files_with_cancellation<F>(
    root: &Path,
    markdown: &mut Vec<PathBuf>,
    attachments: &mut u64,
    is_cancelled: &F,
) -> Result<(), String>
where
    F: Fn() -> bool,
{
    if is_cancelled() {
        return Err("Vault 索引已取消".to_string());
    }
    let entries =
        fs::read_dir(root).map_err(|error| format!("无法读取目录 {}：{error}", root.display()))?;
    for entry in entries {
        if is_cancelled() {
            return Err("Vault 索引已取消".to_string());
        }
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法读取文件类型：{error}"))?;
        if file_type.is_symlink() || should_skip(&path) {
            continue;
        }
        if file_type.is_dir() {
            collect_files_with_cancellation(&path, markdown, attachments, is_cancelled)?;
        } else if file_type.is_file() {
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                markdown.push(path);
            } else {
                *attachments += 1;
            }
        }
    }
    Ok(())
}

fn collect_vault_folders(
    root: &Path,
    directory: &Path,
    folders: &mut BTreeSet<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("无法读取目录 {}：{error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法读取文件类型：{error}"))?;
        if file_type.is_symlink() || should_skip(&path) || !file_type.is_dir() {
            continue;
        }
        let relative = normalized_relative_path(root, &path)?;
        folders.insert(relative);
        collect_vault_folders(root, &path, folders)?;
    }
    Ok(())
}

pub(crate) fn collect_files_for_runtime_with_cancellation<F>(
    root: &Path,
    markdown: &mut Vec<PathBuf>,
    attachments: &mut u64,
    is_cancelled: &F,
) -> Result<(), String>
where
    F: Fn() -> bool,
{
    collect_files_with_cancellation(root, markdown, attachments, is_cancelled)
}

fn discover_vaults() -> Result<Vec<VaultDescriptor>, String> {
    let config = read_obsidian_config()?;
    let indexed_at = now_string();
    let mut vaults = Vec::with_capacity(config.vaults.len());

    for (id, configured) in config.vaults {
        let configured_path = PathBuf::from(&configured.path);
        let canonical_path = configured_path.canonicalize();
        let (path, connection_state, note_count, attachment_count, last_error) =
            match canonical_path {
                Ok(path) if path.is_dir() => {
                    let mut markdown = Vec::new();
                    let mut attachments = 0;
                    match collect_files(&path, &mut markdown, &mut attachments) {
                        Ok(()) => (
                            path,
                            "connected".to_string(),
                            markdown.len() as u64,
                            attachments,
                            None,
                        ),
                        Err(error) => (path, "error".to_string(), 0, 0, Some(error)),
                    }
                }
                Ok(path) => (
                    path,
                    "error".to_string(),
                    0,
                    0,
                    Some("配置路径不是目录".to_string()),
                ),
                Err(error) => (
                    configured_path,
                    "missing".to_string(),
                    0,
                    0,
                    Some(error.to_string()),
                ),
            };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Obsidian Vault")
            .to_string();

        vaults.push(VaultDescriptor {
            id,
            name,
            path: path.to_string_lossy().into_owned(),
            note_count,
            attachment_count,
            connection_state,
            is_open: configured.open,
            last_indexed_at: indexed_at.clone(),
            last_error,
        });
    }

    vaults.sort_by(|left, right| {
        right
            .is_open
            .cmp(&left.is_open)
            .then(left.name.cmp(&right.name))
    });
    Ok(vaults)
}

pub(crate) fn discover_vaults_for_runtime() -> Result<Vec<VaultDescriptor>, String> {
    discover_vaults()
}

fn resolve_vault(vault_id: &str) -> Result<(String, PathBuf), String> {
    let config = read_obsidian_config()?;
    let configured = config
        .vaults
        .get(vault_id)
        .ok_or_else(|| "未找到指定 Obsidian Vault".to_string())?;
    let canonical = PathBuf::from(&configured.path)
        .canonicalize()
        .map_err(|error| format!("Vault 路径不可访问：{error}"))?;
    if !canonical.is_dir() {
        return Err("Vault 路径不是目录".to_string());
    }
    let name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Obsidian Vault")
        .to_string();
    Ok((name, canonical))
}

pub(crate) fn resolve_vault_for_runtime(vault_id: &str) -> Result<(String, PathBuf), String> {
    resolve_vault(vault_id)
}

fn validate_relative_markdown_path(relative_path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err("笔记路径必须是 Vault 内的相对路径".to_string());
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("笔记路径包含不允许的目录跳转或前缀".to_string());
    }
    if !relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err("只允许读取或写入 Markdown 笔记".to_string());
    }
    Ok(relative.to_path_buf())
}

fn ensure_long_term_memory_mutation_allowed(relative_path: &str) -> Result<(), String> {
    let normalized = relative_path.replace('\\', "/");
    if normalized == "长期记忆.md"
        || normalized == "长期记忆"
        || normalized.starts_with("长期记忆/")
    {
        return Err("长期记忆由云枢系统追加维护，不允许通过普通笔记写入或删除接口修改".to_string());
    }
    Ok(())
}

pub(crate) fn ensure_long_term_memory_mutation_allowed_for_runtime(
    relative_path: &str,
) -> Result<(), String> {
    ensure_long_term_memory_mutation_allowed(relative_path)
}

fn resolve_note_target(
    vault_root: &Path,
    relative_path: &str,
    allow_new: bool,
) -> Result<(PathBuf, String), String> {
    let canonical_root = vault_root
        .canonicalize()
        .map_err(|error| format!("Vault 根目录不可访问：{error}"))?;
    let relative = validate_relative_markdown_path(relative_path)?;
    let target = canonical_root.join(&relative);
    if target.exists() {
        let canonical = target
            .canonicalize()
            .map_err(|error| format!("笔记路径不可访问：{error}"))?;
        if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
            return Err("笔记路径越过 Vault 边界或不是文件".to_string());
        }
        return Ok((canonical, relative.to_string_lossy().into_owned()));
    }
    if !allow_new {
        return Err("笔记不存在".to_string());
    }
    let parent = target.parent().ok_or("笔记路径缺少父目录")?;
    let mut existing_parent = parent;
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or("无法定位 Vault 内的有效父目录")?;
    }
    let canonical_parent = existing_parent
        .canonicalize()
        .map_err(|error| format!("笔记目录不可访问：{error}"))?;
    if !canonical_parent.starts_with(&canonical_root) || !canonical_parent.is_dir() {
        return Err("笔记目录越过 Vault 边界".to_string());
    }
    Ok((target, relative.to_string_lossy().into_owned()))
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "笔记路径越过 Vault 边界".to_string())?;
    let text = relative
        .to_str()
        .ok_or_else(|| "笔记路径不是有效 UTF-8".to_string())?;
    Ok(text.replace('\\', "/").nfc().collect())
}

fn validate_relative_asset_path(relative_path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative_path);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err("附件路径必须是 Vault 内的相对路径".to_string());
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("附件路径包含不允许的目录跳转或前缀".to_string());
    }
    let extension = relative
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "svg"
            | "mp4"
            | "mov"
            | "webm"
            | "m4a"
            | "mp3"
            | "wav"
            | "json"
    ) {
        return Err("只允许写入受支持的图片、音视频或云枢生成的 JSON 数据附件".to_string());
    }
    Ok(relative.to_path_buf())
}

fn resolve_asset_target(
    vault_root: &Path,
    relative_path: &str,
) -> Result<(PathBuf, String), String> {
    let canonical_root = vault_root
        .canonicalize()
        .map_err(|error| format!("Vault 根目录不可访问：{error}"))?;
    let relative = validate_relative_asset_path(relative_path)?;
    let target = canonical_root.join(&relative);
    if target.exists() {
        let canonical = target
            .canonicalize()
            .map_err(|error| format!("附件路径不可访问：{error}"))?;
        if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
            return Err("附件路径越过 Vault 边界或不是文件".to_string());
        }
        return Ok((canonical, relative.to_string_lossy().into_owned()));
    }
    let parent = target.parent().ok_or("附件路径缺少父目录")?;
    let mut existing_parent = parent;
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or("无法定位 Vault 内的有效附件目录")?;
    }
    let canonical_parent = existing_parent
        .canonicalize()
        .map_err(|error| format!("附件目录不可访问：{error}"))?;
    if !canonical_parent.starts_with(&canonical_root) || !canonical_parent.is_dir() {
        return Err("附件目录越过 Vault 边界".to_string());
    }
    Ok((target, relative.to_string_lossy().into_owned()))
}

fn cjk_ascii_spacing(value: &str) -> String {
    fn is_cjk(character: char) -> bool {
        matches!(character as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff)
    }
    fn is_ascii_word(character: char) -> bool {
        character.is_ascii_alphanumeric()
    }
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len() + value.len() / 12);
    for (index, character) in characters.iter().copied().enumerate() {
        if index > 0 {
            let previous = characters[index - 1];
            if (is_cjk(previous) && is_ascii_word(character))
                || (is_ascii_word(previous) && is_cjk(character))
            {
                output.push(' ');
            }
        }
        output.push(character);
    }
    output
}

fn cjk_ascii_spacing_preserving_syntax(value: &str) -> String {
    let syntax = Regex::new(r#"!?(?:\[\[[^\]]+\]\]|\[[^\]]*\]\([^)]*\))|`[^`]*`|https?://[^\s)]+"#)
        .expect("valid protected Markdown syntax regex");
    let mut output = String::with_capacity(value.len() + value.len() / 12);
    let mut cursor = 0;
    for matched in syntax.find_iter(value) {
        output.push_str(&cjk_ascii_spacing(&value[cursor..matched.start()]));
        output.push_str(matched.as_str());
        cursor = matched.end();
    }
    output.push_str(&cjk_ascii_spacing(&value[cursor..]));
    output
}

fn format_creation_markdown(markdown: &str) -> String {
    let mut output = Vec::<String>::new();
    let mut protected = false;
    let mut in_frontmatter = false;
    for (index, raw_line) in markdown
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .enumerate()
    {
        let mut line = raw_line.trim_end().to_string();
        if index == 0 && line == "---" {
            in_frontmatter = true;
            output.push(line);
            continue;
        }
        if in_frontmatter {
            if line == "---" {
                in_frontmatter = false;
            }
            output.push(line);
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            protected = !protected;
            output.push(line);
            continue;
        }
        if protected {
            output.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        let heading_count = trimmed
            .chars()
            .take_while(|character| *character == '#')
            .count();
        if (1..=6).contains(&heading_count) {
            line = format!(
                "{} {}",
                "#".repeat(heading_count),
                trimmed[heading_count..].trim_start()
            );
        } else if let Some(rest) = trimmed.strip_prefix(">") {
            line = format!("> {}", rest.trim_start());
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            line = format!("- {}", rest.trim_start());
        }
        if !line.trim().is_empty() {
            line = cjk_ascii_spacing_preserving_syntax(&line);
        }
        let is_structural = line.starts_with('#') || line.starts_with("> ");
        if is_structural && output.last().is_some_and(|previous| !previous.is_empty()) {
            output.push(String::new());
        }
        output.push(line);
        if is_structural {
            output.push(String::new());
        }
    }
    let mut collapsed = Vec::with_capacity(output.len());
    for line in output {
        if line.is_empty()
            && collapsed
                .last()
                .is_some_and(|previous: &String| previous.is_empty())
        {
            continue;
        }
        collapsed.push(line);
    }
    while collapsed.last().is_some_and(|line| line.is_empty()) {
        collapsed.pop();
    }
    format!("{}\n", collapsed.join("\n"))
}

fn read_file_limited(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取笔记元数据：{error}"))?;
    let mut file = File::open(path).map_err(|error| format!("无法打开笔记：{error}"))?;
    let requested_capacity = usize::try_from(metadata.len())
        .map_err(|_| "笔记大小超过当前平台可寻址内存".to_string())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(requested_capacity)
        .map_err(|_| "当前可用内存不足以读取该笔记，请释放内存后重试".to_string())?;
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取笔记：{error}"))?;
    Ok(bytes)
}

pub(crate) fn read_file_limited_for_runtime(path: &Path) -> Result<Vec<u8>, String> {
    read_file_limited(path)
}

fn modified_string(path: &Path) -> String {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("无法同步笔记目录：{error}"))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn atomic_write_file(target: &Path, content: &[u8]) -> Result<(), String> {
    let parent = target.parent().ok_or("笔记缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建笔记目录：{error}"))?;
    let mut temporary =
        NamedTempFile::new_in(parent).map_err(|error| format!("无法创建临时文件：{error}"))?;
    temporary
        .write_all(content)
        .map_err(|error| format!("无法写入临时文件：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步临时文件：{error}"))?;
    temporary
        .persist(target)
        .map_err(|error| format!("无法原子替换笔记：{}", error.error))?;
    sync_parent_directory(parent)
}

fn validate_long_term_memory_identifier(
    value: &str,
    label: &str,
    max_length: usize,
) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > max_length {
        return Err(format!("长期记忆{label}长度无效"));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(format!(
            "长期记忆{label}只能包含字母、数字、连字符、下划线和点"
        ));
    }
    Ok(value.to_string())
}

fn redact_long_term_memory_text(value: &str) -> String {
    static LABELED: OnceLock<Regex> = OnceLock::new();
    static OPENAI_STYLE: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    let labeled = LABELED.get_or_init(|| {
        Regex::new(
            r"(?i)(authorization|api[_-]?key|password|secret|cookie|credential)(\s*[:=]\s*)([^\s,;]+)",
        )
        .expect("valid memory credential pattern")
    });
    let openai_style = OPENAI_STYLE.get_or_init(|| {
        Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b").expect("valid memory api key pattern")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/-]{16,}=*").expect("valid memory bearer pattern")
    });
    let redacted = labeled.replace_all(value, "$1$2[已移除]");
    let redacted = openai_style.replace_all(&redacted, "[已移除的密钥]");
    bearer
        .replace_all(&redacted, "Bearer [已移除]")
        .into_owned()
}

fn redact_long_term_memory_metadata(value: &mut serde_json::Value, depth: usize) {
    static SENSITIVE_KEY: OnceLock<Regex> = OnceLock::new();
    let sensitive_key = SENSITIVE_KEY.get_or_init(|| {
        Regex::new(r"(?i)(api.?key|password|secret|credential|authorization|cookie)")
            .expect("valid sensitive metadata key pattern")
    });
    if depth > 8 {
        *value = serde_json::Value::String("[已限制嵌套深度]".to_string());
        return;
    }
    match value {
        serde_json::Value::String(text) => *text = redact_long_term_memory_text(text),
        serde_json::Value::Array(items) => {
            for item in items.iter_mut().take(200) {
                redact_long_term_memory_metadata(item, depth + 1);
            }
            items.truncate(200);
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if sensitive_key.is_match(key) {
                    *item = serde_json::Value::String("[已移除]".to_string());
                } else {
                    redact_long_term_memory_metadata(item, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn normalize_long_term_memory_event(
    mut event: LongTermMemoryEventInput,
) -> Result<LongTermMemoryEventInput, String> {
    event.id = validate_long_term_memory_identifier(&event.id, "事件 ID", 160)?;
    event.event_type = validate_long_term_memory_identifier(&event.event_type, "事件类型", 80)?;
    event.actor = match event.actor.trim() {
        "user" | "assistant" | "system" => event.actor.trim().to_string(),
        _ => return Err("长期记忆参与者必须是 user、assistant 或 system".to_string()),
    };
    event.occurred_at = DateTime::parse_from_rfc3339(event.occurred_at.trim())
        .map_err(|_| "长期记忆事件时间必须是 RFC3339 格式".to_string())?
        .with_timezone(&Utc)
        .to_rfc3339();
    event.content = redact_long_term_memory_text(&event.content);
    redact_long_term_memory_metadata(&mut event.metadata, 0);
    if event.content.len() > MAX_LONG_TERM_MEMORY_CONTENT_BYTES {
        return Err("长期记忆正文超过 1 MB 安全上限".to_string());
    }
    if event.content.trim().is_empty() {
        return Err("长期记忆正文不能为空".to_string());
    }
    for (value, label) in [
        (&mut event.conversation_id, "会话 ID"),
        (&mut event.task_id, "任务 ID"),
        (&mut event.trace_id, "追踪 ID"),
    ] {
        if let Some(identifier) = value {
            *identifier = validate_long_term_memory_identifier(identifier, label, 160)?;
        }
    }
    if !event.metadata.is_object() {
        return Err("长期记忆元数据必须是对象".to_string());
    }
    let metadata = serde_json::to_vec(&event.metadata)
        .map_err(|error| format!("无法序列化长期记忆元数据：{error}"))?;
    if metadata.len() > MAX_LONG_TERM_MEMORY_METADATA_BYTES {
        return Err("长期记忆元数据超过 256 KB 安全上限".to_string());
    }
    Ok(event)
}

pub(crate) fn finalize_pending_long_term_memory_events_for_runtime(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<(), String> {
    for pending in database.pending_long_term_memory_events(workspace_scope, 200)? {
        let event = match serde_json::from_value::<LongTermMemoryEventInput>(pending.payload) {
            Ok(event) => normalize_long_term_memory_event(event),
            Err(error) => Err(format!("长期记忆记录格式无效：{error}")),
        };
        match event {
            Ok(event) => {
                let payload = serde_json::to_vec(&event)
                    .map_err(|error| format!("无法序列化长期记忆事件：{error}"))?;
                let committed_at = now_string();
                database.commit_long_term_memory_event_internal(
                    workspace_scope,
                    &event.id,
                    &hash_bytes(&payload),
                    &committed_at,
                )?;
            }
            Err(error) => {
                database.fail_long_term_memory_event(workspace_scope, &pending.id, &error)?
            }
        }
    }
    Ok(())
}

pub(crate) fn recover_vault_batch_manifests_for_runtime(
    app: &AppHandle,
    database: &RuntimeDatabase,
) -> Result<(), String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    let summary = vault_batch::recover_batch_manifests(&app_data, |audit| {
        database.append_operation_event(&OperationEvent {
            id: audit.id.clone(),
            task_id: audit.task_id.clone(),
            trace_id: audit.trace_id.clone(),
            event_type: audit.event_type.clone(),
            state: "success".to_string(),
            created_at: audit.created_at.clone(),
            vault_id: None,
            relative_path: None,
            detail: audit.detail.clone(),
        })
    });
    if summary.completed_audits > 0 || summary.rolled_back_batches > 0 {
        log::info!(
            "跨 Vault 批次恢复完成：补写审计 {} 个，回滚 {} 个",
            summary.completed_audits,
            summary.rolled_back_batches
        );
    }
    if summary.failures.is_empty() {
        Ok(())
    } else {
        Err(summary.failures.join("；"))
    }
}

#[tauri::command]
pub fn append_long_term_memory_event(
    database: State<'_, RuntimeDatabase>,
    event: LongTermMemoryEventInput,
) -> Result<LongTermMemoryReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let event = normalize_long_term_memory_event(event)?;
    let payload =
        serde_json::to_value(&event).map_err(|error| format!("无法序列化长期记忆事件：{error}"))?;
    let duplicate = database.stage_long_term_memory_event(
        &workspace_scope,
        &event.id,
        &event.event_type,
        &event.occurred_at,
        &payload,
    )?;
    let payload_bytes =
        serde_json::to_vec(&event).map_err(|error| format!("无法序列化长期记忆事件：{error}"))?;
    let committed_at = now_string();
    let content_hash = hash_bytes(&payload_bytes);
    database.commit_long_term_memory_event_internal(
        &workspace_scope,
        &event.id,
        &content_hash,
        &committed_at,
    )?;
    Ok(LongTermMemoryReceipt {
        event_id: event.id,
        relative_path: None,
        content_hash,
        committed_at,
        duplicate,
    })
}

fn title_from_markdown(path: &Path, content: &str) -> String {
    content
        .lines()
        .find_map(|line| {
            line.strip_prefix("# ")
                .map(str::trim)
                .filter(|title| !title.is_empty())
        })
        .or_else(|| path.file_stem().and_then(|name| name.to_str()))
        .unwrap_or("无标题笔记")
        .nfc()
        .collect()
}

fn excerpt_around(content: &str, query: &str) -> String {
    let normalized = content.replace(['\n', '\r'], " ");
    let lower = normalized.to_lowercase();
    let query_lower = query.to_lowercase();
    let start = lower.find(&query_lower).unwrap_or(0).saturating_sub(80);
    normalized
        .chars()
        .skip(start)
        .take(240)
        .collect::<String>()
        .trim()
        .to_string()
}

#[tauri::command]
pub fn discover_obsidian_vaults() -> Result<Vec<VaultDescriptor>, String> {
    discover_vaults()
}

#[tauri::command]
pub fn set_local_vault_selection(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    let normalized = vault_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all");
    if let Some(id) = normalized {
        resolve_vault(id)?;
    }
    database.save_explicit_vault_selection(&workspace_scope, normalized)
}

#[tauri::command]
pub fn list_vault_folders(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<Vec<VaultFolderDescriptor>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
    let (_, root) = resolve_vault(&vault_id)?;
    let mut folders = BTreeSet::new();
    collect_vault_folders(&root, &root, &mut folders)?;
    let mut counts = HashMap::<String, u64>::new();
    let mut markdown = Vec::new();
    let mut attachments = 0;
    collect_files(&root, &mut markdown, &mut attachments)?;
    for path in markdown {
        if let Ok(relative) = path.strip_prefix(&root) {
            if let Some(parent) = relative.parent() {
                let Some(parent) = parent.to_str() else {
                    continue;
                };
                let value = parent.replace('\\', "/").nfc().collect::<String>();
                if !value.is_empty() {
                    *counts.entry(value).or_insert(0) += 1;
                }
            }
        }
    }
    Ok(folders
        .into_iter()
        .map(|relative_path| VaultFolderDescriptor {
            note_count: counts.get(&relative_path).copied().unwrap_or(0),
            relative_path,
        })
        .collect())
}

#[tauri::command]
pub fn beautify_creation_markdown(markdown: String) -> Result<BeautifyMarkdownResult, String> {
    let formatted = format_creation_markdown(&markdown);
    Ok(BeautifyMarkdownResult {
        changed: formatted != markdown,
        markdown: formatted,
        skill_id: "beautify-markdown".to_string(),
    })
}

#[tauri::command]
pub fn search_vault_notes(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<VaultSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    let limit = limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let workspace_scope = database.local_workspace_scope()?;
    let scoped_vault_id = vault_id
        .as_deref()
        .filter(|value| !value.trim().is_empty() && *value != "all");
    if let Some(vault_id) = scoped_vault_id {
        database.ensure_vault_read_allowed(&workspace_scope, vault_id)?;
    }
    let discovered = discover_vaults()?;
    let selected = discovered
        .into_iter()
        .filter(|vault| vault.connection_state == "connected")
        .filter(|vault| match vault_id.as_deref() {
            None | Some("all") => true,
            Some(id) => id == vault.id,
        })
        .filter(|vault| {
            scoped_vault_id.is_some()
                || database
                    .ensure_vault_read_allowed(&workspace_scope, &vault.id)
                    .is_ok()
        })
        .collect::<Vec<_>>();
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for vault in selected {
        let root = PathBuf::from(&vault.path);
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files(&root, &mut markdown, &mut attachments)?;
        for path in markdown {
            let path_text = match normalized_relative_path(&root, &path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let path_match = path_text.to_lowercase().contains(&query_lower);
            let bytes = match read_file_limited(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let content = match String::from_utf8(bytes) {
                Ok(value) => value.nfc().collect::<String>(),
                Err(_) => continue,
            };
            let content_lower = content.to_lowercase();
            if !path_match && !content_lower.contains(&query_lower) {
                continue;
            }
            let title = title_from_markdown(&path, &content);
            let title_match = title.to_lowercase().contains(&query_lower);

            // 分析匹配原因
            use crate::search_match::MatchReasonAnalyzer;
            let mut match_reasons = crate::search_match::MatchReasons::default();

            if title_match {
                match_reasons.title_match = MatchReasonAnalyzer::analyze_title_match(&title, query);
            }

            if !path_match && !title_match {
                // 仅内容匹配
                match_reasons.content_match =
                    MatchReasonAnalyzer::analyze_content_match(&content, query, 3);
            }

            results.push(VaultSearchResult {
                vault_id: vault.id.clone(),
                vault_name: vault.name.clone(),
                relative_path: path_text,
                title,
                excerpt: excerpt_around(&content, query),
                modified_at: modified_string(&path),
                score: if title_match {
                    100
                } else if path_match {
                    80
                } else {
                    60
                },
                match_reasons: Some(match_reasons),
            });
        }
    }

    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(right.modified_at.cmp(&left.modified_at))
    });
    results.truncate(limit);
    Ok(results)
}

#[tauri::command]
pub fn read_vault_note(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
) -> Result<VaultNote, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
    let (vault_name, root) = resolve_vault(&vault_id)?;
    let (target, normalized_relative) = resolve_note_target(&root, &relative_path, false)?;
    let bytes = read_file_limited(&target)?;
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| "笔记不是有效 UTF-8 Markdown".to_string())?;

    // 记录笔记查看事件
    let _ = crate::metrics::record_activity_event(
        database.inner(),
        "note_view",
        Some(&vault_id),
        Some(&normalized_relative),
        None,
    );

    Ok(VaultNote {
        vault_id,
        vault_name,
        relative_path: normalized_relative,
        content,
        content_hash: hash_bytes(&bytes),
        modified_at: modified_string(&target),
        byte_length: bytes.len() as u64,
    })
}

fn note_path_without_markdown_extension(relative_path: &str) -> String {
    if relative_path.to_ascii_lowercase().ends_with(".md") {
        relative_path[..relative_path.len() - 3].to_string()
    } else {
        relative_path.to_string()
    }
}

fn obsidian_open_url(vault_name: &str, relative_path: &str) -> Result<String, String> {
    let note_path = note_path_without_markdown_extension(relative_path);
    let normalized_vault_name = vault_name.nfc().collect::<String>();
    let normalized_note_path = note_path.replace('\\', "/").nfc().collect::<String>();
    obsidian_open_uri(&normalized_vault_name, &normalized_note_path)
}

fn obsidian_open_vault_url(vault_name: &str) -> Result<String, String> {
    let normalized_vault_name = vault_name.nfc().collect::<String>();
    let mut url = Url::parse("obsidian://open")
        .map_err(|error| format!("无法构造 Obsidian 链接：{error}"))?;
    url.query_pairs_mut()
        .append_pair("vault", &normalized_vault_name);
    Ok(url.to_string().replace('+', "%20"))
}

fn trigger_obsidian_graph_shortcut() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = r#"
tell application "Obsidian" to activate
delay 0.6
tell application "System Events"
    tell process "Obsidian"
        keystroke "g" using {command down}
    end tell
end tell
"#;
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .output()
            .map_err(|error| format!("无法调用 Obsidian 原生图谱命令：{error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if detail.is_empty() {
                "系统未允许云枢调用 Obsidian 原生图谱快捷命令".to_string()
            } else {
                format!("系统未允许云枢调用 Obsidian 原生图谱快捷命令：{detail}")
            })
        }
    }

    #[cfg(target_os = "windows")]
    {
        let script = r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class YunspireWindow {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr handle);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr handle, int command);
}
"@
Start-Sleep -Milliseconds 600
$process = Get-Process Obsidian -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if ($null -eq $process) { exit 1 }
[YunspireWindow]::ShowWindow($process.MainWindowHandle, 9) | Out-Null
[YunspireWindow]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait('^g')
"#;
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script,
            ])
            .output()
            .map_err(|error| format!("无法调用 Obsidian 原生图谱命令：{error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err("系统未能把 Obsidian 窗口置前并调用原生图谱快捷命令".to_string())
        }
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        Err("当前平台没有受支持的 Obsidian 原生图谱唤起方式".to_string())
    }
}

fn open_obsidian_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = Command::new("/usr/bin/open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("explorer.exe").arg(url).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(url).status();

    let status = status.map_err(|error| format!("无法启动 Obsidian：{error}"))?;
    if !status.success() {
        return Err(format!(
            "系统未能通过 Obsidian 协议打开笔记，退出状态：{}",
            status
                .code()
                .map_or_else(|| "unknown".to_string(), |code| code.to_string())
        ));
    }
    Ok(())
}

#[tauri::command]
pub fn open_vault_note_in_obsidian(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
) -> Result<String, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
    let (vault_name, root) = resolve_vault(&vault_id)?;
    let (_, normalized_relative) = resolve_note_target(&root, &relative_path, false)?;
    let url = obsidian_open_url(&vault_name, &normalized_relative)?;
    open_obsidian_url(&url)?;
    Ok(url)
}

#[tauri::command]
pub fn open_obsidian_note(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
) -> Result<String, String> {
    open_vault_note_in_obsidian(database, vault_id, relative_path)
}

#[tauri::command]
pub fn open_obsidian_vault(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<String, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
    let (vault_name, _) = resolve_vault(&vault_id)?;
    let url = obsidian_open_vault_url(&vault_name)?;
    open_obsidian_url(&url)?;
    Ok(url)
}

#[tauri::command]
pub fn open_obsidian_graph(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<ObsidianGraphLaunchResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
    let (vault_name, _) = resolve_vault(&vault_id)?;
    let vault_url = obsidian_open_vault_url(&vault_name)?;
    open_obsidian_url(&vault_url)?;
    match trigger_obsidian_graph_shortcut() {
        Ok(()) => Ok(ObsidianGraphLaunchResult {
            vault_url,
            graph_opened: true,
            message: "已打开 Obsidian 原生知识图谱".to_string(),
        }),
        Err(error) => Ok(ObsidianGraphLaunchResult {
            vault_url,
            graph_opened: false,
            message: format!("已打开 Obsidian，但原生图谱未自动切换：{error}"),
        }),
    }
}

#[tauri::command]
pub fn list_vault_notes(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<VaultNoteSummary>, String> {
    let max = limit.unwrap_or(500).clamp(1, 2000);
    let workspace_scope = database.local_workspace_scope()?;
    let scoped_vault_id = vault_id
        .as_deref()
        .filter(|value| !value.trim().is_empty() && *value != "all");
    if let Some(vault_id) = scoped_vault_id {
        database.ensure_vault_read_allowed(&workspace_scope, vault_id)?;
    }
    let vaults = discover_vaults()?;
    let mut result = Vec::new();
    for vault in vaults
        .into_iter()
        .filter(|item| item.connection_state == "connected")
    {
        if let Some(selected) = vault_id.as_deref() {
            if selected != "all" && selected != vault.id {
                continue;
            }
        }
        if scoped_vault_id.is_none()
            && database
                .ensure_vault_read_allowed(&workspace_scope, &vault.id)
                .is_err()
        {
            continue;
        }
        let root = PathBuf::from(&vault.path);
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files(&root, &mut markdown, &mut attachments)?;
        for path in markdown {
            if result.len() >= max {
                break;
            }
            let relative = match normalized_relative_path(&root, &path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let bytes = match read_file_limited(&path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let content_hash = hash_bytes(&bytes);
            let content = match String::from_utf8(bytes) {
                Ok(value) => value.nfc().collect::<String>(),
                Err(_) => continue,
            };
            result.push(VaultNoteSummary {
                vault_id: vault.id.clone(),
                vault_name: vault.name.clone(),
                relative_path: relative,
                title: title_from_markdown(&path, &content),
                content,
                content_hash,
                modified_at: modified_string(&path),
            });
        }
        if result.len() >= max {
            break;
        }
    }
    Ok(result)
}

fn collect_vault_note_candidates(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    vault_id: Option<&str>,
) -> Result<Vec<VaultNoteCandidate>, String> {
    let scoped_vault_id = vault_id.filter(|value| !value.trim().is_empty() && *value != "all");
    if let Some(vault_id) = scoped_vault_id {
        database.ensure_vault_read_allowed(workspace_scope, vault_id)?;
    }
    let vaults = discover_vaults()?;
    let mut candidates = Vec::new();
    for vault in vaults
        .into_iter()
        .filter(|item| item.connection_state == "connected")
    {
        if vault_id.is_some_and(|selected| selected != "all" && selected != vault.id) {
            continue;
        }
        if scoped_vault_id.is_none()
            && database
                .ensure_vault_read_allowed(workspace_scope, &vault.id)
                .is_err()
        {
            continue;
        }
        let root = PathBuf::from(&vault.path);
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files(&root, &mut markdown, &mut attachments)?;
        for path in markdown {
            let relative_path = normalized_relative_path(&root, &path).map_err(|error| {
                format!("无法规范化知识库 Markdown 路径 {}：{error}", path.display())
            })?;
            candidates.push(VaultNoteCandidate {
                vault_id: vault.id.clone(),
                vault_name: vault.name.clone(),
                path,
                relative_path,
            });
        }
    }
    candidates.sort_by(|left, right| {
        (&left.vault_id, &left.relative_path).cmp(&(&right.vault_id, &right.relative_path))
    });
    Ok(candidates)
}

#[tauri::command]
pub fn list_vault_notes_page(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    after_vault_id: Option<String>,
    after_relative_path: Option<String>,
    limit: Option<usize>,
    max_bytes: Option<u64>,
    folder_prefix: Option<String>,
) -> Result<VaultNotePage, String> {
    list_vault_notes_page_inner(
        &database,
        vault_id,
        after_vault_id,
        after_relative_path,
        limit,
        max_bytes,
        folder_prefix,
    )
}

fn list_vault_notes_page_inner(
    database: &RuntimeDatabase,
    vault_id: Option<String>,
    after_vault_id: Option<String>,
    after_relative_path: Option<String>,
    limit: Option<usize>,
    max_bytes: Option<u64>,
    folder_prefix: Option<String>,
) -> Result<VaultNotePage, String> {
    if after_vault_id.is_some() != after_relative_path.is_some() {
        return Err("分页游标必须同时包含 Vault ID 和相对路径".to_string());
    }
    let page_limit = limit.unwrap_or(128).clamp(1, 512);
    let page_byte_budget = max_bytes
        .unwrap_or(8 * 1024 * 1024)
        .clamp(64 * 1024, 32 * 1024 * 1024);
    let workspace_scope = database.local_workspace_scope()?;
    let normalized_folder = folder_prefix
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let normalized = value
                .replace('\\', "/")
                .trim_matches('/')
                .nfc()
                .collect::<String>();
            let path = Path::new(&normalized);
            if normalized.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err("知识库文件夹必须是 Vault 内的安全相对路径".to_string());
            }
            Ok(normalized)
        })
        .transpose()?;
    let mut candidates =
        collect_vault_note_candidates(database, &workspace_scope, vault_id.as_deref())?;
    if let Some(folder) = normalized_folder.as_deref() {
        let prefix = format!("{folder}/");
        candidates.retain(|candidate| {
            candidate.relative_path == folder || candidate.relative_path.starts_with(&prefix)
        });
    }
    let cursor = after_vault_id
        .as_deref()
        .zip(after_relative_path.as_deref());
    let mut index = cursor.map_or(0, |cursor| {
        candidates.partition_point(|candidate| {
            (
                candidate.vault_id.as_str(),
                candidate.relative_path.as_str(),
            ) <= cursor
        })
    });
    let mut notes = Vec::new();
    let mut failures = Vec::new();
    let mut returned_bytes = 0_u64;
    let mut processed = 0_usize;
    let mut last_processed: Option<(String, String)> = None;
    while index < candidates.len() && processed < page_limit {
        let candidate = &candidates[index];
        let bytes = match read_file_limited(&candidate.path) {
            Ok(value) => value,
            Err(error) => {
                failures.push(VaultNoteReadFailure {
                    vault_id: candidate.vault_id.clone(),
                    vault_name: candidate.vault_name.clone(),
                    relative_path: candidate.relative_path.clone(),
                    reason: error,
                });
                last_processed =
                    Some((candidate.vault_id.clone(), candidate.relative_path.clone()));
                index += 1;
                processed += 1;
                continue;
            }
        };
        let byte_length = bytes.len() as u64;
        if !notes.is_empty() && returned_bytes.saturating_add(byte_length) > page_byte_budget {
            break;
        }
        last_processed = Some((candidate.vault_id.clone(), candidate.relative_path.clone()));
        index += 1;
        processed += 1;
        let content_hash = hash_bytes(&bytes);
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(error) => {
                failures.push(VaultNoteReadFailure {
                    vault_id: candidate.vault_id.clone(),
                    vault_name: candidate.vault_name.clone(),
                    relative_path: candidate.relative_path.clone(),
                    reason: format!("不是有效 UTF-8 Markdown：{error}"),
                });
                continue;
            }
        };
        let content = content.nfc().collect::<String>();
        returned_bytes = returned_bytes.saturating_add(byte_length);
        notes.push(VaultNoteSummary {
            vault_id: candidate.vault_id.clone(),
            vault_name: candidate.vault_name.clone(),
            relative_path: candidate.relative_path.clone(),
            title: title_from_markdown(&candidate.path, &content),
            content,
            content_hash,
            modified_at: modified_string(&candidate.path),
        });
    }
    let has_more = index < candidates.len();
    let (next_after_vault_id, next_after_relative_path) = if has_more {
        last_processed
            .map(|(vault_id, relative_path)| (Some(vault_id), Some(relative_path)))
            .unwrap_or((after_vault_id, after_relative_path))
    } else {
        (None, None)
    };
    if has_more && next_after_vault_id.is_none() {
        return Err("知识库分页没有取得进展".to_string());
    }
    Ok(VaultNotePage {
        notes,
        failures,
        candidate_count: candidates.len(),
        next_after_vault_id,
        next_after_relative_path,
        has_more,
        returned_bytes,
    })
}

#[derive(Clone)]
struct CaptureImageObservation {
    asset_id: String,
    reference_id: String,
    observation: String,
    text: String,
    context: String,
    evidence: String,
    confidence: f64,
}

#[derive(Clone)]
struct CaptureImageBinding {
    asset_id: String,
    reference_ids: Vec<String>,
    original_sha256: String,
    analysis_sha256: String,
    original_byte_length: u64,
    analysis_byte_length: u64,
    analysis_mime_type: String,
    derived: bool,
}

fn capture_reference_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 180
        || value.chars().any(char::is_control)
        || value.contains("attachment://")
    {
        return Err("采集附件的 asset/reference id 无效".to_string());
    }
    Ok(value.to_string())
}

fn normalize_capture_sha256(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    let value = value
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("sha256:"))
        .map(|_| &value[7..])
        .unwrap_or(value);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} 必须是完整的 SHA-256"));
    }
    Ok(value.to_ascii_lowercase())
}

fn capture_binding_sha256(value: Option<&Value>, label: &str) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} 缺失或不是字符串"))?;
    normalize_capture_sha256(value, label)
}

fn capture_binding_byte_length(value: Option<&Value>, label: &str) -> Result<u64, String> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{label} 必须是大于 0 的整数"))
}

fn capture_binding_mime_type(value: Option<&Value>, asset_id: &str) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if value.len() <= "image/".len()
        || !value.starts_with("image/")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'))
    {
        return Err(format!(
            "asset_id={asset_id} 的 image binding 缺少有效 analysis_mime_type"
        ));
    }
    Ok(value)
}

fn capture_image_bindings(
    analysis: &Value,
) -> Result<HashMap<String, CaptureImageBinding>, String> {
    let Some(raw_bindings) = analysis
        .get("image_bindings")
        .or_else(|| analysis.get("imageBindings"))
    else {
        return Ok(HashMap::new());
    };
    let raw_bindings = raw_bindings
        .as_array()
        .ok_or_else(|| "模型结果中的 image_bindings 必须是结构化数组".to_string())?;
    let mut bindings = HashMap::new();
    for (index, item) in raw_bindings.iter().enumerate() {
        let position = index + 1;
        let object = item
            .as_object()
            .ok_or_else(|| format!("第 {position} 个 image binding 不是对象"))?;
        let asset_id = object
            .get("asset_id")
            .or_else(|| object.get("assetId"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("第 {position} 个 image binding 缺少 asset_id"))
            .and_then(capture_reference_id)?;
        let raw_reference_ids = object
            .get("reference_ids")
            .or_else(|| object.get("referenceIds"))
            .or_else(|| object.get("allowed_reference_ids"))
            .or_else(|| object.get("allowedReferenceIds"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!("asset_id={asset_id} 的 image binding 缺少 reference_ids 数组")
            })?;
        let mut reference_ids = Vec::new();
        for reference_id in raw_reference_ids {
            let reference_id = reference_id
                .as_str()
                .ok_or_else(|| {
                    format!("asset_id={asset_id} 的 image binding 包含非字符串 reference_id")
                })
                .and_then(capture_reference_id)?;
            if !reference_ids.contains(&reference_id) {
                reference_ids.push(reference_id);
            }
        }
        if reference_ids.is_empty() {
            return Err(format!(
                "asset_id={asset_id} 的 image binding 没有允许的 reference_ids"
            ));
        }
        let original_sha256 = capture_binding_sha256(
            object
                .get("original_sha256")
                .or_else(|| object.get("originalSha256")),
            &format!("asset_id={asset_id} 的 original_sha256"),
        )?;
        let analysis_sha256 = capture_binding_sha256(
            object
                .get("analysis_sha256")
                .or_else(|| object.get("analysisSha256")),
            &format!("asset_id={asset_id} 的 analysis_sha256"),
        )?;
        let original_byte_length = capture_binding_byte_length(
            object
                .get("original_byte_length")
                .or_else(|| object.get("originalByteLength")),
            &format!("asset_id={asset_id} 的 original_byte_length"),
        )?;
        let analysis_byte_length = capture_binding_byte_length(
            object
                .get("analysis_byte_length")
                .or_else(|| object.get("analysisByteLength")),
            &format!("asset_id={asset_id} 的 analysis_byte_length"),
        )?;
        let analysis_mime_type = capture_binding_mime_type(
            object
                .get("analysis_mime_type")
                .or_else(|| object.get("analysisMimeType")),
            &asset_id,
        )?;
        let derived = object
            .get("derived")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("asset_id={asset_id} 的 image binding 缺少 derived"))?;
        if !derived
            && (original_sha256 != analysis_sha256 || original_byte_length != analysis_byte_length)
        {
            return Err(format!(
                "asset_id={asset_id} 标记为非派生输入，但原始/分析哈希或字节数不一致"
            ));
        }
        let binding = CaptureImageBinding {
            asset_id: asset_id.clone(),
            reference_ids,
            original_sha256,
            analysis_sha256,
            original_byte_length,
            analysis_byte_length,
            analysis_mime_type,
            derived,
        };
        if bindings.insert(asset_id.clone(), binding).is_some() {
            return Err(format!(
                "模型结果包含重复的 image binding asset_id={asset_id}"
            ));
        }
    }
    Ok(bindings)
}

fn validate_capture_image_bindings(
    analysis: &Value,
    attachments: &[CaptureVaultAttachmentInput],
) -> Result<HashMap<String, CaptureImageBinding>, String> {
    let bindings = capture_image_bindings(analysis)?;
    let image_asset_ids = attachments
        .iter()
        .filter(|attachment| is_image_attachment(attachment))
        .map(|attachment| attachment.asset_id.as_str())
        .collect::<HashSet<_>>();
    for binding_asset_id in bindings.keys() {
        if !image_asset_ids.contains(binding_asset_id.as_str()) {
            return Err(format!(
                "image binding asset_id={binding_asset_id} 没有对应的图片附件"
            ));
        }
    }
    for attachment in attachments
        .iter()
        .filter(|attachment| is_image_attachment(attachment))
    {
        let asset_id = capture_reference_id(&attachment.asset_id)?;
        let binding = bindings
            .get(&asset_id)
            .ok_or_else(|| format!("图片附件 asset_id={asset_id} 缺少结构化 image binding"))?;
        let mut allowed_reference_ids = attachment_position_reference_ids(attachment)?;
        if allowed_reference_ids.is_empty() {
            allowed_reference_ids.push(asset_id.clone());
        }
        let expected_references = allowed_reference_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let binding_references = binding
            .reference_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if expected_references != binding_references {
            return Err(format!(
                "图片附件 asset_id={asset_id} 的允许位置与 image binding reference_ids 冲突：附件={expected_references:?}，binding={binding_references:?}",
            ));
        }
        if let Some(expected_sha256) = attachment
            .expected_sha256
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let expected_sha256 = capture_binding_sha256(
                Some(&Value::String(expected_sha256.to_string())),
                &format!("图片附件 asset_id={asset_id} 的 expected_sha256"),
            )?;
            if expected_sha256 != binding.original_sha256 {
                return Err(format!(
                    "图片附件 asset_id={asset_id} 的原件 SHA-256 与 image binding 冲突"
                ));
            }
        }
        if let Some(encoded) = attachment
            .content_base64
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| format!("图片附件 asset_id={asset_id} 不是有效的 Base64"))?;
            if bytes.len() as u64 != binding.original_byte_length {
                return Err(format!(
                    "图片附件 asset_id={asset_id} 的原始字节数与 image binding 冲突"
                ));
            }
            if hash_bytes(&bytes) != binding.original_sha256 {
                return Err(format!(
                    "图片附件 asset_id={asset_id} 的原件 SHA-256 与 image binding 冲突"
                ));
            }
        }
        if attachment
            .content_base64
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
            && attachment
                .staged_attachment_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && attachment
                .expected_sha256
                .as_deref()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(format!(
                "暂存图片附件 asset_id={asset_id} 缺少原件 SHA-256，无法验证 image binding"
            ));
        }
        if !binding.derived
            && binding.analysis_mime_type != attachment.mime_type.trim().to_ascii_lowercase()
        {
            return Err(format!(
                "图片附件 asset_id={asset_id} 的非派生分析 MIME 与原件不一致"
            ));
        }
    }
    Ok(bindings)
}

fn capture_analysis_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(value @ (Value::Array(_) | Value::Object(_))) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        Some(value) if !value.is_null() => value.to_string(),
        _ => String::new(),
    }
}

fn capture_analysis_strings(analysis: &Value, snake_case: &str, camel_case: &str) -> Vec<String> {
    analysis
        .get(snake_case)
        .or_else(|| analysis.get(camel_case))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let value = match item {
                Value::String(value) => value.trim().to_string(),
                Value::Object(object) => ["name", "label", "title", "value"]
                    .into_iter()
                    .find_map(|key| object.get(key).and_then(Value::as_str))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                _ => String::new(),
            };
            (!value.is_empty()).then_some(value)
        })
        .collect()
}

fn capture_image_observations(
    analysis: &Value,
) -> Result<HashMap<String, CaptureImageObservation>, String> {
    let mut observations = HashMap::new();
    for item in analysis
        .get("image_observations")
        .or_else(|| analysis.get("imageObservations"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(object) = item.as_object() else {
            continue;
        };
        let asset_id = object
            .get("asset_id")
            .or_else(|| object.get("assetId"))
            .and_then(Value::as_str)
            .ok_or_else(|| "模型图片分析缺少 asset_id".to_string())
            .and_then(capture_reference_id)?;
        let reference_id = object
            .get("reference_id")
            .or_else(|| object.get("referenceId"))
            .and_then(Value::as_str)
            .map(capture_reference_id)
            .transpose()?
            .unwrap_or_else(|| asset_id.clone());
        let observation = capture_analysis_text(
            object
                .get("observation")
                .or_else(|| object.get("description"))
                .or_else(|| object.get("summary")),
        );
        if observation.is_empty() {
            return Err(format!("模型没有返回 asset_id={asset_id} 的有效图片分析"));
        }
        let observation_value = CaptureImageObservation {
            asset_id: asset_id.clone(),
            reference_id: reference_id.clone(),
            observation,
            text: capture_analysis_text(
                object
                    .get("text")
                    .or_else(|| object.get("ocr_text"))
                    .or_else(|| object.get("ocrText")),
            ),
            context: capture_analysis_text(
                object.get("context").or_else(|| object.get("position")),
            ),
            evidence: capture_analysis_text(object.get("evidence")),
            confidence: object
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0),
        };
        if observations
            .insert(asset_id, observation_value.clone())
            .is_some()
        {
            return Err("模型返回了重复的图片 asset_id".to_string());
        }
        if reference_id != observation_value.asset_id {
            observations
                .entry(reference_id)
                .or_insert(observation_value);
        }
    }
    Ok(observations)
}

fn capture_safe_title(value: &str) -> String {
    let mut title = value
        .trim()
        .trim_end_matches(".md")
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '-'
            } else {
                character
            }
        })
        .collect::<String>();
    title = title
        .trim_matches([' ', '.', '-'])
        .chars()
        .take(160)
        .collect();
    if title.is_empty() {
        "未命名采集".to_string()
    } else {
        title
    }
}

fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn obsidian_open_uri(vault_name: &str, relative_path: &str) -> Result<String, String> {
    let mut url = Url::parse("obsidian://open")
        .map_err(|error| format!("无法构造 Obsidian 链接：{error}"))?;
    url.query_pairs_mut()
        .append_pair("vault", vault_name)
        .append_pair("file", relative_path);
    Ok(url.to_string().replace('+', "%20"))
}

fn is_image_attachment(attachment: &CaptureVaultAttachmentInput) -> bool {
    attachment
        .mime_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/")
}

fn attachment_stable_reference_keys(attachment: &CaptureVaultAttachmentInput) -> Vec<String> {
    let mut keys = vec![attachment.asset_id.clone()];
    if let Some(reference_id) = attachment
        .reference_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !keys.iter().any(|key| key == reference_id) {
            keys.push(reference_id.to_string());
        }
    }
    for reference_id in &attachment.reference_ids {
        let reference_id = reference_id.trim();
        if !reference_id.is_empty() && !keys.iter().any(|key| key == reference_id) {
            keys.push(reference_id.to_string());
        }
    }
    keys
}

fn attachment_reference_keys(attachment: &CaptureVaultAttachmentInput) -> Vec<String> {
    let mut keys = attachment_stable_reference_keys(attachment);
    if let Some(name) = attachment
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !keys.iter().any(|key| key == name) {
            keys.push(name.to_string());
        }
    }
    keys
}

fn attachment_position_reference_ids(
    attachment: &CaptureVaultAttachmentInput,
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    if let Some(reference_id) = attachment
        .reference_id
        .as_deref()
        .map(capture_reference_id)
        .transpose()?
    {
        ids.push(reference_id);
    }
    for reference_id in &attachment.reference_ids {
        let reference_id = capture_reference_id(reference_id)?;
        if !ids.contains(&reference_id) {
            ids.push(reference_id);
        }
    }
    Ok(ids)
}

fn markdown_contains_attachment_reference(markdown: &str, key: &str) -> bool {
    [key.to_string(), encode_uri_component(key)]
        .into_iter()
        .any(|token_key| {
            let escaped = regex::escape(&format!("attachment://{token_key}"));
            Regex::new(&format!(r#"{escaped}(?:$|[\s)\]}}>'\"])"#))
                .is_ok_and(|pattern| pattern.is_match(markdown))
        })
}

fn validate_capture_attachment_reference_owners(
    raw_markdown: &str,
    attachments: &[CaptureVaultAttachmentInput],
) -> Result<(), String> {
    let mut owners = HashMap::<String, String>::new();
    for attachment in attachments {
        let mut keys = attachment_stable_reference_keys(attachment);
        if let Some(name) = attachment
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| markdown_contains_attachment_reference(raw_markdown, name))
        {
            if !keys.iter().any(|key| key == name) {
                keys.push(name.to_string());
            }
        }
        for key in keys {
            if let Some(owner) = owners.insert(key.clone(), attachment.asset_id.clone()) {
                if owner != attachment.asset_id {
                    return Err(format!(
                        "附件引用 {key} 同时指向多个 asset_id，无法保证原文图片位置"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn replace_attachment_reference(
    markdown: &mut String,
    attachment: &CaptureVaultAttachmentInput,
    replacement: &str,
) -> Result<bool, String> {
    let mut replaced = false;
    for key in attachment_reference_keys(attachment) {
        replaced |= replace_attachment_reference_key(markdown, &key, replacement)?;
    }
    Ok(replaced)
}

fn replace_attachment_reference_key(
    markdown: &mut String,
    key: &str,
    replacement: &str,
) -> Result<bool, String> {
    let mut replaced = false;
    for token_key in [key.to_string(), encode_uri_component(key)] {
        let token = format!("attachment://{token_key}");
        if !markdown.contains(&token) {
            continue;
        }
        let escaped = regex::escape(&token);
        let destination = format!(r"(?:<\s*{escaped}\s*>|{escaped})");
        let optional_title = r#"(?:\s+(?:"(?:\\.|[^"])*"|'(?:\\.|[^'])*'|\((?:\\.|[^)])*\)))?"#;
        let image = Regex::new(&format!(
            r"!\[[^\]]*\]\(\s*{destination}{optional_title}\s*\)"
        ))
        .map_err(|error| format!("无法构造附件引用规则：{error}"))?;
        let wiki_image = Regex::new(&format!(r"!\[\[\s*{escaped}(?:\|[^\]]*)?\]\]"))
            .map_err(|error| format!("无法构造附件引用规则：{error}"))?;
        let link = Regex::new(&format!(
            r"\[([^\]]+)\]\(\s*{destination}{optional_title}\s*\)"
        ))
        .map_err(|error| format!("无法构造附件引用规则：{error}"))?;
        let bare = Regex::new(&format!(r#"{escaped}(?P<suffix>$|[\s)\]}}>'\"])"#))
            .map_err(|error| format!("无法构造附件裸引用规则：{error}"))?;
        let before = markdown.clone();
        *markdown = image
            .replace_all(markdown, |_: &regex::Captures<'_>| replacement)
            .into_owned();
        *markdown = wiki_image
            .replace_all(markdown, |_: &regex::Captures<'_>| replacement)
            .into_owned();
        *markdown = link
            .replace_all(markdown, |_: &regex::Captures<'_>| replacement)
            .into_owned();
        *markdown = bare
            .replace_all(markdown, |captures: &regex::Captures<'_>| {
                format!(
                    "{replacement}{}",
                    captures
                        .name("suffix")
                        .map(|value| value.as_str())
                        .unwrap_or_default()
                )
            })
            .into_owned();
        replaced |= *markdown != before;
    }
    Ok(replaced)
}

fn materialize_capture_raw_markdown(
    raw_markdown: &str,
    attachments: &[CaptureVaultAttachmentInput],
    _source_type: &str,
) -> Result<(String, HashSet<String>), String> {
    let mut markdown = raw_markdown.to_string();
    let mut referenced = HashSet::new();
    let mut paths = HashSet::new();
    for attachment in attachments {
        let asset_id = capture_reference_id(&attachment.asset_id)?;
        if !paths.insert(attachment.relative_path.clone()) {
            return Err("采集批次不能把多个附件写入同一路径".to_string());
        }
        let replacement = if is_image_attachment(attachment) {
            format!("![[{}]]", attachment.relative_path)
        } else {
            format!("[[{}]]", attachment.relative_path)
        };
        let position_reference_ids = attachment_position_reference_ids(attachment)?;
        if attachment.placement_required && position_reference_ids.len() > 1 {
            let matched = position_reference_ids
                .iter()
                .filter(|reference_id| {
                    markdown_contains_attachment_reference(&markdown, reference_id)
                })
                .count();
            if matched > 0 && matched != position_reference_ids.len() {
                let missing = position_reference_ids
                    .iter()
                    .filter(|reference_id| {
                        !markdown_contains_attachment_reference(&markdown, reference_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                return Err(format!(
                    "原文缺少 asset_id={asset_id} 的部分图片位置：{}",
                    missing.join("、")
                ));
            }
        }
        if replace_attachment_reference(&mut markdown, attachment, &replacement)? {
            referenced.insert(asset_id.clone());
        } else if attachment.placement_required {
            return Err(format!(
                "原文没有找到 asset_id={asset_id} 的附件位置，已阻止生成不完整原文"
            ));
        }
    }
    let unresolved = Regex::new(r#"attachment://[^\s)\]}>'\"]+"#)
        .expect("valid attachment placeholder regex")
        .find_iter(&markdown)
        .take(4)
        .map(|matched| matched.as_str().to_string())
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        return Err(format!(
            "原文仍有未解析的本地附件占位：{}",
            unresolved.join("、")
        ));
    }
    let unplaced = attachments
        .iter()
        .filter(|attachment| !referenced.contains(attachment.asset_id.trim()))
        .collect::<Vec<_>>();
    if !unplaced.is_empty() {
        markdown.push_str("\n\n## 原始附件\n\n");
        for attachment in unplaced {
            let label = attachment
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&attachment.asset_id);
            if is_image_attachment(attachment) {
                markdown.push_str(&format!("![[{}]]\n\n", attachment.relative_path));
            } else {
                markdown.push_str(&format!("- [[{}|{}]]\n", attachment.relative_path, label));
            }
        }
    }
    Ok((markdown, referenced))
}

fn validate_external_image_localization(
    raw_markdown: &str,
    failures: &[Value],
) -> Result<(), String> {
    if !failures.is_empty()
        || raw_markdown.contains("[外链图片本地化失败：")
        || raw_markdown.contains("external_image_localization_incomplete")
        || raw_markdown.contains("web_external_image_localization_incomplete")
    {
        return Err("外链图片尚未完整本地化，已阻止双库写入".to_string());
    }
    Ok(())
}

fn faithful_capture_markdown(
    materialized_source: &str,
    source_url: Option<&str>,
    source_type: &str,
) -> String {
    let mut markdown = String::from("---\n");
    markdown.push_str("yunspire_schema: yunspire.faithful-source.v1\n");
    markdown.push_str(&format!(
        "source_type: {}\n",
        serde_json::to_string(source_type).unwrap_or_else(|_| "\"unknown\"".to_string())
    ));
    if let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) {
        markdown.push_str(&format!(
            "source_url: {}\n",
            serde_json::to_string(source_url).unwrap_or_else(|_| "\"\"".to_string())
        ));
    }
    markdown.push_str(&format!("captured_at: {}\n", now_string()));
    markdown.push_str("content_role: faithful_original\n---\n\n## 来源证据\n\n");
    markdown.push_str(&format!("- 来源类型：{}\n", source_type.trim()));
    if let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) {
        markdown.push_str(&format!("- 原始来源：<{source_url}>\n"));
    }
    markdown.push_str("\n## 原文\n\n");
    markdown.push_str(materialized_source.trim());
    markdown.push('\n');
    markdown
}

fn markdown_callout_text(value: &str) -> String {
    value
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn capture_image_analysis_block(
    observation: &CaptureImageObservation,
    binding: &CaptureImageBinding,
    raw_vault_name: &str,
    attachment: &CaptureVaultAttachmentInput,
    reference_ids: &[String],
) -> Result<String, String> {
    let link = obsidian_open_uri(raw_vault_name, &attachment.relative_path)?;
    let references = if reference_ids.is_empty() {
        observation.reference_id.clone()
    } else {
        reference_ids.join("`, `")
    };
    let reference_label = if reference_ids.len() > 1 {
        "references"
    } else {
        "reference"
    };
    let mut lines = vec![
        format!(
            "> [!info] 图片理解 `{}` · {reference_label} `{references}`",
            observation.asset_id
        ),
        format!("> [查看忠实原图]({link})"),
        markdown_callout_text(&observation.observation),
    ];
    if !observation.text.is_empty() {
        lines.push(format!(
            "> **画面文字**\n{}",
            markdown_callout_text(&observation.text)
        ));
    }
    if !observation.context.is_empty() {
        lines.push(format!(
            "> **原文位置**\n{}",
            markdown_callout_text(&observation.context)
        ));
    }
    if !observation.evidence.is_empty() {
        lines.push(format!(
            "> **分析证据**\n{}",
            markdown_callout_text(&observation.evidence)
        ));
    }
    lines.push(format!("> **置信度** {:.2}", observation.confidence));
    let binding_json = serde_json::to_string_pretty(&serde_json::json!({
        "asset_id": binding.asset_id,
        "original_sha256": binding.original_sha256,
        "analysis_input_sha256": binding.analysis_sha256,
        "original_byte_length": binding.original_byte_length,
        "analysis_byte_length": binding.analysis_byte_length,
        "analysis_mime_type": binding.analysis_mime_type,
        "derived": binding.derived,
        "reference_ids": binding.reference_ids,
    }))
    .map_err(|error| format!("无法序列化图片 binding：{error}"))?;
    lines.push("> **结构化视觉输入绑定**".to_string());
    lines.push("> ```json".to_string());
    lines.extend(binding_json.lines().map(|line| format!("> {line}")));
    lines.push("> ```".to_string());
    Ok(format!("\n{}\n", lines.join("\n")))
}

fn strip_markdown_frontmatter(markdown: &str) -> &str {
    let normalized = markdown.trim_start_matches('\u{feff}');
    if !normalized.starts_with("---\n") {
        return normalized;
    }
    normalized[4..]
        .find("\n---\n")
        .map(|end| &normalized[4 + end + 5..])
        .unwrap_or(normalized)
}

fn wiki_link_target(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 120
        || value.chars().any(|character| {
            character.is_control() || matches!(character, '[' | ']' | '|' | '#' | '^')
        })
    {
        None
    } else {
        Some(value.to_string())
    }
}

fn find_related_agent_notes(
    agent_root: &Path,
    target_relative_path: &str,
    terms: &[String],
) -> Result<Vec<String>, String> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let normalized_terms = terms
        .iter()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| term.chars().count() >= 2)
        .collect::<Vec<_>>();
    let mut markdown = Vec::new();
    let mut attachments = 0;
    collect_files(agent_root, &mut markdown, &mut attachments)?;
    let mut candidates = Vec::new();
    for path in markdown {
        let relative = path
            .strip_prefix(agent_root)
            .map_err(|_| "相关笔记路径越过 Agent 库边界")?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == target_relative_path || relative == "Agent 库说明.md" {
            continue;
        }
        let bytes = match read_file_limited(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let content = String::from_utf8_lossy(&bytes);
        let title = title_from_markdown(&path, &content);
        let title_lower = title.to_lowercase();
        let content_lower = content.to_lowercase();
        let score = normalized_terms
            .iter()
            .map(|term| {
                if title_lower.contains(term) {
                    4u32
                } else if content_lower.contains(term) {
                    1u32
                } else {
                    0u32
                }
            })
            .sum::<u32>();
        if score > 0 {
            candidates.push((score, relative));
        }
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    Ok(candidates
        .into_iter()
        .take(8)
        .map(|(_, path)| path.trim_end_matches(".md").to_string())
        .collect())
}

fn ensure_default_agent_vault(agent_root: &Path) -> Result<(), String> {
    let expected = yunspire_vault_root()?.join("Agent 库");
    let expected = expected
        .canonicalize()
        .map_err(|error| format!("默认 Agent 库不可访问：{error}"))?;
    if expected != agent_root {
        return Err("Agent 理解稿只能写入云枢默认 Agent 库".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_agent_capture_markdown(
    title: &str,
    source_url: Option<&str>,
    source_type: &str,
    raw_vault_name: &str,
    raw_relative_path: Option<&str>,
    analysis: &Value,
    attachments: &[CaptureVaultAttachmentInput],
    related_notes: &[String],
) -> Result<String, String> {
    let analysis_markdown = capture_analysis_text(
        analysis
            .get("analysis_markdown")
            .or_else(|| analysis.get("analysisMarkdown"))
            .or_else(|| analysis.get("summary")),
    );
    if analysis_markdown.is_empty() {
        return Err("Agent 理解稿缺少模型生成的结构化原文".to_string());
    }
    let image_bindings = validate_capture_image_bindings(analysis, attachments)?;
    let observations = capture_image_observations(analysis)?;
    let mut agent_body = strip_markdown_frontmatter(&analysis_markdown).to_string();
    let mut observations_placed = HashSet::new();
    for attachment in attachments {
        let asset_id = capture_reference_id(&attachment.asset_id)?;
        let mut position_reference_ids = attachment_position_reference_ids(attachment)?;
        if position_reference_ids.is_empty() {
            position_reference_ids.push(asset_id.clone());
        }
        if is_image_attachment(attachment) {
            let binding = image_bindings
                .get(&asset_id)
                .ok_or_else(|| format!("图片附件 asset_id={asset_id} 缺少结构化 image binding"))?;
            let observation = observations
                .get(&asset_id)
                .or_else(|| {
                    position_reference_ids
                        .iter()
                        .find_map(|reference_id| observations.get(reference_id))
                })
                .ok_or_else(|| format!("模型没有返回图片 asset_id={asset_id} 的逐图分析"))?;
            let mut placed = false;
            for reference_id in &position_reference_ids {
                let replacement = capture_image_analysis_block(
                    observation,
                    binding,
                    raw_vault_name,
                    attachment,
                    std::slice::from_ref(reference_id),
                )?;
                placed |=
                    replace_attachment_reference_key(&mut agent_body, reference_id, &replacement)?;
            }

            // Older extractors used a shared attachment name for every occurrence. Preserve
            // those positions, but label the block with every known occurrence identifier.
            let legacy_replacement = capture_image_analysis_block(
                observation,
                binding,
                raw_vault_name,
                attachment,
                &position_reference_ids,
            )?;
            for legacy_key in [Some(asset_id.as_str()), attachment.name.as_deref()]
                .into_iter()
                .flatten()
            {
                if position_reference_ids
                    .iter()
                    .any(|reference_id| reference_id == legacy_key)
                {
                    continue;
                }
                placed |= replace_attachment_reference_key(
                    &mut agent_body,
                    legacy_key,
                    &legacy_replacement,
                )?;
            }
            if placed {
                observations_placed.insert(asset_id);
            }
        } else {
            let link = obsidian_open_uri(raw_vault_name, &attachment.relative_path)?;
            let label = attachment
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&asset_id);
            let replacement = format!("[原始附件：{label}]({link})");
            if replace_attachment_reference(&mut agent_body, attachment, &replacement)? {
                observations_placed.insert(asset_id);
            }
        }
    }

    let unplaced_image_observations = attachments
        .iter()
        .filter(|attachment| {
            is_image_attachment(attachment)
                && !observations_placed.contains(attachment.asset_id.trim())
        })
        .map(|attachment| {
            let mut reference_ids = attachment.reference_ids.clone();
            if let Some(reference_id) = attachment.reference_id.as_ref() {
                if !reference_ids.contains(reference_id) {
                    reference_ids.insert(0, reference_id.clone());
                }
            }
            if reference_ids.is_empty() {
                reference_ids.push(attachment.asset_id.clone());
            }
            let observation = observations
                .get(attachment.asset_id.trim())
                .or_else(|| {
                    reference_ids
                        .iter()
                        .find_map(|reference_id| observations.get(reference_id.trim()))
                })
                .ok_or_else(|| {
                    format!(
                        "模型没有返回图片 asset_id={} 的逐图分析",
                        attachment.asset_id
                    )
                })?;
            let binding = image_bindings
                .get(attachment.asset_id.trim())
                .ok_or_else(|| {
                    format!(
                        "图片附件 asset_id={} 缺少结构化 image binding",
                        attachment.asset_id
                    )
                })?;
            capture_image_analysis_block(
                observation,
                binding,
                raw_vault_name,
                attachment,
                &reference_ids,
            )
        })
        .collect::<Result<Vec<_>, String>>()?;

    let tags = capture_analysis_strings(analysis, "tags", "tags");
    let entities = capture_analysis_strings(analysis, "entities", "entities");
    let key_points = capture_analysis_strings(analysis, "key_points", "keyPoints");
    let source_note_uri = raw_relative_path
        .map(|path| obsidian_open_uri(raw_vault_name, path))
        .transpose()?;
    let mut markdown = String::new();
    markdown.push_str("---\n");
    markdown.push_str("yunspire_schema: yunspire.agent-understood-source.v1\n");
    markdown.push_str("content_role: analyzed_original\n");
    markdown.push_str(&format!(
        "source_type: {}\n",
        serde_json::to_string(source_type).unwrap_or_else(|_| "\"unknown\"".to_string())
    ));
    if let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) {
        markdown.push_str(&format!(
            "source_url: {}\n",
            serde_json::to_string(source_url).unwrap_or_else(|_| "\"\"".to_string())
        ));
    }
    if let Some(raw_relative_path) = raw_relative_path {
        markdown.push_str(&format!(
            "raw_vault: {}\nraw_note: {}\n",
            serde_json::to_string(raw_vault_name).unwrap_or_else(|_| "\"\"".to_string()),
            serde_json::to_string(raw_relative_path).unwrap_or_else(|_| "\"\"".to_string())
        ));
    }
    markdown.push_str("knowledge_association: obsidian-tags-and-wikilinks\n");
    markdown.push_str("tags:\n");
    if tags.is_empty() {
        markdown.push_str("  - \"未分类\"\n");
    } else {
        for tag in &tags {
            markdown.push_str(&format!(
                "  - {}\n",
                serde_json::to_string(tag).unwrap_or_else(|_| "\"未分类\"".to_string())
            ));
        }
    }
    markdown.push_str("---\n\n");
    markdown.push_str(&format!("# {title}\n\n"));
    markdown.push_str("## 来源证据\n\n");
    if let Some(source_note_uri) = source_note_uri {
        markdown.push_str(&format!(
            "- 忠实原文：[在 {raw_vault_name} 中打开]({source_note_uri})\n"
        ));
    }
    if let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) {
        markdown.push_str(&format!("- 原始来源：<{source_url}>\n"));
    }
    markdown.push_str("\n## 分析内容\n\n");
    markdown.push_str(agent_body.trim());
    if !unplaced_image_observations.is_empty() {
        markdown.push_str("\n\n### 逐图理解\n");
        for block in unplaced_image_observations {
            markdown.push_str(&block);
        }
    }
    markdown.push('\n');
    if !key_points.is_empty() {
        markdown.push_str("\n## 关键点\n\n");
        for point in key_points {
            markdown.push_str(&format!("- {point}\n"));
        }
    }

    let relations = analysis
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|relation| {
            let source = relation
                .get("source_id")
                .or_else(|| relation.get("sourceId"))?
                .as_str()?
                .trim();
            let target = relation
                .get("target_id")
                .or_else(|| relation.get("targetId"))?
                .as_str()?
                .trim();
            let kind = relation.get("relation")?.as_str()?.trim();
            let evidence = relation.get("evidence")?.as_str()?.trim();
            if source.is_empty() || target.is_empty() || kind.is_empty() || evidence.is_empty() {
                return None;
            }
            let confidence = relation
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            Some(format!(
                "- `{source}` -> `{target}`：{kind}（证据：{evidence}；置信度 {confidence:.2}）"
            ))
        })
        .collect::<Vec<_>>();
    if !relations.is_empty() {
        markdown.push_str("\n## 文档内关系\n\n");
        markdown.push_str(&relations.join("\n"));
        markdown.push('\n');
    }

    let concept_links = tags
        .iter()
        .chain(entities.iter())
        .filter_map(|value| wiki_link_target(value))
        .collect::<BTreeSet<_>>();
    if !concept_links.is_empty() || !related_notes.is_empty() {
        markdown.push_str("\n## 知识关联\n\n");
        if !concept_links.is_empty() {
            markdown.push_str("### 主题与对象\n\n");
            for target in concept_links {
                markdown.push_str(&format!("- [[{target}]]\n"));
            }
            markdown.push('\n');
        }
        if !related_notes.is_empty() {
            markdown.push_str("### 相关笔记\n\n");
            for target in related_notes {
                markdown.push_str(&format!("- [[{target}]]\n"));
            }
        }
    }
    Ok(markdown.trim_end().to_string() + "\n")
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn prepare_note_write(
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    content: String,
    analysis_receipt: String,
    expected_hash: Option<String>,
    expected_absent: Option<bool>,
    write_manifest_digest: Option<String>,
    operation_context: Option<OperationContext>,
) -> Result<WritePreview, String> {
    prepare_note_write_inner(
        analysis_state.inner(),
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        vault_id,
        relative_path,
        content,
        analysis_receipt,
        expected_hash,
        expected_absent,
        write_manifest_digest,
        operation_context,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn prepare_note_write_from_durable_asset(
    app: AppHandle,
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    durable_asset_id: String,
    analysis_receipt: String,
    expected_hash: Option<String>,
    expected_absent: Option<bool>,
    write_manifest_digest: Option<String>,
    operation_context: Option<OperationContext>,
) -> Result<WritePreview, String> {
    let (descriptor, source_path) =
        resolve_ready_asset_path(&app, database.inner(), durable_asset_id.trim())?;
    if !descriptor
        .mime_type
        .to_ascii_lowercase()
        .starts_with("text/")
    {
        return Err("Vault Markdown 写入只接受 UTF-8 文本耐久资产".to_string());
    }
    prepare_note_write_source_inner(
        analysis_state.inner(),
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        vault_id,
        relative_path,
        PendingNoteSource::Durable(source_path),
        analysis_receipt,
        expected_hash,
        expected_absent,
        write_manifest_digest,
        operation_context,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_note_write_inner(
    analysis_state: &ModelAnalysisState,
    state: &ObsidianAdapterState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    vault_id: String,
    relative_path: String,
    content: String,
    analysis_receipt: String,
    expected_hash: Option<String>,
    expected_absent: Option<bool>,
    write_manifest_digest: Option<String>,
    operation_context: Option<OperationContext>,
) -> Result<WritePreview, String> {
    prepare_note_write_source_inner(
        analysis_state,
        state,
        database,
        ticket_state,
        vault_id,
        relative_path,
        PendingNoteSource::Text(content),
        analysis_receipt,
        expected_hash,
        expected_absent,
        write_manifest_digest,
        operation_context,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_note_write_source_inner(
    analysis_state: &ModelAnalysisState,
    state: &ObsidianAdapterState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    vault_id: String,
    relative_path: String,
    source: PendingNoteSource,
    analysis_receipt: String,
    mut expected_hash: Option<String>,
    expected_absent: Option<bool>,
    write_manifest_digest: Option<String>,
    operation_context: Option<OperationContext>,
) -> Result<WritePreview, String> {
    let write_manifest_digest = analysis_state.validate_write_manifest(
        "local",
        &analysis_receipt,
        write_manifest_digest.as_deref(),
    )?;
    let expected_absent = expected_absent.unwrap_or(false);
    if expected_absent && expected_hash.is_some() {
        return Err("expectedHash 与 expectedAbsent 不能同时设置".to_string());
    }
    if write_manifest_digest.is_some() {
        expected_hash = expected_hash
            .as_deref()
            .map(|value| normalize_capture_sha256(value, "expectedHash"))
            .transpose()?;
        if !expected_absent && expected_hash.is_none() {
            return Err("绑定写入清单的笔记必须声明 expectedHash 或 expectedAbsent".to_string());
        }
    }
    let workspace_scope = database.local_workspace_scope()?;
    let (_, root) = resolve_vault(&vault_id)?;
    let (target, normalized_relative) = resolve_note_target(&root, &relative_path, true)?;
    ensure_long_term_memory_mutation_allowed(&normalized_relative)?;
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &normalized_relative)?;
    let is_new_file = !target.exists();
    let previous_hash = (!is_new_file)
        .then(|| hash_file_streaming(&target))
        .transpose()?;
    if expected_absent && !is_new_file {
        return Err("笔记本应不存在，但目标已被 Obsidian 或其他程序创建".to_string());
    }
    if let Some(expected) = expected_hash.as_ref() {
        if previous_hash.as_ref() != Some(expected) {
            return Err("笔记已被 Obsidian 或其他程序修改，请重新读取后再生成变更".to_string());
        }
    }
    let previous_byte_length = if is_new_file {
        0
    } else {
        fs::metadata(&target)
            .map(|metadata| metadata.len())
            .map_err(|error| format!("无法读取现有笔记元数据：{error}"))?
    };
    let previous_line_count = if is_new_file {
        0
    } else {
        validate_utf8_file_and_count_lines(&target)
            .map_err(|_| "现有笔记不是有效 UTF-8 Markdown，无法生成安全写入预览".to_string())?
    };
    let next_hash = source.content_hash()?;
    let next_byte_length = source.byte_length()?;
    let next_line_count = source.line_count()?;
    let full_diff = previous_byte_length <= FULL_NOTE_DIFF_PREVIEW_BYTES
        && next_byte_length <= FULL_NOTE_DIFF_PREVIEW_BYTES;
    let (diff, diff_mode) = if full_diff {
        let previous_text = if is_new_file {
            String::new()
        } else {
            fs::read_to_string(&target)
                .map_err(|_| "现有笔记不是有效 UTF-8 Markdown，无法生成安全写入预览".to_string())?
        };
        let next_text = source.read_to_string()?;
        (
            TextDiff::from_lines(&previous_text, &next_text)
                .unified_diff()
                .context_radius(3)
                .header(
                    &format!("a/{normalized_relative}"),
                    &format!("b/{normalized_relative}"),
                )
                .to_string(),
            "full".to_string(),
        )
    } else {
        (
            bounded_note_diff_preview(
                &normalized_relative,
                previous_hash.as_deref(),
                &next_hash,
                previous_byte_length,
                next_byte_length,
                previous_line_count,
                next_line_count,
            ),
            "bounded".to_string(),
        )
    };
    let approval_id = Uuid::new_v4().to_string();
    let effect_digest =
        write_effect_digest_from_hash("note", &vault_id, &normalized_relative, &next_hash);
    let bound_execution = bind_write_execution_ticket(
        database,
        ticket_state,
        WriteExecutionBinding {
            workspace_scope: &workspace_scope,
            operation_context,
            vault_id: &vault_id,
            relative_path: &normalized_relative,
            approval_id: &approval_id,
            effect_digest: &effect_digest,
        },
    )?;
    let mut pending_writes = state
        .pending_writes
        .lock()
        .map_err(|_| "写入审批状态不可用".to_string())?;
    pending_writes.retain(|_, pending| {
        pending
            .created_at
            .elapsed()
            .map(|elapsed| elapsed <= WRITE_APPROVAL_TTL)
            .unwrap_or(false)
    });
    if pending_writes.len() >= MAX_PENDING_WRITES {
        return Err("待审批写入数量已达到上限，请先处理或拒绝现有审批".to_string());
    }
    pending_writes.insert(
        approval_id.clone(),
        PendingWrite {
            task_id: bound_execution.task_id,
            trace_id: bound_execution.trace_id,
            vault_id: vault_id.clone(),
            vault_path: root,
            relative_path: normalized_relative.clone(),
            target_path: target,
            source,
            content_hash: next_hash.clone(),
            expected_hash,
            expected_absent,
            previous_hash: previous_hash.clone(),
            analysis_receipt,
            write_manifest_digest,
            execution_ticket: bound_execution.execution_ticket,
            effect_digest,
            created_at: SystemTime::now(),
        },
    );

    Ok(WritePreview {
        approval_id,
        vault_id,
        relative_path: normalized_relative,
        previous_hash: previous_hash.clone(),
        next_hash,
        is_new_file,
        diff,
        diff_mode,
        previous_byte_length,
        next_byte_length,
        previous_line_count,
        next_line_count,
    })
}

#[tauri::command]
pub fn commit_note_write(
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    app: AppHandle,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    approval_id: String,
) -> Result<WriteCommitResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let pending = state
        .pending_writes
        .lock()
        .map_err(|_| "写入审批状态不可用".to_string())?
        .get(&approval_id)
        .cloned()
        .ok_or_else(|| "审批令牌不存在或已经失效".to_string())?;
    if pending.write_manifest_digest.is_some() {
        return Err("绑定写入清单的笔记必须通过整批提交".to_string());
    }
    analysis_state.validate_write_manifest("local", &pending.analysis_receipt, None)?;
    if pending
        .created_at
        .elapsed()
        .map(|elapsed| elapsed > WRITE_APPROVAL_TTL)
        .unwrap_or(true)
    {
        return Err("审批令牌已过期，请重新生成文件级 diff".to_string());
    }
    database.ensure_vault_write_allowed(
        &workspace_scope,
        &pending.vault_id,
        &pending.relative_path,
    )?;
    let (_, current_root) = resolve_vault(&pending.vault_id)?;
    if current_root != pending.vault_path {
        return Err("Vault 路径在审批后发生变化，已拒绝写入".to_string());
    }
    let current_hash = if pending.target_path.exists() {
        Some(hash_file_streaming(&pending.target_path)?)
    } else {
        None
    };
    if current_hash != pending.previous_hash
        || (pending.expected_absent && current_hash.is_some())
        || pending
            .expected_hash
            .as_ref()
            .is_some_and(|expected| current_hash.as_ref() != Some(expected))
    {
        return Err("笔记在审批期间发生变化，已拒绝覆盖".to_string());
    }
    if pending.source.content_hash()? != pending.content_hash {
        return Err("待写入耐久正文在审批期间发生变化，已拒绝提交".to_string());
    }
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        pending.task_id.as_deref(),
        pending.trace_id.as_deref(),
    )?;

    let ticket_token = pending.execution_ticket.as_deref();
    if ticket_token.is_some() {
        let task_id = pending
            .task_id
            .as_deref()
            .ok_or_else(|| "执行票据缺少绑定的原生任务".to_string())?;
        database.ensure_runtime_task_authorized(
            &workspace_scope,
            task_id,
            VAULT_WRITE_CAPABILITIES,
            VAULT_WRITE_OPERATIONS,
            Some(&pending.vault_id),
            &["running"],
        )?;
    }

    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    let checkpoint_dir = app_data.join("checkpoints").join(&approval_id);
    fs::create_dir_all(&checkpoint_dir).map_err(|error| format!("无法创建检查点：{error}"))?;
    let checkpoint_path = checkpoint_dir.join("before.md");
    if pending.target_path.exists() {
        fs::copy(&pending.target_path, &checkpoint_path)
            .map_err(|error| format!("无法保存写入前检查点：{error}"))?;
    } else {
        fs::write(&checkpoint_path, b"")
            .map_err(|error| format!("无法保存新文件检查点：{error}"))?;
    }

    let parent = pending.target_path.parent().ok_or("笔记缺少父目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建笔记目录：{error}"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("无法规范化笔记目录：{error}"))?;
    if !canonical_parent.starts_with(&current_root) {
        return Err("笔记目录在审批后越过 Vault 边界".to_string());
    }
    if let Some(token) = ticket_token {
        ticket_state.begin_commit(
            token,
            &workspace_scope,
            pending
                .task_id
                .as_deref()
                .ok_or_else(|| "执行票据缺少绑定的原生任务".to_string())?,
            &[(&approval_id, &pending.effect_digest)],
        )?;
    }
    let consumed_receipt = match analysis_state.consume("local", &pending.analysis_receipt) {
        Ok(receipt) => receipt,
        Err(error) => {
            if let Some(token) = ticket_token {
                ticket_state.release_commit(token);
            }
            return Err(error);
        }
    };
    let write_result = match &pending.source {
        PendingNoteSource::Text(content) => {
            atomic_write_file(&pending.target_path, content.as_bytes())
        }
        PendingNoteSource::Durable(source_path) => {
            atomic_copy_file(&pending.target_path, source_path)
        }
    };
    if let Err(error) = write_result {
        analysis_state.restore(&pending.analysis_receipt, consumed_receipt);
        if let Some(token) = ticket_token {
            ticket_state.release_commit(token);
        }
        return Err(error);
    }

    let committed_at = now_string();
    let content_hash = pending.content_hash.clone();
    let event = OperationEvent {
        id: Uuid::new_v4().to_string(),
        task_id: pending.task_id.clone(),
        trace_id: Some(trace_id.clone()),
        event_type: "vault.note.write".to_string(),
        state: "success".to_string(),
        created_at: committed_at.clone(),
        vault_id: Some(pending.vault_id.clone()),
        relative_path: Some(pending.relative_path.clone()),
        detail: format!("审批 {approval_id} 已提交，检查点已创建"),
    };
    if let Err(error) = database.append_operation_event(&event) {
        let rollback = if pending.previous_hash.is_some() {
            atomic_copy_file(&pending.target_path, &checkpoint_path)
        } else if pending.target_path.exists() {
            fs::remove_file(&pending.target_path)
                .map_err(|remove_error| format!("无法移除新建笔记：{remove_error}"))
        } else {
            Ok(())
        };
        analysis_state.restore(&pending.analysis_receipt, consumed_receipt);
        if let Some(token) = ticket_token {
            ticket_state.release_commit(token);
        }
        return match rollback {
            Ok(()) => Err(format!("写入审计失败，笔记已回滚：{error}")),
            Err(rollback_error) => Err(format!(
                "写入审计失败且笔记回滚失败：{error}；{rollback_error}"
            )),
        };
    }
    if let Err(error) = database.enqueue_vault_index_path_with_trace(
        &pending.vault_id,
        &pending.vault_path,
        &pending.target_path,
        &trace_id,
    ) {
        log::warn!(
            "笔记已提交，但无法继承 Trace 入队索引 {}：{error}",
            pending.relative_path
        );
    }
    if let Some(token) = ticket_token {
        ticket_state.complete_commit(token)?;
    }
    state
        .pending_writes
        .lock()
        .map_err(|_| "写入审批状态不可用".to_string())?
        .remove(&approval_id);

    Ok(WriteCommitResult {
        approval_id,
        vault_id: pending.vault_id,
        relative_path: pending.relative_path,
        content_hash,
        checkpoint_path: checkpoint_path.to_string_lossy().into_owned(),
        committed_at,
    })
}

#[tauri::command]
pub fn discard_note_write(
    state: State<'_, ObsidianAdapterState>,
    approval_id: String,
) -> Result<bool, String> {
    Ok(state
        .pending_writes
        .lock()
        .map_err(|_| "写入审批状态不可用".to_string())?
        .remove(&approval_id)
        .is_some())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn prepare_asset_write(
    app: AppHandle,
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    relative_path: String,
    content_base64: Option<String>,
    staged_attachment_id: Option<String>,
    durable_asset_id: Option<String>,
    expected_sha256: Option<String>,
    analysis_receipt: String,
    task_id: Option<String>,
    trace_id: Option<String>,
    execution_ticket: Option<String>,
) -> Result<AssetWritePreview, String> {
    let durable_asset = durable_asset_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|asset_id| resolve_ready_asset_path(&app, database.inner(), asset_id))
        .transpose()?;
    prepare_asset_write_inner(
        analysis_state.inner(),
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        vault_id,
        relative_path,
        content_base64,
        staged_attachment_id,
        durable_asset,
        expected_sha256,
        analysis_receipt,
        task_id,
        trace_id,
        execution_ticket,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_asset_write_inner(
    analysis_state: &ModelAnalysisState,
    state: &ObsidianAdapterState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    vault_id: String,
    relative_path: String,
    content_base64: Option<String>,
    staged_attachment_id: Option<String>,
    durable_asset: Option<(crate::durable_asset::DurableAssetDescriptor, PathBuf)>,
    expected_sha256: Option<String>,
    analysis_receipt: String,
    task_id: Option<String>,
    trace_id: Option<String>,
    execution_ticket: Option<String>,
) -> Result<AssetWritePreview, String> {
    analysis_state.validate_unbound_write_manifest("local", &analysis_receipt)?;
    let workspace_scope = database.local_workspace_scope()?;
    let (_, root) = resolve_vault(&vault_id)?;
    let (target, normalized_relative) = resolve_asset_target(&root, &relative_path)?;
    database.ensure_vault_write_allowed(&workspace_scope, &vault_id, &normalized_relative)?;
    let previous_hash = if target.exists() {
        Some(hash_file_streaming(&target)?)
    } else {
        None
    };
    let approval_id = Uuid::new_v4().to_string();
    let inline = content_base64.filter(|value| !value.is_empty());
    let staged = staged_attachment_id.filter(|value| !value.trim().is_empty());
    if usize::from(inline.is_some())
        + usize::from(staged.is_some())
        + usize::from(durable_asset.is_some())
        != 1
    {
        return Err("附件必须且只能提供 Base64 内容、采集暂存 ID 或耐久资产 ID 之一".to_string());
    }
    let normalized_expected_sha256 = expected_sha256
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| normalize_capture_sha256(value, "附件 expected_sha256"))
        .transpose()?;
    let (source, content_hash, byte_length) = if let Some(encoded) = inline {
        let content = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| "附件内容不是有效的 Base64".to_string())?;
        if content.is_empty() {
            return Err("附件内容不能为空".to_string());
        }
        let content_hash = hash_bytes(&content);
        let byte_length = content.len() as u64;
        if normalized_expected_sha256
            .as_deref()
            .is_some_and(|expected| expected != content_hash)
        {
            return Err("附件哈希与提取结果不一致".to_string());
        }
        (
            PendingAssetSource::Bytes(content),
            content_hash,
            byte_length,
        )
    } else if let Some(token) = staged {
        let path = claim_staged_capture_attachment(&token, &approval_id)?;
        let byte_length = fs::metadata(&path)
            .map_err(|error| {
                remove_claimed_capture_attachment(&path);
                format!("无法读取暂存附件元数据：{error}")
            })?
            .len();
        if byte_length == 0 {
            remove_claimed_capture_attachment(&path);
            return Err("暂存附件内容不能为空".to_string());
        }
        let content_hash = match hash_file_streaming(&path) {
            Ok(hash) => hash,
            Err(error) => {
                remove_claimed_capture_attachment(&path);
                return Err(error);
            }
        };
        if normalized_expected_sha256
            .as_deref()
            .is_some_and(|expected| expected != content_hash)
        {
            remove_claimed_capture_attachment(&path);
            return Err("暂存附件哈希与提取结果不一致".to_string());
        }
        (PendingAssetSource::Staged(path), content_hash, byte_length)
    } else {
        let (descriptor, path) = durable_asset.expect("durable asset source checked");
        let byte_length = fs::metadata(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    "source_missing: 耐久资产文件不存在".to_string()
                } else {
                    format!("无法读取耐久资产元数据：{error}")
                }
            })?
            .len();
        if byte_length == 0 || byte_length != descriptor.byte_length {
            return Err("耐久资产字节数与资产账本不一致".to_string());
        }
        let content_hash = hash_file_streaming(&path)?;
        let ledger_hash = descriptor
            .sha256
            .as_deref()
            .map(|value| normalize_capture_sha256(value, "耐久资产 sha256"))
            .transpose()?;
        if ledger_hash
            .as_deref()
            .is_some_and(|expected| expected != content_hash)
        {
            return Err("耐久资产哈希与资产账本不一致".to_string());
        }
        if normalized_expected_sha256
            .as_deref()
            .is_some_and(|expected| expected != content_hash)
        {
            return Err("耐久资产哈希与写入请求不一致".to_string());
        }
        (PendingAssetSource::Durable(path), content_hash, byte_length)
    };
    let effect_digest =
        write_effect_digest_from_hash("asset", &vault_id, &normalized_relative, &content_hash);
    let bound_execution = match bind_write_execution_ticket(
        database,
        ticket_state,
        WriteExecutionBinding {
            workspace_scope: &workspace_scope,
            operation_context: Some(OperationContext {
                task_id,
                trace_id,
                execution_ticket,
            }),
            vault_id: &vault_id,
            relative_path: &normalized_relative,
            approval_id: &approval_id,
            effect_digest: &effect_digest,
        },
    ) {
        Ok(bound) => bound,
        Err(error) => {
            if let PendingAssetSource::Staged(path) = &source {
                remove_claimed_capture_attachment(path);
            }
            return Err(error);
        }
    };
    let pending = PendingAssetWrite {
        task_id: bound_execution.task_id,
        trace_id: bound_execution.trace_id,
        vault_id: vault_id.clone(),
        vault_path: root,
        relative_path: normalized_relative.clone(),
        target_path: target,
        source,
        content_hash,
        previous_hash: previous_hash.clone(),
        analysis_receipt,
        execution_ticket: bound_execution.execution_ticket,
        effect_digest,
        created_at: SystemTime::now(),
    };
    let mut pending_assets = match state.pending_assets.lock() {
        Ok(value) => value,
        Err(_) => {
            if let PendingAssetSource::Staged(path) = &pending.source {
                remove_claimed_capture_attachment(path);
            }
            return Err("附件审批状态不可用".to_string());
        }
    };
    pending_assets.insert(approval_id.clone(), pending);
    Ok(AssetWritePreview {
        approval_id,
        vault_id,
        relative_path: normalized_relative,
        previous_hash: previous_hash.clone(),
        byte_length,
        is_new_file: previous_hash.is_none(),
    })
}

fn discard_prepared_capture_writes(
    state: &ObsidianAdapterState,
    note_approval_ids: &[String],
    asset_approval_ids: &[String],
) {
    if let Ok(mut pending) = state.pending_writes.lock() {
        for approval_id in note_approval_ids {
            pending.remove(approval_id);
        }
    }
    if let Ok(mut pending) = state.pending_assets.lock() {
        for approval_id in asset_approval_ids {
            if let Some(asset) = pending.remove(approval_id) {
                if let PendingAssetSource::Staged(path) = asset.source {
                    remove_claimed_capture_attachment(&path);
                }
            }
        }
    }
}

#[tauri::command]
pub fn prepare_capture_vault_writes(
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    input: CaptureVaultWriteInput,
) -> Result<CaptureVaultWritePreview, String> {
    prepare_capture_vault_writes_inner(
        analysis_state.inner(),
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        input,
    )
}

fn prepare_capture_vault_writes_inner(
    analysis_state: &ModelAnalysisState,
    state: &ObsidianAdapterState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    mut input: CaptureVaultWriteInput,
) -> Result<CaptureVaultWritePreview, String> {
    analysis_state.validate_analysis("local", &input.analysis_receipt, &input.analysis)?;
    validate_external_image_localization(&input.raw_markdown, &input.external_image_failures)?;
    let title = capture_safe_title(&input.title);
    let (raw_vault_name, raw_root) = resolve_vault(&input.raw_vault_id)?;
    let (_, agent_root) = resolve_vault(&input.agent_vault_id)?;
    ensure_default_agent_vault(&agent_root)?;

    let requested_agent_path = input
        .agent_relative_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("资料库/原文/{title}.md"));
    let agent_relative_path = validate_relative_markdown_path(&requested_agent_path)?
        .to_string_lossy()
        .replace('\\', "/");
    if !agent_relative_path.starts_with("资料库/原文/") {
        return Err("Agent 理解稿必须写入 Agent 库的 资料库/原文 目录".to_string());
    }

    let requested_raw_path = validate_relative_markdown_path(&input.raw_relative_path)?
        .to_string_lossy()
        .replace('\\', "/");
    let raw_note_included = raw_root != agent_root;
    let raw_relative_path = requested_raw_path;

    let mut asset_ids = HashSet::new();
    let mut asset_paths = HashSet::new();
    for attachment in &mut input.attachments {
        attachment.asset_id = capture_reference_id(&attachment.asset_id)?;
        if !asset_ids.insert(attachment.asset_id.clone()) {
            return Err(format!(
                "采集批次包含重复的 asset_id={}",
                attachment.asset_id
            ));
        }
        attachment.reference_id = attachment
            .reference_id
            .as_deref()
            .map(capture_reference_id)
            .transpose()?;
        attachment.reference_ids = attachment
            .reference_ids
            .iter()
            .map(|value| capture_reference_id(value))
            .collect::<Result<Vec<_>, _>>()?;
        attachment.reference_ids.sort();
        attachment.reference_ids.dedup();
        attachment.relative_path = validate_relative_asset_path(&attachment.relative_path)?
            .to_string_lossy()
            .replace('\\', "/");
        if !asset_paths.insert(attachment.relative_path.clone()) {
            return Err("采集批次包含重复的原始附件路径".to_string());
        }
    }
    validate_capture_attachment_reference_owners(&input.raw_markdown, &input.attachments)?;

    let (materialized_raw_markdown, _) = materialize_capture_raw_markdown(
        &input.raw_markdown,
        &input.attachments,
        input.source_type.trim(),
    )?;
    let raw_markdown = faithful_capture_markdown(
        &materialized_raw_markdown,
        input.source_url.as_deref(),
        input.source_type.trim(),
    );
    let tags = capture_analysis_strings(&input.analysis, "tags", "tags");
    let entities = capture_analysis_strings(&input.analysis, "entities", "entities");
    let related_terms = tags
        .iter()
        .chain(entities.iter())
        .cloned()
        .collect::<Vec<_>>();
    let related_notes =
        find_related_agent_notes(&agent_root, &agent_relative_path, &related_terms)?;
    let agent_markdown = build_agent_capture_markdown(
        &title,
        input.source_url.as_deref(),
        input.source_type.trim(),
        &raw_vault_name,
        raw_note_included.then_some(raw_relative_path.as_str()),
        &input.analysis,
        &input.attachments,
        &related_notes,
    )?;
    let image_bindings = capture_image_bindings(&input.analysis)?;

    let mut note_previews = Vec::new();
    let mut asset_previews = Vec::new();
    let operation_context = input.operation_context.clone();
    let preparation = (|| -> Result<(), String> {
        if raw_note_included {
            let raw_preview = prepare_note_write_inner(
                analysis_state,
                state,
                database,
                ticket_state,
                input.raw_vault_id.clone(),
                raw_relative_path.clone(),
                raw_markdown,
                input.analysis_receipt.clone(),
                None,
                Some(true),
                None,
                operation_context.clone(),
            )?;
            let raw_is_new_file = raw_preview.is_new_file;
            let raw_conflict_path = raw_preview.relative_path.clone();
            note_previews.push(raw_preview);
            if !raw_is_new_file {
                return Err(format!(
                    "采集目标已存在，已阻止覆盖忠实原文：{raw_conflict_path}"
                ));
            }
        }

        let agent_preview = prepare_note_write_inner(
            analysis_state,
            state,
            database,
            ticket_state,
            input.agent_vault_id.clone(),
            agent_relative_path.clone(),
            agent_markdown.clone(),
            input.analysis_receipt.clone(),
            None,
            Some(true),
            None,
            operation_context.clone(),
        )?;
        let agent_is_new_file = agent_preview.is_new_file;
        let agent_conflict_path = agent_preview.relative_path.clone();
        note_previews.push(agent_preview);
        if !agent_is_new_file {
            return Err(format!(
                "采集目标已存在，已阻止覆盖 Agent 理解稿：{agent_conflict_path}"
            ));
        }

        if raw_note_included {
            for attachment in input.attachments {
                let image_binding = is_image_attachment(&attachment)
                    .then(|| {
                        image_bindings
                            .get(&attachment.asset_id)
                            .cloned()
                            .ok_or_else(|| {
                                format!(
                                    "图片附件 asset_id={} 缺少结构化 image binding",
                                    attachment.asset_id
                                )
                            })
                    })
                    .transpose()?;
                let asset_preview = prepare_asset_write_inner(
                    analysis_state,
                    state,
                    database,
                    ticket_state,
                    input.raw_vault_id.clone(),
                    attachment.relative_path,
                    attachment.content_base64,
                    attachment.staged_attachment_id,
                    None,
                    attachment.expected_sha256,
                    input.analysis_receipt.clone(),
                    operation_context
                        .as_ref()
                        .and_then(|context| context.task_id.clone()),
                    operation_context
                        .as_ref()
                        .and_then(|context| context.trace_id.clone()),
                    operation_context
                        .as_ref()
                        .and_then(|context| context.execution_ticket.clone()),
                )?;
                let image_byte_length_conflict = image_binding.as_ref().is_some_and(|binding| {
                    asset_preview.byte_length != binding.original_byte_length
                });
                let asset_is_new_file = asset_preview.is_new_file;
                let asset_conflict_path = asset_preview.relative_path.clone();
                asset_previews.push(asset_preview);
                if image_byte_length_conflict {
                    return Err(format!(
                        "图片附件 asset_id={} 的实际字节数与 image binding 冲突",
                        image_binding
                            .as_ref()
                            .map(|binding| binding.asset_id.as_str())
                            .unwrap_or_default()
                    ));
                }
                if !asset_is_new_file {
                    return Err(format!(
                        "采集目标已存在，已阻止覆盖原始附件：{asset_conflict_path}"
                    ));
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = preparation {
        let note_ids = note_previews
            .iter()
            .map(|preview| preview.approval_id.clone())
            .collect::<Vec<_>>();
        let asset_ids = asset_previews
            .iter()
            .map(|preview| preview.approval_id.clone())
            .collect::<Vec<_>>();
        discard_prepared_capture_writes(state, &note_ids, &asset_ids);
        return Err(error);
    }

    Ok(CaptureVaultWritePreview {
        raw_vault_id: input.raw_vault_id,
        agent_vault_id: input.agent_vault_id,
        raw_relative_path,
        agent_relative_path,
        raw_note_included,
        note_previews,
        asset_previews,
        agent_markdown,
        related_notes,
    })
}

#[tauri::command]
pub fn discard_asset_write(
    state: State<'_, ObsidianAdapterState>,
    approval_id: String,
) -> Result<bool, String> {
    let pending = state
        .pending_assets
        .lock()
        .map_err(|_| "附件审批状态不可用".to_string())?
        .remove(&approval_id);
    if let Some(pending) = &pending {
        if let PendingAssetSource::Staged(path) = &pending.source {
            remove_claimed_capture_attachment(path);
        }
    }
    Ok(pending.is_some())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn commit_capture_batch(
    analysis_state: State<'_, ModelAnalysisState>,
    ticket_state: State<'_, ExecutionTicketState>,
    app: AppHandle,
    state: State<'_, ObsidianAdapterState>,
    database: State<'_, RuntimeDatabase>,
    note_approval_ids: Vec<String>,
    asset_approval_ids: Vec<String>,
    batch_kind: Option<String>,
) -> Result<Vec<WriteCommitResult>, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
    commit_capture_batch_inner(
        &app_data,
        analysis_state.inner(),
        state.inner(),
        database.inner(),
        Some(ticket_state.inner()),
        note_approval_ids,
        asset_approval_ids,
        batch_kind,
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_capture_batch_inner(
    app_data: &Path,
    analysis_state: &ModelAnalysisState,
    state: &ObsidianAdapterState,
    database: &RuntimeDatabase,
    ticket_state: Option<&ExecutionTicketState>,
    note_approval_ids: Vec<String>,
    asset_approval_ids: Vec<String>,
    batch_kind: Option<String>,
) -> Result<Vec<WriteCommitResult>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let batch_kind = capture_batch_kind(batch_kind.as_deref())?;
    if note_approval_ids.is_empty() {
        return Err(format!("{}批次至少需要一个 Markdown 审批", batch_kind.0));
    }
    let notes = state
        .pending_writes
        .lock()
        .map_err(|_| "写入审批状态不可用".to_string())?;
    let assets = state
        .pending_assets
        .lock()
        .map_err(|_| "附件审批状态不可用".to_string())?;
    let mut batch = Vec::with_capacity(note_approval_ids.len() + asset_approval_ids.len());
    for approval_id in &note_approval_ids {
        let pending = notes
            .get(approval_id)
            .cloned()
            .ok_or_else(|| format!("Markdown 审批令牌不存在或已经失效：{approval_id}"))?;
        batch.push((approval_id.clone(), BatchPendingWrite::Note(pending)));
    }
    for approval_id in &asset_approval_ids {
        let pending = assets
            .get(approval_id)
            .cloned()
            .ok_or_else(|| format!("附件审批令牌不存在或已经失效：{approval_id}"))?;
        batch.push((approval_id.clone(), BatchPendingWrite::Asset(pending)));
    }
    drop(notes);
    drop(assets);

    let analysis_receipt = batch
        .first()
        .map(|(_, pending)| pending.analysis_receipt().to_string())
        .ok_or_else(|| format!("{}批次为空", batch_kind.0))?;
    if batch
        .iter()
        .any(|(_, pending)| pending.analysis_receipt() != analysis_receipt)
    {
        return Err(format!("{}批次必须来自同一次完整模型分析", batch_kind.0));
    }
    let write_manifest_digest = batch.iter().find_map(|(_, pending)| match pending {
        BatchPendingWrite::Note(note) => note.write_manifest_digest.clone(),
        BatchPendingWrite::Asset(_) => None,
    });
    if let Some(expected_digest) = write_manifest_digest.as_deref() {
        if !asset_approval_ids.is_empty() {
            return Err("绑定笔记写入清单的批次不能包含清单外附件".to_string());
        }
        if batch.iter().any(|(_, pending)| match pending {
            BatchPendingWrite::Note(note) => {
                note.write_manifest_digest.as_deref() != Some(expected_digest)
            }
            BatchPendingWrite::Asset(_) => true,
        }) {
            return Err("整批 Markdown 审批必须携带同一个写入清单摘要".to_string());
        }
        let manifest_entries = batch
            .iter()
            .filter_map(|(_, pending)| match pending {
                BatchPendingWrite::Note(note) => Some(note),
                BatchPendingWrite::Asset(_) => None,
            })
            .map(|note| {
                let previous = if note.expected_absent {
                    if note.expected_hash.is_some() {
                        return Err("写入清单中的笔记旧状态同时声明了 hash 与 absent".to_string());
                    }
                    "absent".to_string()
                } else {
                    let hash = note
                        .expected_hash
                        .as_deref()
                        .ok_or_else(|| "绑定写入清单的笔记缺少明确的旧版本状态".to_string())?;
                    format!("sha256:{hash}")
                };
                let next_content_hash = note.source.content_hash()?;
                if next_content_hash != note.content_hash {
                    return Err(format!(
                        "待写入正文在审批期间发生变化：{}",
                        note.relative_path
                    ));
                }
                Ok(NoteWriteManifestEntry {
                    vault_id: note.vault_id.clone(),
                    relative_path: note.relative_path.clone(),
                    previous,
                    next_content_hash,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let actual_digest = note_write_manifest_digest(manifest_entries)?;
        if actual_digest != expected_digest {
            return Err("实际 Markdown 审批集合与模型分析凭证绑定的写入清单不一致".to_string());
        }
    }
    analysis_state.validate_write_manifest(
        "local",
        &analysis_receipt,
        write_manifest_digest.as_deref(),
    )?;

    let mut targets = std::collections::HashSet::new();
    for (_, pending) in &batch {
        if !targets.insert(pending.target_path().to_path_buf()) {
            return Err(format!("同一{}批次不能重复写入相同目标文件", batch_kind.0));
        }
        if pending
            .created_at()
            .elapsed()
            .map(|elapsed| elapsed > WRITE_APPROVAL_TTL)
            .unwrap_or(true)
        {
            return Err(format!(
                "{}批次中有审批令牌已过期，请重新生成 diff",
                batch_kind.0
            ));
        }
        database.ensure_vault_write_allowed(
            &workspace_scope,
            pending.vault_id(),
            pending.relative_path(),
        )?;
        let (_, current_root) = resolve_vault(pending.vault_id())?;
        if current_root != pending.vault_path() {
            return Err("Vault 路径在审批后发生变化，已拒绝整批写入".to_string());
        }
        let current_hash = if pending.target_path().exists() {
            Some(hash_file_streaming(pending.target_path())?)
        } else {
            None
        };
        if &current_hash != pending.previous_hash() {
            return Err(format!(
                "文件在审批期间发生变化，已拒绝整批写入：{}",
                pending.relative_path()
            ));
        }
        if let BatchPendingWrite::Note(note) = pending {
            if (note.expected_absent && current_hash.is_some())
                || note
                    .expected_hash
                    .as_ref()
                    .is_some_and(|expected| current_hash.as_ref() != Some(expected))
            {
                return Err(format!(
                    "笔记与预期版本不一致，已拒绝整批写入：{}",
                    pending.relative_path()
                ));
            }
        }
    }

    let execution_ticket = batch
        .first()
        .and_then(|(_, pending)| pending.execution_ticket())
        .map(str::to_string);
    if batch
        .iter()
        .any(|(_, pending)| pending.execution_ticket() != execution_ticket.as_deref())
    {
        return Err(format!(
            "同一{}批次必须使用同一张能力范围执行票据",
            batch_kind.0
        ));
    }
    let ticket_task_id = if execution_ticket.is_some() {
        let task_id = batch
            .first()
            .and_then(|(_, pending)| pending.task_id())
            .ok_or_else(|| "执行票据缺少绑定的原生任务".to_string())?;
        if batch
            .iter()
            .any(|(_, pending)| pending.task_id() != Some(task_id))
        {
            return Err(format!("同一{}批次必须绑定同一个原生任务", batch_kind.0));
        }
        for (_, pending) in &batch {
            database.ensure_runtime_task_authorized(
                &workspace_scope,
                task_id,
                VAULT_WRITE_CAPABILITIES,
                VAULT_WRITE_OPERATIONS,
                Some(pending.vault_id()),
                &["running"],
            )?;
        }
        Some(task_id.to_string())
    } else {
        None
    };
    let ticket_state = match (execution_ticket.as_ref(), ticket_state) {
        (Some(_), Some(state)) => Some(state),
        (Some(_), None) => return Err("执行票据状态不可用".to_string()),
        (None, _) => None,
    };

    let batch_id = Uuid::new_v4().to_string();
    let manifest_entries = batch
        .iter()
        .map(|(approval_id, pending)| {
            Ok(BatchManifestEntryInput {
                approval_id: approval_id.clone(),
                vault_id: pending.vault_id().to_string(),
                vault_root: pending.vault_path().to_path_buf(),
                relative_path: pending.relative_path().to_string(),
                previous_hash: pending.previous_hash().clone(),
                next_hash: pending.content_hash()?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let supplied_trace_id = batch.iter().find_map(|(_, pending)| pending.trace_id());
    let trace_id = database.resolve_operation_trace_id(
        &workspace_scope,
        batch.first().and_then(|(_, pending)| pending.task_id()),
        supplied_trace_id,
    )?;
    if batch.iter().any(|(_, pending)| {
        pending
            .trace_id()
            .is_some_and(|pending_trace_id| pending_trace_id != trace_id)
    }) {
        return Err(format!("同一{}批次必须绑定同一个 Trace", batch_kind.0));
    }
    let (checkpoint_dir, mut manifest) = vault_batch::prepare_batch_manifest(
        app_data,
        batch_id.clone(),
        batch_kind.0,
        batch_kind.1,
        batch
            .iter()
            .find_map(|(_, pending)| pending.task_id().map(str::to_string)),
        Some(trace_id.clone()),
        manifest_entries,
    )?;
    if let (Some(token), Some(task_id)) = (execution_ticket.as_deref(), ticket_task_id.as_deref()) {
        let approvals = batch
            .iter()
            .map(|(approval_id, pending)| (approval_id.as_str(), pending.effect_digest()))
            .collect::<Vec<_>>();
        ticket_state.expect("ticket state checked").begin_commit(
            token,
            &workspace_scope,
            task_id,
            &approvals,
        )?;
    }
    let consumed_receipt = match analysis_state.consume("local", &analysis_receipt) {
        Ok(receipt) => receipt,
        Err(error) => {
            if let (Some(token), Some(ticket_state)) = (execution_ticket.as_deref(), ticket_state) {
                ticket_state.release_commit(token);
            }
            return Err(error);
        }
    };
    let sources = batch
        .iter()
        .map(|(_, pending)| pending.batch_source())
        .collect::<Vec<_>>();
    if let Err(error) = vault_batch::commit_batch_sources(&checkpoint_dir, &mut manifest, &sources)
    {
        analysis_state.restore(&analysis_receipt, consumed_receipt);
        if let (Some(token), Some(ticket_state)) = (execution_ticket.as_deref(), ticket_state) {
            ticket_state.release_commit(token);
        }
        return match vault_batch::rollback_batch_manifest(&checkpoint_dir, &mut manifest) {
            Ok(()) => Err(format!("{}批次写入失败并已回滚：{error}", batch_kind.0)),
            Err(rollback) => Err(format!(
                "{}批次写入失败，且回滚失败：{rollback}；检查点仍保留：{error}",
                batch_kind.0
            )),
        };
    }

    let committed_at = now_string();
    let results = batch
        .iter()
        .enumerate()
        .map(|(index, (approval_id, pending))| WriteCommitResult {
            approval_id: approval_id.clone(),
            vault_id: pending.vault_id().to_string(),
            relative_path: pending.relative_path().to_string(),
            content_hash: pending
                .content_hash()
                .unwrap_or_else(|_| "unavailable".to_string()),
            checkpoint_path: vault_batch::checkpoint_path(&checkpoint_dir, &manifest, index)
                .to_string_lossy()
                .into_owned(),
            committed_at: committed_at.clone(),
        })
        .collect::<Vec<_>>();
    let primary_result = results
        .first()
        .expect("validated capture batch always contains at least one note");
    if let Err(error) = database.append_operation_event(&OperationEvent {
        id: manifest.audit.id.clone(),
        task_id: manifest.audit.task_id.clone(),
        trace_id: manifest.audit.trace_id.clone(),
        event_type: manifest.audit.event_type.clone(),
        state: "success".to_string(),
        created_at: manifest.audit.created_at.clone(),
        vault_id: Some(primary_result.vault_id.clone()),
        relative_path: Some(primary_result.relative_path.clone()),
        detail: manifest.audit.detail.clone(),
    }) {
        analysis_state.restore(&analysis_receipt, consumed_receipt);
        if let (Some(token), Some(ticket_state)) = (execution_ticket.as_deref(), ticket_state) {
            ticket_state.release_commit(token);
        }
        return match vault_batch::rollback_batch_manifest(&checkpoint_dir, &mut manifest) {
            Ok(()) => Err(format!(
                "{}批次日志写入失败，文件已回滚：{error}",
                batch_kind.0
            )),
            Err(rollback) => Err(format!(
                "{}批次日志写入失败，且{rollback}；检查点仍保留：{error}",
                batch_kind.0
            )),
        };
    }
    for (_, pending) in &batch {
        let BatchPendingWrite::Note(note) = pending else {
            continue;
        };
        if let Err(error) = database.enqueue_vault_index_path_with_trace(
            &note.vault_id,
            &note.vault_path,
            &note.target_path,
            &trace_id,
        ) {
            log::warn!(
                "批次笔记已提交，但无法继承 Trace 入队索引 {}：{error}",
                note.relative_path
            );
        }
    }
    if let Err(error) = vault_batch::mark_batch_committed(&checkpoint_dir, &mut manifest) {
        log::warn!("批次 {batch_id} 已提交且审计成功，但 manifest 完成标记失败：{error}");
    }
    if let (Some(token), Some(ticket_state)) = (execution_ticket.as_deref(), ticket_state) {
        if let Err(error) = ticket_state.complete_commit(token) {
            log::warn!("批次 {batch_id} 已提交，但执行票据终态写入失败：{error}");
        }
    }
    match state.pending_writes.lock() {
        Ok(mut notes) => {
            for approval_id in &note_approval_ids {
                notes.remove(approval_id);
            }
        }
        Err(poisoned) => {
            log::warn!("批次 {batch_id} 已提交，但写入审批状态锁已中毒；继续清理缓存");
            let mut notes = poisoned.into_inner();
            for approval_id in &note_approval_ids {
                notes.remove(approval_id);
            }
        }
    }
    match state.pending_assets.lock() {
        Ok(mut assets) => {
            for approval_id in &asset_approval_ids {
                if let Some(pending) = assets.remove(approval_id) {
                    if let PendingAssetSource::Staged(path) = pending.source {
                        remove_claimed_capture_attachment(&path);
                    }
                }
            }
        }
        Err(poisoned) => {
            log::warn!("批次 {batch_id} 已提交，但附件审批状态锁已中毒；继续清理缓存");
            let mut assets = poisoned.into_inner();
            for approval_id in &asset_approval_ids {
                if let Some(pending) = assets.remove(approval_id) {
                    if let PendingAssetSource::Staged(path) = pending.source {
                        remove_claimed_capture_attachment(&path);
                    }
                }
            }
        }
    }
    Ok(results)
}

fn capture_batch_kind(batch_kind: Option<&str>) -> Result<(&'static str, &'static str), String> {
    match batch_kind {
        None | Some("capture") => Ok(("采集", "vault.capture.batch.write")),
        Some("creation") => Ok(("创作", "vault.creation.batch.write")),
        Some("maintenance") => Ok(("知识维护", "vault.maintenance.batch.write")),
        Some(value) => Err(format!("不支持的 Vault 批次类型：{value}")),
    }
}

#[tauri::command]
pub fn list_operation_events(
    database: State<'_, RuntimeDatabase>,
    limit: Option<usize>,
) -> Result<Vec<OperationEvent>, String> {
    database.list_native_operation_events(limit.unwrap_or(100))
}
