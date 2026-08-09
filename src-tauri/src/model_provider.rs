use base64::Engine;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::{
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
    multipart::{Form, Part},
    redirect::Policy,
    Client, StatusCode, Url,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::{
    durable_asset::{
        delete_for_runtime as delete_durable_asset_for_runtime, store_generated_image_base64,
        DurableAssetDescriptor, DurableAssetState,
    },
    execution_ticket::{ExecutionTicketState, TrustedHandlerUsage},
    model_config::assistant_context_budget,
    obsidian::OperationContext,
    prompt::{
        prompt_text as runtime_prompt, render_prompt_template as render_runtime_prompt_template,
    },
    runtime_db::{ModelUsageRecord, RuntimeDatabase, RuntimeScheduleDispatchBinding},
};

// Provider metadata and unsuccessful diagnostic bodies are control-plane data,
// so they remain bounded. Successful streamed article text is decoded
// incrementally and intentionally has no product-level aggregate size limit.
const MAX_MODEL_CONTROL_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MODEL_EVENT_DELTA_BYTES: usize = 256 * 1024;
const MAX_IMAGE_MODEL_RESPONSE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_EMBEDDING_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MODEL_REQUEST_TIMEOUT_SECONDS: u64 = 20;
const EMBEDDING_REQUEST_TIMEOUT_SECONDS: u64 = 45;
const ASSISTANT_REQUEST_TIMEOUT_SECONDS: u64 = 300;
const ANALYSIS_REQUEST_TIMEOUT_SECONDS: u64 = 120;
const MAX_ANALYSIS_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ANALYSIS_IMAGES_PER_REQUEST: usize = 8;
const MAX_ANALYSIS_IMAGE_BYTES_PER_REQUEST: usize = 12 * 1024 * 1024;
const DEFAULT_ASSISTANT_CONTEXT_PAGE_BYTES: usize = 16 * 1024 * 1024;
const ASSISTANT_CONTEXT_PAGE_MARKER_TOKENS: usize = 160;
const MAX_ASSISTANT_IMAGE_DATA_URL_CHARS: usize = 16 * 1024 * 1024;
const MAX_EMBEDDING_BATCH_INPUTS: usize = 64;
pub(crate) const MAX_EMBEDDING_INPUT_CHARS: usize = 32_000;
pub(crate) const MAX_EMBEDDING_TOTAL_CHARS: usize = 512_000;
const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
const ANALYSIS_RECEIPT_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ANALYSIS_RECEIPTS: usize = 512;
const INTENT_RECEIPT_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_INTENT_RECEIPTS: usize = 512;
const LOCAL_MODEL_SCOPE: &str = "local";
const RESEARCH_INTENT_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-research-routing.txt");
const USER_SKILL_ROUTING_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-skill-routing.txt");
const APPROVED_SKILL_EXECUTION_SYSTEM_PROMPT: &str =
    include_str!("../../prompts/runtime/approved-skill-execution-system.txt");
const ANALYSIS_SYSTEM_PROMPT: &str =
    include_str!("../../prompts/runtime/capture-analysis-system.txt");
const ASSISTANT_SYSTEM_PROMPT: &str = include_str!("../../prompts/runtime/assistant-system.txt");
const PERMANENT_DELETE_ROUTING_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-permanent-delete-routing.txt");
const ASSISTANT_SLASH_COMMAND_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-slash-command-system.txt");
const ASSISTANT_PROFILE_CONTEXT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-profile-context.template.txt");
const ASSISTANT_CAPABILITY_CATALOG_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-capability-catalog.template.txt");
const ASSISTANT_CONTEXT_OMISSION_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-context-omission.template.txt");
const CAPTURE_ANALYSIS_IMAGE_SOURCES_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/capture-analysis-image-sources.template.txt");
const CAPTURE_ANALYSIS_IMAGE_BINDINGS_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/capture-analysis-image-bindings.template.txt");
const CAPTURE_ANALYSIS_USER_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/capture-analysis-user.template.txt");
const ASSISTANT_ATTACHMENT_TEXT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/model-provider/assistant-attachment-text.template.txt");
const ASSISTANT_IMAGE_ATTACHMENT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/model-provider/assistant-image-attachment.template.txt");
const ASSISTANT_RECEIVED_ATTACHMENT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/model-provider/assistant-received-attachment.template.txt");
const ASSISTANT_ATTACHMENT_UNNAMED_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-unnamed.txt");
const DEFAULT_ASSISTANT_NAME_PROMPT: &str =
    include_str!("../../prompts/runtime/model-provider/default-assistant-name.txt");
const DEFAULT_ASSISTANT_LANGUAGE_PROMPT: &str =
    include_str!("../../prompts/runtime/model-provider/default-assistant-language.txt");
const DEFAULT_ASSISTANT_STYLE_PROMPT: &str =
    include_str!("../../prompts/runtime/model-provider/default-assistant-style.txt");

const ASSISTANT_INTENTS: [&str; 19] = [
    "chat",
    "image",
    "settings",
    "schedule",
    "inbox",
    "capture",
    "skills",
    "reports",
    "optimization",
    "research",
    "knowledge_maintenance",
    "create",
    "search",
    "tasks",
    "logs",
    "vaults",
    "dashboard",
    "delete",
    "external",
];
const ASSISTANT_OPERATIONS: [&str; 17] = [
    "none", "create", "update", "move", "rename", "restore", "pause", "resume", "cancel", "delete",
    "retry", "run", "query", "generate", "edit", "open", "send",
];

pub(crate) struct ModelAnalysisReceipt {
    workspace_scope: String,
    analysis_digest: Option<String>,
    write_manifest_digest: Option<String>,
    created_at: SystemTime,
}

#[derive(Default)]
pub struct ModelAnalysisState {
    receipts: Mutex<HashMap<String, ModelAnalysisReceipt>>,
}

#[derive(Default)]
pub struct ModelRequestState {
    requests: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl ModelRequestState {
    fn register(&self, request_id: &str) -> Result<Arc<AtomicBool>, String> {
        let mut requests = self
            .requests
            .lock()
            .map_err(|_| "模型请求取消状态不可用".to_string())?;
        if requests.contains_key(request_id) {
            return Err("模型请求 ID 已经在运行".to_string());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        requests.insert(request_id.to_string(), Arc::clone(&cancellation));
        Ok(cancellation)
    }

    fn finish(&self, request_id: &str) {
        if let Ok(mut requests) = self.requests.lock() {
            requests.remove(request_id);
        }
    }

    fn cancel(&self, request_id: &str) -> Result<bool, String> {
        let requests = self
            .requests
            .lock()
            .map_err(|_| "模型请求取消状态不可用".to_string())?;
        let Some(cancellation) = requests.get(request_id) else {
            return Ok(false);
        };
        cancellation.store(true, Ordering::Release);
        Ok(true)
    }

    pub(crate) fn cancel_all(&self) -> Result<usize, String> {
        let mut requests = self
            .requests
            .lock()
            .map_err(|_| "模型请求取消状态不可用".to_string())?;
        let active = std::mem::take(&mut *requests);
        let count = active.len();
        for cancellation in active.values() {
            cancellation.store(true, Ordering::Release);
        }
        Ok(count)
    }
}

impl ModelAnalysisState {
    pub(crate) fn clear(&self) -> Result<usize, String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析回执状态不可用".to_string())?;
        let count = receipts.len();
        receipts.clear();
        Ok(count)
    }

    fn prune(receipts: &mut HashMap<String, ModelAnalysisReceipt>) {
        receipts.retain(|_, receipt| {
            receipt
                .created_at
                .elapsed()
                .is_ok_and(|elapsed| elapsed <= ANALYSIS_RECEIPT_TTL)
        });
    }

    fn issue_with_analysis(
        &self,
        workspace_scope: &str,
        analysis: &Value,
    ) -> Result<String, String> {
        self.issue_with_digest(workspace_scope, Some(capture_analysis_digest(analysis)))
    }

    fn issue_with_digest(
        &self,
        workspace_scope: &str,
        analysis_digest: Option<String>,
    ) -> Result<String, String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        if receipts.len() >= MAX_ANALYSIS_RECEIPTS {
            return Err("待处理的模型分析凭证过多，请先完成或取消现有写入".to_string());
        }
        let receipt_id = Uuid::new_v4().to_string();
        receipts.insert(
            receipt_id.clone(),
            ModelAnalysisReceipt {
                workspace_scope: workspace_scope.to_string(),
                analysis_digest,
                write_manifest_digest: None,
                created_at: SystemTime::now(),
            },
        );
        Ok(receipt_id)
    }

    pub(crate) fn bind_write_manifest(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
        write_manifest_digest: &str,
    ) -> Result<String, String> {
        let digest = normalize_write_manifest_digest(write_manifest_digest)?;
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get_mut(receipt_id)
            .ok_or_else(|| "模型分析凭证不存在、已使用或已过期，必须重新分析".to_string())?;
        if receipt.workspace_scope != workspace_scope {
            return Err("模型分析凭证不属于当前本地工作区".to_string());
        }
        match receipt.write_manifest_digest.as_deref() {
            Some(existing) if existing == digest => Ok(digest),
            Some(_) => Err("模型分析凭证已绑定其他写入清单，不能改绑".to_string()),
            None => {
                receipt.write_manifest_digest = Some(digest.clone());
                Ok(digest)
            }
        }
    }

    pub(crate) fn validate_write_manifest(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
        write_manifest_digest: Option<&str>,
    ) -> Result<Option<String>, String> {
        let supplied_digest = write_manifest_digest
            .map(normalize_write_manifest_digest)
            .transpose()?;
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "模型分析凭证不存在、已使用或已过期，必须重新分析".to_string())?;
        if receipt.workspace_scope != workspace_scope {
            return Err("模型分析凭证不属于当前本地工作区".to_string());
        }
        match (
            receipt.write_manifest_digest.as_deref(),
            supplied_digest.as_deref(),
        ) {
            (Some(expected), Some(supplied)) if expected == supplied => Ok(supplied_digest),
            (Some(_), Some(_)) => Err("写入清单摘要与模型分析凭证绑定值不一致".to_string()),
            (Some(_), None) => Err("模型分析凭证已绑定写入清单，本次写入缺少清单摘要".to_string()),
            (None, Some(_)) => Err("模型分析凭证尚未绑定写入清单".to_string()),
            (None, None) => Ok(None),
        }
    }

    pub(crate) fn validate_unbound_write_manifest(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
    ) -> Result<(), String> {
        self.validate_write_manifest(workspace_scope, receipt_id, None)
            .map(|_| ())
    }

    pub(crate) fn validate_analysis(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
        analysis: &Value,
    ) -> Result<(), String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "模型分析凭证不存在、已使用或已过期，必须重新分析".to_string())?;
        if receipt.workspace_scope != workspace_scope {
            return Err("模型分析凭证不属于当前本地工作区".to_string());
        }
        if receipt
            .analysis_digest
            .as_deref()
            .is_some_and(|expected| expected != capture_analysis_digest(analysis))
        {
            return Err("待写入的分析结果与模型分析凭证不一致".to_string());
        }
        Ok(())
    }

    pub(crate) fn consume(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
    ) -> Result<ModelAnalysisReceipt, String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型分析凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "模型分析凭证不存在、已使用或已过期，必须重新分析".to_string())?;
        if receipt.workspace_scope != workspace_scope {
            return Err("模型分析凭证不属于当前本地工作区".to_string());
        }
        receipts
            .remove(receipt_id)
            .ok_or_else(|| "模型分析凭证不存在、已使用或已过期，必须重新分析".to_string())
    }

    pub(crate) fn restore(&self, receipt_id: &str, mut receipt: ModelAnalysisReceipt) {
        if let Ok(mut receipts) = self.receipts.lock() {
            receipt.created_at = SystemTime::now();
            receipts.insert(receipt_id.to_string(), receipt);
        }
    }
}

fn normalize_write_manifest_digest(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("写入清单摘要必须是 64 位 SHA-256".to_string());
    }
    Ok(value.to_ascii_lowercase())
}

fn capture_analysis_digest(analysis: &Value) -> String {
    let mut normalized = analysis.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.remove("analysisReceipt");
        object.remove("analysis_receipt");
        object.remove("yunspireBatchMeta");
        object.remove("yunspire_batch_meta");
    }
    let bytes = serde_json::to_vec(&normalized).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

struct ModelIntentReceipt {
    workspace_scope: String,
    intent: String,
    capability_ids: HashSet<String>,
    operation: String,
    parameters: Value,
    created_at: SystemTime,
}

#[derive(Default)]
pub struct ModelIntentState {
    receipts: Mutex<HashMap<String, ModelIntentReceipt>>,
}

impl ModelIntentState {
    pub(crate) fn clear(&self) -> Result<usize, String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型意图凭证状态不可用".to_string())?;
        let count = receipts.len();
        receipts.clear();
        Ok(count)
    }

    fn prune(receipts: &mut HashMap<String, ModelIntentReceipt>) {
        receipts.retain(|_, receipt| {
            receipt
                .created_at
                .elapsed()
                .is_ok_and(|elapsed| elapsed <= INTENT_RECEIPT_TTL)
        });
    }

    fn issue(
        &self,
        workspace_scope: &str,
        intent: &str,
        capability_ids: &[String],
        operation: &str,
        parameters: &Value,
    ) -> Result<String, String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型意图凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        if receipts.len() >= MAX_INTENT_RECEIPTS {
            return Err("待执行的模型意图凭证过多，请稍后重试".to_string());
        }
        let receipt_id = Uuid::new_v4().to_string();
        receipts.insert(
            receipt_id.clone(),
            ModelIntentReceipt {
                workspace_scope: workspace_scope.to_string(),
                intent: intent.to_string(),
                capability_ids: capability_ids.iter().cloned().collect(),
                operation: operation.to_string(),
                parameters: parameters.clone(),
                created_at: SystemTime::now(),
            },
        );
        Ok(receipt_id)
    }

    pub(crate) fn consume(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
        intent: &str,
        capability_id: &str,
        operation: &str,
        parameters: &Value,
    ) -> Result<(), String> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型意图凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "模型意图凭证不存在、已使用或已过期".to_string())?;
        if receipt.workspace_scope != workspace_scope {
            return Err("模型意图凭证不属于当前本地工作区".to_string());
        }
        if receipt.intent != intent {
            return Err("模型意图与待执行任务不一致".to_string());
        }
        if !receipt.capability_ids.contains(capability_id) {
            return Err("模型没有选择待执行任务所需的系统能力".to_string());
        }
        if receipt.operation != operation || receipt.parameters != *parameters {
            return Err("待执行操作或参数与模型原始决策不一致".to_string());
        }
        receipts.remove(receipt_id);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn consume_after<T, F>(
        &self,
        workspace_scope: &str,
        receipt_id: &str,
        intent: &str,
        capability_id: &str,
        operation: &str,
        parameters: &Value,
        action: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
    {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| "模型意图凭证状态不可用".to_string())?;
        Self::prune(&mut receipts);
        let receipt = receipts
            .get(receipt_id)
            .ok_or_else(|| "模型意图凭证不存在、已使用或已过期".to_string())?;
        if receipt.workspace_scope != workspace_scope
            || receipt.intent != intent
            || !receipt.capability_ids.contains(capability_id)
            || receipt.operation != operation
            || receipt.parameters != *parameters
        {
            return Err("待执行命令与模型原始决策不一致".to_string());
        }
        let result = action()?;
        receipts.remove(receipt_id);
        Ok(result)
    }
}

pub(crate) fn suspend_model_runtime(app: &AppHandle) -> Result<usize, String> {
    let cancelled = app.state::<ModelRequestState>().cancel_all()?;
    let analysis_receipts = app.state::<ModelAnalysisState>().clear()?;
    let intent_receipts = app.state::<ModelIntentState>().clear()?;
    Ok(cancelled + analysis_receipts + intent_receipts)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    id: String,
    name: String,
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantChatMessage {
    role: String,
    content: String,
    #[serde(default)]
    attachments: Vec<AssistantChatAttachment>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantChatAttachment {
    name: String,
    mime_type: String,
    #[serde(default)]
    data_url: Option<String>,
    #[serde(default)]
    text_content: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantProfile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    style: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantScheduleDispatchContext {
    occurrence_id: String,
    runtime_task_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantCapability {
    id: String,
    name: String,
    kind: String,
    description: String,
    enabled: bool,
    #[serde(default)]
    user_selected: bool,
    #[serde(default)]
    version: Option<i64>,
    #[serde(default)]
    payload_hash: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    input_schema: Option<String>,
    #[serde(default)]
    output_schema: Option<String>,
    #[serde(default)]
    declared_capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantTurn {
    reply: String,
    intent: String,
    action: String,
    confidence: f64,
    capability_ids: Vec<String>,
    operation: String,
    parameters: Value,
    reason: String,
    decision_receipt: String,
    choices: Vec<AssistantChoice>,
    usage: ModelUsageSummary,
    trace_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageSummary {
    request_id: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    estimated_cost_usd: Option<f64>,
    source: String,
    duration_ms: u64,
}

impl ModelUsageSummary {
    pub(crate) fn trusted_handler_usage(&self, elapsed: Duration) -> TrustedHandlerUsage {
        TrustedHandlerUsage {
            tool_calls: 1,
            runtime_seconds: elapsed.as_secs().max(1),
            tokens: self.total_tokens,
            cost: self.estimated_cost_usd,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovedSkillModelInput {
    pub(crate) skill_id: String,
    pub(crate) skill_name: String,
    pub(crate) version: i64,
    pub(crate) payload_hash: String,
    pub(crate) instructions: String,
    pub(crate) input_schema: String,
    pub(crate) output_schema: String,
    pub(crate) declared_capabilities: Vec<String>,
    pub(crate) user_input: Value,
    pub(crate) request_id: Option<String>,
    pub(crate) trace_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ApprovedSkillModelResult {
    pub(crate) output_text: String,
    pub(crate) output_data: Value,
    pub(crate) warnings: Vec<String>,
    pub(crate) request_id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) usage: ModelUsageSummary,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantModelEvent {
    request_id: String,
    kind: String,
    received_bytes: usize,
    duration_ms: u64,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantChoice {
    id: String,
    label: String,
    description: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImageResult {
    images: Vec<String>,
    assets: Vec<DurableAssetDescriptor>,
    prompt: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureImageBinding {
    asset_id: String,
    #[serde(default, alias = "allowedReferenceIds")]
    reference_ids: Vec<String>,
    original_sha256: String,
    analysis_sha256: String,
    original_byte_length: u64,
    analysis_byte_length: u64,
    analysis_mime_type: String,
    derived: bool,
}

#[derive(Debug)]
struct PreparedCaptureAnalysisImages {
    images: Vec<(String, String)>,
    bindings: Vec<CaptureImageBinding>,
}

fn provider_base_url(provider: &str, base_url: &str) -> Result<Url, String> {
    if !matches!(
        provider,
        "openai" | "anthropic" | "openrouter" | "ollama" | "custom"
    ) {
        return Err("不支持的模型接口类型".to_string());
    }
    let mut url = Url::parse(base_url.trim()).map_err(|_| "API URL 格式无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("API URL 只允许 http 或 https 协议".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("API URL 不能包含用户名或密码".to_string());
    }
    url.set_query(None);
    url.set_fragment(None);

    Ok(url)
}

fn url_with_path(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(if path.is_empty() { "/" } else { path });
    url
}

fn append_path(base: &str, suffix: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        suffix.to_string()
    } else {
        format!("{base}{suffix}")
    }
}

fn api_operation_base(path: &str) -> (&str, bool) {
    let path = path.trim_end_matches('/');
    for suffix in [
        "/chat/completions",
        "/responses",
        "/messages",
        "/models",
        "/embeddings",
    ] {
        if let Some(base) = path.strip_suffix(suffix) {
            return (base.trim_end_matches('/'), true);
        }
    }
    (path, false)
}

fn push_endpoint(endpoints: &mut Vec<Url>, base: &Url, path: String) {
    let endpoint = url_with_path(base, &path);
    if !endpoints.iter().any(|item| item == &endpoint) {
        endpoints.push(endpoint);
    }
}

fn model_endpoints(provider: &str, base_url: &str) -> Result<Vec<Url>, String> {
    let url = provider_base_url(provider, base_url)?;
    let current = url.path().trim_end_matches('/');
    let mut endpoints = Vec::new();

    match provider {
        "ollama" => {
            let root = current
                .strip_suffix("/api/tags")
                .or_else(|| current.strip_suffix("/api/chat"))
                .unwrap_or(current)
                .trim_end_matches("/v1");
            push_endpoint(&mut endpoints, &url, append_path(root, "/api/tags"));
        }
        "anthropic" => {
            let (root, _) = api_operation_base(current);
            let path = if root.ends_with("/v1") {
                append_path(root, "/models")
            } else {
                append_path(root, "/v1/models")
            };
            push_endpoint(&mut endpoints, &url, path);
        }
        _ => {
            let (root, explicit_endpoint) = api_operation_base(current);
            if root.ends_with("/v1") {
                push_endpoint(&mut endpoints, &url, append_path(root, "/models"));
            } else if explicit_endpoint {
                // A complete unversioned endpoint is intentional, so try its sibling first.
                push_endpoint(&mut endpoints, &url, append_path(root, "/models"));
                push_endpoint(&mut endpoints, &url, append_path(root, "/v1/models"));
            } else {
                // OpenAI-compatible APIs conventionally expose models below /v1.
                push_endpoint(&mut endpoints, &url, append_path(root, "/v1/models"));
                push_endpoint(&mut endpoints, &url, append_path(root, "/models"));
            }
        }
    }

    Ok(endpoints)
}

fn parse_models(provider: &str, payload: &Value) -> Result<Vec<ModelDescriptor>, String> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.pointer("/data/models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
        .ok_or_else(|| "模型接口响应缺少 data 或 models 数组".to_string())?;

    let mut seen = HashSet::new();
    let mut models = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.as_str().or_else(|| {
                entry
                    .get("id")
                    .or_else(|| entry.get("model"))
                    .or_else(|| entry.get("model_id"))
                    .or_else(|| entry.get("modelId"))
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
            })?;
            let id = id.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                return None;
            }
            let name = entry
                .get("display_name")
                .or_else(|| entry.get("displayName"))
                .or_else(|| entry.get("model_name"))
                .or_else(|| entry.get("modelName"))
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(id);
            let positive_integer = |fields: &[&str]| {
                fields.iter().find_map(|field| {
                    let value = entry.get(*field)?;
                    value
                        .as_u64()
                        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
                        .filter(|value| *value > 0)
                })
            };
            Some(ModelDescriptor {
                id: id.to_string(),
                name: name.to_string(),
                provider: provider.to_string(),
                context_window_tokens: positive_integer(&[
                    "context_window_tokens",
                    "contextWindowTokens",
                    "context_length",
                    "contextLength",
                    "max_context_tokens",
                    "maxContextTokens",
                ]),
                max_output_tokens: positive_integer(&[
                    "max_output_tokens",
                    "maxOutputTokens",
                    "output_token_limit",
                    "outputTokenLimit",
                ]),
            })
        })
        .collect::<Vec<_>>();
    models.sort_by_key(|model| model.id.to_lowercase());
    if models.is_empty() {
        return Err("模型接口返回成功，但没有可用模型".to_string());
    }
    Ok(models)
}

fn sanitized_upstream_message(bytes: &[u8], api_key: &str) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    if text.trim_start().starts_with('<') {
        return Some("接口返回了 HTML 页面，请填写 API 基础地址而不是控制台或官网地址".to_string());
    }
    let parsed = serde_json::from_slice::<Value>(bytes).ok();
    let message = parsed
        .as_ref()
        .and_then(|payload| {
            payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| payload.get("message").and_then(Value::as_str))
                .or_else(|| payload.get("error").and_then(Value::as_str))
        })
        .unwrap_or(text.trim());
    let mut message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if !api_key.is_empty() {
        message = message.replace(api_key, "[已隐藏]");
    }
    if message.is_empty() {
        return None;
    }
    let mut limited = message.chars().take(240).collect::<String>();
    if message.chars().count() > 240 {
        limited.push('…');
    }
    Some(limited)
}

fn analysis_endpoint(provider: &str, base_url: &str) -> Result<Url, String> {
    let mut url = provider_base_url(provider, base_url)?;
    let current = url.path().trim_end_matches('/');
    let path = match provider {
        "ollama" if current.ends_with("/api/chat") => current.to_string(),
        "ollama" => {
            let root = current
                .strip_suffix("/api/tags")
                .unwrap_or(current)
                .trim_end_matches("/v1");
            append_path(root, "/api/chat")
        }
        "anthropic" if current.ends_with("/messages") => current.to_string(),
        "anthropic" => {
            let (root, _) = api_operation_base(current);
            if root.ends_with("/v1") {
                append_path(root, "/messages")
            } else {
                append_path(root, "/v1/messages")
            }
        }
        _ if current.ends_with("/chat/completions") => current.to_string(),
        _ => {
            let (root, explicit_endpoint) = api_operation_base(current);
            if root.ends_with("/v1") || explicit_endpoint {
                append_path(root, "/chat/completions")
            } else {
                append_path(root, "/v1/chat/completions")
            }
        }
    };
    url.set_path(if path.is_empty() {
        "/chat/completions"
    } else {
        &path
    });
    Ok(url)
}

fn embedding_endpoint(provider: &str, base_url: &str) -> Result<Url, String> {
    if provider == "anthropic" {
        return Err("Anthropic 当前不提供 Embedding 接口".to_string());
    }
    let mut url = provider_base_url(provider, base_url)?;
    let current = url.path().trim_end_matches('/');
    let path = if provider == "ollama" {
        if current.ends_with("/api/embed") {
            current.to_string()
        } else {
            let root = current
                .strip_suffix("/api/tags")
                .or_else(|| current.strip_suffix("/api/chat"))
                .unwrap_or(current)
                .trim_end_matches("/v1");
            append_path(root, "/api/embed")
        }
    } else if current.ends_with("/embeddings") {
        current.to_string()
    } else {
        let (root, explicit_endpoint) = api_operation_base(current);
        if root.ends_with("/v1") || explicit_endpoint {
            append_path(root, "/embeddings")
        } else {
            append_path(root, "/v1/embeddings")
        }
    };
    url.set_path(if path.is_empty() {
        "/embeddings"
    } else {
        &path
    });
    Ok(url)
}

fn response_text_fragment(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("text").and_then(Value::as_str))
        .or_else(|| value.pointer("/text/value").and_then(Value::as_str))
}

fn append_response_text(content: &mut String, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    if let Some(fragment) = response_text_fragment(value) {
        content.push_str(fragment);
        return;
    }
    if let Some(parts) = value.as_array() {
        for part in parts {
            if let Some(fragment) = response_text_fragment(part) {
                content.push_str(fragment);
            }
        }
    }
}

fn model_text(payload: &Value) -> Result<String, String> {
    let mut content = String::new();
    for value in [
        payload.pointer("/choices/0/message/content"),
        payload.pointer("/choices/0/text"),
        payload.pointer("/message/content"),
        payload.pointer("/content/0/text"),
        payload.pointer("/output/0/content"),
        payload.pointer("/response/output/0/content"),
        payload.get("output_text"),
        payload.pointer("/response/output_text"),
    ] {
        append_response_text(&mut content, value);
        if !content.trim().is_empty() {
            return Ok(content);
        }
    }
    Err("模型响应缺少文本内容".to_string())
}

fn model_response_payloads(bytes: &[u8]) -> Vec<Value> {
    if let Ok(payload) = serde_json::from_slice::<Value>(bytes) {
        return vec![payload];
    }
    let mut decoder = ModelTransportStreamDecoder::default();
    let mut payloads = decoder.push(bytes);
    payloads.extend(decoder.finish());
    payloads
}

fn model_request_error(
    prefix: &str,
    status: reqwest::StatusCode,
    bytes: &[u8],
    key: &str,
) -> String {
    let detail = sanitized_upstream_message(bytes, key)
        .map(|message| format!("：{message}"))
        .unwrap_or_default();
    format!("{prefix}返回 HTTP {}{detail}", status.as_u16())
}

fn sanitize_assistant_attachment(
    mut attachment: AssistantChatAttachment,
) -> Result<AssistantChatAttachment, String> {
    attachment.name = attachment.name.trim().chars().take(160).collect();
    attachment.mime_type = attachment.mime_type.trim().to_lowercase();
    if attachment.name.is_empty() {
        attachment.name = runtime_prompt(ASSISTANT_ATTACHMENT_UNNAMED_PROMPT).to_string();
    }
    if let Some(text) = attachment.text_content.take() {
        let text = text.trim();
        if !text.is_empty() {
            attachment.text_content = Some(text.to_string());
        }
    }
    if let Some(data_url) = attachment.data_url.take() {
        if data_url.len() > MAX_ASSISTANT_IMAGE_DATA_URL_CHARS {
            return Err(format!(
                "AI助手附件“{}”的单张图片输入超过 16 MB 请求边界",
                attachment.name
            ));
        }
        let valid_image = data_url
            .strip_prefix("data:")
            .and_then(|value| value.split_once(';'))
            .is_some_and(|(mime, rest)| mime.starts_with("image/") && rest.starts_with("base64,"));
        if !valid_image {
            return Err(format!("AI助手附件“{}”的图片输入格式无效", attachment.name));
        }
        attachment.data_url = Some(data_url);
    }
    Ok(attachment)
}

fn estimate_assistant_tokens(value: &str) -> usize {
    let (ascii_characters, non_ascii_characters) =
        value
            .chars()
            .fold((0usize, 0usize), |(ascii, non_ascii), character| {
                if character.is_ascii() {
                    (ascii.saturating_add(1), non_ascii)
                } else {
                    (ascii, non_ascii.saturating_add(1))
                }
            });
    non_ascii_characters.saturating_add(ascii_characters.div_ceil(4))
}

fn merge_assistant_usage_payload(merged: &mut serde_json::Map<String, Value>, payload: &Value) {
    for candidate in [
        payload.get("usage"),
        payload.pointer("/response/usage"),
        payload.pointer("/data/usage"),
        payload.pointer("/message/usage"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(object) = candidate.as_object() {
            for (key, value) in object {
                if !value.is_null() {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
    }
    for (source, target) in [
        ("prompt_eval_count", "prompt_tokens"),
        ("eval_count", "completion_tokens"),
    ] {
        if let Some(value) = payload.get(source).filter(|value| !value.is_null()) {
            merged.insert(target.to_string(), value.clone());
        }
    }
}

fn usage_u64(usage: &Value, fields: &[&str]) -> Option<u64> {
    fields
        .iter()
        .find_map(|field| usage.get(*field).and_then(Value::as_u64))
}

fn assistant_usage_summary_from_payload(
    request_id: &str,
    usage: Option<&Value>,
    prompt_estimate: u64,
    completion_estimate: u64,
    duration_ms: u64,
) -> ModelUsageSummary {
    let prompt_tokens = usage
        .and_then(|value| {
            usage_u64(
                value,
                &[
                    "prompt_tokens",
                    "input_tokens",
                    "promptTokens",
                    "inputTokens",
                ],
            )
        })
        .unwrap_or(prompt_estimate);
    let completion_tokens = usage
        .and_then(|value| {
            usage_u64(
                value,
                &[
                    "completion_tokens",
                    "output_tokens",
                    "completionTokens",
                    "outputTokens",
                ],
            )
        })
        .unwrap_or(completion_estimate);
    let total_tokens = usage
        .and_then(|value| usage_u64(value, &["total_tokens", "totalTokens"]))
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    let estimated_cost_usd = usage.and_then(|value| {
        value
            .get("cost")
            .or_else(|| value.get("total_cost"))
            .or_else(|| value.get("estimated_cost"))
            .and_then(Value::as_f64)
    });
    ModelUsageSummary {
        request_id: request_id.to_string(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd,
        source: if usage.is_some() {
            if estimated_cost_usd.is_some() {
                "provider_usage_and_cost".to_string()
            } else {
                "provider_usage_cost_unavailable".to_string()
            }
        } else {
            "local_estimate_cost_unavailable".to_string()
        },
        duration_ms,
    }
}

fn assistant_usage_summary_from_attempts(
    request_id: &str,
    attempts: &[Value],
    prompt_estimate: u64,
    completion_estimate: u64,
    duration_ms: u64,
) -> ModelUsageSummary {
    if attempts.is_empty() {
        return assistant_usage_summary_from_payload(
            request_id,
            None,
            prompt_estimate,
            completion_estimate,
            duration_ms,
        );
    }
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut total_tokens = 0u64;
    let mut estimated_cost_usd = 0f64;
    let mut has_cost = false;
    for usage in attempts {
        let summary = assistant_usage_summary_from_payload(request_id, Some(usage), 0, 0, 0);
        prompt_tokens = prompt_tokens.saturating_add(summary.prompt_tokens);
        completion_tokens = completion_tokens.saturating_add(summary.completion_tokens);
        total_tokens = total_tokens.saturating_add(summary.total_tokens);
        if let Some(cost) = summary.estimated_cost_usd {
            estimated_cost_usd += cost;
            has_cost = true;
        }
    }
    ModelUsageSummary {
        request_id: request_id.to_string(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd: has_cost.then_some(estimated_cost_usd),
        source: if has_cost {
            "provider_usage_and_cost".to_string()
        } else {
            "provider_usage_cost_unavailable".to_string()
        },
        duration_ms,
    }
}

fn record_assistant_usage_attempt(attempts: &mut Vec<Value>, response: &CancellableModelResponse) {
    if let Some(usage) = response.usage.as_ref() {
        attempts.push(usage.clone());
    }
}

type NormalizedAssistantMessage = (String, String, Vec<AssistantChatAttachment>);
type AssistantMessagePage = (Vec<NormalizedAssistantMessage>, usize);

fn normalize_assistant_messages(
    messages: Vec<AssistantChatMessage>,
) -> Result<Vec<NormalizedAssistantMessage>, String> {
    let mut normalized = Vec::new();
    for message in messages {
        let role = message.role.trim().to_lowercase();
        if !matches!(role.as_str(), "user" | "assistant") {
            continue;
        }
        let content = message.content.trim().to_string();
        let attachments = message
            .attachments
            .into_iter()
            .map(sanitize_assistant_attachment)
            .collect::<Result<Vec<_>, _>>()?;
        if content.is_empty() && attachments.is_empty() {
            continue;
        }
        normalized.push((role, content, attachments));
    }
    Ok(normalized)
}

fn assistant_message_cost(message: &NormalizedAssistantMessage) -> (usize, usize) {
    let (_, content, attachments) = message;
    let attachment_tokens = attachments
        .iter()
        .map(|attachment| {
            estimate_assistant_tokens(&attachment.name)
                .saturating_add(estimate_assistant_tokens(&attachment.mime_type))
                .saturating_add(
                    attachment
                        .text_content
                        .as_deref()
                        .map(estimate_assistant_tokens)
                        .unwrap_or_default(),
                )
                .saturating_add(if attachment.data_url.is_some() {
                    1_024
                } else {
                    0
                })
        })
        .sum::<usize>();
    let bytes = content
        .len()
        .saturating_add(
            attachments
                .iter()
                .map(|attachment| {
                    attachment
                        .name
                        .len()
                        .saturating_add(attachment.mime_type.len())
                        .saturating_add(
                            attachment
                                .text_content
                                .as_deref()
                                .map(str::len)
                                .unwrap_or_default(),
                        )
                        .saturating_add(
                            attachment
                                .data_url
                                .as_deref()
                                .map(str::len)
                                .unwrap_or_default(),
                        )
                })
                .sum::<usize>(),
        )
        .saturating_add(256);
    (
        12usize
            .saturating_add(estimate_assistant_tokens(content))
            .saturating_add(attachment_tokens),
        bytes,
    )
}

fn page_assistant_messages(
    messages: Vec<NormalizedAssistantMessage>,
    token_budget: Option<usize>,
    byte_budget: usize,
) -> Result<AssistantMessagePage, String> {
    let total_messages = messages.len();
    let mut selected = Vec::new();
    let mut selected_tokens = 0usize;
    let mut selected_bytes = 0usize;
    for message in messages.into_iter().rev() {
        let (message_tokens, message_bytes) = assistant_message_cost(&message);
        let exceeds_tokens = token_budget
            .is_some_and(|budget| selected_tokens.saturating_add(message_tokens) > budget);
        let exceeds_bytes = selected_bytes.saturating_add(message_bytes) > byte_budget;
        if exceeds_tokens || exceeds_bytes {
            if selected.is_empty() {
                return Err(
                    "最新一条助手消息或附件超过当前模型的单请求上下文页；请使用耐久资产分块分析后再提交摘要"
                        .to_string(),
                );
            }
            break;
        }
        selected_tokens = selected_tokens.saturating_add(message_tokens);
        selected_bytes = selected_bytes.saturating_add(message_bytes);
        selected.push(message);
    }
    selected.reverse();
    let omitted = total_messages.saturating_sub(selected.len());
    Ok((selected, omitted))
}

fn is_assistant_slash_command(messages: &[NormalizedAssistantMessage]) -> bool {
    let Some((role, content, _)) = messages.last() else {
        return false;
    };
    if role != "user" {
        return false;
    }
    content
        .trim_start()
        .strip_prefix('/')
        .and_then(|command| command.split_whitespace().next())
        .is_some_and(|command| {
            matches!(
                command.to_lowercase().as_str(),
                "help"
                    | "new"
                    | "clear"
                    | "rename"
                    | "compact"
                    | "reflect"
                    | "style"
                    | "image"
                    | "edit"
            )
        })
}

fn report_subscription_operation(messages: &[NormalizedAssistantMessage]) -> Option<&'static str> {
    let (role, content, _) = messages.last()?;
    if role != "user"
        || !content.contains("订阅")
        || !["日报", "周报", "月报", "年报", "报告"]
            .iter()
            .any(|label| content.contains(label))
    {
        return None;
    }
    if content.contains("删除") || content.contains("取消订阅") {
        Some("delete")
    } else if content.contains("暂停") || content.contains("停用") {
        Some("pause")
    } else if content.contains("恢复") || content.contains("启用") {
        Some("resume")
    } else if content.contains("修改") || content.contains("更新") || content.contains("调整")
    {
        Some("update")
    } else {
        Some("create")
    }
}

fn external_delivery_requested(messages: &[NormalizedAssistantMessage]) -> bool {
    let Some((role, content, _)) = messages.last() else {
        return false;
    };
    role == "user"
        && ["发送", "投递", "同步", "发布"]
            .iter()
            .any(|action| content.contains(action))
        && [
            "微信",
            "企业微信",
            "飞书",
            "邮箱",
            "邮件",
            "Webhook",
            "webhook",
        ]
        .iter()
        .any(|target| content.contains(target))
}

fn external_delivery_content_present(parameters: &Value) -> bool {
    ["content", "text", "message", "body"].iter().any(|key| {
        parameters
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn decode_image_data_url(data_url: &str) -> Option<(String, String)> {
    let value = data_url.strip_prefix("data:")?;
    let (header, encoded) = value.split_once(",")?;
    let mime_type = header.strip_suffix(";base64")?.to_lowercase();
    if !mime_type.starts_with("image/") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .ok()?;
    if bytes.len() > MAX_ANALYSIS_IMAGE_BYTES_PER_REQUEST {
        return None;
    }
    Some((
        mime_type,
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

fn assistant_attachment_text(attachment: &AssistantChatAttachment) -> String {
    if let Some(text) = attachment.text_content.as_deref() {
        return format!(
            "\n\n{}",
            render_runtime_prompt_template(
                ASSISTANT_ATTACHMENT_TEXT_PROMPT_TEMPLATE,
                &[("name", &attachment.name), ("text", text)],
            )
            .expect("bundled assistant attachment Prompt must be valid")
        );
    }
    if attachment.data_url.is_some() {
        return format!(
            "\n\n{}",
            render_runtime_prompt_template(
                ASSISTANT_IMAGE_ATTACHMENT_PROMPT_TEMPLATE,
                &[("name", &attachment.name)],
            )
            .expect("bundled assistant image Prompt must be valid")
        );
    }
    format!(
        "\n\n{}",
        render_runtime_prompt_template(
            ASSISTANT_RECEIVED_ATTACHMENT_PROMPT_TEMPLATE,
            &[("name", &attachment.name)],
        )
        .expect("bundled assistant attachment receipt Prompt must be valid")
    )
}

fn openai_assistant_message(
    role: &str,
    content: &str,
    attachments: &[AssistantChatAttachment],
) -> Value {
    if attachments.is_empty() {
        return serde_json::json!({"role": role, "content": content});
    }
    let mut parts = vec![serde_json::json!({"type": "text", "text": content})];
    for attachment in attachments {
        if let Some(data_url) = attachment.data_url.as_deref() {
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": {"url": data_url, "detail": "auto"},
            }));
        } else {
            parts.push(serde_json::json!({
                "type": "text",
                "text": assistant_attachment_text(attachment),
            }));
        }
    }
    serde_json::json!({"role": role, "content": parts})
}

fn anthropic_assistant_message(
    role: &str,
    content: &str,
    attachments: &[AssistantChatAttachment],
) -> Value {
    if attachments.is_empty() {
        return serde_json::json!({"role": role, "content": content});
    }
    let mut parts = vec![serde_json::json!({"type": "text", "text": content})];
    for attachment in attachments {
        if let Some(data_url) = attachment.data_url.as_deref() {
            if let Some((media_type, data)) = decode_image_data_url(data_url) {
                parts.push(serde_json::json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data},
                }));
                continue;
            }
        }
        parts.push(serde_json::json!({
            "type": "text",
            "text": assistant_attachment_text(attachment),
        }));
    }
    serde_json::json!({"role": role, "content": parts})
}

fn ollama_assistant_message(
    role: &str,
    content: &str,
    attachments: &[AssistantChatAttachment],
) -> Value {
    let mut text = content.to_string();
    let mut images = Vec::new();
    for attachment in attachments {
        if let Some(data_url) = attachment.data_url.as_deref() {
            if let Some((_, data)) = decode_image_data_url(data_url) {
                images.push(data);
                text.push_str(&assistant_attachment_text(attachment));
                continue;
            }
        }
        text.push_str(&assistant_attachment_text(attachment));
    }
    if images.is_empty() {
        serde_json::json!({"role": role, "content": text})
    } else {
        serde_json::json!({"role": role, "content": text, "images": images})
    }
}

fn should_retry_without_json_constraint(status: StatusCode, bytes: &[u8]) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    let message = String::from_utf8_lossy(bytes).to_lowercase();
    [
        "response_format",
        "json_object",
        "must contain the word 'json'",
        "unsupported parameter",
        "unrecognized request argument",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn should_retry_without_stream_options(status: StatusCode, bytes: &[u8]) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    let message = String::from_utf8_lossy(bytes).to_lowercase();
    message.contains("stream_options")
        && [
            "unsupported",
            "not supported",
            "unrecognized",
            "unknown",
            "invalid parameter",
            "extra inputs",
            "not permitted",
        ]
        .iter()
        .any(|marker| message.contains(marker))
}

fn should_retry_model_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

async fn wait_for_model_retry(attempt: usize) {
    let delay_ms = match attempt {
        1 => 350,
        2 => 900,
        _ => 1_800,
    };
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}

async fn send_model_request_with_retry(
    request: reqwest::RequestBuilder,
    label: &str,
) -> Result<reqwest::Response, String> {
    let mut last_error = None;
    for attempt in 1..=3 {
        let attempt_request = request
            .try_clone()
            .ok_or_else(|| format!("{label}无法创建安全重试请求"))?;
        match attempt_request.send().await {
            Ok(response) if attempt < 3 && should_retry_model_status(response.status()) => {
                wait_for_model_retry(attempt).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < 3 && (error.is_connect() || error.is_timeout()) => {
                last_error = Some(error.to_string());
                wait_for_model_retry(attempt).await;
            }
            Err(error) => return Err(format!("{label}失败：{error}")),
        }
    }
    Err(format!(
        "{label}连续 3 次网络重试失败：{}",
        last_error.unwrap_or_else(|| "未知网络错误".to_string())
    ))
}

async fn wait_until_model_request_cancelled(cancellation: &AtomicBool) {
    while !cancellation.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn send_cancellable_model_request_with_retry(
    request: reqwest::RequestBuilder,
    label: &str,
    cancellation: &AtomicBool,
) -> Result<reqwest::Response, String> {
    let mut last_error = None;
    for attempt in 1..=3 {
        if cancellation.load(Ordering::Acquire) {
            return Err("AI助手模型请求已取消".to_string());
        }
        let attempt_request = request
            .try_clone()
            .ok_or_else(|| format!("{label}无法创建安全重试请求"))?;
        let response = tokio::select! {
            response = attempt_request.send() => response,
            _ = wait_until_model_request_cancelled(cancellation) => {
                return Err("AI助手模型请求已取消".to_string());
            }
        };
        match response {
            Ok(response) if attempt < 3 && should_retry_model_status(response.status()) => {
                tokio::select! {
                    _ = wait_for_model_retry(attempt) => {},
                    _ = wait_until_model_request_cancelled(cancellation) => {
                        return Err("AI助手模型请求已取消".to_string());
                    }
                }
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < 3 && (error.is_connect() || error.is_timeout()) => {
                last_error = Some(error.to_string());
                tokio::select! {
                    _ = wait_for_model_retry(attempt) => {},
                    _ = wait_until_model_request_cancelled(cancellation) => {
                        return Err("AI助手模型请求已取消".to_string());
                    }
                }
            }
            Err(error) => return Err(format!("{label}失败：{error}")),
        }
    }
    Err(format!(
        "{label}连续 3 次网络重试失败：{}",
        last_error.unwrap_or_else(|| "未知网络错误".to_string())
    ))
}

fn emit_assistant_model_event(
    app: &AppHandle,
    request_id: &str,
    kind: &str,
    received_bytes: usize,
    started: Instant,
    detail: impl Into<String>,
) {
    let _ = app.emit(
        "yunspire://assistant-model-event",
        AssistantModelEvent {
            request_id: request_id.to_string(),
            kind: kind.to_string(),
            received_bytes,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            detail: detail.into(),
            content_delta: None,
            provider_sequence: None,
            channel: None,
        },
    );
}

fn emit_assistant_model_delta(
    app: &AppHandle,
    request_id: &str,
    received_bytes: usize,
    started: Instant,
    content_delta: String,
    provider_sequence: u64,
) {
    let _ = app.emit(
        "yunspire://assistant-model-event",
        AssistantModelEvent {
            request_id: request_id.to_string(),
            kind: "contentDelta".to_string(),
            received_bytes,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            detail: "正在接收模型正文".to_string(),
            content_delta: Some(content_delta),
            provider_sequence: Some(provider_sequence),
            channel: Some("text".to_string()),
        },
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonStringRole {
    Key,
    Target,
    Ignore,
}

struct JsonFieldDeltaDecoder {
    field: String,
    depth: usize,
    expecting_key: bool,
    pending_target_key: bool,
    awaiting_target_value: bool,
    in_string: bool,
    role: JsonStringRole,
    escaped: bool,
    unicode_digits: u8,
    unicode_value: u16,
    pending_high_surrogate: Option<u16>,
    key: String,
    key_too_long: bool,
    complete: bool,
}

impl JsonFieldDeltaDecoder {
    fn new(field: &str) -> Self {
        Self {
            field: field.to_string(),
            depth: 0,
            expecting_key: false,
            pending_target_key: false,
            awaiting_target_value: false,
            in_string: false,
            role: JsonStringRole::Ignore,
            escaped: false,
            unicode_digits: 0,
            unicode_value: 0,
            pending_high_surrogate: None,
            key: String::new(),
            key_too_long: false,
            complete: false,
        }
    }

    fn start_string(&mut self, role: JsonStringRole) {
        self.in_string = true;
        self.role = role;
        self.escaped = false;
        self.unicode_digits = 0;
        self.unicode_value = 0;
        self.pending_high_surrogate = None;
        self.key.clear();
        self.key_too_long = false;
    }

    fn emit_character(&mut self, character: char, output: &mut String) {
        if self.pending_high_surrogate.take().is_some() {
            self.emit_decoded('\u{fffd}', output);
        }
        self.emit_decoded(character, output);
    }

    fn emit_decoded(&mut self, character: char, output: &mut String) {
        match self.role {
            JsonStringRole::Key if !self.key_too_long => {
                self.key.push(character);
                if self.key.chars().count() > self.field.chars().count().saturating_add(8) {
                    self.key.clear();
                    self.key_too_long = true;
                }
            }
            JsonStringRole::Target => output.push(character),
            JsonStringRole::Key | JsonStringRole::Ignore => {}
        }
    }

    fn emit_code_unit(&mut self, unit: u16, output: &mut String) {
        if (0xd800..=0xdbff).contains(&unit) {
            if self.pending_high_surrogate.replace(unit).is_some() {
                self.emit_decoded('\u{fffd}', output);
            }
            return;
        }
        if (0xdc00..=0xdfff).contains(&unit) {
            let Some(high) = self.pending_high_surrogate.take() else {
                self.emit_decoded('\u{fffd}', output);
                return;
            };
            let scalar = 0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
            self.emit_decoded(char::from_u32(scalar).unwrap_or('\u{fffd}'), output);
            return;
        }
        if self.pending_high_surrogate.take().is_some() {
            self.emit_decoded('\u{fffd}', output);
        }
        self.emit_decoded(
            char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'),
            output,
        );
    }

    fn finish_string(&mut self, output: &mut String) {
        if self.pending_high_surrogate.take().is_some() {
            self.emit_decoded('\u{fffd}', output);
        }
        self.in_string = false;
        match self.role {
            JsonStringRole::Key => {
                self.pending_target_key = !self.key_too_long && self.key == self.field;
            }
            JsonStringRole::Target => {
                self.complete = true;
                self.awaiting_target_value = false;
            }
            JsonStringRole::Ignore => {}
        }
        self.role = JsonStringRole::Ignore;
    }

    fn push_string_character(&mut self, character: char, output: &mut String) {
        if self.unicode_digits > 0 {
            let Some(value) = character.to_digit(16) else {
                self.unicode_digits = 0;
                self.unicode_value = 0;
                self.emit_character('\u{fffd}', output);
                return;
            };
            self.unicode_value = (self.unicode_value << 4) | value as u16;
            self.unicode_digits -= 1;
            if self.unicode_digits == 0 {
                let unit = self.unicode_value;
                self.unicode_value = 0;
                self.emit_code_unit(unit, output);
            }
            return;
        }
        if self.escaped {
            self.escaped = false;
            match character {
                '"' => self.emit_character('"', output),
                '\\' => self.emit_character('\\', output),
                '/' => self.emit_character('/', output),
                'b' => self.emit_character('\u{0008}', output),
                'f' => self.emit_character('\u{000c}', output),
                'n' => self.emit_character('\n', output),
                'r' => self.emit_character('\r', output),
                't' => self.emit_character('\t', output),
                'u' => {
                    self.unicode_digits = 4;
                    self.unicode_value = 0;
                }
                _ => self.emit_character('\u{fffd}', output),
            }
            return;
        }
        match character {
            '\\' => self.escaped = true,
            '"' => self.finish_string(output),
            _ => self.emit_character(character, output),
        }
    }

    fn push(&mut self, fragment: &str) -> String {
        if self.complete || fragment.is_empty() {
            return String::new();
        }
        let mut output = String::new();
        for character in fragment.chars() {
            if self.complete {
                break;
            }
            if self.in_string {
                self.push_string_character(character, &mut output);
                continue;
            }
            if self.awaiting_target_value {
                if character.is_whitespace() {
                    continue;
                }
                if character == '"' {
                    self.start_string(JsonStringRole::Target);
                } else {
                    self.awaiting_target_value = false;
                    self.complete = true;
                }
                continue;
            }
            match character {
                '"' => {
                    let role = if self.depth == 1 && self.expecting_key {
                        JsonStringRole::Key
                    } else {
                        JsonStringRole::Ignore
                    };
                    self.start_string(role);
                }
                '{' | '[' => {
                    self.depth = self.depth.saturating_add(1);
                    if self.depth == 1 {
                        self.expecting_key = true;
                    }
                }
                '}' | ']' => {
                    self.depth = self.depth.saturating_sub(1);
                }
                ':' if self.depth == 1 => {
                    self.awaiting_target_value = self.pending_target_key;
                    self.pending_target_key = false;
                    self.expecting_key = false;
                }
                ',' if self.depth == 1 => {
                    self.expecting_key = true;
                    self.pending_target_key = false;
                }
                character if !character.is_whitespace() && self.pending_target_key => {
                    self.pending_target_key = false;
                }
                _ => {}
            }
        }
        output
    }
}

#[derive(Default)]
struct ModelTransportStreamDecoder {
    pending: Vec<u8>,
}

impl ModelTransportStreamDecoder {
    fn parse_line(line: &[u8]) -> Option<Value> {
        let mut line = line;
        while line.first().is_some_and(u8::is_ascii_whitespace) {
            line = &line[1..];
        }
        while line.last().is_some_and(u8::is_ascii_whitespace) {
            line = &line[..line.len().saturating_sub(1)];
        }
        if line.is_empty()
            || line.starts_with(b":")
            || line.starts_with(b"event:")
            || line.starts_with(b"id:")
            || line.starts_with(b"retry:")
        {
            return None;
        }
        if line.starts_with(b"data:") {
            line = &line[5..];
            while line.first().is_some_and(u8::is_ascii_whitespace) {
                line = &line[1..];
            }
        }
        if line.is_empty() || line == b"[DONE]" {
            return None;
        }
        serde_json::from_slice::<Value>(line).ok()
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.pending.extend_from_slice(chunk);
        let Some(last_newline) = self.pending.iter().rposition(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        let remainder = self.pending.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.pending, remainder);
        complete
            .split(|byte| *byte == b'\n')
            .filter_map(Self::parse_line)
            .collect()
    }

    fn finish(&mut self) -> Vec<Value> {
        let pending = std::mem::take(&mut self.pending);
        Self::parse_line(&pending).into_iter().collect()
    }
}

fn model_stream_text_fragment(payload: &Value) -> Option<String> {
    let mut content = String::new();
    let is_ollama_stream = payload.get("done").is_some();
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let values = [
        payload.pointer("/choices/0/delta/content"),
        payload.pointer("/choices/0/text"),
        (is_ollama_stream)
            .then(|| payload.pointer("/message/content"))
            .flatten(),
        event_type
            .contains("content_block")
            .then(|| payload.pointer("/delta/text"))
            .flatten(),
        event_type
            .contains("content_block")
            .then(|| payload.pointer("/content_block/text"))
            .flatten(),
        event_type
            .ends_with("output_text.delta")
            .then(|| payload.get("delta"))
            .flatten(),
    ];
    for value in values {
        append_response_text(&mut content, value);
        if !content.is_empty() {
            break;
        }
    }
    (!content.is_empty()).then_some(content)
}

fn emit_decoded_model_delta(
    app: &AppHandle,
    request_id: &str,
    received_bytes: usize,
    started: Instant,
    delta: &str,
    provider_sequence: &mut u64,
) {
    for_each_model_delta_chunk(delta, |chunk| {
        emit_assistant_model_delta(
            app,
            request_id,
            received_bytes,
            started,
            chunk.to_string(),
            *provider_sequence,
        );
        *provider_sequence = provider_sequence.saturating_add(1);
    });
}

fn for_each_model_delta_chunk(delta: &str, mut visitor: impl FnMut(&str)) {
    let mut start = 0;
    while start < delta.len() {
        let mut end = start
            .saturating_add(MAX_MODEL_EVENT_DELTA_BYTES)
            .min(delta.len());
        while end > start && !delta.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        visitor(&delta[start..end]);
        start = end;
    }
}

struct CancellableModelResponse {
    status: StatusCode,
    response_text: String,
    usage: Option<Value>,
    finish_reason: String,
    diagnostic_bytes: Vec<u8>,
}

impl CancellableModelResponse {
    fn diagnostic_bytes(&self) -> &[u8] {
        &self.diagnostic_bytes
    }

    fn take_response_text(&mut self) -> Result<String, String> {
        if self.response_text.trim().is_empty() {
            if matches!(self.finish_reason.as_str(), "length" | "max_tokens") {
                return Err("AI助手模型已耗尽输出 token 上限，未生成最终意图结果".to_string());
            }
            return Err("AI助手流式响应缺少文本内容".to_string());
        }
        Ok(std::mem::take(&mut self.response_text))
    }
}

#[allow(clippy::too_many_arguments)]
fn absorb_model_stream_payload(
    payload: Value,
    content: &mut String,
    direct_text: &mut Option<String>,
    usage: &mut serde_json::Map<String, Value>,
    finish_reason: &mut String,
    field_decoder: &mut Option<JsonFieldDeltaDecoder>,
    app: &AppHandle,
    request_id: &str,
    received_bytes: usize,
    started: Instant,
    provider_sequence: &mut u64,
) {
    merge_assistant_usage_payload(usage, &payload);
    if let Some(reason) = payload
        .pointer("/choices/0/finish_reason")
        .or_else(|| payload.get("done_reason"))
        .or_else(|| payload.pointer("/delta/stop_reason"))
        .and_then(Value::as_str)
    {
        finish_reason.clear();
        finish_reason.push_str(reason);
    }
    if let Some(fragment) = model_stream_text_fragment(&payload) {
        direct_text.take();
        if let Some(decoder) = field_decoder.as_mut() {
            let delta = decoder.push(&fragment);
            if !delta.is_empty() {
                emit_decoded_model_delta(
                    app,
                    request_id,
                    received_bytes,
                    started,
                    &delta,
                    provider_sequence,
                );
            }
        }
        content.push_str(&fragment);
    } else if content.is_empty() {
        if let Ok(text) = model_text(&payload) {
            *direct_text = Some(text);
        }
    }
}

async fn read_cancellable_model_response(
    response: reqwest::Response,
    request_id: &str,
    cancellation: &AtomicBool,
    app: &AppHandle,
    started: Instant,
    stream_json_field: Option<&str>,
    provider_sequence: &mut u64,
) -> Result<CancellableModelResponse, String> {
    let status = response.status();
    if !status.is_success()
        && response
            .content_length()
            .is_some_and(|length| length > MAX_MODEL_CONTROL_RESPONSE_BYTES)
    {
        return Err("AI助手模型错误响应超过 2 MB 安全上限".to_string());
    }
    let direct_json_response = status.is_success()
        && response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase)
            .is_some_and(|content_type| {
                content_type.contains("application/json")
                    && !content_type.contains("ndjson")
                    && !content_type.contains("jsonl")
            });
    let mut stream = response.bytes_stream();
    let emit_content_deltas = status.is_success() && stream_json_field.is_some();
    let mut transport_decoder = ModelTransportStreamDecoder::default();
    let mut field_decoder = stream_json_field.map(JsonFieldDeltaDecoder::new);
    let mut response_text = String::new();
    let mut direct_text = None;
    let mut direct_json_bytes = Vec::new();
    let mut usage = serde_json::Map::new();
    let mut finish_reason = String::new();
    let mut diagnostic_bytes = Vec::new();
    let mut received_bytes = 0usize;
    loop {
        let chunk = tokio::select! {
            chunk = stream.next() => chunk,
            _ = wait_until_model_request_cancelled(cancellation) => {
                return Err("AI助手模型请求已取消".to_string());
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|error| format!("无法读取 AI助手模型流：{error}"))?;
        received_bytes = received_bytes.saturating_add(chunk.len());
        if status.is_success() && direct_json_response {
            direct_json_bytes.extend_from_slice(&chunk);
        } else if status.is_success() {
            for payload in transport_decoder.push(&chunk) {
                absorb_model_stream_payload(
                    payload,
                    &mut response_text,
                    &mut direct_text,
                    &mut usage,
                    &mut finish_reason,
                    &mut field_decoder,
                    app,
                    request_id,
                    received_bytes,
                    started,
                    provider_sequence,
                );
            }
        } else {
            if diagnostic_bytes.len().saturating_add(chunk.len())
                > MAX_MODEL_CONTROL_RESPONSE_BYTES as usize
            {
                return Err("AI助手模型错误响应超过 2 MB 安全上限".to_string());
            }
            diagnostic_bytes.extend_from_slice(&chunk);
        }
        emit_assistant_model_event(
            app,
            request_id,
            "chunk",
            received_bytes,
            started,
            "正在接收模型响应",
        );
    }
    if status.is_success() {
        let payloads = if direct_json_response {
            serde_json::from_slice::<Value>(&direct_json_bytes)
                .map(|payload| vec![payload])
                .unwrap_or_else(|_| model_response_payloads(&direct_json_bytes))
        } else {
            transport_decoder.finish()
        };
        for payload in payloads {
            absorb_model_stream_payload(
                payload,
                &mut response_text,
                &mut direct_text,
                &mut usage,
                &mut finish_reason,
                &mut field_decoder,
                app,
                request_id,
                received_bytes,
                started,
                provider_sequence,
            );
        }
        if response_text.is_empty() {
            if let Some(text) = direct_text.take() {
                if emit_content_deltas {
                    if let Some(decoder) = field_decoder.as_mut() {
                        let delta = decoder.push(&text);
                        if !delta.is_empty() {
                            emit_decoded_model_delta(
                                app,
                                request_id,
                                received_bytes,
                                started,
                                &delta,
                                provider_sequence,
                            );
                        }
                    }
                }
                response_text = text;
            }
        }
    }
    Ok(CancellableModelResponse {
        status,
        response_text,
        usage: (!usage.is_empty()).then_some(Value::Object(usage)),
        finish_reason,
        diagnostic_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
async fn send_and_read_cancellable_model_request(
    request: reqwest::RequestBuilder,
    label: &str,
    request_id: &str,
    cancellation: &AtomicBool,
    app: &AppHandle,
    started: Instant,
    stream_json_field: Option<&str>,
    provider_sequence: &mut u64,
) -> Result<CancellableModelResponse, String> {
    let response = send_cancellable_model_request_with_retry(request, label, cancellation).await?;
    read_cancellable_model_response(
        response,
        request_id,
        cancellation,
        app,
        started,
        stream_json_field,
        provider_sequence,
    )
    .await
}

fn parse_assistant_turn(text: &str) -> Result<AssistantTurn, String> {
    let trimmed = text.trim();
    let json = serde_json::from_str::<Value>(trimmed)
        .ok()
        .or_else(|| first_json_object(trimmed))
        .ok_or_else(|| "模型没有返回有效的意图 JSON".to_string())?;
    let reply = json
        .get("reply")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "模型意图响应缺少 reply".to_string())?;
    let intent = json
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .trim()
        .to_lowercase();
    let intent = if ASSISTANT_INTENTS.contains(&intent.as_str()) || intent == "general" {
        intent
    } else {
        "chat".to_string()
    };
    let action = json
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .trim()
        .to_lowercase();
    let action = if matches!(action.as_str(), "chat" | "execute" | "clarify") {
        action
    } else {
        "chat".to_string()
    };
    let confidence = json
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let capability_ids = json
        .get("capability_ids")
        .or_else(|| json.get("capabilityIds"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .take(16)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let operation = json
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .trim()
        .to_lowercase();
    let operation = if ASSISTANT_OPERATIONS.contains(&operation.as_str()) {
        operation
    } else {
        "none".to_string()
    };
    let parameters = json
        .get("parameters")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let reason = json
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(480)
        .collect::<String>();
    let choices = json
        .get("choices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(6)
                .filter_map(|item| {
                    let object = item.as_object()?;
                    let id = object.get("id").and_then(Value::as_str)?.trim();
                    let label = object.get("label").and_then(Value::as_str)?.trim();
                    if id.is_empty() || label.is_empty() {
                        return None;
                    }
                    Some(AssistantChoice {
                        id: id.chars().take(64).collect(),
                        label: label.chars().take(120).collect(),
                        description: object
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .chars()
                            .take(240)
                            .collect(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(AssistantTurn {
        reply: reply.to_string(),
        intent,
        action,
        confidence,
        capability_ids,
        operation,
        parameters,
        reason,
        decision_receipt: String::new(),
        choices,
        usage: ModelUsageSummary::default(),
        trace_id: String::new(),
    })
}

fn explicit_skill_run_requested(messages: &[NormalizedAssistantMessage]) -> bool {
    let Some((role, content, _)) = messages.iter().rev().find(|(role, _, _)| role == "user") else {
        return false;
    };
    if role != "user" {
        return false;
    }
    let normalized = content.to_lowercase();
    [
        "运行", "执行", "应用", "使用", "run", "execute", "apply", "use",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn fallback_skill_run_input(messages: &[NormalizedAssistantMessage]) -> Value {
    let Some((_, content, attachments)) = messages.iter().rev().find(|(role, _, _)| role == "user")
    else {
        return serde_json::json!({"message": "", "attachments": []});
    };
    serde_json::json!({
        "message": content,
        "attachments": attachments.iter().map(|attachment| serde_json::json!({
            "name": attachment.name,
            "mimeType": attachment.mime_type,
            "textContent": attachment.text_content,
        })).collect::<Vec<_>>(),
    })
}

fn selected_skill_input(parameters: &Value, skill_id: &str) -> Option<Value> {
    let entry_input = parameters
        .get("skills")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find_map(|entry| {
                let entry_id = entry
                    .get("skillId")
                    .or_else(|| entry.get("skill_id"))
                    .or_else(|| entry.get("id"))
                    .and_then(Value::as_str)?
                    .trim();
                let entry_id = entry_id.strip_prefix("skill:").unwrap_or(entry_id);
                if entry_id == skill_id {
                    entry.get("input").cloned()
                } else {
                    None
                }
            })
        });
    entry_input.or_else(|| {
        parameters
            .get("skillInput")
            .or_else(|| parameters.get("skill_input"))
            .or_else(|| parameters.get("input"))
            .cloned()
    })
}

fn force_selected_skill_run(
    turn: &mut AssistantTurn,
    selected_user_skills: &[Value],
    messages: &[NormalizedAssistantMessage],
) {
    if selected_user_skills.is_empty() || !explicit_skill_run_requested(messages) {
        return;
    }
    let fallback_input = fallback_skill_run_input(messages);
    let original_parameters = turn.parameters.clone();
    let selected_skill_ids = selected_user_skills
        .iter()
        .filter_map(|capability| capability.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let selected_skill_metadata = selected_user_skills
        .iter()
        .filter_map(|capability| {
            let capability_id = capability.get("id").and_then(Value::as_str)?;
            let skill_id = capability_id.strip_prefix("skill:")?;
            Some(serde_json::json!({
                "skillId": skill_id,
                "version": capability.get("version").cloned().unwrap_or(Value::Null),
                "payloadHash": capability.get("payloadHash").cloned().unwrap_or(Value::Null),
                "input": selected_skill_input(&original_parameters, skill_id)
                    .unwrap_or_else(|| fallback_input.clone()),
            }))
        })
        .collect::<Vec<_>>();
    turn.action = "execute".to_string();
    turn.intent = "skills".to_string();
    turn.operation = "run".to_string();
    turn.capability_ids = std::iter::once("system:skills".to_string())
        .chain(selected_skill_ids)
        .collect();
    turn.parameters = serde_json::json!({"skills": selected_skill_metadata});
    turn.confidence = turn.confidence.max(0.75);
    turn.reason = if turn.reason.is_empty() {
        "用户已明确要求运行显式选中的 Skill".to_string()
    } else {
        format!("{}；已按显式选中的 Skill 收敛为 skills/run", turn.reason)
    };
}

fn first_json_object(text: &str) -> Option<Value> {
    for (start, character) in text.char_indices() {
        if character != '{' {
            continue;
        }
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, current) in text[start..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if current == '\\' {
                    escaped = true;
                } else if current == '"' {
                    in_string = false;
                }
                continue;
            }
            match current {
                '"' => in_string = true,
                '{' => depth = depth.saturating_add(1),
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let end = start + offset + current.len_utf8();
                        if let Ok(value) = serde_json::from_str::<Value>(&text[start..end]) {
                            return Some(value);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn analysis_identifier(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    if value.is_empty()
        || value.chars().count() > 180
        || value.chars().any(char::is_control)
        || value.contains("attachment://")
    {
        return None;
    }
    Some(value.to_string())
}

fn analysis_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(value @ (Value::Object(_) | Value::Array(_))) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        Some(value) if !value.is_null() => value.to_string(),
        _ => String::new(),
    }
}

fn analysis_string_list(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let text = match item {
                Value::String(value) => value.trim().to_string(),
                Value::Object(object) => ["name", "label", "title", "value"]
                    .into_iter()
                    .find_map(|field| object.get(field).and_then(Value::as_str))
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                _ => String::new(),
            };
            (!text.is_empty()).then_some(Value::String(text))
        })
        .collect()
}

fn visual_manifest_asset_ids(content: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in content.lines() {
        let Some((_, remainder)) = line.split_once("asset_id=") else {
            continue;
        };
        let identifier = remainder
            .split(|character: char| {
                character.is_whitespace() || matches!(character, '；' | ';' | ',' | '，')
            })
            .next()
            .unwrap_or_default()
            .trim();
        if identifier.is_empty()
            || identifier.chars().count() > 180
            || identifier.chars().any(char::is_control)
        {
            continue;
        }
        if !ids.iter().any(|existing| existing == identifier) {
            ids.push(identifier.to_string());
        }
    }
    ids
}

fn capture_binding_identifier(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 180
        || value.chars().any(char::is_control)
        || value.contains("attachment://")
    {
        return None;
    }
    Some(value.to_string())
}

fn capture_binding_sha256(value: &str, label: &str) -> Result<String, String> {
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

fn capture_binding_mime_type(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() <= "image/".len()
        || !value.starts_with("image/")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'))
    {
        return Err("图片 binding 的 analysisMimeType 无效".to_string());
    }
    Ok(value)
}

fn normalize_capture_image_binding(
    binding: CaptureImageBinding,
    index: usize,
) -> Result<CaptureImageBinding, String> {
    let position = index + 1;
    let asset_id = capture_binding_identifier(&binding.asset_id)
        .ok_or_else(|| format!("第 {position} 个图片 binding 的 assetId 无效"))?;
    let mut seen_references = HashSet::new();
    let mut reference_ids = Vec::new();
    for reference_id in binding.reference_ids {
        let reference_id = capture_binding_identifier(&reference_id)
            .ok_or_else(|| format!("assetId={asset_id} 的图片 binding 包含无效 referenceId"))?;
        if seen_references.insert(reference_id.clone()) {
            reference_ids.push(reference_id);
        }
    }
    if reference_ids.is_empty() {
        reference_ids.push(asset_id.clone());
    }
    let original_sha256 = capture_binding_sha256(
        &binding.original_sha256,
        &format!("assetId={asset_id} 的 originalSha256"),
    )?;
    let analysis_sha256 = capture_binding_sha256(
        &binding.analysis_sha256,
        &format!("assetId={asset_id} 的 analysisSha256"),
    )?;
    if binding.original_byte_length == 0 || binding.analysis_byte_length == 0 {
        return Err(format!(
            "assetId={asset_id} 的图片 binding 字节数必须大于 0"
        ));
    }
    let analysis_mime_type = capture_binding_mime_type(&binding.analysis_mime_type)?;
    if !binding.derived
        && (original_sha256 != analysis_sha256
            || binding.original_byte_length != binding.analysis_byte_length)
    {
        return Err(format!(
            "assetId={asset_id} 标记为非派生输入，但原始/分析哈希或字节数不一致"
        ));
    }
    Ok(CaptureImageBinding {
        asset_id,
        reference_ids,
        original_sha256,
        analysis_sha256,
        original_byte_length: binding.original_byte_length,
        analysis_byte_length: binding.analysis_byte_length,
        analysis_mime_type,
        derived: binding.derived,
    })
}

fn prepare_capture_analysis_images(
    image_data_urls: &[String],
    image_bindings: Option<Vec<CaptureImageBinding>>,
) -> Result<PreparedCaptureAnalysisImages, String> {
    let mut accepted_images = Vec::new();
    let mut accepted_image_facts = Vec::new();
    let mut accepted_image_bytes = 0usize;
    for (index, data_url) in image_data_urls.iter().enumerate() {
        let Some((header, encoded)) = data_url
            .strip_prefix("data:")
            .and_then(|value| value.split_once(','))
        else {
            return Err(format!(
                "第 {} 个模型分析图片不是有效的 data URL",
                index + 1
            ));
        };
        let mut header_parts = header.split(';');
        let mime_type = capture_binding_mime_type(header_parts.next().unwrap_or_default())?;
        if !header_parts.any(|part| part.trim().eq_ignore_ascii_case("base64")) {
            return Err(format!(
                "第 {} 个模型分析图片不是 base64 data URL",
                index + 1
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| format!("第 {} 个模型分析图片包含无效 base64", index + 1))?;
        if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
            return Err("单张模型分析图片为空或超过 4 MB 安全上限".to_string());
        }
        if accepted_image_bytes.saturating_add(bytes.len()) > MAX_ANALYSIS_IMAGE_BYTES_PER_REQUEST {
            return Err("单次模型分析图片总量超过 12 MB，请由云枢继续分批".to_string());
        }
        accepted_image_bytes += bytes.len();
        let byte_length = bytes.len() as u64;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let normalized = base64::engine::general_purpose::STANDARD.encode(bytes);
        accepted_image_facts.push((mime_type.clone(), byte_length, sha256));
        accepted_images.push((mime_type, normalized));
    }

    let raw_bindings = image_bindings.unwrap_or_default();
    if !accepted_images.is_empty() && raw_bindings.len() != accepted_images.len() {
        return Err(format!(
            "本地视觉输入有 {} 张图片，但 imageBindings 有 {} 项；必须按相同顺序逐一绑定",
            accepted_images.len(),
            raw_bindings.len()
        ));
    }
    let mut seen_assets = HashSet::new();
    let mut normalized_bindings = Vec::with_capacity(raw_bindings.len());
    for (index, binding) in raw_bindings.into_iter().enumerate() {
        let binding = normalize_capture_image_binding(binding, index)?;
        if !seen_assets.insert(binding.asset_id.clone()) {
            return Err(format!(
                "图片 binding 的 assetId={} 重复，无法确定视觉输入归属",
                binding.asset_id
            ));
        }
        normalized_bindings.push(binding);
    }
    for (index, (binding, (mime_type, byte_length, sha256))) in normalized_bindings
        .iter()
        .zip(&accepted_image_facts)
        .enumerate()
    {
        if binding.analysis_mime_type != *mime_type {
            return Err(format!(
                "第 {} 个视觉输入 MIME 与 assetId={} 的 binding 不一致",
                index + 1,
                binding.asset_id
            ));
        }
        if binding.analysis_byte_length != *byte_length {
            return Err(format!(
                "第 {} 个视觉输入字节数与 assetId={} 的 analysisByteLength 不一致",
                index + 1,
                binding.asset_id
            ));
        }
        if binding.analysis_sha256 != *sha256 {
            return Err(format!(
                "第 {} 个视觉输入哈希与 assetId={} 的 analysisSha256 不一致",
                index + 1,
                binding.asset_id
            ));
        }
    }
    Ok(PreparedCaptureAnalysisImages {
        images: accepted_images,
        bindings: normalized_bindings,
    })
}

fn image_observation_constraints(
    expected_asset_ids: &[String],
    image_bindings: &[CaptureImageBinding],
) -> HashMap<String, Vec<String>> {
    let mut constraints = expected_asset_ids
        .iter()
        .map(|asset_id| (asset_id.clone(), vec![asset_id.clone()]))
        .collect::<HashMap<_, _>>();
    for binding in image_bindings {
        constraints.insert(binding.asset_id.clone(), binding.reference_ids.clone());
    }
    constraints
}

fn normalize_image_observations(
    value: Option<&Value>,
    constraints: &HashMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut observations = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let object = item.as_object();
        let asset_id = object.and_then(|object| {
            analysis_identifier(object.get("asset_id").or_else(|| object.get("assetId"))).or_else(
                || {
                    analysis_identifier(
                        object
                            .get("reference_id")
                            .or_else(|| object.get("referenceId")),
                    )
                },
            )
        });
        let Some(asset_id) = asset_id else {
            warnings.push(format!("第 {} 条图片观察缺少 asset_id，已忽略", index + 1));
            continue;
        };
        let Some(reference_ids) = constraints.get(&asset_id) else {
            warnings.push(format!(
                "图片观察引用了未提交的 asset_id={asset_id}，已忽略"
            ));
            continue;
        };
        if !seen.insert(asset_id.clone()) {
            warnings.push(format!("asset_id={asset_id} 的重复图片观察已忽略"));
            continue;
        }
        let requested_reference_id = object.and_then(|object| {
            analysis_identifier(
                object
                    .get("reference_id")
                    .or_else(|| object.get("referenceId")),
            )
        });
        let fallback_reference_id = reference_ids
            .first()
            .cloned()
            .unwrap_or_else(|| asset_id.clone());
        let reference_id = requested_reference_id
            .as_ref()
            .filter(|reference_id| reference_ids.contains(reference_id))
            .cloned()
            .unwrap_or_else(|| fallback_reference_id.clone());
        if requested_reference_id
            .as_ref()
            .is_some_and(|requested| requested != &reference_id)
        {
            warnings.push(format!(
                "asset_id={asset_id} 返回了未绑定的 reference_id={}，已规范化为 {reference_id}",
                requested_reference_id.as_deref().unwrap_or_default()
            ));
        }
        let observation = object
            .map(|object| {
                analysis_text(
                    object
                        .get("observation")
                        .or_else(|| object.get("description"))
                        .or_else(|| object.get("summary"))
                        .or_else(|| object.get("content")),
                )
            })
            .unwrap_or_else(|| analysis_text(Some(item)));
        if observation.is_empty() {
            warnings.push(format!(
                "asset_id={asset_id} 的图片观察没有有效内容，已忽略"
            ));
            continue;
        }
        let text = object
            .map(|object| {
                analysis_text(
                    object
                        .get("text")
                        .or_else(|| object.get("ocr_text"))
                        .or_else(|| object.get("ocrText")),
                )
            })
            .unwrap_or_default();
        let context = object
            .map(|object| analysis_text(object.get("context").or_else(|| object.get("position"))))
            .unwrap_or_default();
        let evidence = object
            .map(|object| analysis_text(object.get("evidence")))
            .unwrap_or_default();
        let confidence = object
            .and_then(|object| object.get("confidence"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        observations.push(serde_json::json!({
            "asset_id": asset_id,
            "reference_id": reference_id,
            "observation": observation,
            "text": text,
            "context": context,
            "evidence": evidence,
            "confidence": confidence,
        }));
    }
    observations
}

fn normalize_document_relations(value: Option<&Value>, warnings: &mut Vec<String>) -> Vec<Value> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut relations = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            warnings.push(format!("第 {} 条文档关系不是对象，已忽略", index + 1));
            continue;
        };
        let source_id =
            analysis_identifier(object.get("source_id").or_else(|| object.get("sourceId")));
        let target_id =
            analysis_identifier(object.get("target_id").or_else(|| object.get("targetId")));
        let relation = analysis_text(object.get("relation").or_else(|| object.get("type")));
        let evidence = analysis_text(object.get("evidence"));
        let (Some(source_id), Some(target_id)) = (source_id, target_id) else {
            warnings.push(format!(
                "第 {} 条文档关系缺少 source_id 或 target_id，已忽略",
                index + 1
            ));
            continue;
        };
        if relation.is_empty() || evidence.is_empty() {
            warnings.push(format!(
                "{source_id} -> {target_id} 缺少关系类型或证据，已忽略"
            ));
            continue;
        }
        let confidence = object
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        relations.push(serde_json::json!({
            "source_id": source_id,
            "target_id": target_id,
            "relation": relation,
            "evidence": evidence,
            "confidence": confidence,
        }));
    }
    relations
}

fn normalize_capture_analysis(
    parsed: Value,
    expected_asset_ids: &[String],
    image_bindings: &[CaptureImageBinding],
) -> Result<Value, String> {
    let input = parsed
        .as_object()
        .ok_or_else(|| "模型分析结果必须是 JSON 对象".to_string())?;
    let summary = analysis_text(input.get("summary"));
    let analysis_markdown = analysis_text(
        input
            .get("analysis_markdown")
            .or_else(|| input.get("analysisMarkdown")),
    );
    let analysis_markdown = if analysis_markdown.is_empty() {
        summary.clone()
    } else {
        analysis_markdown
    };
    if analysis_markdown.trim().is_empty() {
        return Err("模型分析没有返回有效摘要或分析正文".to_string());
    }
    let mut warnings = input
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|warning| analysis_text(Some(warning)))
        .filter(|warning| !warning.is_empty())
        .collect::<Vec<_>>();
    let image_constraints = image_observation_constraints(expected_asset_ids, image_bindings);
    let image_observations = normalize_image_observations(
        input
            .get("image_observations")
            .or_else(|| input.get("imageObservations")),
        &image_constraints,
        &mut warnings,
    );
    let relations = normalize_document_relations(input.get("relations"), &mut warnings);
    if !image_constraints.is_empty() {
        let observed = image_observations
            .iter()
            .filter_map(|item| item.get("asset_id").and_then(Value::as_str))
            .collect::<HashSet<_>>();
        let mut expected_assets = image_constraints.keys().collect::<Vec<_>>();
        expected_assets.sort_unstable();
        for asset_id in expected_assets {
            if !observed.contains(asset_id.as_str()) {
                warnings.push(format!(
                    "视觉输入 asset_id={asset_id} 没有返回可验证的逐图分析"
                ));
            }
        }
    }
    Ok(serde_json::json!({
        "summary": if summary.is_empty() { analysis_markdown.clone() } else { summary },
        "tags": analysis_string_list(input.get("tags")),
        "entities": analysis_string_list(input.get("entities")),
        "key_points": analysis_string_list(input.get("key_points").or_else(|| input.get("keyPoints"))),
        "analysis_markdown": analysis_markdown,
        "image_observations": image_observations,
        "image_bindings": image_bindings,
        "relations": relations,
        "warnings": warnings,
    }))
}

struct ConfiguredChatModel {
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ConfiguredEmbeddingModel {
    pub(crate) provider_id: String,
    pub(crate) provider: String,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) model: String,
}

pub(crate) fn configured_embedding_model(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<Option<ConfiguredEmbeddingModel>, String> {
    let providers = database.load_model_providers(workspace_scope)?;
    for profile in providers {
        let Some(model) = profile
            .defaults
            .get("embedding")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let model_available = profile.available_models.as_array().is_some_and(|models| {
            models
                .iter()
                .any(|candidate| candidate.get("id").and_then(Value::as_str) == Some(model))
        });
        let model_assigned = profile
            .assignments
            .get("embedding")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models
                    .iter()
                    .any(|candidate| candidate.as_str() == Some(model))
            });
        if !model_available || !model_assigned {
            return Err(format!(
                "当前 Embedding 默认模型 {model} 不在供应商的可用与已分配模型集合中"
            ));
        }
        let provider = profile.provider.trim().to_lowercase();
        let api_key = if profile.api_key_ciphertext.is_empty() {
            String::new()
        } else {
            let encryption_key = database.device_encryption_key()?;
            crate::model_config::decrypt_api_key_with_key(
                &encryption_key,
                &format!("{workspace_scope}:model-provider:{}", profile.id),
                &profile.api_key_ciphertext,
            )?
        };
        if provider != "ollama" && api_key.trim().is_empty() {
            return Err("Embedding 模型需要本地 API 密钥，请在设置中保存一次".to_string());
        }
        return Ok(Some(ConfiguredEmbeddingModel {
            provider_id: profile.id,
            provider,
            base_url: profile.base_url.trim().to_string(),
            api_key,
            model: model.to_string(),
        }));
    }
    Ok(None)
}

fn normalize_embedding_vector(value: &Value) -> Result<Vec<f32>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| "Embedding 向量必须是数值数组".to_string())?;
    if entries.is_empty() || entries.len() > MAX_EMBEDDING_DIMENSIONS {
        return Err("Embedding 向量维度为空或超过安全上限".to_string());
    }
    let mut vector = entries
        .iter()
        .map(|entry| {
            let value = entry
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| "Embedding 向量包含非有限数值".to_string())?;
            let value = value as f32;
            value
                .is_finite()
                .then_some(value)
                .ok_or_else(|| "Embedding 向量数值超出 f32 范围".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON {
        return Err("Embedding 向量不能是零向量".to_string());
    }
    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(vector)
}

fn parse_embedding_vectors(
    provider: &str,
    payload: &Value,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, String> {
    let mut vectors = if provider == "ollama" {
        payload
            .get("embeddings")
            .and_then(Value::as_array)
            .ok_or_else(|| "Ollama Embedding 响应缺少 embeddings 数组".to_string())?
            .iter()
            .map(normalize_embedding_vector)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let entries = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "Embedding 响应缺少 data 数组".to_string())?;
        let mut ordered = vec![None; expected_count];
        for (position, entry) in entries.iter().enumerate() {
            let index = entry
                .get("index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(position);
            if index >= expected_count || ordered[index].is_some() {
                return Err("Embedding 响应包含越界或重复索引".to_string());
            }
            let vector = entry
                .get("embedding")
                .ok_or_else(|| "Embedding 响应项缺少 embedding 数组".to_string())?;
            ordered[index] = Some(normalize_embedding_vector(vector)?);
        }
        ordered
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "Embedding 响应缺少部分输入向量".to_string())?
    };
    if vectors.len() != expected_count {
        return Err(format!(
            "Embedding 响应数量不完整：期望 {expected_count}，实际 {}",
            vectors.len()
        ));
    }
    let dimensions = vectors.first().map(Vec::len).unwrap_or_default();
    if vectors.iter().any(|vector| vector.len() != dimensions) {
        return Err("Embedding 响应的向量维度不一致".to_string());
    }
    Ok(std::mem::take(&mut vectors))
}

pub(crate) async fn request_embeddings(
    configured: &ConfiguredEmbeddingModel,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() || inputs.len() > MAX_EMBEDDING_BATCH_INPUTS {
        return Err("Embedding 批次为空或超过 64 个输入".to_string());
    }
    let mut total_characters = 0_usize;
    for input in inputs {
        let characters = input.chars().count();
        if characters == 0 || characters > MAX_EMBEDDING_INPUT_CHARS {
            return Err("单个 Embedding 输入为空或超过 32000 个字符".to_string());
        }
        total_characters = total_characters.saturating_add(characters);
    }
    if total_characters > MAX_EMBEDDING_TOTAL_CHARS {
        return Err("Embedding 批次总输入超过 512000 个字符".to_string());
    }
    let endpoint = embedding_endpoint(&configured.provider, &configured.base_url)?;
    let body = if configured.provider == "ollama" {
        serde_json::json!({
            "model": configured.model,
            "input": inputs,
            "truncate": true,
        })
    } else {
        serde_json::json!({
            "model": configured.model,
            "input": inputs,
            "encoding_format": "float",
        })
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(EMBEDDING_REQUEST_TIMEOUT_SECONDS))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化 Embedding 请求：{error}"))?;
    let request = client
        .post(endpoint.clone())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(&body);
    let request = if configured.provider == "ollama" && configured.api_key.trim().is_empty() {
        request
    } else {
        request.header(
            AUTHORIZATION,
            format!("Bearer {}", configured.api_key.trim()),
        )
    };
    let response = request
        .send()
        .await
        .map_err(|error| format!("Embedding 请求失败（{}）：{error}", endpoint.path()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_EMBEDDING_RESPONSE_BYTES as u64)
    {
        return Err("Embedding 响应超过 16 MB 安全上限".to_string());
    }
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取 Embedding 响应失败：{error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_EMBEDDING_RESPONSE_BYTES {
            return Err("Embedding 响应超过 16 MB 安全上限".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(model_request_error(
            "Embedding 接口",
            status,
            &bytes,
            &configured.api_key,
        ));
    }
    let payload = serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("Embedding 接口没有返回有效 JSON：{error}"))?;
    parse_embedding_vectors(&configured.provider, &payload, inputs.len())
}

fn configured_chat_model(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<ConfiguredChatModel, String> {
    let providers = database.load_model_providers(workspace_scope)?;
    for profile in providers {
        let Some(model) = profile
            .defaults
            .get("chat")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let model_available = profile.available_models.as_array().is_some_and(|models| {
            models
                .iter()
                .any(|candidate| candidate.get("id").and_then(Value::as_str) == Some(model))
        });
        let model_assigned = profile
            .assignments
            .get("chat")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models
                    .iter()
                    .any(|candidate| candidate.as_str() == Some(model))
            });
        if !model_available || !model_assigned {
            return Err(format!(
                "当前聊天默认模型 {model} 不在供应商的可用与已分配模型集合中"
            ));
        }
        let api_key = if profile.api_key_ciphertext.is_empty() {
            String::new()
        } else {
            let encryption_key = database.device_encryption_key()?;
            crate::model_config::decrypt_api_key_with_key(
                &encryption_key,
                &format!("{workspace_scope}:model-provider:{}", profile.id),
                &profile.api_key_ciphertext,
            )?
        };
        return Ok(ConfiguredChatModel {
            provider: profile.provider.trim().to_lowercase(),
            base_url: profile.base_url.trim().to_string(),
            api_key,
            model: model.to_string(),
        });
    }
    Err("尚未配置聊天默认模型，无法执行自定义 Skill".to_string())
}

fn valid_model_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 160
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn parse_approved_skill_model_output(text: &str) -> Result<(String, Value, Vec<String>), String> {
    let trimmed = text.trim();
    let payload = serde_json::from_str::<Value>(trimmed)
        .ok()
        .or_else(|| first_json_object(trimmed))
        .ok_or_else(|| "Skill 模型没有返回有效 JSON".to_string())?;
    let output_text = payload
        .get("outputText")
        .or_else(|| payload.get("output_text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Skill 模型响应缺少 outputText".to_string())?
        .chars()
        .take(200_000)
        .collect::<String>();
    let output_data = payload
        .get("outputData")
        .or_else(|| payload.get("output_data"))
        .cloned()
        .ok_or_else(|| "Skill 模型响应缺少 outputData".to_string())?;
    let warnings = payload
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .take(64)
                .map(|value| value.chars().take(1_000).collect::<String>())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok((output_text, output_data, warnings))
}

async fn execute_approved_skill_model_inner(
    app: &AppHandle,
    configured: &ConfiguredChatModel,
    input: &ApprovedSkillModelInput,
    request_id: &str,
    cancellation: &AtomicBool,
    started: Instant,
) -> Result<ApprovedSkillModelResult, String> {
    if configured.model.is_empty() {
        return Err("尚未选择真实模型".to_string());
    }
    if configured.provider != "ollama" && configured.api_key.trim().is_empty() {
        return Err("Skill 执行需要本地 API 密钥，请在设置中保存一次".to_string());
    }
    let execution_envelope = serde_json::json!({
        "contract": "yunspire.approved-skill-execution.v1",
        "skill": {
            "id": input.skill_id,
            "name": input.skill_name,
            "version": input.version,
            "payloadHash": input.payload_hash,
            "instructions": input.instructions,
            "inputSchema": input.input_schema,
            "outputSchema": input.output_schema,
            "declaredCapabilities": input.declared_capabilities,
        },
        "userInput": input.user_input,
    });
    let user_content = serde_json::to_string(&execution_envelope)
        .map_err(|error| format!("无法序列化 Skill 模型输入：{error}"))?;
    let skill_system_prompt = runtime_prompt(APPROVED_SKILL_EXECUTION_SYSTEM_PROMPT);
    let prompt_token_estimate = estimate_assistant_tokens(skill_system_prompt) as u64
        + estimate_assistant_tokens(&user_content) as u64;
    let endpoint = analysis_endpoint(&configured.provider, &configured.base_url)?;
    let request_body = match configured.provider.as_str() {
        "anthropic" => serde_json::json!({
            "model": configured.model,
            "max_tokens": 8192,
            "system": skill_system_prompt,
            "messages": [{"role": "user", "content": user_content}],
        }),
        "ollama" => serde_json::json!({
            "model": configured.model,
            "stream": false,
            "format": "json",
            "messages": [
                {"role": "system", "content": skill_system_prompt},
                {"role": "user", "content": user_content}
            ],
        }),
        _ => serde_json::json!({
            "model": configured.model,
            "temperature": 0.2,
            "max_tokens": 8192,
            "stream": true,
            "stream_options": {"include_usage": true},
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": skill_system_prompt},
                {"role": "user", "content": user_content}
            ],
        }),
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(ASSISTANT_REQUEST_TIMEOUT_SECONDS))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化 Skill 模型请求：{error}"))?;
    let build_request = |body: &Value| {
        let request = client
            .post(endpoint.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .json(body);
        match configured.provider.as_str() {
            "anthropic" => request
                .header("x-api-key", configured.api_key.trim())
                .header("anthropic-version", "2023-06-01"),
            "ollama" if configured.api_key.trim().is_empty() => request,
            _ => request.header(
                AUTHORIZATION,
                format!("Bearer {}", configured.api_key.trim()),
            ),
        }
    };
    let mut provider_sequence = 0u64;
    let mut usage_attempts = Vec::new();
    let mut response = send_and_read_cancellable_model_request(
        build_request(&request_body),
        "Skill 模型请求",
        request_id,
        cancellation,
        app,
        started,
        None,
        &mut provider_sequence,
    )
    .await?;
    record_assistant_usage_attempt(&mut usage_attempts, &response);
    if configured.provider != "anthropic"
        && request_body.get("stream_options").is_some()
        && should_retry_without_stream_options(response.status, response.diagnostic_bytes())
    {
        let mut fallback_body = request_body.clone();
        if let Some(object) = fallback_body.as_object_mut() {
            object.remove("stream_options");
        }
        response = send_and_read_cancellable_model_request(
            build_request(&fallback_body),
            "Skill 模型流式 usage 兼容重试",
            request_id,
            cancellation,
            app,
            started,
            None,
            &mut provider_sequence,
        )
        .await?;
        record_assistant_usage_attempt(&mut usage_attempts, &response);
    }
    if configured.provider != "anthropic"
        && should_retry_without_json_constraint(response.status, response.diagnostic_bytes())
        && request_body.get("response_format").is_some()
    {
        let mut fallback_body = request_body.clone();
        if let Some(object) = fallback_body.as_object_mut() {
            object.remove("response_format");
            object.remove("temperature");
            object.remove("stream_options");
        }
        response = send_and_read_cancellable_model_request(
            build_request(&fallback_body),
            "Skill 模型兼容重试",
            request_id,
            cancellation,
            app,
            started,
            None,
            &mut provider_sequence,
        )
        .await?;
        record_assistant_usage_attempt(&mut usage_attempts, &response);
    }
    if !response.status.is_success() {
        return Err(model_request_error(
            "Skill 模型接口",
            response.status,
            response.diagnostic_bytes(),
            configured.api_key.trim(),
        ));
    }
    let response_text = response.take_response_text()?;
    let (output_text, output_data, warnings) = parse_approved_skill_model_output(&response_text)?;
    let usage = assistant_usage_summary_from_attempts(
        request_id,
        &usage_attempts,
        prompt_token_estimate,
        estimate_assistant_tokens(&response_text) as u64,
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    );
    Ok(ApprovedSkillModelResult {
        output_text,
        output_data,
        warnings,
        request_id: request_id.to_string(),
        provider: configured.provider.clone(),
        model: configured.model.clone(),
        usage,
    })
}

pub(crate) async fn execute_approved_skill_model(
    app: &AppHandle,
    request_state: &ModelRequestState,
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: ApprovedSkillModelInput,
) -> Result<ApprovedSkillModelResult, String> {
    let request_id = input
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if !valid_model_request_id(&request_id) {
        return Err("模型请求 ID 无效".to_string());
    }
    crate::trace::validate_trace_id(&input.trace_id)?;
    let configured = configured_chat_model(database, workspace_scope)?;
    let cancellation = request_state.register(&request_id)?;
    let started = Instant::now();
    if let Err(error) = database.record_model_usage(&ModelUsageRecord {
        request_id: &request_id,
        trace_id: &input.trace_id,
        operation: "skill.execute",
        provider: &configured.provider,
        model: &configured.model,
        state: "started",
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost_usd: None,
        cost_source: "pending",
        duration_ms: 0,
        error: None,
    }) {
        request_state.finish(&request_id);
        return Err(error);
    }
    emit_assistant_model_event(
        app,
        &request_id,
        "started",
        0,
        started,
        "已连接受控 Skill 模型运行时",
    );
    let result = execute_approved_skill_model_inner(
        app,
        &configured,
        &input,
        &request_id,
        cancellation.as_ref(),
        started,
    )
    .await;
    let final_result = match result {
        Ok(result) => {
            let usage = &result.usage;
            database.record_model_usage(&ModelUsageRecord {
                request_id: &request_id,
                trace_id: &input.trace_id,
                operation: "skill.execute",
                provider: &configured.provider,
                model: &configured.model,
                state: "succeeded",
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                estimated_cost_usd: usage.estimated_cost_usd,
                cost_source: &usage.source,
                duration_ms: usage.duration_ms,
                error: None,
            })?;
            emit_assistant_model_event(
                app,
                &request_id,
                "completed",
                0,
                started,
                format!("Skill 模型已完成，共 {} token", usage.total_tokens),
            );
            Ok(result)
        }
        Err(error) => {
            let cancelled =
                cancellation.load(Ordering::Acquire) || error.contains("模型请求已取消");
            let record_result = database.record_model_usage(&ModelUsageRecord {
                request_id: &request_id,
                trace_id: &input.trace_id,
                operation: "skill.execute",
                provider: &configured.provider,
                model: &configured.model,
                state: if cancelled { "cancelled" } else { "failed" },
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: None,
                cost_source: "unavailable",
                duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                error: Some(&error),
            });
            emit_assistant_model_event(
                app,
                &request_id,
                if cancelled { "cancelled" } else { "failed" },
                0,
                started,
                if cancelled {
                    "Skill 模型请求已取消"
                } else {
                    "Skill 模型请求失败"
                },
            );
            Err(match record_result {
                Ok(()) => error,
                Err(record_error) => format!("{error}；同时无法记录模型运行结果：{record_error}"),
            })
        }
    };
    request_state.finish(&request_id);
    final_result
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn chat_with_assistant(
    app: AppHandle,
    request_state: State<'_, ModelRequestState>,
    database: State<'_, RuntimeDatabase>,
    intent_state: State<'_, ModelIntentState>,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    messages: Vec<AssistantChatMessage>,
    capabilities: Vec<AssistantCapability>,
    assistant_profile: Option<AssistantProfile>,
    context_window_tokens: Option<u64>,
    reserved_output_tokens: Option<u64>,
    context_messages_omitted: Option<usize>,
    request_id: Option<String>,
    trace_id: Option<String>,
    schedule_dispatch_context: Option<AssistantScheduleDispatchContext>,
) -> Result<AssistantTurn, String> {
    let request_id = request_id
        .unwrap_or_else(|| Uuid::new_v4().to_string())
        .trim()
        .to_string();
    if request_id.is_empty()
        || request_id.chars().count() > 160
        || !request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err("模型请求 ID 无效".to_string());
    }
    let trace_id = trace_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(crate::trace::new_trace_id);
    crate::trace::validate_trace_id(&trace_id)?;
    let schedule_dispatch_binding = schedule_dispatch_context
        .map(|context| {
            let workspace_scope = database.local_workspace_scope()?;
            database.runtime_schedule_dispatch_binding(
                &workspace_scope,
                &context.occurrence_id,
                &context.runtime_task_id,
            )
        })
        .transpose()?;
    let cancellation = request_state.register(&request_id)?;
    let started = Instant::now();
    let provider_for_record = provider.trim().to_lowercase();
    let model_for_record = model.trim().to_string();
    if let Err(error) = database.record_model_usage(&ModelUsageRecord {
        request_id: &request_id,
        trace_id: &trace_id,
        operation: "assistant.chat",
        provider: &provider_for_record,
        model: &model_for_record,
        state: "started",
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost_usd: None,
        cost_source: "pending",
        duration_ms: 0,
        error: None,
    }) {
        request_state.finish(&request_id);
        return Err(error);
    }
    emit_assistant_model_event(&app, &request_id, "started", 0, started, "已连接模型运行时");
    let result = chat_with_assistant_inner(
        intent_state.inner(),
        schedule_dispatch_binding.as_ref(),
        provider,
        base_url,
        api_key,
        model,
        messages,
        capabilities,
        assistant_profile,
        context_window_tokens,
        reserved_output_tokens,
        context_messages_omitted,
        &request_id,
        cancellation.as_ref(),
        &app,
        started,
    )
    .await;
    let final_result = match result {
        Ok(mut turn) => {
            turn.trace_id.clone_from(&trace_id);
            let usage = &turn.usage;
            let record_result = database.record_model_usage(&ModelUsageRecord {
                request_id: &request_id,
                trace_id: &trace_id,
                operation: "assistant.chat",
                provider: &provider_for_record,
                model: &model_for_record,
                state: "succeeded",
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                estimated_cost_usd: usage.estimated_cost_usd,
                cost_source: &usage.source,
                duration_ms: usage.duration_ms,
                error: None,
            });
            match record_result {
                Ok(()) => {
                    emit_assistant_model_event(
                        &app,
                        &request_id,
                        "completed",
                        0,
                        started,
                        format!("已完成，共 {} token", usage.total_tokens),
                    );
                    Ok(turn)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => {
            let cancelled =
                cancellation.load(Ordering::Acquire) || error.contains("模型请求已取消");
            let record_result = database.record_model_usage(&ModelUsageRecord {
                request_id: &request_id,
                trace_id: &trace_id,
                operation: "assistant.chat",
                provider: &provider_for_record,
                model: &model_for_record,
                state: if cancelled { "cancelled" } else { "failed" },
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: None,
                cost_source: "unavailable",
                duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                error: Some(&error),
            });
            emit_assistant_model_event(
                &app,
                &request_id,
                if cancelled { "cancelled" } else { "failed" },
                0,
                started,
                if cancelled {
                    "模型请求已取消"
                } else {
                    "模型请求失败"
                },
            );
            Err(match record_result {
                Ok(()) => error,
                Err(record_error) => format!("{error}；同时无法记录模型运行结果：{record_error}"),
            })
        }
    };
    request_state.finish(&request_id);
    final_result
}

#[allow(clippy::too_many_arguments)]
async fn chat_with_assistant_inner(
    intent_state: &ModelIntentState,
    schedule_dispatch_binding: Option<&RuntimeScheduleDispatchBinding>,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    messages: Vec<AssistantChatMessage>,
    capabilities: Vec<AssistantCapability>,
    assistant_profile: Option<AssistantProfile>,
    context_window_tokens: Option<u64>,
    reserved_output_tokens: Option<u64>,
    context_messages_omitted: Option<usize>,
    request_id: &str,
    cancellation: &AtomicBool,
    app: &AppHandle,
    started: Instant,
) -> Result<AssistantTurn, String> {
    let provider = provider.trim().to_lowercase();
    let model = model.trim();
    let key = api_key.trim();
    if model.is_empty() {
        return Err("尚未选择真实模型".to_string());
    }
    if provider != "ollama" && key.is_empty() {
        return Err("AI助手对话需要本地 API 密钥，请在设置中保存一次".to_string());
    }
    if messages.is_empty() {
        return Err("对话消息不能为空".to_string());
    }
    let normalized_messages = normalize_assistant_messages(messages)?;
    if normalized_messages.is_empty() {
        return Err("对话消息没有有效内容".to_string());
    }

    let enabled_capabilities = capabilities
        .into_iter()
        .filter(|capability| capability.enabled)
        .take(128)
        .map(|capability| {
            serde_json::json!({
                "id": capability.id.chars().take(96).collect::<String>(),
                "name": capability.name.chars().take(96).collect::<String>(),
                "kind": capability.kind.chars().take(32).collect::<String>(),
                "description": capability.description.chars().take(320).collect::<String>(),
                "userSelected": capability.user_selected,
                "version": capability.version,
                "payloadHash": capability.payload_hash.map(|value| value.chars().take(96).collect::<String>()),
                "instructions": capability.instructions.map(|value| value.chars().take(32_000).collect::<String>()),
                "inputSchema": capability.input_schema.map(|value| value.chars().take(64_000).collect::<String>()),
                "outputSchema": capability.output_schema.map(|value| value.chars().take(64_000).collect::<String>()),
                "declaredCapabilities": capability.declared_capabilities.into_iter().take(16).map(|value| value.chars().take(64).collect::<String>()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let capability_ids = enabled_capabilities
        .iter()
        .filter_map(|capability| capability.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let selected_user_skills = enabled_capabilities
        .iter()
        .filter(|capability| {
            capability.get("kind").and_then(Value::as_str) == Some("skill")
                && capability
                    .get("userSelected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    let profile = assistant_profile.unwrap_or_default();
    let assistant_name = if profile.name.trim().is_empty() {
        runtime_prompt(DEFAULT_ASSISTANT_NAME_PROMPT)
    } else {
        profile.name.trim()
    };
    let assistant_language = if profile.language.trim().is_empty() {
        runtime_prompt(DEFAULT_ASSISTANT_LANGUAGE_PROMPT)
    } else {
        profile.language.trim()
    };
    let assistant_style = if profile.style.trim().is_empty() {
        runtime_prompt(DEFAULT_ASSISTANT_STYLE_PROMPT)
    } else {
        profile.style.trim()
    };
    let profile_context = render_runtime_prompt_template(
        ASSISTANT_PROFILE_CONTEXT_PROMPT_TEMPLATE,
        &[
            ("assistant_name", assistant_name),
            ("assistant_language", assistant_language),
            ("assistant_style", assistant_style),
        ],
    )?;
    let slash_command = is_assistant_slash_command(&normalized_messages);
    let assistant_prompt = runtime_prompt(if slash_command {
        ASSISTANT_SLASH_COMMAND_PROMPT
    } else {
        ASSISTANT_SYSTEM_PROMPT
    });
    let research_prompt = if slash_command {
        ""
    } else {
        runtime_prompt(RESEARCH_INTENT_PROMPT)
    };
    let permanent_delete_prompt = if slash_command {
        ""
    } else {
        runtime_prompt(PERMANENT_DELETE_ROUTING_PROMPT)
    };
    let capability_catalog_json = Value::Array(enabled_capabilities).to_string();
    let capability_catalog = render_runtime_prompt_template(
        ASSISTANT_CAPABILITY_CATALOG_PROMPT_TEMPLATE,
        &[("capabilities", &capability_catalog_json)],
    );
    let capability_catalog = capability_catalog?;
    let mut system_prompt = String::with_capacity(
        assistant_prompt
            .len()
            .saturating_add(research_prompt.len())
            .saturating_add(permanent_delete_prompt.len())
            .saturating_add(profile_context.len())
            .saturating_add(runtime_prompt(USER_SKILL_ROUTING_PROMPT).len())
            .saturating_add(capability_catalog.len())
            .saturating_add(2),
    );
    system_prompt.push_str(assistant_prompt);
    system_prompt.push_str(research_prompt);
    system_prompt.push_str(permanent_delete_prompt);
    system_prompt.push_str(&profile_context);
    system_prompt.push('\n');
    system_prompt.push_str(runtime_prompt(USER_SKILL_ROUTING_PROMPT));
    system_prompt.push('\n');
    system_prompt.push_str(&capability_catalog);
    let context_budget = assistant_context_budget(context_window_tokens, reserved_output_tokens)?;
    let system_tokens = estimate_assistant_tokens(&system_prompt);
    let message_token_budget = context_budget
        .map(|budget| {
            budget
                .input_tokens
                .checked_sub(system_tokens.saturating_add(ASSISTANT_CONTEXT_PAGE_MARKER_TOKENS))
                .ok_or_else(|| "AI助手系统与能力目录已经占满当前模型上下文窗口".to_string())
        })
        .transpose()?;
    let message_byte_budget = DEFAULT_ASSISTANT_CONTEXT_PAGE_BYTES
        .checked_sub(system_prompt.len())
        .ok_or_else(|| "AI助手系统与能力目录超过单请求字节边界".to_string())?;
    let (normalized_messages, omitted_messages) = page_assistant_messages(
        normalized_messages,
        message_token_budget,
        message_byte_budget,
    )?;
    let omitted_messages =
        omitted_messages.saturating_add(context_messages_omitted.unwrap_or_default());
    if omitted_messages > 0 {
        let omitted_messages = omitted_messages.to_string();
        system_prompt.push_str(&render_runtime_prompt_template(
            ASSISTANT_CONTEXT_OMISSION_PROMPT_TEMPLATE,
            &[("omitted_messages", &omitted_messages)],
        )?);
    }
    let prompt_token_estimate = estimate_assistant_tokens(&system_prompt) as u64
        + normalized_messages
            .iter()
            .map(|(_, content, attachments)| {
                estimate_assistant_tokens(content) as u64
                    + attachments
                        .iter()
                        .map(|attachment| {
                            estimate_assistant_tokens(&attachment.name) as u64
                                + attachment
                                    .text_content
                                    .as_deref()
                                    .map(estimate_assistant_tokens)
                                    .unwrap_or_default() as u64
                                + if attachment.data_url.is_some() {
                                    1_024
                                } else {
                                    0
                                }
                        })
                        .sum::<u64>()
            })
            .sum::<u64>();
    let endpoint = analysis_endpoint(&provider, &base_url)?;
    let anthropic_output_tokens = reserved_output_tokens.unwrap_or(3_000);
    let default_output_tokens = reserved_output_tokens.unwrap_or(8_192);
    let request_body = match provider.as_str() {
        "anthropic" => serde_json::json!({
            "model": model,
            "max_tokens": anthropic_output_tokens,
            "stream": true,
            "system": system_prompt,
            "messages": normalized_messages.iter().map(|(role, content, attachments)| anthropic_assistant_message(role, content, attachments)).collect::<Vec<_>>(),
        }),
        "ollama" => {
            let mut request_messages =
                vec![serde_json::json!({"role": "system", "content": system_prompt})];
            request_messages.extend(normalized_messages.iter().map(
                |(role, content, attachments)| ollama_assistant_message(role, content, attachments),
            ));
            let mut body = serde_json::json!({
                "model": model,
                "stream": true,
                "format": "json",
                "messages": request_messages,
                "options": { "num_predict": default_output_tokens }
            });
            if let Some(context_window_tokens) = context_window_tokens {
                body["options"]["num_ctx"] = serde_json::json!(context_window_tokens);
            }
            body
        }
        _ => {
            let mut request_messages =
                vec![serde_json::json!({"role": "system", "content": system_prompt})];
            request_messages.extend(normalized_messages.iter().map(
                |(role, content, attachments)| openai_assistant_message(role, content, attachments),
            ));
            serde_json::json!({
                "model": model,
                "temperature": 0.3,
                "max_tokens": default_output_tokens,
                "stream": true,
                "stream_options": {"include_usage": true},
                "response_format": {"type": "json_object"},
                "messages": request_messages,
            })
        }
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(ASSISTANT_REQUEST_TIMEOUT_SECONDS))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化 AI助手请求：{error}"))?;
    let mut request = client
        .post(endpoint.clone())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(&request_body);
    request = match provider.as_str() {
        "anthropic" => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "ollama" if key.is_empty() => request,
        _ => request.header(AUTHORIZATION, format!("Bearer {key}")),
    };
    let mut provider_sequence = 0u64;
    let mut usage_attempts = Vec::new();
    let mut response = send_and_read_cancellable_model_request(
        request,
        "AI助手模型请求",
        request_id,
        cancellation,
        app,
        started,
        Some("reply"),
        &mut provider_sequence,
    )
    .await?;
    record_assistant_usage_attempt(&mut usage_attempts, &response);
    if provider != "anthropic"
        && request_body.get("stream_options").is_some()
        && should_retry_without_stream_options(response.status, response.diagnostic_bytes())
    {
        let mut fallback_body = request_body.clone();
        if let Some(object) = fallback_body.as_object_mut() {
            object.remove("stream_options");
        }
        let fallback_request = client
            .post(endpoint.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {key}"))
            .json(&fallback_body);
        response = send_and_read_cancellable_model_request(
            fallback_request,
            "AI助手流式 usage 兼容重试",
            request_id,
            cancellation,
            app,
            started,
            Some("reply"),
            &mut provider_sequence,
        )
        .await?;
        record_assistant_usage_attempt(&mut usage_attempts, &response);
    }
    if provider != "anthropic"
        && should_retry_without_json_constraint(response.status, response.diagnostic_bytes())
        && request_body.get("response_format").is_some()
    {
        let mut fallback_body = request_body.clone();
        if let Some(object) = fallback_body.as_object_mut() {
            object.remove("response_format");
            object.remove("temperature");
            object.remove("stream_options");
        }
        let mut fallback_request = client
            .post(endpoint.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .json(&fallback_body);
        fallback_request = fallback_request.header(AUTHORIZATION, format!("Bearer {key}"));
        response = send_and_read_cancellable_model_request(
            fallback_request,
            "AI助手模型兼容重试",
            request_id,
            cancellation,
            app,
            started,
            Some("reply"),
            &mut provider_sequence,
        )
        .await?;
        record_assistant_usage_attempt(&mut usage_attempts, &response);
    }
    if !response.status.is_success() {
        return Err(model_request_error(
            "AI助手模型接口",
            response.status,
            response.diagnostic_bytes(),
            key,
        ));
    }
    let mut response_text = match response.take_response_text() {
        Ok(text) => text,
        Err(error)
            if provider != "anthropic"
                && provider != "ollama"
                && provider_sequence == 0
                && error == "AI助手流式响应缺少文本内容" =>
        {
            let mut recovery_body = request_body.clone();
            if let Some(object) = recovery_body.as_object_mut() {
                object.remove("response_format");
                object.remove("temperature");
                object.remove("stream_options");
            }
            let recovery_request = client
                .post(endpoint.clone())
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {key}"))
                .json(&recovery_body);
            let mut recovery_response = send_and_read_cancellable_model_request(
                recovery_request,
                "AI助手空响应兼容重试",
                request_id,
                cancellation,
                app,
                started,
                Some("reply"),
                &mut provider_sequence,
            )
            .await?;
            record_assistant_usage_attempt(&mut usage_attempts, &recovery_response);
            if !recovery_response.status.is_success() {
                return Err(model_request_error(
                    "AI助手空响应兼容重试接口",
                    recovery_response.status,
                    recovery_response.diagnostic_bytes(),
                    key,
                ));
            }
            recovery_response.take_response_text()?
        }
        Err(error) => return Err(error),
    };
    let mut turn = match parse_assistant_turn(&response_text) {
        Ok(turn) => turn,
        Err(parse_error)
            if provider != "anthropic" && provider != "ollama" && provider_sequence == 0 =>
        {
            let mut recovery_body = request_body.clone();
            if let Some(object) = recovery_body.as_object_mut() {
                object.remove("response_format");
                object.remove("temperature");
                object.remove("stream_options");
            }
            let recovery_request = client
                .post(endpoint)
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {key}"))
                .json(&recovery_body);
            let mut recovery_response = send_and_read_cancellable_model_request(
                recovery_request,
                "AI助手意图格式兼容重试",
                request_id,
                cancellation,
                app,
                started,
                Some("reply"),
                &mut provider_sequence,
            )
            .await?;
            record_assistant_usage_attempt(&mut usage_attempts, &recovery_response);
            if !recovery_response.status.is_success() {
                return Err(model_request_error(
                    "AI助手意图格式兼容重试接口",
                    recovery_response.status,
                    recovery_response.diagnostic_bytes(),
                    key,
                ));
            }
            response_text = recovery_response.take_response_text()?;
            parse_assistant_turn(&response_text).map_err(|_| parse_error)?
        }
        Err(error) => return Err(error),
    };
    force_selected_skill_run(&mut turn, &selected_user_skills, &normalized_messages);
    if turn.action == "execute" {
        if external_delivery_requested(&normalized_messages) {
            turn.intent = "external".to_string();
            turn.capability_ids = vec!["system:external".to_string()];
            turn.operation = "send".to_string();
            turn.reason = if turn.reason.is_empty() {
                "外部发送请求由本地能力契约限定为 system:external".to_string()
            } else {
                format!(
                    "{}；外部发送请求由本地能力契约限定为 system:external",
                    turn.reason
                )
            };
            if !external_delivery_content_present(&turn.parameters) {
                turn.action = "clarify".to_string();
                turn.capability_ids.clear();
                turn.operation = "none".to_string();
                turn.reply =
                    "请明确提供要发送的正文；在正文可验证前，Yunspire 不会创建外部投递任务。"
                        .to_string();
                turn.choices = vec![AssistantChoice {
                    id: "provide-external-content".to_string(),
                    label: "补充发送正文".to_string(),
                    description: "在下一条消息中写明目标平台和完整正文".to_string(),
                }];
            }
        } else if let Some(operation) = report_subscription_operation(&normalized_messages) {
            turn.intent = "reports".to_string();
            turn.capability_ids = vec!["system:reports".to_string()];
            turn.operation = operation.to_string();
            turn.reason = format!(
                "{}{}",
                turn.reason,
                if turn.reason.is_empty() {
                    "报告订阅请求由本地能力契约限定为 system:reports"
                } else {
                    "；报告订阅请求由本地能力契约限定为 system:reports"
                }
            );
        }
    }
    turn.capability_ids
        .retain(|capability_id| capability_ids.contains(capability_id));
    if turn.action != "execute" {
        turn.capability_ids.clear();
        turn.operation = "none".to_string();
        turn.parameters = serde_json::json!({});
        turn.decision_receipt.clear();
    } else {
        let required_capability = format!("system:{}", turn.intent);
        if turn.confidence < 0.55
            || turn.operation == "none"
            || !turn.capability_ids.contains(&required_capability)
        {
            turn.action = "clarify".to_string();
            turn.capability_ids.clear();
            turn.operation = "none".to_string();
            turn.parameters = serde_json::json!({});
            turn.decision_receipt.clear();
            turn.reply =
                "我还不能安全确定需要执行的系统能力，请补充目标、来源或需要修改的具体任务。"
                    .to_string();
        } else {
            if let Some(binding) = schedule_dispatch_binding {
                bind_schedule_dispatch_parameters(&mut turn, binding)?;
            }
            turn.decision_receipt = intent_state.issue(
                LOCAL_MODEL_SCOPE,
                &turn.intent,
                &turn.capability_ids,
                &turn.operation,
                &turn.parameters,
            )?;
        }
    }
    if turn.action != "clarify" {
        turn.choices.clear();
    }
    turn.usage = assistant_usage_summary_from_attempts(
        request_id,
        &usage_attempts,
        prompt_token_estimate,
        estimate_assistant_tokens(&response_text) as u64,
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    );
    Ok(turn)
}

fn bind_schedule_dispatch_parameters(
    turn: &mut AssistantTurn,
    binding: &RuntimeScheduleDispatchBinding,
) -> Result<(), String> {
    let expected_intent = match binding.schedule_kind.as_str() {
        "collection" => "capture",
        "report" => "reports",
        _ => return Err("日程 occurrence 类型不受支持".to_string()),
    };
    if turn.intent != expected_intent {
        return Err("模型意图与日程 occurrence 类型不匹配".to_string());
    }
    let parameters = turn
        .parameters
        .as_object_mut()
        .ok_or_else(|| "模型执行参数必须是 JSON 对象".to_string())?;
    for (key, value) in [
        ("schedule_id", binding.schedule_id.clone()),
        ("schedule_kind", binding.schedule_kind.clone()),
        ("schedule_occurrence_id", binding.occurrence_id.clone()),
        ("schedule_wrapper_task_id", binding.runtime_task_id.clone()),
        ("schedule_scheduled_for", binding.scheduled_for.clone()),
        ("schedule_revision", binding.schedule_revision.to_string()),
        (
            "schedule_payload_hash",
            binding.schedule_payload_hash.clone(),
        ),
    ] {
        parameters.insert(key.to_string(), Value::String(value));
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_assistant_request(
    request_state: State<'_, ModelRequestState>,
    request_id: String,
) -> Result<bool, String> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Err("缺少模型请求 ID".to_string());
    }
    request_state.cancel(request_id)
}

#[tauri::command]
pub fn consume_assistant_decision(
    intent_state: State<'_, ModelIntentState>,
    receipt: String,
    intent: String,
    capability_id: String,
    operation: String,
    parameters: Value,
) -> Result<(), String> {
    let receipt = receipt.trim();
    let intent = intent.trim();
    let capability_id = capability_id.trim();
    let operation = operation.trim();
    if receipt.is_empty() || intent.is_empty() || capability_id.is_empty() || operation.is_empty() {
        return Err("执行前缺少模型意图凭证".to_string());
    }
    intent_state.consume(
        LOCAL_MODEL_SCOPE,
        receipt,
        intent,
        capability_id,
        operation,
        &parameters,
    )
}

fn image_endpoint(provider: &str, base_url: &str, operation: &str) -> Result<Url, String> {
    let mut url = provider_base_url(provider, base_url)?;
    let current = url.path().trim_end_matches('/');
    let requested_suffix = format!("/images/{operation}");
    if current.ends_with(&requested_suffix) {
        return Ok(url);
    }
    let normalized = current
        .strip_suffix("/images/generations")
        .or_else(|| current.strip_suffix("/images/edits"))
        .unwrap_or(current);
    let (root, explicit_endpoint) = api_operation_base(normalized);
    let path = if root.ends_with("/v1") || explicit_endpoint {
        append_path(root, &requested_suffix)
    } else {
        append_path(root, &format!("/v1{requested_suffix}"))
    };
    url.set_path(&path);
    Ok(url)
}

fn assistant_image_operations(has_source_image: bool) -> (&'static str, &'static str) {
    if has_source_image {
        ("edit", "edits")
    } else {
        ("generate", "generations")
    }
}

fn capture_analysis_runtime_identity(
    runtime_capability: Option<&str>,
) -> Result<(&'static str, &'static str), String> {
    match runtime_capability
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("system:research") => Ok(("system:research", "run")),
        Some("system:optimization") => Ok(("system:optimization", "run")),
        Some(_) => Err("模型分析 Runtime 身份只允许 research/run 或 optimization/run".to_string()),
    }
}

fn assistant_image_usage_summary(
    request_id: &str,
    payload: &Value,
    prompt: &str,
    duration: Duration,
) -> ModelUsageSummary {
    let mut usage_payload = serde_json::Map::new();
    merge_assistant_usage_payload(&mut usage_payload, payload);
    let usage_payload = (!usage_payload.is_empty()).then_some(Value::Object(usage_payload));
    assistant_usage_summary_from_payload(
        request_id,
        usage_payload.as_ref(),
        estimate_assistant_tokens(prompt) as u64,
        0,
        duration.as_millis().min(u128::from(u64::MAX)) as u64,
    )
}

fn generated_image_base64_payloads(payload: &Value) -> Result<Vec<&str>, String> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            let mut images = Vec::new();
            for item in items.iter().take(4) {
                if let Some(encoded) = item.get("b64_json").and_then(Value::as_str) {
                    if !encoded.is_empty() {
                        images.push(encoded);
                    }
                    continue;
                }
                if item.get("url").and_then(Value::as_str).is_some() {
                    return Err(
                        "图像供应商没有按请求返回 Base64 图片；为避免留下短期远程链接，本次结果未保存"
                            .to_string(),
                    );
                }
            }
            Ok(images)
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

async fn read_image_model_response(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_MODEL_RESPONSE_BYTES)
    {
        return Err(format!(
            "图像模型响应超过单次请求 {} MB 安全上限",
            MAX_IMAGE_MODEL_RESPONSE_BYTES / (1024 * 1024)
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("无法读取图像响应：{error}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_IMAGE_MODEL_RESPONSE_BYTES as usize {
            return Err(format!(
                "图像模型响应超过单次请求 {} MB 安全上限",
                MAX_IMAGE_MODEL_RESPONSE_BYTES / (1024 * 1024)
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[allow(clippy::too_many_arguments)]
async fn send_image_edit_request_with_retry(
    client: &Client,
    endpoint: &Url,
    key: &str,
    model: &str,
    prompt: &str,
    mime_type: &str,
    bytes: &[u8],
) -> Result<reqwest::Response, String> {
    let mut last_error = None;
    for attempt in 1..=3 {
        let part = Part::bytes(bytes.to_vec())
            .file_name("assistant-input.png")
            .mime_str(mime_type)
            .map_err(|_| "图像编辑 MIME 类型无效".to_string())?;
        let request = client
            .post(endpoint.clone())
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, format!("Bearer {key}"))
            .multipart(
                Form::new()
                    .text("model", model.to_string())
                    .text("prompt", prompt.to_string())
                    .text("n", "1")
                    .text("response_format", "b64_json")
                    .part("image", part),
            );
        match request.send().await {
            Ok(response) if attempt < 3 && should_retry_model_status(response.status()) => {
                wait_for_model_retry(attempt).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < 3 && (error.is_connect() || error.is_timeout()) => {
                last_error = Some(error.to_string());
                wait_for_model_retry(attempt).await;
            }
            Err(error) => return Err(format!("图像编辑请求失败：{error}")),
        }
    }
    Err(format!(
        "图像编辑连续 3 次网络重试失败：{}",
        last_error.unwrap_or_else(|| "未知网络错误".to_string())
    ))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn generate_assistant_image(
    app: AppHandle,
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    durable_asset_state: State<'_, DurableAssetState>,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    prompt: String,
    image_data_url: Option<String>,
    owner_id: Option<String>,
    operation_context: OperationContext,
) -> Result<GeneratedImageResult, String> {
    let handler_started = Instant::now();
    let provider = provider.trim().to_lowercase();
    let key = api_key.trim();
    let model = model.trim();
    let prompt = prompt.trim();
    if model.is_empty() || prompt.is_empty() {
        return Err("图像模型和描述不能为空".to_string());
    }
    if provider == "ollama" || provider == "anthropic" {
        return Err("当前供应商未提供 OpenAI Images 兼容接口".to_string());
    }
    if key.is_empty() {
        return Err("图像生成需要 API 密钥".to_string());
    }
    let (capability_operation, endpoint_operation) =
        assistant_image_operations(image_data_url.is_some());
    let workspace_scope = database.local_workspace_scope()?;
    database.validate_runtime_effectful_handler(
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        "system:image",
        capability_operation,
    )?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化图像请求：{error}"))?;
    let endpoint = image_endpoint(&provider, &base_url, endpoint_operation)?;
    let response = if let Some(image_data_url) = image_data_url {
        let (mime_type, encoded) = image_data_url
            .strip_prefix("data:")
            .and_then(|value| value.split_once(","))
            .and_then(|(header, encoded)| {
                Some((header.strip_suffix(";base64")?.to_string(), encoded))
            })
            .ok_or_else(|| "图像编辑输入格式无效".to_string())?;
        if !mime_type.starts_with("image/") {
            return Err("图像编辑只支持图片输入".to_string());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| "图像编辑输入不是有效的 base64 图片".to_string())?;
        if bytes.len() > MAX_ANALYSIS_IMAGE_BYTES_PER_REQUEST {
            return Err("图像编辑输入超过 12 MB 安全上限".to_string());
        }
        send_image_edit_request_with_retry(
            &client, &endpoint, key, model, prompt, &mime_type, &bytes,
        )
        .await?
    } else {
        send_model_request_with_retry(
            client
                .post(endpoint)
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {key}"))
                .json(&serde_json::json!({
                    "model": model,
                    "prompt": prompt,
                    "n": 1,
                    "size": "1024x1024",
                    "response_format": "b64_json",
                })),
            "图像生成请求",
        )
        .await?
    };
    let status = response.status();
    let bytes = read_image_model_response(response).await?;
    if !status.is_success() {
        return Err(model_request_error("图像模型接口", status, &bytes, key));
    }
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| "图像模型响应不是有效 JSON".to_string())?;
    drop(bytes);
    let usage = assistant_image_usage_summary(
        &format!("assistant-image-{}", Uuid::new_v4()),
        &payload,
        prompt,
        handler_started.elapsed(),
    );
    let generated_images = generated_image_base64_payloads(&payload)?;
    if generated_images.is_empty() {
        return Err("图像模型响应没有返回图片".to_string());
    }
    let owner_id = owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("image-request-{}", Uuid::new_v4()));
    let stored_prompt = prompt.chars().take(8_000).collect::<String>();
    let mut assets: Vec<DurableAssetDescriptor> = Vec::with_capacity(generated_images.len());
    for (index, encoded) in generated_images.iter().enumerate() {
        let mime_type = "image/png";
        let extension = match mime_type {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => "png",
        };
        let asset = match store_generated_image_base64(
            &app,
            database.inner(),
            durable_asset_state.inner(),
            &owner_id,
            &format!(
                "generated-{}-{}.{}",
                Utc::now().timestamp_millis(),
                index + 1,
                extension
            ),
            mime_type,
            encoded,
            serde_json::json!({
                "prompt": stored_prompt.as_str(),
                "provider": provider.as_str(),
                "model": model,
                "ordinal": index + 1,
                "contentRole": "assistant_generated_image",
            }),
        ) {
            Ok(asset) => asset,
            Err(error) => {
                for stored in &assets {
                    let _ = delete_durable_asset_for_runtime(
                        &app,
                        database.inner(),
                        durable_asset_state.inner(),
                        &stored.asset_id,
                    );
                }
                return Err(error);
            }
        };
        assets.push(asset);
    }
    if let Err(error) = database.record_runtime_effectful_handler_completion(
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        "system:image",
        capability_operation,
        usage.trusted_handler_usage(handler_started.elapsed()),
    ) {
        for stored in &assets {
            let _ = delete_durable_asset_for_runtime(
                &app,
                database.inner(),
                durable_asset_state.inner(),
                &stored.asset_id,
            );
        }
        return Err(error);
    }
    Ok(GeneratedImageResult {
        // Kept as an empty compatibility field so older callers fail closed instead
        // of receiving and persisting multi-megabyte Base64 data URLs.
        images: Vec::new(),
        assets,
        prompt: prompt.to_string(),
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn analyze_capture_content(
    analysis_state: State<'_, ModelAnalysisState>,
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    content: String,
    image_urls: Vec<String>,
    image_data_urls: Vec<String>,
    image_bindings: Option<Vec<CaptureImageBinding>>,
    issue_receipt: Option<bool>,
    operation_context: Option<OperationContext>,
    runtime_capability: Option<String>,
) -> Result<Value, String> {
    let handler_started = Instant::now();
    let provider = provider.trim().to_lowercase();
    let model = model.trim();
    let key = api_key.trim();
    if model.is_empty() {
        return Err("尚未选择真实模型".to_string());
    }
    if content.trim().is_empty() && image_urls.is_empty() && image_data_urls.is_empty() {
        return Err("没有可供模型分析的正文或图片".to_string());
    }
    if content.len() > MAX_ANALYSIS_CONTENT_BYTES {
        return Err(
            "单次模型分析请求的正文字节数超过 4 MB；文件整体不受此限制，请由云枢分批处理"
                .to_string(),
        );
    }
    if provider != "ollama" && key.is_empty() {
        return Err("该接口需要 API 密钥".to_string());
    }
    if image_urls.len().saturating_add(image_data_urls.len()) > MAX_ANALYSIS_IMAGES_PER_REQUEST {
        return Err("单次模型分析最多接收 8 张图片，请由云枢分批处理".to_string());
    }
    if runtime_capability
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
        && operation_context.is_none()
    {
        return Err("声明 Runtime 模型分析身份时必须提供 claimed 子任务上下文".to_string());
    }
    let runtime_identity = operation_context
        .as_ref()
        .map(|_| capture_analysis_runtime_identity(runtime_capability.as_deref()))
        .transpose()?;
    let runtime_workspace_scope = if let (Some(context), Some((capability_id, operation))) =
        (operation_context.as_ref(), runtime_identity)
    {
        let workspace_scope = database.local_workspace_scope()?;
        database.validate_runtime_effectful_handler(
            ticket_state.inner(),
            &workspace_scope,
            context,
            capability_id,
            operation,
        )?;
        Some(workspace_scope)
    } else {
        None
    };
    let PreparedCaptureAnalysisImages {
        images: accepted_images,
        bindings: image_bindings,
    } = prepare_capture_analysis_images(&image_data_urls, image_bindings)?;
    let endpoint = analysis_endpoint(&provider, &base_url)?;
    let image_context = if image_urls.is_empty() {
        String::new()
    } else {
        let image_urls = image_urls.join("\n");
        render_runtime_prompt_template(
            CAPTURE_ANALYSIS_IMAGE_SOURCES_PROMPT_TEMPLATE,
            &[("image_urls", &image_urls)],
        )?
    };
    let binding_context = if image_bindings.is_empty() {
        String::new()
    } else {
        let image_bindings = serde_json::to_string(&image_bindings)
            .map_err(|error| format!("无法序列化视觉输入绑定：{error}"))?;
        render_runtime_prompt_template(
            CAPTURE_ANALYSIS_IMAGE_BINDINGS_PROMPT_TEMPLATE,
            &[("image_bindings", &image_bindings)],
        )?
    };
    let user_content = render_runtime_prompt_template(
        CAPTURE_ANALYSIS_USER_PROMPT_TEMPLATE,
        &[
            ("content", &content),
            ("image_context", &image_context),
            ("binding_context", &binding_context),
        ],
    )?;
    let openai_text_content = {
        let mut parts = vec![serde_json::json!({"type": "text", "text": user_content})];
        for url in &image_urls {
            if url.starts_with("http://") || url.starts_with("https://") {
                parts.push(serde_json::json!({"type": "image_url", "image_url": {"url": url}}));
            }
        }
        for (mime_type, encoded) in &accepted_images {
            parts.push(serde_json::json!({"type": "image_url", "image_url": {"url": format!("data:{mime_type};base64,{encoded}")}}));
        }
        Value::Array(parts)
    };
    let anthropic_content = {
        let mut parts = vec![serde_json::json!({"type": "text", "text": user_content})];
        for (mime_type, encoded) in &accepted_images {
            parts.push(serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": mime_type, "data": encoded}}));
        }
        Value::Array(parts)
    };
    let ollama_images = accepted_images
        .iter()
        .map(|(_, encoded)| Value::String(encoded.clone()))
        .collect::<Vec<_>>();
    let analysis_system_prompt = runtime_prompt(ANALYSIS_SYSTEM_PROMPT);
    let request_body = match provider.as_str() {
        "anthropic" => serde_json::json!({
            "model": model,
            "max_tokens": 4000,
            "system": analysis_system_prompt,
            "messages": [{"role": "user", "content": anthropic_content}],
        }),
        "ollama" => serde_json::json!({
            "model": model,
            "stream": false,
            "format": "json",
            "messages": [
                {"role": "system", "content": analysis_system_prompt},
                {"role": "user", "content": user_content, "images": ollama_images},
            ],
        }),
        _ => serde_json::json!({
            "model": model,
            "temperature": 0.2,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": analysis_system_prompt},
                {"role": "user", "content": openai_text_content},
            ],
        }),
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(ANALYSIS_REQUEST_TIMEOUT_SECONDS))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化模型请求：{error}"))?;
    let mut request = client
        .post(endpoint.clone())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(&request_body);
    request = match provider.as_str() {
        "anthropic" => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "ollama" if key.is_empty() => request,
        _ => request.header(AUTHORIZATION, format!("Bearer {key}")),
    };
    let mut response = send_model_request_with_retry(request, "模型分析请求").await?;
    let mut status = response.status();
    let mut bytes = response
        .bytes()
        .await
        .map_err(|error| format!("无法读取模型分析响应：{error}"))?;
    if provider != "anthropic"
        && should_retry_without_json_constraint(status, &bytes)
        && request_body.get("response_format").is_some()
    {
        let mut fallback_body = request_body.clone();
        if let Some(object) = fallback_body.as_object_mut() {
            object.remove("response_format");
            object.remove("temperature");
        }
        let mut fallback_request = client
            .post(endpoint)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .json(&fallback_body);
        fallback_request = fallback_request.header(AUTHORIZATION, format!("Bearer {key}"));
        response = send_model_request_with_retry(fallback_request, "模型分析兼容重试").await?;
        status = response.status();
        bytes = response
            .bytes()
            .await
            .map_err(|error| format!("无法读取模型分析兼容重试响应：{error}"))?;
    }
    if !status.is_success() {
        return Err(model_request_error("模型分析接口", status, &bytes, key));
    }
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("模型分析响应不是有效 JSON：{error}"))?;
    let text = model_text(&payload)?;
    let mut usage_payload = serde_json::Map::new();
    merge_assistant_usage_payload(&mut usage_payload, &payload);
    let usage_payload = (!usage_payload.is_empty()).then_some(Value::Object(usage_payload));
    let usage = assistant_usage_summary_from_payload(
        &format!("capture-analysis-{}", Uuid::new_v4()),
        usage_payload.as_ref(),
        estimate_assistant_tokens(analysis_system_prompt) as u64
            + estimate_assistant_tokens(&user_content) as u64,
        estimate_assistant_tokens(&text) as u64,
        handler_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64,
    );
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        serde_json::json!({"summary": text, "tags": [], "entities": [], "key_points": [], "analysis_markdown": text, "image_observations": [], "relations": [], "warnings": ["模型没有返回严格 JSON"]})
    });
    let expected_asset_ids = visual_manifest_asset_ids(&content);
    let mut parsed = normalize_capture_analysis(parsed, &expected_asset_ids, &image_bindings)?;
    let receipt = if issue_receipt.unwrap_or(true) {
        Some(analysis_state.issue_with_analysis(LOCAL_MODEL_SCOPE, &parsed)?)
    } else {
        None
    };
    let parsed_object = parsed
        .as_object_mut()
        .ok_or_else(|| "模型分析结果必须是 JSON 对象".to_string())?;
    if let Some(receipt) = receipt {
        parsed_object.insert("analysisReceipt".to_string(), Value::String(receipt));
    }
    if let (Some(context), Some(workspace_scope), Some((capability_id, operation))) = (
        operation_context.as_ref(),
        runtime_workspace_scope.as_deref(),
        runtime_identity,
    ) {
        database.record_runtime_effectful_handler_completion(
            ticket_state.inner(),
            workspace_scope,
            context,
            capability_id,
            operation,
            usage.trusted_handler_usage(handler_started.elapsed()),
        )?;
    }
    Ok(parsed)
}

#[tauri::command]
pub fn bind_capture_analysis_write_manifest(
    analysis_state: State<'_, ModelAnalysisState>,
    analysis_receipt: String,
    write_manifest_digest: String,
) -> Result<String, String> {
    let receipt = analysis_receipt.trim();
    if receipt.is_empty() {
        return Err("模型分析凭证不能为空".to_string());
    }
    analysis_state.bind_write_manifest(LOCAL_MODEL_SCOPE, receipt, &write_manifest_digest)
}

#[tauri::command]
pub fn discard_capture_analysis_receipt(
    analysis_state: State<'_, ModelAnalysisState>,
    analysis_receipt: String,
) -> Result<(), String> {
    let receipt = analysis_receipt.trim();
    if receipt.is_empty() {
        return Err("模型分析凭证不能为空".to_string());
    }
    analysis_state
        .consume(LOCAL_MODEL_SCOPE, receipt)
        .map(|_| ())
}

#[tauri::command]
pub async fn fetch_provider_models(
    provider: String,
    base_url: String,
    api_key: String,
) -> Result<Vec<ModelDescriptor>, String> {
    let provider = provider.trim().to_lowercase();
    let endpoints = model_endpoints(&provider, &base_url)?;
    let key = api_key.trim();
    if provider != "ollama" && key.is_empty() {
        return Err("该接口需要 API 密钥".to_string());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(MODEL_REQUEST_TIMEOUT_SECONDS))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化模型请求：{error}"))?;
    let mut failures = Vec::new();
    for endpoint in endpoints {
        let endpoint_label = endpoint.path().to_string();
        let mut request = client
            .get(endpoint)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json");
        request = match provider.as_str() {
            "anthropic" => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            "ollama" if key.is_empty() => request,
            _ => request.header(AUTHORIZATION, format!("Bearer {key}")),
        };

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                failures.push(format!("{endpoint_label} 请求失败：{error}"));
                continue;
            }
        };
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MODEL_CONTROL_RESPONSE_BYTES)
        {
            failures.push(format!("{endpoint_label} 响应超过 2 MB 安全上限"));
            continue;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                failures.push(format!("{endpoint_label} 响应读取失败：{error}"));
                continue;
            }
        };
        if bytes.len() as u64 > MAX_MODEL_CONTROL_RESPONSE_BYTES {
            failures.push(format!("{endpoint_label} 响应超过 2 MB 安全上限"));
            continue;
        }
        if !status.is_success() {
            let detail = sanitized_upstream_message(&bytes, key)
                .map(|message| format!("：{message}"))
                .unwrap_or_default();
            failures.push(format!(
                "{endpoint_label} 返回 HTTP {}{detail}",
                status.as_u16()
            ));
            continue;
        }
        let payload: Value = match serde_json::from_slice(&bytes) {
            Ok(payload) => payload,
            Err(_) => {
                let detail = sanitized_upstream_message(&bytes, key)
                    .unwrap_or_else(|| "响应不是有效 JSON".to_string());
                failures.push(format!("{endpoint_label}：{detail}"));
                continue;
            }
        };
        match parse_models(&provider, &payload) {
            Ok(models) => return Ok(models),
            Err(error) => failures.push(format!("{endpoint_label}：{error}")),
        }
    }

    Err(format!("无法读取模型列表。{}", failures.join("；")))
}
