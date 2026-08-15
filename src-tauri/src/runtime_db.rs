use crate::execution_ticket::{
    ExecutionTicketState, TrustedExecutionReceipt, TrustedHandlerReservation, TrustedHandlerUsage,
};
use crate::obsidian::{
    collect_files_for_runtime_with_cancellation, read_file_limited_for_runtime,
    resolve_vault_for_runtime, OperationContext, OperationEvent, VaultDescriptor,
};
use crate::policy::{ApplicationCommand, CommandBudget, PolicyDecision, PolicyOutcome};
use crate::prompt::render_prompt_template;
use crate::task_runtime::{
    NativeRuntimeTask, RuntimeReadOnlyCapabilityResult, RuntimeScheduleDispatchAckInput,
    RuntimeTaskCompletionContract, RuntimeTaskCompletionContractInput, RuntimeTaskCompletionMode,
    RuntimeTaskCompletionRequirement, RuntimeTaskCompletionRequirementInput,
    RuntimeTaskCompletionStatus, RuntimeTaskContractSnapshot, RuntimeTaskEvidence,
    RuntimeTaskEvidenceInput, RuntimeTaskEvidenceSourceKind, RuntimeTaskExecutionBudgetStatus,
    RuntimeTaskPlan, RuntimeTaskPlanInput, RuntimeTaskPlanSnapshot, RuntimeTaskPlanStep,
    RuntimeTaskPlanStepInput, RuntimeTaskRequirementStatus, RuntimeTaskStepClaim,
    RuntimeTaskStepClaimBatch, RuntimeTaskStepClaimInput, RuntimeTaskStepCommandBinding,
    RuntimeTaskStepCompletionInput, RuntimeTaskStepEffectClass, RuntimeTaskStepFailureInput,
    RuntimeTaskStepFrontierItem, RuntimeTaskStepKind, RuntimeTaskStepLeaseRenewalInput,
    RuntimeTaskStepLeaseRenewalReceipt, RuntimeTaskStepReceipt, MAX_RUNTIME_TASK_EVIDENCE,
};
use chrono::{Duration as ChronoDuration, Utc};
use regex::Regex;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{ErrorKind, Write},
    path::Path,
    path::PathBuf,
    sync::Mutex,
    time::Instant,
};
use tauri::{AppHandle, Manager, State};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

const MAX_SNAPSHOT_RECORDS: usize = 10_000; // 将逐步迁移到 DatabaseConfig
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024; // 将逐步迁移到 DatabaseConfig
const MAX_CREATION_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_CREATION_RESOURCE_JSON_DEPTH: usize = 32;
const MAX_CREATION_RESOURCE_JSON_NODES: usize = 20_000;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_INBOUND_RECORD_BYTES: usize = 512 * 1024;
const MAX_RUNTIME_PLAN_BYTES: usize = 256 * 1024;
const MAX_RUNTIME_EVIDENCE_BYTES: usize = 256 * 1024;
const DEFAULT_LOCAL_WORKSPACE_SCOPE: &str = "local";
const CURRENT_SCHEMA_VERSION: i64 = 45;
const APPLICATION_AUTHORIZATION_VERSION: i64 = 1;
const VAULT_INDEX_DEBOUNCE_MS: i64 = 300;
const VAULT_INDEX_MAX_ATTEMPTS: i64 = 5;
const VAULT_INDEX_RETRY_BASE_MS: i64 = 1_000;
pub(crate) const VAULT_INDEX_BATCH_SIZE: usize = 32;
const LOCAL_FEATURE_VECTOR_VERSION: i64 = 1;
const LOCAL_FEATURE_VECTOR_DIMENSIONS: usize = 384;
const MAX_LOCAL_VECTOR_CONTENT_CHARS: usize = 250_000;
const MIN_LOCAL_VECTOR_SIMILARITY: f64 = 0.025;
const MIN_NEURAL_EMBEDDING_SIMILARITY: f64 = 0.1;
const MAX_NEURAL_EMBEDDING_INPUT_CHARS: usize = 24_000;
const NEURAL_EMBEDDING_BATCH_SIZE: usize = 32;
const MAX_NEURAL_EMBEDDING_REFRESH_NOTES: usize = 64;
const NEURAL_NOTE_EMBEDDING_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/search/neural-note-embedding.template.txt");
const NEURAL_RRF_WEIGHT: f64 = 2.0;
const LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL: f64 = 0.5;
const RRF_K: f64 = 60.0;
pub(crate) const OPTIMIZATION_RUNTIME_CAPABILITY_ID: &str = "system:optimization";
pub(crate) const OPTIMIZATION_RUNTIME_OPERATION: &str = "run";
const SCHEDULE_RUNTIME_HANDLER_PAIRS: &[(&str, &str)] = &[
    ("system:schedule", "create"),
    ("system:schedule", "update"),
    ("system:schedule", "pause"),
    ("system:schedule", "resume"),
    ("system:schedule", "delete"),
    ("system:schedule", "retry"),
];
const REPORT_RECORD_UPSERT_RUNTIME_HANDLER_PAIRS: &[(&str, &str)] =
    &[("system:reports", "run"), ("system:external", "send")];
const REPORT_RESOURCE_DELETE_RUNTIME_HANDLER_PAIRS: &[(&str, &str)] =
    &[("system:reports", "delete")];
const REPORT_SUBSCRIPTION_UPSERT_RUNTIME_HANDLER_PAIRS: &[(&str, &str)] = &[
    ("system:reports", "create"),
    ("system:reports", "update"),
    ("system:reports", "pause"),
    ("system:reports", "resume"),
];

pub struct RuntimeDatabase {
    pub(crate) connection: Mutex<Connection>,
    path: PathBuf,
    config: crate::database::DatabaseConfig,
}

pub(crate) struct ModelUsageRecord<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) trace_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) state: &'a str,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) estimated_cost_usd: Option<f64>,
    pub(crate) cost_source: &'a str,
    pub(crate) duration_ms: u64,
    pub(crate) error: Option<&'a str>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    tasks: Vec<Value>,
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    approvals: Vec<Value>,
    #[serde(default)]
    operation_logs: Vec<Value>,
    #[serde(default)]
    selected_task_id: String,
    #[serde(default)]
    client_state: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMessagePage {
    items: Vec<Value>,
    next_cursor_created_at: Option<String>,
    next_cursor_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMessageSearchResult {
    conversation_id: String,
    message_id: String,
    role: String,
    created_at: String,
    snippet: String,
    score: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseHealth {
    path: String,
    journal_mode: String,
    integrity: String,
    schema_version: i64,
    task_count: i64,
    approval_count: i64,
    message_count: i64,
    operation_event_count: i64,
    indexed_note_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationAuthorizationState {
    status: String,
    authorization_version: i64,
    decided_at: Option<String>,
    updated_at: Option<String>,
}

impl ApplicationAuthorizationState {
    pub(crate) fn is_granted(&self) -> bool {
        self.status == "granted" && self.authorization_version == APPLICATION_AUTHORIZATION_VERSION
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupResult {
    pub(crate) path: String,
    byte_length: u64,
    created_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupInfo {
    path: String,
    file_name: String,
    byte_length: u64,
    modified_at: String,
    schema_version: i64,
    integrity: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRestorePreflight {
    path: String,
    file_name: String,
    byte_length: u64,
    schema_version: i64,
    integrity: String,
    compatible: bool,
    reason: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRestoreResult {
    pub(crate) restored_from: String,
    safety_backup: String,
    schema_version: i64,
    integrity: String,
    restored_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBuildResult {
    vault_id: String,
    indexed_notes: usize,
    skipped_notes: usize,
    completed_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ClaimedVaultIndexChange {
    pub(crate) id: i64,
    pub(crate) vault_id: String,
    pub(crate) canonical_root: PathBuf,
    pub(crate) relative_path: String,
    pub(crate) generation: i64,
    pub(crate) attempt_count: i64,
    pub(crate) trace_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AppliedVaultIndexChange {
    pub(crate) vault_id: String,
    pub(crate) relative_path: String,
    pub(crate) change_kind: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VaultIndexReconcileResult {
    pub(crate) queued_upserts: usize,
    pub(crate) queued_deletes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VaultIndexFailureOutcome {
    pub(crate) updated: bool,
    pub(crate) terminal: bool,
}

struct PreparedNoteIndex {
    relative_path: String,
    title: String,
    content_hash: String,
    modified_at: String,
    byte_length: u64,
    tags_json: String,
    wiki_links_json: String,
    content: String,
    feature_vector: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchResult {
    vault_id: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    score: f64,
    tags: Vec<String>,
    wiki_links: Vec<String>,
    source_kind: String,
    ranking_signals: IndexedSearchSignals,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexedSearchSignals {
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    neural_rank: Option<usize>,
    local_vector_rank: Option<usize>,
    lexical_rrf: f64,
    vector_rrf: f64,
    neural_rrf: f64,
    local_vector_rrf: f64,
    vector_similarity: Option<f64>,
    neural_similarity: Option<f64>,
    local_vector_similarity: Option<f64>,
    title_path_bonus: f64,
    relation_bonus: f64,
    recency_bonus: f64,
    vector_kind: String,
    embedding_provider: Option<String>,
    embedding_model: Option<String>,
    embedding_index_state: Option<String>,
}

#[derive(Clone)]
struct IndexedSearchCandidate {
    vault_id: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    lexical_score: Option<f64>,
    vector_similarity: Option<f64>,
    tags: Vec<String>,
    wiki_links: Vec<String>,
}

#[derive(Clone, Debug)]
struct NeuralSearchContext {
    workspace_scope: String,
    provider_id: String,
    provider: String,
    model: String,
    query_vector: Vec<f32>,
    index_state: String,
}

#[derive(Clone, Debug)]
struct NeuralEmbeddingNoteInput {
    vault_id: String,
    relative_path: String,
    content_hash: String,
    input_hash: String,
    input: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeuralEmbeddingVaultIndexStatus {
    vault_id: String,
    state: String,
    total_notes: i64,
    indexed_notes: i64,
    pending_notes: i64,
    last_error: Option<String>,
    updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeuralEmbeddingIndexStatus {
    workspace_scope: String,
    vault_id: Option<String>,
    configured: bool,
    provider_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    state: String,
    total_notes: i64,
    indexed_notes: i64,
    pending_notes: i64,
    cache_entries: i64,
    last_error: Option<String>,
    updated_at: Option<String>,
    vaults: Vec<NeuralEmbeddingVaultIndexStatus>,
}

#[derive(Default)]
struct NeuralEmbeddingRefreshOutcome {
    loaded_notes: usize,
    indexed_notes: usize,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DueRuntimeSchedule {
    pub(crate) id: String,
    pub(crate) schedule_kind: String,
    pub(crate) payload: Value,
    pub(crate) payload_hash: String,
    pub(crate) schedule_revision: u64,
    pub(crate) occurrence_id: String,
    pub(crate) scheduled_for: String,
    pub(crate) runtime_task_id: String,
}

pub(crate) struct RuntimeScheduleDispatchBinding {
    pub(crate) schedule_id: String,
    pub(crate) schedule_kind: String,
    pub(crate) occurrence_id: String,
    pub(crate) scheduled_for: String,
    pub(crate) schedule_revision: u64,
    pub(crate) schedule_payload_hash: String,
    pub(crate) runtime_task_id: String,
}

struct ScheduleOccurrenceTask {
    occurrence_id: String,
    runtime_task_id: String,
    schedule_revision: u64,
    payload: Value,
    payload_hash: String,
}

struct ScheduleOccurrenceClaim<'a> {
    workspace_scope: &'a str,
    schedule_id: &'a str,
    schedule_kind: &'a str,
    scheduled_for: &'a str,
    schedule_revision: i64,
    schedule_payload: &'a Value,
    schedule_payload_hash: &'a str,
}

struct RuntimeTaskPlanStepRecord {
    step_id: String,
    step_kind: RuntimeTaskStepKind,
    title: String,
    depends_on: Vec<String>,
    parameters: Value,
    effect_class: RuntimeTaskStepEffectClass,
}

#[derive(Clone)]
struct RuntimeChildExecutionExpectation {
    binding: RuntimeTaskStepCommandBinding,
    command_id: String,
    trace_id: String,
    capability_id: String,
    operation: String,
    parameters: Value,
    vault_id: Option<String>,
    command_effectful: bool,
    effect_class: RuntimeTaskStepEffectClass,
    run_state: String,
    lease_expires_at: String,
    parent_state: String,
    cancellation_fence: u64,
    budget_cancellation_fence: u64,
    cancelled_at: Option<String>,
    reserved_tool_calls: u64,
    reserved_runtime_seconds: u64,
    reserved_tokens: u64,
    reserved_cost: f64,
    max_tokens: Option<u64>,
    max_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuntimeEffectfulHandlerAuthorization {
    pub(crate) execution_ticket: String,
    pub(crate) child_task_id: String,
    pub(crate) command_id: String,
    pub(crate) trace_id: String,
    pub(crate) capability_id: String,
    pub(crate) operation: String,
    pub(crate) binding: RuntimeTaskStepCommandBinding,
    pub(crate) reservation: TrustedHandlerReservation,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeEffectMutationKey {
    pub(crate) command_id: String,
    pub(crate) handler_kind: &'static str,
    pub(crate) request_hash: String,
}

impl RuntimeEffectMutationKey {
    pub(crate) fn completion_key(&self) -> String {
        format!("{}:{}", self.handler_kind, self.request_hash)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRecovery {
    task_id: String,
    recommendation: String,
    resume_step_id: Option<String>,
    resume_step_index: Option<i64>,
    resume_checkpoint_id: Option<String>,
    evidence: Vec<String>,
    plan_revision: Option<u64>,
    completion_satisfied: Option<bool>,
    missing_requirement_ids: Vec<String>,
    replacement_key: Option<String>,
    replacement_task_id: Option<String>,
    detail: String,
    detected_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRecoveryReplacement {
    interrupted_task_id: String,
    replacement_key: String,
    replacement_task_id: Option<String>,
    state: String,
    updated_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundContentRecordInput {
    id: String,
    state: String,
    source_type: String,
    source_ref: String,
    title: String,
    content_hash: String,
    content_characters: usize,
    attachment_count: usize,
    image_count: usize,
    #[serde(default)]
    extraction: Value,
    #[serde(default)]
    analysis: Value,
    #[serde(default)]
    quality: Value,
    #[serde(default)]
    target: Value,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundContentRecordReceipt {
    id: String,
    state: String,
    previous_state: Option<String>,
    duplicate_of: Option<String>,
    updated_at: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourceSnapshotInput {
    #[serde(default)]
    custom_skills: Vec<Value>,
    #[serde(default)]
    schedules: Vec<Value>,
    #[serde(default)]
    assistant_profile: Value,
    #[serde(default)]
    optimization_profile: Value,
    #[serde(default)]
    optimization_draft: Value,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourceSnapshot {
    initialized: bool,
    custom_skills: Vec<Value>,
    schedules: Vec<Value>,
    report_subscriptions: Vec<Value>,
    reports: Vec<Value>,
    assistant_profile: Value,
    optimization_profile: Value,
    optimization_draft: Value,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourcePage {
    items: Vec<Value>,
    next_cursor_updated_at: Option<String>,
    next_cursor_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSourceRecord {
    kind: String,
    id: String,
    state: String,
    title: String,
    occurred_at: String,
    payload: Value,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSourcePage {
    items: Vec<ReportSourceRecord>,
    next_cursor_occurred_at: Option<String>,
    next_cursor_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationResourceInput {
    schema_version: String,
    resource_type: String,
    id: String,
    version: String,
    display_name: String,
    #[serde(default)]
    description: String,
    manifest: Value,
    payload: Value,
    #[serde(default)]
    content_hash: Option<String>,
    #[serde(default)]
    source_ref_ids: Vec<String>,
    #[serde(default)]
    model_run_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationResource {
    schema_version: String,
    resource_type: String,
    id: String,
    version: String,
    display_name: String,
    description: String,
    state: String,
    revision: u64,
    manifest: Value,
    payload: Value,
    content_hash: String,
    source_ref_ids: Vec<String>,
    model_run_ids: Vec<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationResourceArchiveReceipt {
    resource_type: String,
    id: String,
    state: String,
    revision: u64,
    content_hash: String,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationResourceRestoreInput {
    resource_type: String,
    id: String,
    revision: u64,
    expected_current_revision: u64,
}

#[derive(Clone, Debug)]
struct ValidatedCreationResource {
    schema_version: String,
    resource_type: String,
    id: String,
    version: String,
    display_name: String,
    description: String,
    manifest: Value,
    payload: Value,
    manifest_json: String,
    payload_json: String,
    content_hash: String,
    source_ref_ids: Vec<String>,
    model_run_ids: Vec<String>,
    source_ref_ids_json: String,
    model_run_ids_json: String,
}

impl ValidatedCreationResource {
    fn to_public(
        &self,
        revision: i64,
        state: &str,
        created_at: &str,
        updated_at: &str,
    ) -> CreationResource {
        CreationResource {
            schema_version: self.schema_version.clone(),
            resource_type: self.resource_type.clone(),
            id: self.id.clone(),
            version: self.version.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            state: state.to_string(),
            revision: revision.max(0) as u64,
            manifest: self.manifest.clone(),
            payload: self.payload.clone(),
            content_hash: self.content_hash.clone(),
            source_ref_ids: self.source_ref_ids.clone(),
            model_run_ids: self.model_run_ids.clone(),
            created_at: created_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }
}

#[derive(Debug)]
struct StoredCreationResourceRow {
    resource_type: String,
    id: String,
    revision: i64,
    state: String,
    schema_version: String,
    version: String,
    display_name: String,
    description: String,
    manifest_json: String,
    payload_json: String,
    content_hash: String,
    source_ref_ids_json: String,
    model_run_ids_json: String,
    created_at: String,
    updated_at: String,
}

impl StoredCreationResourceRow {
    fn into_input(self) -> Result<CreationResourceInput, String> {
        let StoredCreationResourceRow {
            resource_type,
            id,
            revision: _,
            state: _,
            schema_version,
            version,
            display_name,
            description,
            manifest_json,
            payload_json,
            content_hash,
            source_ref_ids_json,
            model_run_ids_json,
            created_at: _,
            updated_at: _,
        } = self;
        Ok(CreationResourceInput {
            schema_version,
            resource_type,
            id,
            version,
            display_name,
            description,
            manifest: serde_json::from_str(&manifest_json)
                .map_err(|error| format!("创作资源 manifest 已损坏：{error}"))?,
            payload: serde_json::from_str(&payload_json)
                .map_err(|error| format!("创作资源 payload 已损坏：{error}"))?,
            content_hash: Some(content_hash),
            source_ref_ids: serde_json::from_str(&source_ref_ids_json)
                .map_err(|error| format!("创作资源来源列表已损坏：{error}"))?,
            model_run_ids: serde_json::from_str(&model_run_ids_json)
                .map_err(|error| format!("创作资源模型运行列表已损坏：{error}"))?,
        })
    }

    fn into_public(self) -> Result<CreationResource, String> {
        let revision = self.revision;
        let state = self.state.clone();
        let created_at = self.created_at.clone();
        let updated_at = self.updated_at.clone();
        if !matches!(state.as_str(), "active" | "archived") {
            return Err("创作资源状态无效".to_string());
        }
        let validated = validate_creation_resource_input(&self.into_input()?)?;
        Ok(validated.to_public(revision, &state, &created_at, &updated_at))
    }
}

pub struct LegacyModelProfile {
    pub role: String,
    pub provider: String,
    pub base_url: String,
    pub selected_model: String,
    pub available_models: Value,
    pub api_key_ciphertext: Vec<u8>,
}

pub struct ModelProviderProfile {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub available_models: Value,
    pub assignments: Value,
    pub defaults: Value,
    pub api_key_ciphertext: Vec<u8>,
}

pub(crate) struct PendingLongTermMemoryEvent {
    pub(crate) id: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryRecord {
    id: String,
    event_type: String,
    occurred_at: String,
    actor: String,
    state: String,
    governance_state: String,
    payload: Value,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryGovernanceInput {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub replacement_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryMetrics {
    total: i64,
    committed: i64,
    pending: i64,
    failed: i64,
    active: i64,
    corrected: i64,
    expired: i64,
    tombstoned: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvidenceEvent {
    id: String,
    event_type: String,
    occurred_at: String,
    actor: String,
    content: String,
    metadata: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvidenceBatch {
    cursor_revision: i64,
    cursor_occurred_at: String,
    cursor_event_id: String,
    next_occurred_at: String,
    next_event_id: String,
    events: Vec<OptimizationEvidenceEvent>,
    has_more: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationCandidateInput {
    id: String,
    expected_cursor_revision: i64,
    summary: String,
    #[serde(default)]
    rules: Vec<String>,
    #[serde(default)]
    skill_hints: Value,
    #[serde(default)]
    metrics: Value,
    #[serde(default)]
    evidence_count: usize,
    #[serde(default)]
    evidence_cursor_occurred_at: String,
    #[serde(default)]
    evidence_cursor_event_id: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationCandidateResult {
    id: String,
    base_version: i64,
    candidate_version: i64,
    state: String,
    summary: String,
    rules: Vec<String>,
    skill_hints: Value,
    metrics: Value,
    evidence_count: usize,
    created_at: String,
    evaluated_at: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvaluationResult {
    candidate_id: String,
    state: String,
    passed: bool,
    checks: Vec<String>,
    evaluated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationProfileResult {
    pub(crate) version: i64,
    pub(crate) candidate_id: Option<String>,
    guidance: String,
    rules: Vec<String>,
    skill_hints: Value,
    updated_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationVersion {
    version: i64,
    candidate_id: Option<String>,
    state: String,
    guidance: String,
    created_at: String,
    rollback_target: Option<i64>,
}

pub(crate) fn load_optimization_profile_in_connection(
    connection: &Connection,
    workspace_scope: &str,
) -> Result<OptimizationProfileResult, String> {
    let row = connection
        .query_row(
            "SELECT version, candidate_id, guidance, rules_json, skill_hints_json, updated_at
             FROM optimization_profiles WHERE workspace_scope=?1",
            [workspace_scope],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .map_err(|error| format!("无法读取当前优化配置：{error}"))?;
    Ok(OptimizationProfileResult {
        version: row.0,
        candidate_id: row.1,
        guidance: row.2,
        rules: serde_json::from_str(&row.3).unwrap_or_default(),
        skill_hints: serde_json::from_str(&row.4)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        updated_at: row.5,
    })
}

pub(crate) fn apply_evaluated_optimization_candidate_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    candidate_id: &str,
) -> Result<OptimizationProfileResult, String> {
    if !valid_runtime_identifier(candidate_id, 160) {
        return Err("优化候选 ID 无效".to_string());
    }
    let candidate = connection
        .query_row(
            "SELECT base_version, candidate_version, state, summary, rules_json,
                    skill_hints_json, expires_at
             FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, candidate_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取待应用优化候选：{error}"))?
        .ok_or_else(|| "优化候选不存在".to_string())?;
    if candidate.2 == "applied" {
        let profile = load_optimization_profile_in_connection(connection, workspace_scope)?;
        if profile.candidate_id.as_deref() == Some(candidate_id) && profile.version == candidate.1 {
            return Ok(profile);
        }
        return Err("优化候选已经应用，但当前配置已发生后续变化".to_string());
    }
    if candidate.2 != "pending_review" {
        return Err(format!(
            "优化候选当前状态为 {}，未通过独立评估",
            candidate.2
        ));
    }
    let evaluated = connection
        .query_row(
            "SELECT 1 FROM optimization_evaluations
             WHERE workspace_scope=?1 AND candidate_id=?2 AND state='pending_review'
             ORDER BY evaluated_at DESC, id DESC LIMIT 1",
            params![workspace_scope, candidate_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法验证优化候选评估：{error}"))?
        .is_some();
    if !evaluated {
        return Err("优化候选缺少通过的独立评估回执".to_string());
    }
    if candidate.6.as_deref().is_some_and(|expires_at| {
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .ok()
            .is_some_and(|value| value.with_timezone(&Utc) <= Utc::now())
    }) {
        return Err("优化候选已过期，需要重新生成和评估".to_string());
    }
    let current_version = connection
        .query_row(
            "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
            [workspace_scope],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
    if candidate.0 != current_version || candidate.1 != current_version + 1 {
        return Err("优化候选基线已变化，需要重新生成和评估".to_string());
    }
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO optimization_profile_revisions
             (workspace_scope, version, candidate_id, state, guidance, rules_json,
              skill_hints_json, created_at, rollback_target)
             VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, ?7, NULL)",
            params![
                workspace_scope,
                candidate.1,
                candidate_id,
                candidate.3,
                candidate.4,
                candidate.5,
                now,
            ],
        )
        .map_err(|error| format!("无法保存优化版本：{error}"))?;
    let changed = connection
        .execute(
            "UPDATE optimization_profiles SET version=?2, candidate_id=?3, guidance=?4,
             rules_json=?5, skill_hints_json=?6, updated_at=?7
             WHERE workspace_scope=?1 AND version=?8",
            params![
                workspace_scope,
                candidate.1,
                candidate_id,
                candidate.3,
                candidate.4,
                candidate.5,
                now,
                current_version,
            ],
        )
        .map_err(|error| format!("无法原子应用优化配置：{error}"))?;
    if changed != 1 {
        return Err("优化配置版本已变化，候选没有应用".to_string());
    }
    let changed = connection
        .execute(
            "UPDATE optimization_candidates SET state='applied'
             WHERE workspace_scope=?1 AND id=?2 AND state='pending_review'",
            params![workspace_scope, candidate_id],
        )
        .map_err(|error| format!("无法更新优化候选状态：{error}"))?;
    if changed != 1 {
        return Err("优化候选状态已变化，候选没有应用".to_string());
    }
    load_optimization_profile_in_connection(connection, workspace_scope)
}

impl RuntimeDatabase {
    pub fn open(app: &AppHandle) -> Result<Self, String> {
        // 初始化数据库基础设施（性能监控等）
        crate::database::init_database_infrastructure();

        // 创建数据库配置（默认配置）
        let config = crate::database::DatabaseConfig::default();

        #[cfg(debug_assertions)]
        let app_data = std::env::var_os("YUNSPIRE_APP_DATA_DIR")
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
        fs::create_dir_all(&app_data).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
        let path = app_data.join("yunspire.sqlite");
        let connection =
            Connection::open(&path).map_err(|error| format!("无法打开 SQLite 数据库：{error}"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("无法设置 SQLite busy timeout：{error}"))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA temp_store=MEMORY;",
            )
            .map_err(|error| format!("无法配置 SQLite：{error}"))?;
        run_migrations(&connection)?;

        log::info!(
            "数据库初始化完成 - 连接池: {}, 批处理: {}, 慢查询阈值: {}ms",
            config.connection_pool_size,
            config.vault_index_batch_size,
            config.slow_query_threshold_ms
        );

        Ok(Self {
            connection: Mutex::new(connection),
            path,
            config,
        })
    }

    pub fn local_workspace_scope(&self) -> Result<String, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT workspace_scope FROM (
                   SELECT workspace_scope AS workspace_scope, updated_at FROM workspace_snapshots
                   UNION ALL
                   SELECT workspace_scope AS workspace_scope, updated_at FROM model_providers
                 ) ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|scope| scope.unwrap_or_else(|| DEFAULT_LOCAL_WORKSPACE_SCOPE.to_string()))
            .map_err(|error| format!("无法读取本地工作区作用域：{error}"))
    }

    pub(crate) fn application_authorization(
        &self,
    ) -> Result<ApplicationAuthorizationState, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT status, authorization_version, decided_at, updated_at
                 FROM application_authorization WHERE id=1",
                [],
                |row| {
                    Ok(ApplicationAuthorizationState {
                        status: row.get(0)?,
                        authorization_version: row.get(1)?,
                        decided_at: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map(|state| {
                state
                    .filter(|state| {
                        state.authorization_version == APPLICATION_AUTHORIZATION_VERSION
                    })
                    .unwrap_or(ApplicationAuthorizationState {
                        status: "pending".to_string(),
                        authorization_version: APPLICATION_AUTHORIZATION_VERSION,
                        decided_at: None,
                        updated_at: None,
                    })
            })
            .map_err(|error| format!("无法读取云枢统一授权状态：{error}"))
    }

    pub(crate) fn set_application_authorization(
        &self,
        granted: bool,
    ) -> Result<ApplicationAuthorizationState, String> {
        let status = if granted { "granted" } else { "denied" };
        let decided_at = Utc::now().to_rfc3339();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO application_authorization
                 (id, status, authorization_version, decided_at, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                   status=excluded.status,
                   authorization_version=excluded.authorization_version,
                   decided_at=excluded.decided_at,
                   updated_at=excluded.updated_at",
                params![status, APPLICATION_AUTHORIZATION_VERSION, decided_at],
            )
            .map_err(|error| format!("无法保存云枢统一授权状态：{error}"))?;
        Ok(ApplicationAuthorizationState {
            status: status.to_string(),
            authorization_version: APPLICATION_AUTHORIZATION_VERSION,
            decided_at: Some(decided_at.clone()),
            updated_at: Some(decided_at),
        })
    }

    pub(crate) fn record_model_usage(&self, record: &ModelUsageRecord<'_>) -> Result<(), String> {
        let workspace_scope = self.local_workspace_scope()?;
        crate::trace::validate_trace_id(record.trace_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始模型用量记录事务：{error}"))?;
        let existing_trace = transaction
            .query_row(
                "SELECT trace_id FROM model_usage_events WHERE request_id=?1",
                [record.request_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|error| format!("无法校验模型请求 Trace：{error}"))?
            .flatten();
        if existing_trace
            .as_deref()
            .is_some_and(|trace_id| trace_id != record.trace_id)
        {
            return Err("同一模型请求不能重新绑定其他 Trace".to_string());
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO model_usage_events
                 (id, workspace_scope, request_id, trace_id, operation, provider, model, state,
                  prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd,
                  cost_source, duration_ms, error, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT(request_id) DO UPDATE SET
                   state=excluded.state,
                   prompt_tokens=excluded.prompt_tokens,
                   completion_tokens=excluded.completion_tokens,
                   total_tokens=excluded.total_tokens,
                   estimated_cost_usd=excluded.estimated_cost_usd,
                   cost_source=excluded.cost_source,
                   duration_ms=excluded.duration_ms,
                   error=excluded.error,
                   completed_at=excluded.completed_at",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    record.request_id,
                    record.trace_id,
                    record.operation,
                    record.provider,
                    record.model,
                    record.state,
                    record.prompt_tokens as i64,
                    record.completion_tokens as i64,
                    record.total_tokens as i64,
                    record.estimated_cost_usd,
                    record.cost_source,
                    record.duration_ms as i64,
                    record.error,
                    now,
                    if record.state == "started" {
                        None
                    } else {
                        Some(now.clone())
                    },
                ],
            )
            .map_err(|error| format!("无法记录模型 Token 与费用：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            &workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: record.trace_id,
                entity_kind: "model_request",
                entity_id: record.request_id,
                event_type: "model.request.state",
                state: record.state,
                payload: &serde_json::json!({
                    "operation": record.operation,
                    "provider": record.provider,
                    "model": record.model,
                    "promptTokens": record.prompt_tokens,
                    "completionTokens": record.completion_tokens,
                    "totalTokens": record.total_tokens,
                    "durationMs": record.duration_ms,
                    "error": record.error,
                }),
                created_at: &now,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交模型用量与 Trace：{error}"))
    }

    pub fn sync_vault_registry(&self, vaults: &[VaultDescriptor]) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("sync_vault_registry")
            .with_threshold(self.config.slow_query_threshold_ms);

        let current_ids = vaults
            .iter()
            .map(|vault| vault.id.as_str())
            .collect::<HashSet<_>>();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始 Vault 注册事务：{error}"))?;
        let stale_ids = {
            let mut statement = transaction
                .prepare("SELECT id FROM vault_registry")
                .map_err(|error| format!("无法读取 Vault 注册表：{error}"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法枚举 Vault 注册表：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法解析 Vault 注册表：{error}"))?
                .into_iter()
                .filter(|vault_id| !current_ids.contains(vault_id.as_str()))
                .collect::<Vec<_>>()
        };
        for vault_id in stale_ids {
            transaction
                .execute("DELETE FROM note_fts WHERE vault_id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的全文索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_lexical_fts WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的中文词法索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_feature_vectors WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的本地特征向量：{error}"))?;
            transaction
                .execute("DELETE FROM note_index WHERE vault_id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的笔记索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM vault_index_changes WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的索引队列：{error}"))?;
            transaction
                .execute("DELETE FROM vault_registry WHERE id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的注册记录：{error}"))?;
        }
        for vault in vaults {
            transaction
                .execute(
                    "INSERT INTO vault_registry (
                       id, display_name, canonical_path, note_count, attachment_count,
                       connection_state, is_open, last_indexed_at, last_error
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(id) DO UPDATE SET
                       display_name=excluded.display_name,
                       canonical_path=excluded.canonical_path,
                       note_count=excluded.note_count,
                       attachment_count=excluded.attachment_count,
                       connection_state=excluded.connection_state,
                       is_open=excluded.is_open,
                       last_indexed_at=excluded.last_indexed_at,
                       last_error=excluded.last_error",
                    params![
                        vault.id,
                        vault.name,
                        vault.path,
                        vault.note_count,
                        vault.attachment_count,
                        vault.connection_state,
                        vault.is_open,
                        vault.last_indexed_at,
                        vault.last_error,
                    ],
                )
                .map_err(|error| format!("无法更新 Vault 注册表：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 注册事务：{error}"))
    }

    pub(crate) fn stage_long_term_memory_event(
        &self,
        workspace_scope: &str,
        event_id: &str,
        event_type: &str,
        occurred_at: &str,
        payload: &Value,
    ) -> Result<bool, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("stage_long_term_memory_event")
            .with_threshold(self.config.slow_query_threshold_ms);

        let serialized = serde_json::to_string(payload)
            .map_err(|error| format!("无法序列化长期记忆投递记录：{error}"))?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err("长期记忆投递记录超过 2 MB 安全上限".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let existing = connection
            .query_row(
                "SELECT workspace_scope, payload, state FROM long_term_memory_events WHERE id=?1",
                [event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法检查长期记忆投递记录：{error}"))?;
        if let Some((existing_user, existing_payload, state)) = existing {
            if existing_user != workspace_scope || existing_payload != serialized {
                return Err("长期记忆事件 ID 已被其他内容占用".to_string());
            }
            let duplicate = state == "committed";
            if !duplicate {
                connection
                    .execute(
                        "UPDATE long_term_memory_events
                         SET state='pending', last_error=NULL, updated_at=?2
                         WHERE id=?1 AND workspace_scope=?3",
                        params![event_id, Utc::now().to_rfc3339(), workspace_scope],
                    )
                    .map_err(|error| format!("无法恢复长期记忆投递：{error}"))?;
            }
            return Ok(duplicate);
        }
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO long_term_memory_events
                 (id, workspace_scope, event_type, occurred_at, payload, state, attempt_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?6)",
                params![event_id, workspace_scope, event_type, occurred_at, serialized, now],
            )
            .map_err(|error| format!("无法暂存长期记忆事件：{error}"))?;
        Ok(false)
    }

    pub(crate) fn pending_long_term_memory_events(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<PendingLongTermMemoryEvent>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("pending_long_term_memory_events")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, payload FROM long_term_memory_events
                 WHERE workspace_scope=?1 AND state IN ('pending', 'failed')
                 ORDER BY occurred_at, created_at LIMIT ?2",
            )
            .map_err(|error| format!("无法准备长期记忆重放查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, limit.clamp(1, 1000) as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| format!("无法读取待写入长期记忆：{error}"))?;
        Ok(rows
            .filter_map(Result::ok)
            .filter_map(|(id, payload)| {
                serde_json::from_str(&payload)
                    .ok()
                    .map(|payload| PendingLongTermMemoryEvent { id, payload })
            })
            .collect())
    }

    pub(crate) fn commit_long_term_memory_event_internal(
        &self,
        workspace_scope: &str,
        event_id: &str,
        content_hash: &str,
        committed_at: &str,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("commit_long_term_memory_event_internal")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let changed = connection
            .execute(
                "UPDATE long_term_memory_events
                 SET state='committed', vault_relative_path=NULL, content_hash=?3,
                     committed_at=?4, last_error=NULL, updated_at=?4
                 WHERE id=?1 AND workspace_scope=?2",
                params![event_id, workspace_scope, content_hash, committed_at],
            )
            .map_err(|error| format!("无法确认长期记忆事件已保存到 SQLite：{error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err("长期记忆投递记录不存在".to_string())
        }
    }

    pub(crate) fn fail_long_term_memory_event(
        &self,
        workspace_scope: &str,
        event_id: &str,
        error: &str,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("fail_long_term_memory_event")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE long_term_memory_events
                 SET state='failed', attempt_count=attempt_count+1, last_error=?3, updated_at=?4
                 WHERE id=?1 AND workspace_scope=?2",
                params![
                    event_id,
                    workspace_scope,
                    error.chars().take(1000).collect::<String>(),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|database_error| format!("无法记录长期记忆写入失败：{database_error}"))?;
        Ok(())
    }

    fn query_long_term_memory(
        &self,
        workspace_scope: &str,
        query: &str,
        include_inactive: bool,
        limit: usize,
    ) -> Result<Vec<LongTermMemoryRecord>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("query_long_term_memory")
            .with_threshold(self.config.slow_query_threshold_ms);

        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err("长期记忆查询超过 512 个字符".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let now = Utc::now().to_rfc3339();
        let mut statement = connection
            .prepare(
                "SELECT e.id, e.event_type, e.occurred_at, e.state, e.payload,
                        COALESCE(g.status, 'active')
                 FROM long_term_memory_events e
                 LEFT JOIN long_term_memory_governance g
                   ON g.workspace_scope=e.workspace_scope AND g.memory_id=e.id
                 WHERE e.workspace_scope=?1
                   AND (?2=1 OR (
                     COALESCE(g.status, 'active')='active'
                     AND (g.expires_at IS NULL OR g.expires_at>?3)
                   ))
                 ORDER BY e.occurred_at DESC
                 LIMIT 5000",
            )
            .map_err(|error| format!("无法准备长期记忆查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, i64::from(include_inactive), now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取长期记忆：{error}"))?;
        let normalized_query = query.trim().to_lowercase();
        let mut records = Vec::new();
        for row in rows.filter_map(Result::ok) {
            if !normalized_query.is_empty()
                && !format!("{} {} {}", row.0, row.1, row.4)
                    .to_lowercase()
                    .contains(&normalized_query)
            {
                continue;
            }
            let payload = serde_json::from_str::<Value>(&row.4)
                .map_err(|error| format!("长期记忆 {} 的载荷损坏：{error}", row.0))?;
            let actor = payload
                .get("actor")
                .and_then(Value::as_str)
                .unwrap_or("system")
                .to_string();
            records.push(LongTermMemoryRecord {
                id: row.0,
                event_type: row.1,
                occurred_at: row.2,
                actor,
                state: row.3,
                governance_state: row.5,
                payload,
            });
            if records.len() >= limit.clamp(1, 1000) {
                break;
            }
        }
        Ok(records)
    }

    fn govern_long_term_memory(
        &self,
        workspace_scope: &str,
        input: &LongTermMemoryGovernanceInput,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("govern_long_term_memory")
            .with_threshold(self.config.slow_query_threshold_ms);

        if !valid_runtime_identifier(&input.id, 160) {
            return Err("长期记忆 ID 无效".to_string());
        }
        let status = match input.action.as_str() {
            "activate" => "active",
            "correct" => "corrected",
            "expire" => "expired",
            "tombstone" => "tombstoned",
            "compress" => "compressed",
            _ => return Err("长期记忆治理操作无效".to_string()),
        };
        if input.action == "correct"
            && input
                .replacement_id
                .as_deref()
                .is_none_or(|value| !valid_runtime_identifier(value, 160))
        {
            return Err("纠错操作必须关联有效的替代记忆 ID".to_string());
        }
        let note = input.note.as_deref().unwrap_or("").trim();
        if note.chars().count() > 4000 || contains_sensitive_memory_value(note) {
            return Err("治理备注过长或包含疑似凭据".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM long_term_memory_events WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, input.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法检查长期记忆：{error}"))?
            .is_some();
        if !exists {
            return Err("长期记忆记录不存在".to_string());
        }
        let now = Utc::now().to_rfc3339();
        let expires_at = if status == "expired" {
            Some(now.clone())
        } else {
            None
        };
        connection
            .execute(
                "INSERT INTO long_term_memory_governance
                 (workspace_scope, memory_id, status, replacement_id, note, expires_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(workspace_scope, memory_id) DO UPDATE SET
                   status=excluded.status, replacement_id=excluded.replacement_id,
                   note=excluded.note, expires_at=excluded.expires_at, updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    input.id,
                    status,
                    input.replacement_id,
                    note,
                    expires_at,
                    now
                ],
            )
            .map_err(|error| format!("无法更新长期记忆治理状态：{error}"))?;
        Ok(())
    }

    fn long_term_memory_metrics(
        &self,
        workspace_scope: &str,
    ) -> Result<LongTermMemoryMetrics, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let event_count = |state: Option<&str>| -> Result<i64, String> {
            if let Some(state) = state {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM long_term_memory_events WHERE workspace_scope=?1 AND state=?2",
                        params![workspace_scope, state],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("无法统计长期记忆：{error}"))
            } else {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM long_term_memory_events WHERE workspace_scope=?1",
                        [workspace_scope],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("无法统计长期记忆：{error}"))
            }
        };
        let governance_count = |status: &str| -> Result<i64, String> {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM long_term_memory_governance WHERE workspace_scope=?1 AND status=?2",
                    params![workspace_scope, status],
                    |row| row.get(0),
                )
                .map_err(|error| format!("无法统计长期记忆治理状态：{error}"))
        };
        let total = event_count(None)?;
        let corrected = governance_count("corrected")?;
        let expired = governance_count("expired")?;
        let tombstoned = governance_count("tombstoned")?;
        let compressed = governance_count("compressed")?;
        Ok(LongTermMemoryMetrics {
            total,
            committed: event_count(Some("committed"))?,
            pending: event_count(Some("pending"))?,
            failed: event_count(Some("failed"))?,
            active: total - corrected - expired - tombstoned - compressed,
            corrected,
            expired,
            tombstoned,
        })
    }

    pub(crate) fn sync_runtime_state(
        &self,
        workspace_scope: &str,
        tasks: &[Value],
        schedules: &[Value],
        report_subscriptions: &[Value],
        scheduler_enabled: bool,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("sync_runtime_state")
            .with_threshold(self.config.slow_query_threshold_ms);

        validate_records(tasks, "原生任务")?;
        validate_records(schedules, "原生定时任务")?;
        let _ = report_subscriptions;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始原生运行时同步事务：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_settings (workspace_scope, scheduler_enabled, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(workspace_scope) DO UPDATE SET
                   scheduler_enabled=excluded.scheduler_enabled, updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    i64::from(scheduler_enabled),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|error| format!("无法保存原生调度开关：{error}"))?;
        sync_runtime_tasks(&transaction, workspace_scope, tasks)?;
        sync_runtime_schedule_group(&transaction, workspace_scope, schedules, "collection")?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生运行时同步：{error}"))
    }

    pub(crate) fn sync_managed_resources(
        &self,
        workspace_scope: &str,
        snapshot: &ManagedResourceSnapshotInput,
    ) -> Result<ManagedResourceSnapshot, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("sync_managed_resources")
            .with_threshold(self.config.slow_query_threshold_ms);

        // Reports and report subscriptions are persisted through their dedicated
        // per-record commands. Keeping them out of this full-snapshot sync avoids
        // turning a request-size guard into a product-level record ceiling.
        let groups = [("schedule", snapshot.schedules.as_slice())];
        let total = snapshot.custom_skills.len()
            + groups.iter().map(|(_, values)| values.len()).sum::<usize>();
        if total > MAX_SNAPSHOT_RECORDS {
            return Err("独立资源数量超过安全上限".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始独立资源事务：{error}"))?;
        for (resource_type, resources) in groups {
            sync_managed_resource_group(&transaction, workspace_scope, resource_type, resources)?;
        }
        let fixed_resources = [
            (
                "assistant_profile",
                "assistant-profile",
                &snapshot.assistant_profile,
            ),
            (
                "optimization_profile",
                "optimization-profile",
                &snapshot.optimization_profile,
            ),
            (
                "optimization_candidate",
                "optimization-draft",
                &snapshot.optimization_draft,
            ),
        ];
        for (resource_type, id, payload) in fixed_resources {
            if payload.is_object() && !payload.as_object().is_some_and(serde_json::Map::is_empty) {
                upsert_managed_resource(&transaction, workspace_scope, resource_type, id, payload)?;
            } else {
                tombstone_managed_resource(&transaction, workspace_scope, resource_type, id)?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交独立资源事务：{error}"))?;
        drop(connection);
        self.load_managed_resources(workspace_scope)
    }

    pub(crate) fn upsert_report_resource(
        &self,
        workspace_scope: &str,
        resource_type: &str,
        payload: &Value,
    ) -> Result<Value, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("upsert_report_resource")
            .with_threshold(self.config.slow_query_threshold_ms);

        if !matches!(resource_type, "report" | "report_subscription") {
            return Err("不支持的报告资源类型".to_string());
        }
        let id = managed_resource_id(payload, resource_type)?;
        if resource_type == "report" {
            validate_report_resource(payload)?;
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始报告资源事务：{error}"))?;
        upsert_managed_resource(&transaction, workspace_scope, resource_type, &id, payload)?;
        if resource_type == "report_subscription" {
            upsert_runtime_schedule_record(&transaction, workspace_scope, payload, "report")?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交报告资源事务：{error}"))?;
        Ok(payload.clone())
    }

    pub(crate) fn delete_report_resource(
        &self,
        workspace_scope: &str,
        resource_type: &str,
        id: &str,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("delete_report_resource")
            .with_threshold(self.config.slow_query_threshold_ms);

        if !matches!(resource_type, "report" | "report_subscription") {
            return Err("不支持的报告资源类型".to_string());
        }
        let id = id.trim();
        if id.is_empty() || id.chars().count() > 180 || id.chars().any(char::is_control) {
            return Err("报告资源 id 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始报告资源删除事务：{error}"))?;
        tombstone_managed_resource(&transaction, workspace_scope, resource_type, id)?;
        if resource_type == "report_subscription" {
            transaction
                .execute(
                    "DELETE FROM runtime_schedules WHERE workspace_scope=?1 AND id=?2 AND schedule_kind='report'",
                    params![workspace_scope, id],
                )
                .map_err(|error| format!("无法删除报告订阅调度记录：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交报告资源删除事务：{error}"))
    }

    pub(crate) fn list_report_resources_page(
        &self,
        workspace_scope: &str,
        resource_type: &str,
        cursor_updated_at: Option<&str>,
        cursor_id: Option<&str>,
        limit: usize,
    ) -> Result<ManagedResourcePage, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("list_report_resources_page")
            .with_threshold(self.config.slow_query_threshold_ms);
        if !matches!(resource_type, "report" | "report_subscription") {
            return Err("不支持的报告资源类型".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let page_limit = limit.clamp(1, 512);
        let cursor_updated_at = cursor_updated_at
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let cursor_id = cursor_id.map(str::trim).filter(|value| !value.is_empty());
        if cursor_updated_at.is_some() != cursor_id.is_some() {
            return Err("报告资源分页游标不完整".to_string());
        }
        let mut statement = connection
            .prepare(
                "SELECT id, payload, updated_at FROM managed_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'
                   AND (?3 IS NULL OR updated_at < ?3 OR (updated_at = ?3 AND id < ?4))
                 ORDER BY updated_at DESC, id DESC LIMIT ?5",
            )
            .map_err(|error| format!("无法准备报告资源分页查询：{error}"))?;
        let rows = statement
            .query_map(
                params![
                    workspace_scope,
                    resource_type,
                    cursor_updated_at,
                    cursor_id,
                    (page_limit + 1) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取报告资源分页：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析报告资源分页：{error}"))?;
        let has_more = rows.len() > page_limit;
        let visible = rows.into_iter().take(page_limit).collect::<Vec<_>>();
        let cursor = has_more
            .then(|| {
                visible
                    .last()
                    .map(|(id, _, updated_at)| (updated_at.clone(), id.clone()))
            })
            .flatten();
        let items = visible
            .into_iter()
            .map(|(_, payload, _)| {
                serde_json::from_str::<Value>(&payload)
                    .map_err(|error| format!("报告资源 JSON 损坏：{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ManagedResourcePage {
            items,
            next_cursor_updated_at: cursor.as_ref().map(|(updated_at, _)| updated_at.clone()),
            next_cursor_id: cursor.map(|(_, id)| id),
        })
    }

    // The explicit cursor fields mirror the Tauri command contract and keep pagination auditable.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_report_source_page(
        &self,
        workspace_scope: &str,
        source_kind: &str,
        start_at: &str,
        end_at: &str,
        cursor_occurred_at: Option<&str>,
        cursor_id: Option<&str>,
        limit: usize,
    ) -> Result<ReportSourcePage, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("read_report_source_page")
            .with_threshold(self.config.slow_query_threshold_ms);

        let start = chrono::DateTime::parse_from_rfc3339(start_at)
            .map_err(|_| "报告数据开始时间必须是 RFC3339".to_string())?
            .with_timezone(&Utc)
            .to_rfc3339();
        let end = chrono::DateTime::parse_from_rfc3339(end_at)
            .map_err(|_| "报告数据结束时间必须是 RFC3339".to_string())?
            .with_timezone(&Utc)
            .to_rfc3339();
        if start > end {
            return Err("报告数据时间范围无效".to_string());
        }
        let cursor_occurred_at = cursor_occurred_at
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let cursor_id = cursor_id.map(str::trim).filter(|value| !value.is_empty());
        if cursor_occurred_at.is_some() != cursor_id.is_some() {
            return Err("报告数据分页游标不完整".to_string());
        }
        if let Some(cursor) = cursor_occurred_at {
            chrono::DateTime::parse_from_rfc3339(cursor)
                .map_err(|_| "报告数据分页时间游标无效".to_string())?;
        }
        let (kind, sql) = match source_kind {
            "task" => (
                "task",
                "SELECT id, state, title, updated_at, payload FROM runtime_tasks
                 WHERE workspace_scope=?1 AND updated_at>=?2 AND updated_at<=?3
                   AND (?4 IS NULL OR updated_at < ?4 OR (updated_at = ?4 AND id < ?5))
                 ORDER BY updated_at DESC, id DESC LIMIT ?6",
            ),
            "operation" => (
                "operation",
                "SELECT id, state, event_type, created_at, payload FROM operation_events
                 WHERE created_at>=?2 AND created_at<=?3
                   AND (?4 IS NULL OR created_at < ?4 OR (created_at = ?4 AND id < ?5))
                 ORDER BY created_at DESC, id DESC LIMIT ?6",
            ),
            "capture" => (
                "capture",
                "SELECT id, state, title, updated_at, '{}' FROM inbound_content_records
                 WHERE workspace_scope=?1 AND updated_at>=?2 AND updated_at<=?3
                   AND (?4 IS NULL OR updated_at < ?4 OR (updated_at = ?4 AND id < ?5))
                 ORDER BY updated_at DESC, id DESC LIMIT ?6",
            ),
            _ => return Err("报告数据来源类型无效".to_string()),
        };
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let page_limit = limit.clamp(1, 512);
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备报告数据分页查询：{error}"))?;
        let rows = statement
            .query_map(
                params![
                    workspace_scope,
                    start,
                    end,
                    cursor_occurred_at,
                    cursor_id,
                    (page_limit + 1) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取报告数据分页：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析报告数据分页：{error}"))?;
        let has_more = rows.len() > page_limit;
        let visible = rows.into_iter().take(page_limit).collect::<Vec<_>>();
        let cursor = has_more
            .then(|| {
                visible
                    .last()
                    .map(|(id, _, _, occurred_at, _)| (occurred_at.clone(), id.clone()))
            })
            .flatten();
        let items = visible
            .into_iter()
            .map(
                |(id, state, title, occurred_at, payload)| ReportSourceRecord {
                    kind: kind.to_string(),
                    id,
                    state,
                    title,
                    occurred_at,
                    payload: serde_json::from_str(&payload)
                        .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
                },
            )
            .collect();
        Ok(ReportSourcePage {
            items,
            next_cursor_occurred_at: cursor.as_ref().map(|(occurred_at, _)| occurred_at.clone()),
            next_cursor_id: cursor.map(|(_, id)| id),
        })
    }

    pub(crate) fn load_managed_resources(
        &self,
        workspace_scope: &str,
    ) -> Result<ManagedResourceSnapshot, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("load_managed_resources")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let list = |resource_type: &str| -> Result<Vec<Value>, String> {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'
                     ORDER BY updated_at DESC",
                )
                .map_err(|error| format!("无法准备独立资源查询：{error}"))?;
            let rows = statement
                .query_map(params![workspace_scope, resource_type], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| format!("无法读取独立资源：{error}"))?;
            Ok(rows
                .filter_map(Result::ok)
                .filter_map(|payload| serde_json::from_str(&payload).ok())
                .collect())
        };
        let fixed = |resource_type: &str, id: &str| -> Result<Value, String> {
            connection
                .query_row(
                    "SELECT payload FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3 AND state='active'",
                    params![workspace_scope, resource_type, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法读取独立配置资源：{error}"))
                .map(|payload| {
                    payload
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
                })
        };
        Ok(ManagedResourceSnapshot {
            initialized: connection
                .query_row(
                    "SELECT COUNT(*) FROM managed_resources WHERE workspace_scope=?1",
                    [workspace_scope],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法检查独立资源初始化状态：{error}"))?
                > 0,
            custom_skills: crate::skill_lifecycle::list_skills_in_connection(
                &connection,
                workspace_scope,
                false,
            )?
            .into_iter()
            .map(|skill| {
                serde_json::to_value(skill)
                    .map_err(|error| format!("无法序列化 Skill 生命周期状态：{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
            schedules: list("schedule")?,
            // Loaded independently through cursor pages so report history never
            // has to fit in one IPC response.
            report_subscriptions: Vec::new(),
            reports: Vec::new(),
            assistant_profile: fixed("assistant_profile", "assistant-profile")?,
            optimization_profile: fixed("optimization_profile", "optimization-profile")?,
            optimization_draft: fixed("optimization_candidate", "optimization-draft")?,
        })
    }

    pub(crate) fn upsert_creation_resource(
        &self,
        workspace_scope: &str,
        input: CreationResourceInput,
    ) -> Result<CreationResource, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("upsert_creation_resource")
            .with_threshold(self.config.slow_query_threshold_ms);

        let resource = validate_creation_resource_input(&input)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始创作资源事务：{error}"))?;
        let existing = transaction
            .query_row(
                "SELECT revision, state, content_hash, created_at, updated_at
                 FROM creation_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
                params![workspace_scope, resource.resource_type, resource.id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取创作资源当前修订：{error}"))?;
        if let Some((revision, state, content_hash, created_at, updated_at)) = &existing {
            if state == "active" && content_hash == &resource.content_hash {
                transaction
                    .commit()
                    .map_err(|error| format!("无法提交创作资源读取事务：{error}"))?;
                return Ok(resource.to_public(*revision, "active", created_at, updated_at));
            }
        }

        let revision = existing
            .as_ref()
            .map_or(1_i64, |(previous, _, _, _, _)| previous + 1);
        let now = Utc::now().to_rfc3339();
        let created_at = existing
            .as_ref()
            .map(|(_, _, _, created_at, _)| created_at.as_str())
            .unwrap_or(now.as_str());
        transaction
            .execute(
                "INSERT INTO creation_resources
                 (workspace_scope, resource_type, id, revision, state, schema_version, version,
                  display_name, description, manifest_json, payload_json, content_hash,
                  source_ref_ids_json, model_run_ids_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(workspace_scope, resource_type, id) DO UPDATE SET
                   revision=excluded.revision, state='active', schema_version=excluded.schema_version,
                   version=excluded.version, display_name=excluded.display_name,
                   description=excluded.description, manifest_json=excluded.manifest_json,
                   payload_json=excluded.payload_json, content_hash=excluded.content_hash,
                   source_ref_ids_json=excluded.source_ref_ids_json,
                   model_run_ids_json=excluded.model_run_ids_json, updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    resource.resource_type,
                    resource.id,
                    revision,
                    resource.schema_version,
                    resource.version,
                    resource.display_name,
                    resource.description,
                    resource.manifest_json,
                    resource.payload_json,
                    resource.content_hash,
                    resource.source_ref_ids_json,
                    resource.model_run_ids_json,
                    created_at,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存创作资源：{error}"))?;
        transaction
            .execute(
                "INSERT INTO creation_resource_revisions
                 (workspace_scope, resource_type, resource_id, revision, state, schema_version,
                  version, display_name, description, manifest_json, payload_json, content_hash,
                  source_ref_ids_json, model_run_ids_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    workspace_scope,
                    resource.resource_type,
                    resource.id,
                    revision,
                    resource.schema_version,
                    resource.version,
                    resource.display_name,
                    resource.description,
                    resource.manifest_json,
                    resource.payload_json,
                    resource.content_hash,
                    resource.source_ref_ids_json,
                    resource.model_run_ids_json,
                    now,
                ],
            )
            .map_err(|error| format!("无法记录创作资源修订：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交创作资源事务：{error}"))?;
        Ok(resource.to_public(revision, "active", created_at, &now))
    }

    pub(crate) fn list_creation_resources(
        &self,
        workspace_scope: &str,
        include_archived: bool,
    ) -> Result<Vec<CreationResource>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("list_creation_resources")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let sql = if include_archived {
            "SELECT resource_type, id, revision, state, schema_version, version, display_name,
                    description, manifest_json, payload_json, content_hash, source_ref_ids_json,
                    model_run_ids_json, created_at, updated_at
             FROM creation_resources
             WHERE workspace_scope=?1
             ORDER BY state ASC, resource_type ASC, display_name COLLATE NOCASE ASC, id ASC"
        } else {
            "SELECT resource_type, id, revision, state, schema_version, version, display_name,
                    description, manifest_json, payload_json, content_hash, source_ref_ids_json,
                    model_run_ids_json, created_at, updated_at
             FROM creation_resources
             WHERE workspace_scope=?1 AND state='active'
             ORDER BY resource_type ASC, display_name COLLATE NOCASE ASC, id ASC"
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备创作资源查询：{error}"))?;
        let rows = statement
            .query_map([workspace_scope], creation_resource_row)
            .map_err(|error| format!("无法读取创作资源：{error}"))?;
        rows.map(|row| {
            row.map_err(|error| format!("无法解析创作资源数据库记录：{error}"))?
                .into_public()
        })
        .collect()
    }

    pub(crate) fn list_creation_resource_revisions(
        &self,
        workspace_scope: &str,
        resource_type: &str,
        id: &str,
    ) -> Result<Vec<CreationResource>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("list_creation_resource_revisions")
            .with_threshold(self.config.slow_query_threshold_ms);

        let resource_type = validate_creation_resource_type(resource_type)?;
        let id = validate_creation_resource_id(id)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT resource_type, resource_id, revision, state, schema_version, version,
                        display_name, description, manifest_json, payload_json, content_hash,
                        source_ref_ids_json, model_run_ids_json, created_at, created_at
                 FROM creation_resource_revisions
                 WHERE workspace_scope=?1 AND resource_type=?2 AND resource_id=?3
                 ORDER BY revision DESC",
            )
            .map_err(|error| format!("无法准备创作资源版本查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, resource_type, id],
                creation_resource_row,
            )
            .map_err(|error| format!("无法读取创作资源版本：{error}"))?;
        rows.map(|row| {
            row.map_err(|error| format!("无法解析创作资源版本记录：{error}"))?
                .into_public()
        })
        .collect()
    }

    pub(crate) fn restore_creation_resource_revision(
        &self,
        workspace_scope: &str,
        input: CreationResourceRestoreInput,
    ) -> Result<CreationResource, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("restore_creation_resource_revision")
            .with_threshold(self.config.slow_query_threshold_ms);

        let resource_type = validate_creation_resource_type(&input.resource_type)?;
        let id = validate_creation_resource_id(&input.id)?;
        if input.revision == 0 || input.expected_current_revision == 0 {
            return Err("创作资源 revision 必须大于 0".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始创作资源恢复事务：{error}"))?;
        let (current_revision, created_at) = transaction
            .query_row(
                "SELECT revision, created_at FROM creation_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
                params![workspace_scope, resource_type, id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取创作资源当前版本：{error}"))?
            .ok_or_else(|| "未找到创作资源".to_string())?;
        if current_revision.max(0) as u64 != input.expected_current_revision {
            return Err("创作资源当前 revision 已变化，请重新加载版本历史".to_string());
        }
        let source_row = transaction
            .query_row(
                "SELECT resource_type, resource_id, revision, state, schema_version, version,
                        display_name, description, manifest_json, payload_json, content_hash,
                        source_ref_ids_json, model_run_ids_json, created_at, created_at
                 FROM creation_resource_revisions
                 WHERE workspace_scope=?1 AND resource_type=?2 AND resource_id=?3 AND revision=?4",
                params![workspace_scope, resource_type, id, input.revision as i64],
                creation_resource_row,
            )
            .optional()
            .map_err(|error| format!("无法读取待恢复创作资源版本：{error}"))?
            .ok_or_else(|| "未找到指定创作资源版本".to_string())?;
        let resource = validate_creation_resource_input(&source_row.into_input()?)?;
        let next_revision = current_revision + 1;
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE creation_resources
                 SET revision=?4, state='active', schema_version=?5, version=?6,
                     display_name=?7, description=?8, manifest_json=?9, payload_json=?10,
                     content_hash=?11, source_ref_ids_json=?12, model_run_ids_json=?13,
                     updated_at=?14
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3 AND revision=?15",
                params![
                    workspace_scope,
                    resource.resource_type,
                    resource.id,
                    next_revision,
                    resource.schema_version,
                    resource.version,
                    resource.display_name,
                    resource.description,
                    resource.manifest_json,
                    resource.payload_json,
                    resource.content_hash,
                    resource.source_ref_ids_json,
                    resource.model_run_ids_json,
                    now,
                    current_revision,
                ],
            )
            .map_err(|error| format!("无法恢复创作资源版本：{error}"))?;
        transaction
            .execute(
                "INSERT INTO creation_resource_revisions
                 (workspace_scope, resource_type, resource_id, revision, state, schema_version,
                  version, display_name, description, manifest_json, payload_json, content_hash,
                  source_ref_ids_json, model_run_ids_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    workspace_scope,
                    resource.resource_type,
                    resource.id,
                    next_revision,
                    resource.schema_version,
                    resource.version,
                    resource.display_name,
                    resource.description,
                    resource.manifest_json,
                    resource.payload_json,
                    resource.content_hash,
                    resource.source_ref_ids_json,
                    resource.model_run_ids_json,
                    now,
                ],
            )
            .map_err(|error| format!("无法记录创作资源恢复版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交创作资源恢复事务：{error}"))?;
        Ok(resource.to_public(next_revision, "active", &created_at, &now))
    }

    pub(crate) fn archive_creation_resource(
        &self,
        workspace_scope: &str,
        resource_type: &str,
        id: &str,
    ) -> Result<CreationResourceArchiveReceipt, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("archive_creation_resource")
            .with_threshold(self.config.slow_query_threshold_ms);

        let resource_type = validate_creation_resource_type(resource_type)?;
        let id = validate_creation_resource_id(id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始创作资源归档事务：{error}"))?;
        let existing = transaction
            .query_row(
                "SELECT revision, state, content_hash, updated_at
                 FROM creation_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
                params![workspace_scope, resource_type, id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取待归档创作资源：{error}"))?
            .ok_or_else(|| "未找到创作资源".to_string())?;
        if existing.1 == "archived" {
            transaction
                .commit()
                .map_err(|error| format!("无法提交创作资源归档读取事务：{error}"))?;
            return Ok(CreationResourceArchiveReceipt {
                resource_type,
                id,
                state: "archived".to_string(),
                revision: existing.0.max(0) as u64,
                content_hash: existing.2,
                updated_at: existing.3,
            });
        }
        let revision = existing.0 + 1;
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE creation_resources
                 SET revision=?4, state='archived', updated_at=?5
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
                params![workspace_scope, resource_type, id, revision, now],
            )
            .map_err(|error| format!("无法归档创作资源：{error}"))?;
        transaction
            .execute(
                "INSERT INTO creation_resource_revisions
                 (workspace_scope, resource_type, resource_id, revision, state, schema_version,
                  version, display_name, description, manifest_json, payload_json, content_hash,
                  source_ref_ids_json, model_run_ids_json, created_at)
                 SELECT workspace_scope, resource_type, id, revision, state, schema_version,
                        version, display_name, description, manifest_json, payload_json, content_hash,
                        source_ref_ids_json, model_run_ids_json, updated_at
                 FROM creation_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
                params![workspace_scope, resource_type, id],
            )
            .map_err(|error| format!("无法记录创作资源归档修订：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交创作资源归档事务：{error}"))?;
        Ok(CreationResourceArchiveReceipt {
            resource_type,
            id,
            state: "archived".to_string(),
            revision: revision.max(0) as u64,
            content_hash: existing.2,
            updated_at: now,
        })
    }

    pub(crate) fn claim_due_runtime_schedules(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<DueRuntimeSchedule>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("claim_due_runtime_schedules")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let scheduler_enabled = connection
            .query_row(
                "SELECT scheduler_enabled FROM runtime_settings WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取原生调度开关：{error}"))?
            .unwrap_or(0);
        if scheduler_enabled == 0 {
            return Ok(Vec::new());
        }
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始调度租约事务：{error}"))?;
        let now = Utc::now().to_rfc3339();
        let mut statement = transaction
            .prepare(
                "SELECT id, schedule_kind, next_run, revision, payload, payload_hash
                 FROM runtime_schedules
                 WHERE workspace_scope=?1 AND enabled=1 AND next_run IS NOT NULL AND next_run<=?2
                   AND (lease_expires_at IS NULL OR lease_expires_at<=?2)
                 ORDER BY next_run LIMIT ?3",
            )
            .map_err(|error| format!("无法查询到期原生日程：{error}"))?;
        let selected = statement
            .query_map(
                params![workspace_scope, now, limit.clamp(1, 128) as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取到期原生日程：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        drop(statement);
        let lease_owner = Uuid::new_v4().to_string();
        let lease_expires_at = (Utc::now() + chrono::Duration::seconds(90)).to_rfc3339();
        let mut due = Vec::new();
        for (id, schedule_kind, scheduled_for, schedule_revision, payload_json, payload_hash) in
            selected
        {
            let changed = transaction
                .execute(
                    "UPDATE runtime_schedules
                     SET lease_owner=?3, lease_expires_at=?4, last_claimed_at=?5, updated_at=?5
                     WHERE workspace_scope=?1 AND id=?2 AND (lease_expires_at IS NULL OR lease_expires_at<=?5)",
                    params![workspace_scope, id, lease_owner, lease_expires_at, now],
                )
                .map_err(|error| format!("无法领取到期原生日程：{error}"))?;
            if changed != 1 {
                continue;
            }
            if let Ok(payload) = serde_json::from_str::<Value>(&payload_json) {
                let Some(occurrence) = ensure_schedule_occurrence_task(
                    &transaction,
                    ScheduleOccurrenceClaim {
                        workspace_scope,
                        schedule_id: &id,
                        schedule_kind: &schedule_kind,
                        scheduled_for: &scheduled_for,
                        schedule_revision,
                        schedule_payload: &payload,
                        schedule_payload_hash: &payload_hash,
                    },
                )?
                else {
                    continue;
                };
                due.push(DueRuntimeSchedule {
                    id,
                    schedule_kind,
                    payload: occurrence.payload,
                    payload_hash: occurrence.payload_hash,
                    schedule_revision: occurrence.schedule_revision,
                    occurrence_id: occurrence.occurrence_id,
                    scheduled_for,
                    runtime_task_id: occurrence.runtime_task_id,
                });
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交调度租约：{error}"))?;
        Ok(due)
    }

    pub(crate) fn recover_interrupted_runtime_tasks(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<RuntimeTaskRecovery>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("recover_interrupted_runtime_tasks")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始任务恢复检查事务：{error}"))?;
        let interrupted = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, payload, updated_at FROM runtime_tasks
                     WHERE workspace_scope=?1 AND state IN ('running', 'awaiting_approval')
                     ORDER BY updated_at",
                )
                .map_err(|error| format!("无法查询中断任务：{error}"))?;
            let rows = statement
                .query_map([workspace_scope], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|error| format!("无法读取中断任务：{error}"))?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            rows
        };
        let detected_at = Utc::now().to_rfc3339();
        for (task_id, payload_json, task_updated_at) in interrupted {
            let payload = serde_json::from_str::<Value>(&payload_json)
                .map_err(|error| format!("中断任务 {task_id} 的快照损坏：{error}"))?;
            let contract_completion =
                evaluate_runtime_task_completion(&transaction, workspace_scope, &task_id)?;
            let plan_revision = contract_completion
                .as_ref()
                .map(|status| status.plan_revision);
            let missing_requirement_ids = contract_completion
                .as_ref()
                .map(|status| {
                    status
                        .requirements
                        .iter()
                        .filter(|requirement| !requirement.satisfied)
                        .map(|requirement| requirement.id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let completed_write_events = transaction
                .query_row(
                    "SELECT COUNT(*) FROM operation_events
                     WHERE task_id=?1 AND state IN ('success', 'succeeded')
                       AND (event_type LIKE 'vault.%write%' OR event_type LIKE 'vault.%delete%')",
                    [&task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法读取任务写入证据：{error}"))?;
            let committed_content = transaction
                .query_row(
                    "SELECT COUNT(*) FROM inbound_content_records
                     WHERE workspace_scope=?1 AND task_id=?2 AND state='committed'",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法读取任务内容提交证据：{error}"))?;
            let legacy_resume_step = transaction
                .query_row(
                    "SELECT step_id, position FROM runtime_task_steps
                     WHERE workspace_scope=?1 AND task_id=?2 AND state NOT IN ('done', 'succeeded')
                     ORDER BY position LIMIT 1",
                    params![workspace_scope, task_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(|error| format!("无法读取任务恢复步骤：{error}"))?;
            let contract_resume_step = if plan_revision.is_some() {
                transaction
                    .query_row(
                        "SELECT step.step_id, step.position
                         FROM runtime_task_plan_steps step
                         JOIN runtime_task_completion_requirements requirement
                           ON requirement.workspace_scope=step.workspace_scope
                          AND requirement.task_id=step.task_id
                          AND requirement.plan_revision=step.plan_revision
                          AND requirement.step_id=step.step_id
                         WHERE step.workspace_scope=?1 AND step.task_id=?2
                           AND step.plan_revision=?3
                           AND requirement.requirement_id IN (SELECT value FROM json_each(?4))
                         ORDER BY requirement.position, step.position LIMIT 1",
                        params![
                            workspace_scope,
                            task_id,
                            plan_revision,
                            serde_json::to_string(&missing_requirement_ids)
                                .map_err(|error| format!("无法序列化恢复要求：{error}"))?,
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()
                    .map_err(|error| format!("无法读取契约恢复步骤：{error}"))?
            } else {
                None
            };
            let resume_step = contract_resume_step.or(legacy_resume_step);
            let resume_checkpoint_id = transaction
                .query_row(
                    "SELECT checkpoint_id FROM runtime_task_checkpoints
                     WHERE workspace_scope=?1 AND task_id=?2 AND state IN ('running', 'completed')
                     ORDER BY sequence DESC, updated_at DESC LIMIT 1",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法读取任务恢复检查点：{error}"))?;
            let attachment_count = payload
                .get("attachmentIds")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let intent = payload
                .get("intent")
                .and_then(Value::as_str)
                .unwrap_or("general");
            let approval = payload
                .get("approval")
                .and_then(Value::as_str)
                .unwrap_or("none");
            let mut evidence = Vec::new();
            if completed_write_events > 0 {
                evidence.push(format!("{completed_write_events} 条原生 Vault 提交事件"));
            }
            if committed_content > 0 {
                evidence.push(format!("{committed_content} 条已提交内容记录"));
            }
            if let Some(status) = contract_completion
                .as_ref()
                .filter(|status| status.satisfied)
            {
                evidence.push(format!(
                    "完成契约 v{} 已由不可变证据满足",
                    status.plan_revision
                ));
            }
            let recommendation = if contract_completion
                .as_ref()
                .is_some_and(|status| status.satisfied)
                || (contract_completion.is_none() && !evidence.is_empty())
            {
                "completed"
            } else if attachment_count > 0 {
                "needs_input"
            } else if intent == "delete" || approval != "none" {
                "manual"
            } else {
                "resume"
            };
            let detail = match recommendation {
                "completed" => "检测到真实副作用已提交，不应重复执行".to_string(),
                "needs_input" => "任务依赖进程内附件，应用重启后需要用户重新提供".to_string(),
                "manual" => "破坏性或外部操作必须重新经过当前用户决策".to_string(),
                _ if !missing_requirement_ids.is_empty() => format!(
                    "完成契约尚缺少 {}，可从首个未满足要求对应步骤恢复",
                    missing_requirement_ids.join("、")
                ),
                _ => "未发现已提交副作用，可从首个未完成步骤重新执行".to_string(),
            };
            transaction
                .execute(
                    "INSERT INTO runtime_task_recoveries
                     (workspace_scope, task_id, interrupted_task_updated_at, recommendation,
                      resume_step_id, resume_step_index, resume_checkpoint_id, evidence_json,
                      plan_revision, completion_satisfied, missing_requirement_ids_json,
                      detail, state, detected_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'pending', ?13, ?13)
                     ON CONFLICT(workspace_scope, task_id) DO UPDATE SET
                       interrupted_task_updated_at=excluded.interrupted_task_updated_at,
                       recommendation=excluded.recommendation,
                       resume_step_id=excluded.resume_step_id,
                       resume_step_index=excluded.resume_step_index,
                       resume_checkpoint_id=excluded.resume_checkpoint_id,
                       evidence_json=excluded.evidence_json, detail=excluded.detail,
                       plan_revision=excluded.plan_revision,
                       completion_satisfied=excluded.completion_satisfied,
                       missing_requirement_ids_json=excluded.missing_requirement_ids_json,
                       state='pending', detected_at=excluded.detected_at, updated_at=excluded.updated_at,
                       resolution=NULL, resolved_at=NULL",
                    params![
                        workspace_scope,
                        task_id,
                        task_updated_at,
                        recommendation,
                        resume_step.as_ref().map(|item| item.0.as_str()),
                        resume_step.as_ref().map(|item| item.1),
                        resume_checkpoint_id,
                        serde_json::to_string(&evidence)
                            .map_err(|error| format!("无法序列化任务恢复证据：{error}"))?,
                        plan_revision.map(|value| value as i64),
                        contract_completion
                            .as_ref()
                            .map(|status| i64::from(status.satisfied)),
                        serde_json::to_string(&missing_requirement_ids)
                            .map_err(|error| format!("无法序列化恢复要求：{error}"))?,
                        detail,
                        detected_at,
                    ],
                )
                .map_err(|error| format!("无法登记任务恢复建议：{error}"))?;
        }
        let recoveries = {
            let mut statement = transaction
                .prepare(
                    "SELECT task_id, recommendation, resume_step_id, resume_step_index,
                            resume_checkpoint_id, evidence_json, plan_revision,
                            completion_satisfied, missing_requirement_ids_json,
                            replacement_key, replacement_task_id, detail, detected_at
                     FROM runtime_task_recoveries
                     WHERE workspace_scope=?1 AND state='pending' ORDER BY detected_at",
                )
                .map_err(|error| format!("无法读取待恢复任务：{error}"))?;
            let rows = statement
                .query_map([workspace_scope], |row| {
                    let evidence_json: String = row.get(5)?;
                    let missing_requirement_ids_json: String = row.get(8)?;
                    Ok(RuntimeTaskRecovery {
                        task_id: row.get(0)?,
                        recommendation: row.get(1)?,
                        resume_step_id: row.get(2)?,
                        resume_step_index: row.get(3)?,
                        resume_checkpoint_id: row.get(4)?,
                        evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
                        plan_revision: row
                            .get::<_, Option<i64>>(6)?
                            .and_then(|value| u64::try_from(value).ok()),
                        completion_satisfied: row.get::<_, Option<i64>>(7)?.map(|value| value != 0),
                        missing_requirement_ids: serde_json::from_str(
                            &missing_requirement_ids_json,
                        )
                        .unwrap_or_default(),
                        replacement_key: row.get(9)?,
                        replacement_task_id: row.get(10)?,
                        detail: row.get(11)?,
                        detected_at: row.get(12)?,
                    })
                })
                .map_err(|error| format!("无法枚举待恢复任务：{error}"))?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            rows
        };
        transaction
            .commit()
            .map_err(|error| format!("无法提交任务恢复检查：{error}"))?;
        Ok(recoveries)
    }

    pub(crate) fn resolve_runtime_task_recovery(
        &self,
        workspace_scope: &str,
        task_id: &str,
        resolution: &str,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("resolve_runtime_task_recovery")
            .with_threshold(self.config.slow_query_threshold_ms);

        if !matches!(
            resolution,
            "completed" | "resumed" | "needs_input" | "manual" | "failed"
        ) {
            return Err("任务恢复结果无效".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE runtime_task_recoveries
                 SET state='resolved', resolution=?3, resolved_at=?4, updated_at=?4
                 WHERE workspace_scope=?1 AND task_id=?2",
                params![
                    workspace_scope,
                    task_id,
                    resolution,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|error| format!("无法完成任务恢复登记：{error}"))?;
        Ok(())
    }

    pub(crate) fn supersede_runtime_task_for_recovery(
        &self,
        workspace_scope: &str,
        interrupted_task_id: &str,
        replacement_key: &str,
    ) -> Result<RuntimeTaskRecoveryReplacement, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("supersede_runtime_task_for_recovery")
            .with_threshold(self.config.slow_query_threshold_ms);

        let interrupted_task_id = interrupted_task_id.trim();
        let replacement_key = replacement_key.trim();
        if !valid_runtime_identifier(interrupted_task_id, 180)
            || !valid_runtime_identifier(replacement_key, 180)
        {
            return Err("任务恢复替换绑定无效".to_string());
        }
        let current = self.runtime_task(workspace_scope, interrupted_task_id)?;
        if matches!(current.state.as_str(), "succeeded" | "failed") {
            return Err(format!("终态任务 {} 不能创建恢复替换", current.state));
        }
        {
            let mut connection = self
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("无法开始恢复替换登记事务：{error}"))?;
            let recovery = transaction
                .query_row(
                    "SELECT state, resolution, replacement_key, recommendation
                     FROM runtime_task_recoveries
                     WHERE workspace_scope=?1 AND task_id=?2",
                    params![workspace_scope, interrupted_task_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("无法读取待替换恢复记录：{error}"))?
                .ok_or_else(|| "待替换任务没有恢复记录".to_string())?;
            if recovery.3 != "resume" {
                return Err("只有明确建议 resume 的恢复记录可以创建 replacement".to_string());
            }
            if recovery.0 == "resolved"
                && !matches!(
                    recovery.1.as_deref(),
                    Some("superseded" | "replaced" | "resumed")
                )
            {
                return Err("任务恢复记录已经按其他结果结算".to_string());
            }
            if recovery
                .2
                .as_deref()
                .is_some_and(|stored| stored != replacement_key)
            {
                return Err("任务恢复记录已经绑定其他 replacement key".to_string());
            }
            let changed = transaction
                .execute(
                    "UPDATE runtime_task_recoveries
                     SET replacement_key=COALESCE(replacement_key, ?3), updated_at=?4
                     WHERE workspace_scope=?1 AND task_id=?2
                       AND (replacement_key IS NULL OR replacement_key=?3)",
                    params![
                        workspace_scope,
                        interrupted_task_id,
                        replacement_key,
                        Utc::now().to_rfc3339(),
                    ],
                )
                .map_err(|error| format!("无法登记恢复 replacement key：{error}"))?;
            if changed != 1 {
                return Err("恢复 replacement key 已被并发修改".to_string());
            }
            transaction
                .commit()
                .map_err(|error| format!("无法提交恢复替换登记：{error}"))?;
        }
        let current = self.runtime_task(workspace_scope, interrupted_task_id)?;
        if current.state != "cancelled" {
            self.transition_native_runtime_task(
                workspace_scope,
                interrupted_task_id,
                "cancelled",
                current.progress,
                "恢复替换已封锁旧任务；后续执行必须使用 replacement 任务",
                Some(&serde_json::json!({
                    "id": format!("recovery-supersede-{replacement_key}"),
                    "replacementKey": replacement_key,
                    "supersededAt": Utc::now().to_rfc3339(),
                })),
            )?;
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始恢复替换封锁事务：{error}"))?;
        let state = transaction
            .query_row(
                "SELECT state FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, interrupted_task_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("无法验证旧恢复任务终态：{error}"))?;
        if state != "cancelled" {
            return Err("旧恢复任务尚未取消，拒绝创建 replacement".to_string());
        }
        let changed = transaction
            .execute(
                "UPDATE runtime_task_recoveries
                 SET state='resolved', resolution='superseded', resolved_at=COALESCE(resolved_at, ?4),
                     updated_at=?4
                 WHERE workspace_scope=?1 AND task_id=?2 AND replacement_key=?3",
                params![workspace_scope, interrupted_task_id, replacement_key, now],
            )
            .map_err(|error| format!("无法结算旧任务恢复记录：{error}"))?;
        if changed != 1 {
            return Err("旧任务已取消，但恢复替换关系没有持久化".to_string());
        }
        let replacement_task_id = transaction
            .query_row(
                "SELECT replacement_task_id FROM runtime_task_recoveries
                 WHERE workspace_scope=?1 AND task_id=?2",
                params![workspace_scope, interrupted_task_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|error| format!("无法读取恢复 replacement：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交恢复替换封锁：{error}"))?;
        Ok(RuntimeTaskRecoveryReplacement {
            interrupted_task_id: interrupted_task_id.to_string(),
            replacement_key: replacement_key.to_string(),
            state: if replacement_task_id.is_some() {
                "bound".to_string()
            } else {
                "superseded".to_string()
            },
            replacement_task_id,
            updated_at: now,
        })
    }

    pub(crate) fn bind_runtime_task_recovery_replacement(
        &self,
        workspace_scope: &str,
        interrupted_task_id: &str,
        replacement_task_id: &str,
        replacement_key: &str,
    ) -> Result<RuntimeTaskRecoveryReplacement, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("bind_runtime_task_recovery_replacement")
            .with_threshold(self.config.slow_query_threshold_ms);

        let interrupted_task_id = interrupted_task_id.trim();
        let replacement_task_id = replacement_task_id.trim();
        let replacement_key = replacement_key.trim();
        if !valid_runtime_identifier(interrupted_task_id, 180)
            || !valid_runtime_identifier(replacement_task_id, 180)
            || !valid_runtime_identifier(replacement_key, 180)
            || interrupted_task_id == replacement_task_id
        {
            return Err("任务恢复 replacement 关系无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 replacement 绑定事务：{error}"))?;
        let interrupted_state = transaction
            .query_row(
                "SELECT state FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, interrupted_task_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("无法读取被替换任务：{error}"))?;
        if interrupted_state != "cancelled" {
            return Err("旧任务未取消，拒绝绑定 replacement".to_string());
        }
        let replacement_state = transaction
            .query_row(
                "SELECT state FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, replacement_task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 replacement 任务：{error}"))?
            .ok_or_else(|| "replacement 任务不存在".to_string())?;
        if matches!(
            replacement_state.as_str(),
            "succeeded" | "failed" | "cancelled"
        ) {
            return Err("replacement 任务已经进入终态".to_string());
        }
        let now = Utc::now().to_rfc3339();
        let changed = transaction
            .execute(
                "UPDATE runtime_task_recoveries
                 SET replacement_task_id=COALESCE(replacement_task_id, ?4),
                     resolution='replaced', updated_at=?5
                 WHERE workspace_scope=?1 AND task_id=?2 AND state='resolved'
                   AND replacement_key=?3
                   AND resolution IN ('superseded', 'replaced', 'resumed')
                   AND (replacement_task_id IS NULL OR replacement_task_id=?4)",
                params![
                    workspace_scope,
                    interrupted_task_id,
                    replacement_key,
                    replacement_task_id,
                    now,
                ],
            )
            .map_err(|error| format!("无法绑定恢复 replacement 任务：{error}"))?;
        if changed != 1 {
            return Err("恢复 replacement key 不匹配或已经绑定其他任务".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 replacement 绑定：{error}"))?;
        Ok(RuntimeTaskRecoveryReplacement {
            interrupted_task_id: interrupted_task_id.to_string(),
            replacement_key: replacement_key.to_string(),
            replacement_task_id: Some(replacement_task_id.to_string()),
            state: "bound".to_string(),
            updated_at: now,
        })
    }

    pub(crate) fn upsert_inbound_content_record(
        &self,
        workspace_scope: &str,
        record: &InboundContentRecordInput,
    ) -> Result<InboundContentRecordReceipt, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("upsert_inbound_content_record")
            .with_threshold(self.config.slow_query_threshold_ms);

        validate_inbound_content_record(record)?;
        let extraction_json = serialize_inbound_record_section(&record.extraction, "提取诊断")?;
        let analysis_json = serialize_inbound_record_section(&record.analysis, "模型分析")?;
        let quality_json = serialize_inbound_record_section(&record.quality, "质量门禁")?;
        let target_json = serialize_inbound_record_section(&record.target, "写入目标")?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始内容处理记录事务：{error}"))?;
        let existing = transaction
            .query_row(
                "SELECT state, content_hash, source_type FROM inbound_content_records
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, record.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取内容处理记录：{error}"))?;
        let duplicate_of = if existing.is_none()
            && matches!(
                record.state.as_str(),
                "extracted" | "analyzing" | "analysis_pending" | "ready_to_write" | "writing"
            ) {
            transaction
                .query_row(
                    "SELECT id FROM inbound_content_records
                     WHERE workspace_scope=?1 AND id<>?2 AND source_type=?3 AND content_hash=?4
                       AND state IN ('ready_to_write', 'writing', 'committed')
                     ORDER BY CASE state WHEN 'committed' THEN 0 WHEN 'writing' THEN 1 ELSE 2 END,
                              updated_at DESC
                     LIMIT 1",
                    params![
                        workspace_scope,
                        record.id,
                        record.source_type,
                        record.content_hash
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法检查跨任务重复内容：{error}"))?
        } else {
            None
        };
        if let Some((previous_state, previous_hash, previous_source_type)) = existing.as_ref() {
            if previous_hash != &record.content_hash || previous_source_type != &record.source_type
            {
                return Err("同一内容记录 ID 不能更换来源类型或正文哈希".to_string());
            }
            if !inbound_content_transition_allowed(previous_state, &record.state) {
                return Err(format!(
                    "内容处理状态不能从 {previous_state} 迁移到 {}",
                    record.state
                ));
            }
        }
        let previous_state = existing.map(|(state, _, _)| state);
        let stored_state = if duplicate_of.is_some() {
            "quality_rejected"
        } else {
            record.state.as_str()
        };
        let now = Utc::now().to_rfc3339();
        let committed_at = (stored_state == "committed").then_some(now.as_str());
        let duplicate_failure = duplicate_of
            .as_deref()
            .map(|id| format!("内容哈希与已有记录 {id} 完全相同，已阻止重复写入"));
        let failure_reason = duplicate_failure
            .as_deref()
            .or(record.failure_reason.as_deref());
        transaction
            .execute(
                "INSERT INTO inbound_content_records
                 (workspace_scope, id, task_id, state, source_type, source_ref, title, content_hash,
                  content_characters, attachment_count, image_count, extraction_json,
                  analysis_json, quality_json, target_json, failure_reason, created_at, updated_at, committed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17, ?18)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   task_id=excluded.task_id, state=excluded.state, source_ref=excluded.source_ref,
                   title=excluded.title, content_characters=excluded.content_characters,
                   attachment_count=excluded.attachment_count, image_count=excluded.image_count,
                   extraction_json=excluded.extraction_json, analysis_json=excluded.analysis_json,
                   quality_json=excluded.quality_json, target_json=excluded.target_json,
                   failure_reason=excluded.failure_reason, updated_at=excluded.updated_at,
                   committed_at=COALESCE(excluded.committed_at, inbound_content_records.committed_at)",
                params![
                    workspace_scope,
                    record.id,
                    record.task_id,
                    stored_state,
                    record.source_type,
                    record.source_ref,
                    record.title,
                    record.content_hash,
                    record.content_characters as i64,
                    record.attachment_count as i64,
                    record.image_count as i64,
                    extraction_json,
                    analysis_json,
                    quality_json,
                    target_json,
                    failure_reason,
                    now,
                    committed_at,
                ],
            )
            .map_err(|error| format!("无法保存内容处理记录：{error}"))?;
        if previous_state.as_deref() != Some(stored_state) {
            transaction
                .execute(
                    "INSERT INTO inbound_content_transitions
                     (id, workspace_scope, content_id, from_state, to_state, detail, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        record.id,
                        previous_state,
                        stored_state,
                        failure_reason
                            .unwrap_or("")
                            .chars()
                            .take(1000)
                            .collect::<String>(),
                        now,
                    ],
                )
                .map_err(|error| format!("无法记录内容处理状态迁移：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交内容处理记录：{error}"))?;
        Ok(InboundContentRecordReceipt {
            id: record.id.clone(),
            state: stored_state.to_string(),
            previous_state,
            duplicate_of,
            updated_at: now,
        })
    }

    pub fn should_initialize_default_vaults(&self, workspace_scope: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let preference = connection
            .query_row(
                "SELECT defaults_initialized, explicit_vault_id FROM vault_preferences WHERE workspace_scope=?1",
                [workspace_scope],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区 Vault 初始化状态：{error}"))?;
        if let Some((initialized, explicit_vault_id)) = preference {
            return Ok(initialized == 0 && explicit_vault_id.is_none());
        }

        let legacy_selection = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区 Vault 选择：{error}"))?
            .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
            .and_then(|payload| {
                payload
                    .get("clientState")
                    .and_then(|state| state.get("currentVaultId"))
                    .and_then(Value::as_str)
                    .filter(|vault_id| !vault_id.is_empty() && *vault_id != "all")
                    .map(str::to_string)
            });
        if let Some(vault_id) = legacy_selection {
            connection
                .execute(
                    "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                     VALUES (?1, 0, ?2, ?3)",
                    params![workspace_scope, vault_id, Utc::now().to_rfc3339()],
                )
                .map_err(|error| format!("无法迁移本地工作区 Vault 选择：{error}"))?;
            return Ok(false);
        }
        Ok(true)
    }

    pub fn mark_default_vaults_initialized(&self, workspace_scope: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                 VALUES (?1, 1, NULL, ?2)
                 ON CONFLICT(workspace_scope) DO UPDATE SET defaults_initialized=1, updated_at=excluded.updated_at",
                params![workspace_scope, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法保存本地工作区 Vault 初始化状态：{error}"))?;
        Ok(())
    }

    pub fn save_explicit_vault_selection(
        &self,
        workspace_scope: &str,
        vault_id: Option<&str>,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                 VALUES (?1, 0, ?2, ?3)
                 ON CONFLICT(workspace_scope) DO UPDATE SET explicit_vault_id=excluded.explicit_vault_id, updated_at=excluded.updated_at",
                params![workspace_scope, vault_id, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法保存本地工作区 Vault 选择：{error}"))?;
        Ok(())
    }

    pub fn load_legacy_model_profiles(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<LegacyModelProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT role, provider, base_url, selected_model, available_models_json, api_key_ciphertext
                 FROM legacy_model_profiles WHERE workspace_scope=?1
                 ORDER BY CASE role WHEN 'chat' THEN 0 WHEN 'analysis' THEN 1 WHEN 'image' THEN 2 ELSE 3 END",
            )
            .map_err(|error| format!("无法准备本地模型配置查询：{error}"))?;
        let rows = statement
            .query_map([workspace_scope], |row| {
                let models: String = row.get(4)?;
                Ok(LegacyModelProfile {
                    role: row.get(0)?,
                    provider: row.get(1)?,
                    base_url: row.get(2)?,
                    selected_model: row.get(3)?,
                    available_models: serde_json::from_str(&models)
                        .unwrap_or_else(|_| Value::Array(Vec::new())),
                    api_key_ciphertext: row.get(5)?,
                })
            })
            .map_err(|error| format!("无法读取本地模型配置：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析本地模型配置：{error}"))
    }

    pub fn clear_legacy_model_profiles(&self, workspace_scope: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "DELETE FROM legacy_model_profiles WHERE workspace_scope=?1",
                [workspace_scope],
            )
            .map_err(|error| format!("无法清除旧版模型配置：{error}"))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_model_provider_record(
        &self,
        workspace_scope: &str,
        id: &str,
        name: &str,
        provider: &str,
        base_url: &str,
        available_models: &Value,
        assignments: &Value,
        defaults: &Value,
        api_key_ciphertext: &[u8],
    ) -> Result<(), String> {
        let available_models_json = serde_json::to_string(available_models)
            .map_err(|error| format!("无法序列化供应商模型列表：{error}"))?;
        let assignments_json = serde_json::to_string(assignments)
            .map_err(|error| format!("无法序列化模型用途：{error}"))?;
        let defaults_json = serde_json::to_string(defaults)
            .map_err(|error| format!("无法序列化默认模型：{error}"))?;
        if available_models_json.len() > MAX_RECORD_BYTES
            || assignments_json.len() > MAX_RECORD_BYTES
            || defaults_json.len() > MAX_RECORD_BYTES
        {
            return Err("供应商模型配置超过 2 MB 安全上限".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let now = Utc::now().to_rfc3339();
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始模型供应商事务：{error}"))?;
        for role in ["chat", "analysis", "image", "embedding"] {
            if defaults
                .get(role)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
            {
                transaction
                    .execute(
                        "UPDATE model_providers
                         SET defaults_json=json_remove(defaults_json, ?3), updated_at=?4
                         WHERE workspace_scope=?1 AND id<>?2",
                        params![workspace_scope, id, format!("$.{role}"), now],
                    )
                    .map_err(|error| format!("无法更新默认模型唯一性：{error}"))?;
            }
        }
        transaction
            .execute(
                "INSERT INTO model_providers
                 (workspace_scope, id, name, provider, base_url, available_models_json,
                  assignments_json, defaults_json, api_key_ciphertext, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   name=excluded.name,
                   provider=excluded.provider,
                   base_url=excluded.base_url,
                   available_models_json=excluded.available_models_json,
                   assignments_json=excluded.assignments_json,
                   defaults_json=excluded.defaults_json,
                   api_key_ciphertext=excluded.api_key_ciphertext,
                   updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    id,
                    name,
                    provider,
                    base_url,
                    available_models_json,
                    assignments_json,
                    defaults_json,
                    api_key_ciphertext,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存模型供应商：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交模型供应商配置：{error}"))
    }

    pub fn load_model_provider(
        &self,
        workspace_scope: &str,
        id: &str,
    ) -> Result<Option<ModelProviderProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT id, name, provider, base_url, available_models_json,
                        assignments_json, defaults_json, api_key_ciphertext
                 FROM model_providers WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |row| {
                    let available_models: String = row.get(4)?;
                    let assignments: String = row.get(5)?;
                    let defaults: String = row.get(6)?;
                    Ok(ModelProviderProfile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        provider: row.get(2)?,
                        base_url: row.get(3)?,
                        available_models: serde_json::from_str(&available_models)
                            .unwrap_or_else(|_| Value::Array(Vec::new())),
                        assignments: serde_json::from_str(&assignments)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        defaults: serde_json::from_str(&defaults)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        api_key_ciphertext: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("无法读取模型供应商：{error}"))
    }

    pub fn load_model_providers(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<ModelProviderProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, provider, base_url, available_models_json,
                        assignments_json, defaults_json, api_key_ciphertext
                 FROM model_providers WHERE workspace_scope=?1 ORDER BY created_at, name, id",
            )
            .map_err(|error| format!("无法准备模型供应商查询：{error}"))?;
        let rows = statement
            .query_map([workspace_scope], |row| {
                let available_models: String = row.get(4)?;
                let assignments: String = row.get(5)?;
                let defaults: String = row.get(6)?;
                Ok(ModelProviderProfile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider: row.get(2)?,
                    base_url: row.get(3)?,
                    available_models: serde_json::from_str(&available_models)
                        .unwrap_or_else(|_| Value::Array(Vec::new())),
                    assignments: serde_json::from_str(&assignments)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    defaults: serde_json::from_str(&defaults)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    api_key_ciphertext: row.get(7)?,
                })
            })
            .map_err(|error| format!("无法读取模型供应商：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析模型供应商：{error}"))
    }

    pub fn delete_model_provider_record(
        &self,
        workspace_scope: &str,
        id: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "DELETE FROM model_providers WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
            )
            .map_err(|error| format!("无法删除模型供应商：{error}"))?;
        Ok(())
    }

    /// Reads or creates the per-device key used to encrypt local API keys.
    /// The key never enters SQLite, logs, Obsidian, or the frontend.
    pub fn device_encryption_key(&self) -> Result<[u8; 32], String> {
        let key_path = self.path.with_file_name("yunspire.sqlite.key");
        let read_key = || -> Result<[u8; 32], String> {
            let bytes = fs::read(&key_path)
                .map_err(|error| format!("无法读取云枢本机设备密钥：{error}"))?;
            if bytes.len() != 32 {
                return Err("云枢本机设备密钥长度无效".to_string());
            }
            #[cfg(unix)]
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("无法收紧本机设备密钥权限：{error}"))?;
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Ok(key)
        };

        if key_path.exists() {
            return read_key();
        }

        let mut key = [0u8; 32];
        key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&key_path) {
            Ok(mut file) => {
                file.write_all(&key)
                    .map_err(|error| format!("无法写入云枢本机设备密钥：{error}"))?;
                file.sync_all()
                    .map_err(|error| format!("无法持久化云枢本机设备密钥：{error}"))?;
                Ok(key)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => read_key(),
            Err(error) => Err(format!("无法创建云枢本机设备密钥：{error}")),
        }
    }

    pub fn append_operation_event(&self, event: &OperationEvent) -> Result<(), String> {
        let workspace_scope = self.local_workspace_scope()?;
        let mut event = event.clone();
        let trace_id = event
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        event.trace_id = Some(trace_id.clone());
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始操作事件事务：{error}"))?;
        let payload = serde_json::to_string(&event)
            .map_err(|error| format!("无法序列化原生操作事件：{error}"))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO operation_events
                 (id, task_id, trace_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    event.task_id,
                    trace_id,
                    event.event_type,
                    event.state,
                    payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法写入 SQLite 操作日志：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            &workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "operation_event",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "taskId": event.task_id,
                    "vaultId": event.vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
        if let Some(vault_id) = event.vault_id.as_deref() {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                &workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id: &trace_id,
                    entity_kind: "vault_operation",
                    entity_id: &event.id,
                    event_type: &event.event_type,
                    state: &event.state,
                    payload: &serde_json::json!({
                        "vaultId": vault_id,
                        "relativePath": event.relative_path,
                        "detail": event.detail,
                    }),
                    created_at: &event.created_at,
                },
            )?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交操作事件与 Trace：{error}"))
    }

    pub(crate) fn persist_application_command(
        &self,
        workspace_scope: &str,
        command: &ApplicationCommand,
        decision: &PolicyDecision,
        trace_id: &str,
        accepted_at: &str,
    ) -> Result<(Option<String>, bool), String> {
        crate::trace::validate_trace_id(trace_id)?;
        let command_payload = serde_json::to_string(command)
            .map_err(|error| format!("无法序列化应用命令：{error}"))?;
        let decision_payload = serde_json::to_string(decision)
            .map_err(|error| format!("无法序列化策略决定：{error}"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始应用命令事务：{error}"))?;
        let duplicate = transaction
            .query_row(
                "SELECT task_id, payload, trace_id FROM application_commands
                 WHERE workspace_scope=?1 AND idempotency_key=?2",
                params![workspace_scope, command.idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法检查应用命令幂等键：{error}"))?;
        if let Some((task_id, stored_payload, stored_trace_id)) = duplicate {
            let stored_command = serde_json::from_str::<ApplicationCommand>(&stored_payload)
                .map_err(|error| format!("无法解析幂等应用命令：{error}"))?;
            if crate::policy::command_authorization_binding(&stored_command)
                != crate::policy::command_authorization_binding(command)
            {
                return Err("应用命令幂等键已经绑定到不同的能力或参数范围".to_string());
            }
            if stored_trace_id != trace_id {
                return Err("应用命令幂等键已经绑定到其他 Trace".to_string());
            }
            transaction
                .commit()
                .map_err(|error| format!("无法完成应用命令幂等查询：{error}"))?;
            return Ok((task_id, true));
        }

        let (command_state, task_state) = match decision.outcome {
            PolicyOutcome::Deny => ("denied", None),
            PolicyOutcome::RequireApproval => ("accepted", Some("awaiting_approval")),
            PolicyOutcome::Allow | PolicyOutcome::AllowWithReducedScope => {
                ("accepted", Some("queued"))
            }
        };
        let step_binding = if task_state.is_some() {
            let binding_checked_at = Utc::now().to_rfc3339();
            validate_runtime_task_step_command_binding_in_connection(
                &transaction,
                workspace_scope,
                command,
                trace_id,
                &binding_checked_at,
            )?
        } else {
            None
        };
        let task_id = task_state.map(|_| format!("task-{}", Uuid::new_v4()));
        transaction
            .execute(
                "INSERT INTO application_commands
                 (workspace_scope, id, idempotency_key, command_type, operation, state,
                  task_id, trace_id, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    workspace_scope,
                    command.id,
                    command.idempotency_key,
                    command.command_type,
                    command.operation,
                    command_state,
                    task_id,
                    trace_id,
                    command_payload,
                    accepted_at,
                ],
            )
            .map_err(|error| format!("无法保存应用命令：{error}"))?;
        transaction
            .execute(
                "INSERT INTO policy_decisions
                 (id, workspace_scope, command_id, outcome, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    command.id,
                    match decision.outcome {
                        PolicyOutcome::Allow => "allow",
                        PolicyOutcome::Deny => "deny",
                        PolicyOutcome::RequireApproval => "require_approval",
                        PolicyOutcome::AllowWithReducedScope => "allow_with_reduced_scope",
                    },
                    decision_payload,
                    accepted_at,
                ],
            )
            .map_err(|error| format!("无法保存策略决定：{error}"))?;
        if let (Some(task_id), Some(task_state)) = (task_id.as_ref(), task_state) {
            let title = command
                .parameters
                .get("title")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&command.intent)
                .chars()
                .take(240)
                .collect::<String>();
            let progress = if task_state == "awaiting_approval" {
                5
            } else {
                0
            };
            let task_payload = serde_json::json!({
                "id": task_id,
                "kind": match command.origin {
                    crate::policy::CommandOrigin::Schedule => "scheduled",
                    crate::policy::CommandOrigin::SystemMaintenance => "maintenance",
                    crate::policy::CommandOrigin::Evolution => "evolution",
                    crate::policy::CommandOrigin::Runtime => "runtime_child",
                    crate::policy::CommandOrigin::DirectUser | crate::policy::CommandOrigin::Assistant => "interactive",
                },
                "state": task_state,
                "title": title,
                "traceId": trace_id,
                "intent": command.intent,
                "capabilityIds": [command.capability_id.clone()],
                "operation": command.operation,
                "parameters": command.parameters,
                "vaultId": command.vault_id,
                "relativePaths": command.relative_paths,
                "networkTargets": command.network_targets,
                "declaredScope": command.declared_scope,
                "budget": command.budget,
                "idempotencyKey": command.idempotency_key,
                "commandId": command.id,
                "policyDecision": decision,
                "approval": decision.approval_type,
                "progress": progress,
                "steps": [],
                "checkpoints": [],
                "createdAt": accepted_at,
                "updatedAt": accepted_at,
            });
            let serialized = serde_json::to_string(&task_payload)
                .map_err(|error| format!("无法序列化原生任务：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_tasks
                     (workspace_scope, id, state, title, trace_id, payload, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![
                        workspace_scope,
                        task_id,
                        task_state,
                        title,
                        trace_id,
                        serialized,
                        accepted_at
                    ],
                )
                .map_err(|error| format!("无法创建原生任务：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_task_attempts
                     (id, workspace_scope, task_id, state, detail, started_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        task_id,
                        task_state,
                        "由类型化应用命令创建",
                        accepted_at,
                    ],
                )
                .map_err(|error| format!("无法记录原生任务首次尝试：{error}"))?;
            if let Some(plan) = command.runtime_plan.as_ref() {
                crate::task_runtime::validate_runtime_task_plan(plan)?;
                let plan_json = canonical_runtime_json_string(
                    &serde_json::to_value(plan)
                        .map_err(|error| format!("无法序列化应用命令原生任务计划：{error}"))?,
                    "应用命令原生任务计划",
                )?;
                if plan_json.len() > MAX_RUNTIME_PLAN_BYTES {
                    return Err("应用命令原生任务计划超过 256 KB 安全上限".to_string());
                }
                let content_hash = format!("sha256:{:x}", Sha256::digest(plan_json.as_bytes()));
                insert_runtime_task_plan_revision(
                    &transaction,
                    workspace_scope,
                    task_id,
                    1,
                    plan,
                    &plan_json,
                    &content_hash,
                    accepted_at,
                )?;
                ensure_runtime_task_execution_budget(
                    &transaction,
                    workspace_scope,
                    task_id,
                    1,
                    &task_payload,
                    Some(&command.budget),
                    accepted_at,
                )?;
            }
            if let Some(binding) = step_binding.as_ref() {
                transaction
                    .execute(
                        "INSERT INTO runtime_task_step_command_bindings
                         (workspace_scope, claim_id, command_id, child_task_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            workspace_scope,
                            binding.step_claim_id,
                            command.id,
                            task_id,
                            accepted_at,
                        ],
                    )
                    .map_err(|error| format!("无法绑定原生任务步骤与子命令：{error}"))?;
            }
        }
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.clone(),
            trace_id: Some(trace_id.to_string()),
            event_type: "command.policy_decided".to_string(),
            state: match decision.outcome {
                PolicyOutcome::Deny => "denied",
                PolicyOutcome::RequireApproval => "awaiting_approval",
                PolicyOutcome::Allow | PolicyOutcome::AllowWithReducedScope => "accepted",
            }
            .to_string(),
            created_at: accepted_at.to_string(),
            vault_id: command.vault_id.clone(),
            relative_path: command.relative_paths.first().cloned(),
            detail: format!("{}：{}", command.operation, decision.reason_codes.join(",")),
        };
        let event_payload = serde_json::to_string(&event)
            .map_err(|error| format!("无法序列化策略决定审计事件：{error}"))?;
        transaction
            .execute(
                "INSERT INTO operation_events
                 (id, task_id, trace_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    event.task_id,
                    trace_id,
                    event.event_type,
                    event.state,
                    event_payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法保存策略决定审计事件：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id,
                entity_kind: "application_command",
                entity_id: &command.id,
                event_type: "command.policy_decided",
                state: command_state,
                payload: &serde_json::json!({
                    "taskId": task_id,
                    "operation": command.operation,
                    "outcome": decision.outcome,
                }),
                created_at: accepted_at,
            },
        )?;
        if let (Some(task_id), Some(task_state)) = (task_id.as_deref(), task_state) {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id,
                    entity_kind: "runtime_task",
                    entity_id: task_id,
                    event_type: "task.created",
                    state: task_state,
                    payload: &serde_json::json!({"commandId": command.id}),
                    created_at: accepted_at,
                },
            )?;
        }
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id,
                entity_kind: "operation_event",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "taskId": event.task_id,
                    "vaultId": event.vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
        if let Some(vault_id) = event.vault_id.as_deref() {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id,
                    entity_kind: "vault_operation",
                    entity_id: &event.id,
                    event_type: &event.event_type,
                    state: &event.state,
                    payload: &serde_json::json!({
                        "vaultId": vault_id,
                        "relativePath": event.relative_path,
                        "detail": event.detail,
                    }),
                    created_at: &event.created_at,
                },
            )?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交应用命令事务：{error}"))?;
        Ok((task_id, false))
    }

    pub(crate) fn runtime_task(
        &self,
        workspace_scope: &str,
        task_id: &str,
    ) -> Result<NativeRuntimeTask, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("runtime_task")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        read_native_runtime_task(&connection, workspace_scope, task_id)
    }

    pub(crate) fn runtime_task_contract(
        &self,
        workspace_scope: &str,
        task_id: &str,
    ) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("runtime_task_contract")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        read_runtime_task_contract(&connection, workspace_scope, task_id)
    }

    pub(crate) fn runtime_schedule_dispatch_binding(
        &self,
        workspace_scope: &str,
        occurrence_id: &str,
        runtime_task_id: &str,
    ) -> Result<RuntimeScheduleDispatchBinding, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("runtime_schedule_dispatch_binding")
            .with_threshold(self.config.slow_query_threshold_ms);

        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let record = connection
            .query_row(
                "SELECT occurrence.schedule_id, occurrence.schedule_kind, occurrence.scheduled_for,
                        occurrence.schedule_revision, occurrence.runtime_task_id, task.payload
                 FROM runtime_schedule_occurrences occurrence
                 JOIN runtime_tasks task
                   ON task.workspace_scope=occurrence.workspace_scope
                  AND task.id=occurrence.runtime_task_id
                 WHERE occurrence.workspace_scope=?1 AND occurrence.occurrence_id=?2",
                params![workspace_scope, occurrence_id.trim()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取日程派发绑定：{error}"))?
            .ok_or_else(|| "未找到日程派发 occurrence".to_string())?;
        let schedule_revision = u64::try_from(record.3)
            .ok()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| "日程 occurrence revision 无效".to_string())?;
        let task_payload = serde_json::from_str::<Value>(&record.5)
            .map_err(|error| format!("日程 wrapper payload 无法解析：{error}"))?;
        let schedule_payload = task_payload
            .get("schedulePayload")
            .cloned()
            .ok_or_else(|| "日程 wrapper 缺少 schedulePayload".to_string())?;
        let schedule_payload_hash = task_payload
            .get("schedulePayloadHash")
            .and_then(Value::as_str)
            .ok_or_else(|| "日程 wrapper 缺少 schedulePayloadHash".to_string())?;
        let (_, schedule_payload_hash) = verified_schedule_payload_snapshot(
            schedule_payload,
            schedule_payload_hash,
            "日程 wrapper 快照",
        )?;
        if task_payload.get("scheduleId").and_then(Value::as_str) != Some(record.0.as_str())
            || task_payload.get("scheduleKind").and_then(Value::as_str) != Some(record.1.as_str())
            || task_payload
                .get("scheduleOccurrenceId")
                .and_then(Value::as_str)
                != Some(occurrence_id.trim())
            || task_payload.get("scheduledFor").and_then(Value::as_str) != Some(record.2.as_str())
            || task_payload.get("scheduleRevision").and_then(Value::as_u64)
                != Some(schedule_revision)
        {
            return Err("日程 wrapper 快照绑定与 occurrence 不一致".to_string());
        }
        if let Some((_, historical_hash)) = read_runtime_schedule_revision_snapshot(
            &connection,
            workspace_scope,
            &record.0,
            &record.1,
            record.3,
        )? {
            if historical_hash != schedule_payload_hash {
                return Err("日程 wrapper 快照与 occurrence 历史 revision 不一致".to_string());
            }
        }
        let binding = RuntimeScheduleDispatchBinding {
            schedule_id: record.0,
            schedule_kind: record.1,
            occurrence_id: occurrence_id.trim().to_string(),
            scheduled_for: record.2,
            schedule_revision,
            schedule_payload_hash,
            runtime_task_id: record.4,
        };
        if binding.runtime_task_id != runtime_task_id.trim() {
            return Err("日程 occurrence 与 wrapper 任务不匹配".to_string());
        }
        Ok(binding)
    }

    pub(crate) fn define_runtime_task_plan(
        &self,
        workspace_scope: &str,
        task_id: &str,
        plan: &RuntimeTaskPlanInput,
    ) -> Result<RuntimeTaskContractSnapshot, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("define_runtime_task_plan")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_task_plan(plan)?;
        if !valid_runtime_identifier(task_id, 180) {
            return Err("原生任务计划 taskId 无效".to_string());
        }
        let plan_value = serde_json::to_value(plan)
            .map_err(|error| format!("无法序列化原生任务计划：{error}"))?;
        let plan_json = canonical_runtime_json_string(&plan_value, "原生任务计划")?;
        if plan_json.len() > MAX_RUNTIME_PLAN_BYTES {
            return Err("原生任务计划超过 256 KB 安全上限".to_string());
        }
        let content_hash = format!("sha256:{:x}", Sha256::digest(plan_json.as_bytes()));
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始原生任务计划事务：{error}"))?;
        let current = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        let latest = transaction
            .query_row(
                "SELECT revision, content_hash FROM runtime_task_plans
                 WHERE workspace_scope=?1 AND task_id=?2 ORDER BY revision DESC LIMIT 1",
                params![workspace_scope, task_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务计划修订：{error}"))?;
        if latest
            .as_ref()
            .is_some_and(|(_, hash)| hash == &content_hash)
        {
            transaction
                .commit()
                .map_err(|error| format!("无法提交原生任务计划幂等查询：{error}"))?;
            drop(connection);
            return self
                .runtime_task_contract(workspace_scope, task_id)?
                .ok_or_else(|| "原生任务计划幂等查询未找到计划".to_string());
        }
        let execution_started = !matches!(
            current.state.as_str(),
            "created" | "queued" | "awaiting_approval"
        ) || transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM runtime_task_transitions
                   WHERE workspace_scope=?1 AND task_id=?2 AND to_state='running'
                 )",
                params![workspace_scope, task_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法检查原生任务计划锁：{error}"))?
            != 0;
        if execution_started {
            return Err("原生任务进入执行或终态后不能追加新的计划修订".to_string());
        }
        let revision = latest.map(|(revision, _)| revision + 1).unwrap_or(1);
        let now = Utc::now().to_rfc3339();
        insert_runtime_task_plan_revision(
            &transaction,
            workspace_scope,
            task_id,
            revision,
            plan,
            &plan_json,
            &content_hash,
            &now,
        )?;
        ensure_runtime_task_execution_budget(
            &transaction,
            workspace_scope,
            task_id,
            revision,
            &current.payload,
            None,
            &now,
        )?;
        let trace_id = current
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        if current.trace_id.as_deref() != Some(trace_id.as_str()) {
            transaction
                .execute(
                    "UPDATE runtime_tasks SET trace_id=?3
                     WHERE workspace_scope=?1 AND id=?2",
                    params![workspace_scope, task_id, trace_id],
                )
                .map_err(|error| format!("无法绑定原生任务计划 Trace：{error}"))?;
        }
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: task_id,
                event_type: "task.plan_defined",
                state: "defined",
                payload: &serde_json::json!({
                    "revision": revision,
                    "contentHash": content_hash,
                    "stepCount": plan.steps.len(),
                    "requirementCount": plan.completion_contract.requirements.len(),
                }),
                created_at: &now,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务计划事务：{error}"))?;
        drop(connection);
        self.runtime_task_contract(workspace_scope, task_id)?
            .ok_or_else(|| "原生任务计划提交后无法读取契约".to_string())
    }

    pub(crate) fn runtime_task_step_frontier(
        &self,
        workspace_scope: &str,
        task_id: &str,
        plan_revision: Option<u64>,
    ) -> Result<Vec<RuntimeTaskStepFrontierItem>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("runtime_task_step_frontier")
            .with_threshold(self.config.slow_query_threshold_ms);

        if !valid_runtime_identifier(task_id, 180) {
            return Err("原生任务步骤 frontier taskId 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始原生任务步骤 frontier 事务：{error}"))?;
        let current = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        let revision = latest_runtime_task_plan_revision(
            &transaction,
            workspace_scope,
            task_id,
            plan_revision,
        )?;
        ensure_runtime_task_execution_budget(
            &transaction,
            workspace_scope,
            task_id,
            revision,
            &current.payload,
            None,
            &current.updated_at,
        )?;
        let now = Utc::now().to_rfc3339();
        expire_runtime_task_step_claims(&transaction, workspace_scope, task_id, revision, &now)?;
        let records =
            load_runtime_task_plan_step_records(&transaction, workspace_scope, task_id, revision)?;
        let states =
            latest_runtime_task_step_states(&transaction, workspace_scope, task_id, revision)?;
        let mut frontier = Vec::new();
        for step in records {
            let completed = states
                .get(&step.step_id)
                .is_some_and(|(state, _)| state == "succeeded");
            if completed {
                continue;
            }
            let active = states
                .get(&step.step_id)
                .is_some_and(|(state, _)| state == "claimed");
            let dependencies_satisfied = step.depends_on.iter().all(|dependency| {
                states
                    .get(dependency)
                    .is_some_and(|(state, _)| state == "succeeded")
            });
            if dependencies_satisfied {
                frontier.push(RuntimeTaskStepFrontierItem {
                    runtime_task_id: task_id.to_string(),
                    plan_revision: u64::try_from(revision).unwrap_or_default(),
                    step_id: step.step_id,
                    step_kind: step.step_kind,
                    title: step.title,
                    depends_on: step.depends_on,
                    parameters: step.parameters,
                    effect_class: step.effect_class,
                    ready: !active && !matches!(current.state.as_str(), "cancelled" | "succeeded"),
                    active,
                });
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务步骤 frontier：{error}"))?;
        Ok(frontier)
    }

    pub(crate) fn claim_runtime_task_plan_steps(
        &self,
        workspace_scope: &str,
        input: &RuntimeTaskStepClaimInput,
    ) -> Result<RuntimeTaskStepClaimBatch, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("claim_runtime_task_plan_steps")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_task_step_claim(input)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始原生任务步骤领取事务：{error}"))?;
        let current = read_native_runtime_task(&transaction, workspace_scope, &input.task_id)?;
        let revision = latest_runtime_task_plan_revision(
            &transaction,
            workspace_scope,
            &input.task_id,
            input.plan_revision,
        )?;
        ensure_runtime_task_execution_budget(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
            &current.payload,
            None,
            &current.updated_at,
        )?;
        ensure_runtime_task_running_for_step_claim(
            &transaction,
            workspace_scope,
            &input.task_id,
            &Utc::now().to_rfc3339(),
        )?;
        let now = Utc::now().to_rfc3339();
        expire_runtime_task_step_claims(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
            &now,
        )?;
        let budget = read_runtime_task_execution_budget(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
        )?;
        if budget.cancelled_at.is_some() {
            return Err("父任务已取消，不能领取任务步骤".to_string());
        }
        if budget.max_tokens.is_some() && input.reservation.max_tokens.is_none() {
            return Err("任务 Token 预算要求步骤领取显式预留额度".to_string());
        }
        if budget.max_cost.is_some() && input.reservation.max_cost.is_none() {
            return Err("任务成本预算要求步骤领取显式预留额度".to_string());
        }
        let records = load_runtime_task_plan_step_records(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
        )?;
        let states = latest_runtime_task_step_states(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
        )?;
        let active_effectful = states.values().any(|(state, effect)| {
            state == "claimed" && *effect == RuntimeTaskStepEffectClass::Effectful
        });
        let active_read_only = states.values().any(|(state, effect)| {
            state == "claimed" && *effect == RuntimeTaskStepEffectClass::ReadOnly
        });
        let candidates = records
            .into_iter()
            .filter(|step| {
                let completed = states
                    .get(&step.step_id)
                    .is_some_and(|(state, _)| state == "succeeded");
                let active = states
                    .get(&step.step_id)
                    .is_some_and(|(state, _)| state == "claimed");
                !completed
                    && !active
                    && step.depends_on.iter().all(|dependency| {
                        states
                            .get(dependency)
                            .is_some_and(|(state, _)| state == "succeeded")
                    })
            })
            .collect::<Vec<_>>();
        let candidates = if active_effectful {
            Vec::new()
        } else if active_read_only {
            candidates
                .into_iter()
                .filter(|step| step.effect_class == RuntimeTaskStepEffectClass::ReadOnly)
                .take(input.max_claims)
                .collect::<Vec<_>>()
        } else if candidates
            .first()
            .is_some_and(|step| step.effect_class == RuntimeTaskStepEffectClass::Effectful)
        {
            candidates.into_iter().take(1).collect::<Vec<_>>()
        } else {
            candidates
                .into_iter()
                .filter(|step| step.effect_class == RuntimeTaskStepEffectClass::ReadOnly)
                .take(input.max_claims)
                .collect::<Vec<_>>()
        };
        let claim_count = candidates.len() as u64;
        if claim_count == 0 {
            let budget = read_runtime_task_execution_budget(
                &transaction,
                workspace_scope,
                &input.task_id,
                revision,
            )?;
            transaction
                .commit()
                .map_err(|error| format!("无法提交空原生任务步骤领取：{error}"))?;
            return Ok(RuntimeTaskStepClaimBatch {
                claims: Vec::new(),
                budget,
            });
        }
        let requested_tool_calls = input.reservation.max_tool_calls;
        let requested_runtime_seconds = input.reservation.max_runtime_seconds;
        let requested_tokens = input.reservation.max_tokens.unwrap_or(0);
        let requested_cost = input.reservation.max_cost.unwrap_or(0.0);
        let available_steps = budget
            .max_steps
            .saturating_sub(budget.consumed_steps + budget.reserved_steps);
        let available_tool_calls = budget
            .max_tool_calls
            .saturating_sub(budget.consumed_tool_calls + budget.reserved_tool_calls);
        let available_runtime_seconds = budget
            .max_runtime_seconds
            .saturating_sub(budget.consumed_runtime_seconds + budget.reserved_runtime_seconds);
        if claim_count > available_steps
            || requested_tool_calls.saturating_mul(claim_count) > available_tool_calls
            || requested_runtime_seconds.saturating_mul(claim_count) > available_runtime_seconds
            || budget.max_tokens.is_some_and(|max| {
                requested_tokens.saturating_mul(claim_count)
                    > max.saturating_sub(budget.consumed_tokens + budget.reserved_tokens)
            })
            || budget.max_cost.is_some_and(|max| {
                requested_cost * claim_count as f64
                    > max - budget.consumed_cost - budget.reserved_cost + f64::EPSILON
            })
        {
            return Err("原生任务步骤领取超出持久化执行预算".to_string());
        }
        let lease_expires_at =
            (Utc::now() + chrono::Duration::seconds(input.lease_seconds as i64)).to_rfc3339();
        let mut claims = Vec::new();
        for step in candidates {
            let attempt: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(attempt), 0) + 1
                     FROM runtime_task_step_runs
                     WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3 AND step_id=?4",
                    params![workspace_scope, input.task_id, revision, step.step_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("无法生成原生任务步骤领取序号：{error}"))?;
            let claim_id = format!("step-claim-{}", Uuid::new_v4());
            transaction
                .execute(
                    "INSERT INTO runtime_task_step_runs
                     (workspace_scope, claim_id, task_id, plan_revision, step_id, attempt,
                      effect_class, state, lease_owner, lease_expires_at, reserved_tool_calls,
                      reserved_runtime_seconds, reserved_tokens, reserved_cost,
                      cancellation_fence, claimed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'claimed', ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        workspace_scope,
                        claim_id,
                        input.task_id,
                        revision,
                        step.step_id,
                        attempt,
                        step.effect_class.as_str(),
                        input.worker_id,
                        lease_expires_at,
                        requested_tool_calls,
                        requested_runtime_seconds,
                        requested_tokens,
                        requested_cost,
                        budget.cancellation_fence,
                        now,
                    ],
                )
                .map_err(|error| format!("无法保存原生任务步骤领取：{error}"))?;
            claims.push(RuntimeTaskStepClaim {
                claim_id,
                runtime_task_id: input.task_id.clone(),
                plan_revision: u64::try_from(revision).unwrap_or_default(),
                step_id: step.step_id,
                step_kind: step.step_kind,
                title: step.title,
                depends_on: step.depends_on,
                parameters: step.parameters,
                effect_class: step.effect_class,
                attempt: u64::try_from(attempt).unwrap_or_default(),
                lease_owner: input.worker_id.clone(),
                lease_expires_at: lease_expires_at.clone(),
                reserved_tool_calls: requested_tool_calls,
                reserved_runtime_seconds: requested_runtime_seconds,
                reserved_tokens: input.reservation.max_tokens,
                reserved_cost: input.reservation.max_cost,
                cancellation_fence: budget.cancellation_fence,
                claimed_at: now.clone(),
            });
        }
        transaction
            .execute(
                "UPDATE runtime_task_execution_budgets
                 SET reserved_steps=reserved_steps+?4,
                     reserved_tool_calls=reserved_tool_calls+?5,
                     reserved_runtime_seconds=reserved_runtime_seconds+?6,
                     reserved_tokens=reserved_tokens+?7,
                     reserved_cost=reserved_cost+?8, updated_at=?9
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                params![
                    workspace_scope,
                    input.task_id,
                    revision,
                    claim_count as i64,
                    checked_sqlite_i64(
                        requested_tool_calls.saturating_mul(claim_count),
                        "工具调用预留"
                    )?,
                    checked_sqlite_i64(
                        requested_runtime_seconds.saturating_mul(claim_count),
                        "运行时间预留",
                    )?,
                    checked_sqlite_i64(requested_tokens.saturating_mul(claim_count), "Token 预留")?,
                    requested_cost * claim_count as f64,
                    now,
                ],
            )
            .map_err(|error| format!("无法更新原生任务步骤预算预留：{error}"))?;
        let budget = read_runtime_task_execution_budget(
            &transaction,
            workspace_scope,
            &input.task_id,
            revision,
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务步骤领取：{error}"))?;
        Ok(RuntimeTaskStepClaimBatch { claims, budget })
    }

    pub(crate) fn renew_runtime_task_step_lease(
        &self,
        workspace_scope: &str,
        input: &RuntimeTaskStepLeaseRenewalInput,
    ) -> Result<RuntimeTaskStepLeaseRenewalReceipt, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("renew_runtime_task_step_lease")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_task_step_lease_renewal(input)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始原生任务步骤 lease 续租事务：{error}"))?;
        let row = transaction
            .query_row(
                "SELECT run.plan_revision, run.step_id, run.state, run.lease_owner,
                        run.lease_expires_at, run.cancellation_fence,
                        budget.cancellation_fence, budget.cancelled_at, task.state
                 FROM runtime_task_step_runs run
                 JOIN runtime_task_execution_budgets budget
                   ON budget.workspace_scope=run.workspace_scope
                  AND budget.task_id=run.task_id
                  AND budget.plan_revision=run.plan_revision
                 JOIN runtime_tasks task
                   ON task.workspace_scope=run.workspace_scope AND task.id=run.task_id
                 WHERE run.workspace_scope=?1 AND run.task_id=?2 AND run.claim_id=?3",
                params![workspace_scope, input.task_id, input.step_claim_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务步骤 lease：{error}"))?
            .ok_or_else(|| "原生任务步骤领取不存在".to_string())?;
        if row.2 != "claimed" {
            return Err("只有 claimed 状态的原生任务步骤可以续租".to_string());
        }
        if row.3 != input.worker_id {
            return Err("原生任务步骤 lease owner 不匹配".to_string());
        }
        if row.7.is_some()
            || row.5 != row.6
            || !matches!(row.8.as_str(), "queued" | "running" | "awaiting_approval")
        {
            return Err("原生任务步骤已被父任务状态或取消栅栏封锁".to_string());
        }
        let now = Utc::now();
        let previous_expiry = chrono::DateTime::parse_from_rfc3339(&row.4)
            .map_err(|_| "原生任务步骤 lease 时间无效".to_string())?
            .with_timezone(&Utc);
        if previous_expiry <= now {
            return Err("原生任务步骤 lease 已过期，必须重新领取".to_string());
        }
        let lease_seconds = i64::try_from(input.lease_seconds)
            .map_err(|_| "原生任务步骤 lease 续租时长无效".to_string())?;
        let proposed_expiry = now
            .checked_add_signed(ChronoDuration::seconds(lease_seconds))
            .ok_or_else(|| "无法计算原生任务步骤 lease 续租时间".to_string())?;
        let renewed_expiry = previous_expiry.max(proposed_expiry).to_rfc3339();
        let changed = transaction
            .execute(
                "UPDATE runtime_task_step_runs SET lease_expires_at=?4
                 WHERE workspace_scope=?1 AND task_id=?2 AND claim_id=?3
                   AND state='claimed' AND lease_owner=?5 AND lease_expires_at=?6",
                params![
                    workspace_scope,
                    input.task_id,
                    input.step_claim_id,
                    renewed_expiry,
                    input.worker_id,
                    row.4,
                ],
            )
            .map_err(|error| format!("无法续租原生任务步骤 lease：{error}"))?;
        if changed != 1 {
            return Err("原生任务步骤 lease 已被并发修改".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务步骤 lease 续租：{error}"))?;
        Ok(RuntimeTaskStepLeaseRenewalReceipt {
            runtime_task_id: input.task_id.clone(),
            step_claim_id: input.step_claim_id.clone(),
            plan_revision: u64::try_from(row.0).unwrap_or_default(),
            step_id: row.1,
            lease_owner: input.worker_id.clone(),
            previous_lease_expires_at: row.4,
            lease_expires_at: renewed_expiry,
            cancellation_fence: u64::try_from(row.5).unwrap_or_default(),
        })
    }

    /// 获取指定 worker 的所有活动 step claims
    #[allow(dead_code)]
    pub(crate) fn get_active_step_claims(
        &self,
        workspace_scope: &str,
        worker_id: &str,
    ) -> Result<Vec<crate::task_runtime::RuntimeTaskStepClaim>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;

        let mut statement = connection
            .prepare(
                "SELECT claim_id, task_id, plan_revision, step_id, step_kind, title,
                        depends_on, parameters, effect_class, attempt, lease_owner,
                        lease_expires_at, reserved_tool_calls, reserved_runtime_seconds,
                        reserved_tokens, reserved_cost, cancellation_fence, claimed_at
                 FROM runtime_task_step_runs
                 WHERE workspace_scope=?1 AND lease_owner=?2 AND state='claimed'
                 ORDER BY claimed_at ASC",
            )
            .map_err(|error| format!("无法准备活动步骤查询：{error}"))?;

        let claims = statement
            .query_map(params![workspace_scope, worker_id], |row| {
                let depends_on_json: String = row.get(6)?;
                let depends_on: Vec<String> =
                    serde_json::from_str(&depends_on_json).unwrap_or_default();
                let parameters_json: String = row.get(7)?;
                let parameters: serde_json::Value = serde_json::from_str(&parameters_json)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                Ok(crate::task_runtime::RuntimeTaskStepClaim {
                    claim_id: row.get(0)?,
                    runtime_task_id: row.get(1)?,
                    plan_revision: row.get::<_, i64>(2)? as u64,
                    step_id: row.get(3)?,
                    step_kind: crate::task_runtime::RuntimeTaskStepKind::parse(
                        &row.get::<_, String>(4)?,
                    )
                    .unwrap_or(crate::task_runtime::RuntimeTaskStepKind::Capability),
                    title: row.get(5)?,
                    depends_on,
                    parameters,
                    effect_class: crate::task_runtime::RuntimeTaskStepEffectClass::parse(
                        &row.get::<_, String>(8)?,
                    )
                    .unwrap_or(crate::task_runtime::RuntimeTaskStepEffectClass::Effectful),
                    attempt: row.get::<_, i64>(9)? as u64,
                    lease_owner: row.get(10)?,
                    lease_expires_at: row.get(11)?,
                    reserved_tool_calls: row.get::<_, i64>(12)? as u64,
                    reserved_runtime_seconds: row.get::<_, i64>(13)? as u64,
                    reserved_tokens: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                    reserved_cost: row.get(15)?,
                    cancellation_fence: row.get::<_, i64>(16)? as u64,
                    claimed_at: row.get(17)?,
                })
            })
            .map_err(|error| format!("无法执行活动步骤查询：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取活动步骤：{error}"))?;

        Ok(claims)
    }

    pub(crate) fn validate_runtime_execution_ticket_renewal(
        &self,
        workspace_scope: &str,
        child_task_id: &str,
        binding: &RuntimeTaskStepCommandBinding,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let child = read_native_runtime_task(&connection, workspace_scope, child_task_id)?;
        if child.payload.get("kind").and_then(Value::as_str) != Some("runtime_child")
            || !matches!(
                child.state.as_str(),
                "queued" | "running" | "awaiting_approval"
            )
        {
            return Err("只有存活的 Runtime 子任务可以续期执行票据".to_string());
        }
        let expected =
            read_runtime_child_execution_expectation(&connection, workspace_scope, child_task_id)?
                .ok_or_else(|| "Runtime 子任务缺少步骤绑定，不能续期执行票据".to_string())?;
        if expected.binding != *binding {
            return Err("Runtime 执行票据续期步骤绑定不一致".to_string());
        }
        validate_live_runtime_child_execution_expectation(&expected, &Utc::now().to_rfc3339())
    }

    pub(crate) fn validate_runtime_effectful_handler(
        &self,
        ticket_state: &ExecutionTicketState,
        workspace_scope: &str,
        operation_context: &OperationContext,
        capability_id: &str,
        operation: &str,
    ) -> Result<RuntimeEffectfulHandlerAuthorization, String> {
        let authorization = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            runtime_effectful_handler_authorization(
                &connection,
                workspace_scope,
                operation_context,
                &[(capability_id, operation)],
            )?
        };
        ticket_state.validate_effectful_handler_authorization(
            &authorization.execution_ticket,
            workspace_scope,
            &authorization.child_task_id,
            &authorization.command_id,
            &authorization.trace_id,
            &authorization.capability_id,
            &authorization.operation,
            &authorization.binding,
        )?;
        Ok(authorization)
    }

    pub(crate) fn validate_runtime_effectful_handler_pairs(
        &self,
        ticket_state: &ExecutionTicketState,
        workspace_scope: &str,
        operation_context: &OperationContext,
        allowed_pairs: &[(&str, &str)],
    ) -> Result<RuntimeEffectfulHandlerAuthorization, String> {
        let authorization = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            runtime_effectful_handler_authorization(
                &connection,
                workspace_scope,
                operation_context,
                allowed_pairs,
            )?
        };
        ticket_state.validate_effectful_handler_authorization(
            &authorization.execution_ticket,
            workspace_scope,
            &authorization.child_task_id,
            &authorization.command_id,
            &authorization.trace_id,
            &authorization.capability_id,
            &authorization.operation,
            &authorization.binding,
        )?;
        Ok(authorization)
    }

    pub(crate) fn record_runtime_effectful_handler_completion(
        &self,
        ticket_state: &ExecutionTicketState,
        workspace_scope: &str,
        operation_context: &OperationContext,
        capability_id: &str,
        operation: &str,
        usage: TrustedHandlerUsage,
    ) -> Result<(), String> {
        let authorization = self.validate_runtime_effectful_handler(
            ticket_state,
            workspace_scope,
            operation_context,
            capability_id,
            operation,
        )?;
        ticket_state.record_effectful_handler_completion(
            &authorization.execution_ticket,
            workspace_scope,
            &authorization.child_task_id,
            &authorization.command_id,
            &authorization.trace_id,
            &authorization.capability_id,
            &authorization.operation,
            &authorization.binding,
            usage,
            authorization.reservation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_runtime_effectful_handler_completion_once(
        &self,
        ticket_state: &ExecutionTicketState,
        workspace_scope: &str,
        operation_context: &OperationContext,
        capability_id: &str,
        operation: &str,
        completion_key: &str,
        usage: TrustedHandlerUsage,
    ) -> Result<bool, String> {
        let authorization = self.validate_runtime_effectful_handler(
            ticket_state,
            workspace_scope,
            operation_context,
            capability_id,
            operation,
        )?;
        ticket_state.record_effectful_handler_completion_once(
            &authorization.execution_ticket,
            workspace_scope,
            &authorization.child_task_id,
            &authorization.command_id,
            &authorization.trace_id,
            &authorization.capability_id,
            &authorization.operation,
            &authorization.binding,
            completion_key,
            usage,
            authorization.reservation,
        )
    }

    pub(crate) fn execute_runtime_read_only_capability(
        &self,
        workspace_scope: &str,
        child_task_id: &str,
        binding: &RuntimeTaskStepCommandBinding,
    ) -> Result<RuntimeReadOnlyCapabilityResult, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let child = read_native_runtime_task(&connection, workspace_scope, child_task_id)?;
        if child.payload.get("kind").and_then(Value::as_str) != Some("runtime_child")
            || child.state != "running"
        {
            return Err("只有 running 状态的 Runtime 子任务可以执行只读原生门禁".to_string());
        }
        let expected =
            read_runtime_child_execution_expectation(&connection, workspace_scope, child_task_id)?
                .ok_or_else(|| "Runtime 子任务缺少只读步骤绑定".to_string())?;
        validate_live_runtime_child_execution_expectation(&expected, &Utc::now().to_rfc3339())?;
        if expected.binding != *binding {
            return Err("Runtime 只读能力步骤绑定不一致".to_string());
        }
        if expected.effect_class != RuntimeTaskStepEffectClass::ReadOnly
            || expected.command_effectful
        {
            return Err("有副作用的 Runtime 能力不能使用只读原生门禁".to_string());
        }
        if !matches!(
            expected.operation.as_str(),
            "read" | "query" | "search" | "list" | "open" | "preview"
        ) {
            return Err("Runtime 只读能力操作不在原生处理器白名单内".to_string());
        }
        let parameter_json =
            canonical_runtime_json_string(&expected.parameters, "Runtime 只读能力参数")?;
        let parameter_hash = format!("sha256:{:x}", Sha256::digest(parameter_json.as_bytes()));
        let mut output = execute_runtime_read_only_handler_in_connection(
            &connection,
            workspace_scope,
            &expected,
        )?;
        let output = output
            .as_object_mut()
            .ok_or_else(|| "Runtime 只读原生处理器返回了无效结果".to_string())?;
        output.insert("validated".to_string(), Value::Bool(true));
        output.insert("handler".to_string(), Value::String("native".to_string()));
        output.insert("parameterHash".to_string(), Value::String(parameter_hash));
        output.insert(
            "snapshotAt".to_string(),
            Value::String(Utc::now().to_rfc3339()),
        );
        Ok(RuntimeReadOnlyCapabilityResult {
            task_id: child.id,
            command_id: expected.command_id,
            trace_id: expected.trace_id,
            capability_id: expected.capability_id,
            operation: expected.operation,
            trust_kind: "read_only_native_handler".to_string(),
            output: Value::Object(output.clone()),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_runtime_task_step(
        &self,
        workspace_scope: &str,
        task_id: &str,
        claim_id: &str,
        receipt_id: &str,
        state: &str,
        consumed_tool_calls: u64,
        consumed_runtime_seconds: u64,
        consumed_tokens: u64,
        consumed_cost: f64,
        output: &Value,
        error: Option<&str>,
    ) -> Result<RuntimeTaskStepReceipt, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始原生任务步骤回执事务：{error}"))?;
        let now = Utc::now().to_rfc3339();
        if let Some(existing) =
            read_runtime_task_step_receipt(&transaction, workspace_scope, receipt_id)?
        {
            if existing.step_claim_id != claim_id
                || existing.runtime_task_id != task_id
                || existing.state != state
                || existing.output != *output
                || existing.error.as_deref() != error
                || existing.consumed_tool_calls != consumed_tool_calls
                || existing.consumed_runtime_seconds != consumed_runtime_seconds
                || existing.consumed_tokens != consumed_tokens
                || (existing.consumed_cost - consumed_cost).abs() > f64::EPSILON
            {
                return Err("原生任务步骤回执 ID 已绑定不同内容".to_string());
            }
            transaction.commit().map_err(|commit_error| {
                format!("无法提交原生任务步骤回执幂等查询：{commit_error}")
            })?;
            return Ok(existing);
        }
        let run = transaction
            .query_row(
                "SELECT plan_revision, step_id, state, lease_expires_at, cancellation_fence,
                        reserved_tool_calls, reserved_runtime_seconds, reserved_tokens,
                        reserved_cost
                 FROM runtime_task_step_runs
                 WHERE workspace_scope=?1 AND claim_id=?2 AND task_id=?3",
                params![workspace_scope, claim_id, task_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, f64>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务步骤领取：{error}"))?
            .ok_or_else(|| "原生任务步骤领取不存在".to_string())?;
        if run.2 != "claimed" {
            return Err("原生任务步骤领取已经完成或被封锁".to_string());
        }
        if run.3.as_str() <= now.as_str() {
            expire_runtime_task_step_claims(&transaction, workspace_scope, task_id, run.0, &now)?;
            return Err("原生任务步骤领取 lease 已过期".to_string());
        }
        let task = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        if task.state == "cancelled" {
            return Err("父任务已取消，拒绝迟到步骤回执".to_string());
        }
        let trace_id = task
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        if task.trace_id.as_deref() != Some(trace_id.as_str()) {
            transaction
                .execute(
                    "UPDATE runtime_tasks SET trace_id=?3
                     WHERE workspace_scope=?1 AND id=?2",
                    params![workspace_scope, task_id, trace_id],
                )
                .map_err(|error| format!("无法绑定步骤回执 Trace：{error}"))?;
        }
        let budget =
            read_runtime_task_execution_budget(&transaction, workspace_scope, task_id, run.0)?;
        if budget.cancelled_at.is_some()
            || budget.cancellation_fence != u64::try_from(run.4).unwrap_or_default()
        {
            return Err("原生任务步骤已被父任务取消栅栏封锁".to_string());
        }
        // Reject an over-reported completion before checking capability-child
        // proof. This keeps the persisted reservation as the first authority
        // for every terminal receipt, including a missing or failed child.
        let reported_cost_valid = consumed_cost.is_finite() && consumed_cost >= 0.0;
        if consumed_tool_calls > u64::try_from(run.5).unwrap_or_default()
            || consumed_runtime_seconds > u64::try_from(run.6).unwrap_or_default()
            || budget.max_tokens.is_some()
                && consumed_tokens > u64::try_from(run.7).unwrap_or_default()
            || !reported_cost_valid
            || budget.max_cost.is_some() && consumed_cost > run.8 + f64::EPSILON
        {
            return Err("原生任务步骤回执消耗超过已预留预算".to_string());
        }
        let trusted_execution_receipt = if state == "succeeded" {
            Self::validate_runtime_capability_step_child_completion(
                &transaction,
                workspace_scope,
                task_id,
                run.0,
                &run.1,
                claim_id,
                &trace_id,
            )?
        } else {
            None
        };
        if let Some(receipt) = trusted_execution_receipt.as_ref() {
            if consumed_tool_calls != receipt.consumed_tool_calls
                || consumed_runtime_seconds != receipt.consumed_runtime_seconds
                || consumed_tokens != receipt.consumed_tokens
                || (consumed_cost - receipt.consumed_cost).abs() > f64::EPSILON
            {
                return Err("capability 步骤上报消耗与 Rust 可信处理器回执不一致".to_string());
            }
        }
        let consumed_tool_calls = trusted_execution_receipt
            .as_ref()
            .map_or(consumed_tool_calls, |receipt| receipt.consumed_tool_calls);
        let consumed_runtime_seconds = trusted_execution_receipt
            .as_ref()
            .map_or(consumed_runtime_seconds, |receipt| {
                receipt.consumed_runtime_seconds
            });
        let consumed_tokens = trusted_execution_receipt
            .as_ref()
            .map_or(consumed_tokens, |receipt| receipt.consumed_tokens);
        let consumed_cost = trusted_execution_receipt
            .as_ref()
            .map_or(consumed_cost, |receipt| receipt.consumed_cost);
        let consumed_cost_valid = consumed_cost.is_finite() && consumed_cost >= 0.0;
        if consumed_tool_calls > u64::try_from(run.5).unwrap_or_default()
            || consumed_runtime_seconds > u64::try_from(run.6).unwrap_or_default()
            || budget.max_tokens.is_some()
                && consumed_tokens > u64::try_from(run.7).unwrap_or_default()
            || !consumed_cost_valid
            || budget.max_cost.is_some() && consumed_cost > run.8 + f64::EPSILON
        {
            return Err("原生任务步骤回执消耗超过已预留预算".to_string());
        }
        let output_json = canonical_runtime_json_string(output, "原生任务步骤回执输出")?;
        let content = serde_json::json!({
            "state": state,
            "output": output,
            "error": error,
            "consumedToolCalls": consumed_tool_calls,
            "consumedRuntimeSeconds": consumed_runtime_seconds,
            "consumedTokens": consumed_tokens,
            "consumedCost": consumed_cost,
            "trustedExecutionReceiptId": trusted_execution_receipt
                .as_ref()
                .map(|receipt| receipt.receipt_id.as_str()),
        });
        let content_json = canonical_runtime_json_string(&content, "原生任务步骤回执")?;
        let content_hash = format!("sha256:{:x}", Sha256::digest(content_json.as_bytes()));
        let changed = transaction
            .execute(
                "UPDATE runtime_task_step_runs
                 SET state=?4, finished_at=?5
                 WHERE workspace_scope=?1 AND claim_id=?2 AND state='claimed'",
                params![workspace_scope, claim_id, task_id, state, now],
            )
            .map_err(|error| format!("无法完成原生任务步骤领取：{error}"))?;
        if changed != 1 {
            return Err("原生任务步骤领取状态已被并发修改".to_string());
        }
        transaction
            .execute(
                "UPDATE runtime_task_execution_budgets
                 SET reserved_steps=MAX(0, reserved_steps-1),
                     reserved_tool_calls=MAX(0, reserved_tool_calls-?4),
                     reserved_runtime_seconds=MAX(0, reserved_runtime_seconds-?5),
                     reserved_tokens=MAX(0, reserved_tokens-?6),
                     reserved_cost=MAX(0, reserved_cost-?7),
                     consumed_steps=consumed_steps+1,
                     consumed_tool_calls=consumed_tool_calls+?8,
                     consumed_runtime_seconds=consumed_runtime_seconds+?9,
                     consumed_tokens=consumed_tokens+?10,
                     consumed_cost=consumed_cost+?11,
                     updated_at=?12
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                params![
                    workspace_scope,
                    task_id,
                    run.0,
                    run.5,
                    run.6,
                    run.7,
                    run.8,
                    checked_sqlite_i64(consumed_tool_calls, "工具调用消耗")?,
                    checked_sqlite_i64(consumed_runtime_seconds, "运行时间消耗")?,
                    checked_sqlite_i64(consumed_tokens, "Token 消耗")?,
                    consumed_cost,
                    now,
                ],
            )
            .map_err(|error| format!("无法结算原生任务步骤预算：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_task_step_receipts
                 (workspace_scope, receipt_id, claim_id, task_id, plan_revision, step_id, state,
                  output_json, error, consumed_tool_calls, consumed_runtime_seconds,
                  consumed_tokens, consumed_cost, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    workspace_scope,
                    receipt_id,
                    claim_id,
                    task_id,
                    run.0,
                    run.1,
                    state,
                    output_json,
                    error,
                    checked_sqlite_i64(consumed_tool_calls, "工具调用消耗")?,
                    checked_sqlite_i64(consumed_runtime_seconds, "运行时间消耗")?,
                    checked_sqlite_i64(consumed_tokens, "Token 消耗")?,
                    consumed_cost,
                    content_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存原生任务步骤回执：{error}"))?;
        if state == "succeeded" {
            append_runtime_step_receipt_evidence(
                &transaction,
                workspace_scope,
                task_id,
                run.0,
                &run.1,
                claim_id,
                receipt_id,
                &content_hash,
                &trace_id,
                &now,
            )?;
        }
        let receipt = read_runtime_task_step_receipt(&transaction, workspace_scope, receipt_id)?
            .ok_or_else(|| "原生任务步骤回执保存后无法读取".to_string())?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务步骤回执：{error}"))?;
        Ok(receipt)
    }

    fn validate_runtime_capability_step_child_completion(
        connection: &Connection,
        workspace_scope: &str,
        task_id: &str,
        plan_revision: i64,
        step_id: &str,
        claim_id: &str,
        parent_trace_id: &str,
    ) -> Result<Option<TrustedExecutionReceipt>, String> {
        let step_kind = connection
            .query_row(
                "SELECT step_kind FROM runtime_task_plan_steps
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3 AND step_id=?4",
                params![workspace_scope, task_id, plan_revision, step_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 capability 步骤类型：{error}"))?
            .ok_or_else(|| "原生任务步骤不存在，无法验证绑定子任务".to_string())?;
        if step_kind != RuntimeTaskStepKind::Capability.as_str() {
            return Ok(None);
        }
        let child_task_id = connection
            .query_row(
                "SELECT binding.child_task_id
                 FROM runtime_task_step_command_bindings binding
                 WHERE binding.workspace_scope=?1 AND binding.claim_id=?2",
                params![workspace_scope, claim_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 capability 步骤绑定子任务：{error}"))?
            .ok_or_else(|| {
                "capability 原生任务步骤成功前必须存在绑定的 Runtime 子任务".to_string()
            })?;
        let child = read_native_runtime_task(connection, workspace_scope, &child_task_id)?;
        if child.state != "succeeded" {
            return Err(format!(
                "capability 原生任务步骤绑定的子任务 {} 尚未成功终态：{}",
                child.id, child.state
            ));
        }
        let expected =
            read_runtime_child_execution_expectation(connection, workspace_scope, &child_task_id)?
                .ok_or_else(|| "capability 子任务缺少可信执行绑定快照".to_string())?;
        if expected.binding.runtime_task_id != task_id
            || expected.binding.plan_revision != u64::try_from(plan_revision).unwrap_or_default()
            || expected.binding.step_id != step_id
            || expected.binding.step_claim_id != claim_id
            || child.trace_id.as_deref() != Some(parent_trace_id)
            || expected.trace_id != parent_trace_id
        {
            return Err("capability 原生任务步骤绑定子任务的 Trace 与父任务不一致".to_string());
        }
        let receipt_value = child
            .payload
            .get("trustedExecutionReceipt")
            .cloned()
            .ok_or_else(|| "capability Runtime 子任务缺少可信原生处理器回执".to_string())?;
        let receipt = serde_json::from_value::<TrustedExecutionReceipt>(receipt_value)
            .map_err(|error| format!("capability Runtime 子任务可信回执无法解析：{error}"))?;
        validate_trusted_execution_receipt(workspace_scope, &child, &expected, &receipt)?;
        validate_trusted_execution_usage_within_reservation(&expected, &receipt)?;
        Ok(Some(receipt))
    }

    pub(crate) fn complete_runtime_task_plan_step(
        &self,
        workspace_scope: &str,
        input: &RuntimeTaskStepCompletionInput,
    ) -> Result<RuntimeTaskStepReceipt, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("complete_runtime_task_plan_step")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_task_step_completion(input)?;
        self.finalize_runtime_task_step(
            workspace_scope,
            &input.task_id,
            &input.step_claim_id,
            &input.receipt_id,
            "succeeded",
            input.consumed_tool_calls,
            input.consumed_runtime_seconds,
            input.consumed_tokens,
            input.consumed_cost,
            &input.output,
            None,
        )
    }

    pub(crate) fn fail_runtime_task_plan_step(
        &self,
        workspace_scope: &str,
        input: &RuntimeTaskStepFailureInput,
    ) -> Result<RuntimeTaskStepReceipt, String> {
        crate::task_runtime::validate_runtime_task_step_failure(input)?;
        self.finalize_runtime_task_step(
            workspace_scope,
            &input.task_id,
            &input.step_claim_id,
            &input.receipt_id,
            "failed",
            input.consumed_tool_calls,
            input.consumed_runtime_seconds,
            input.consumed_tokens,
            input.consumed_cost,
            &input.output,
            Some(input.error.trim()),
        )
    }

    pub(crate) fn list_runtime_task_step_receipts(
        &self,
        workspace_scope: &str,
        task_id: &str,
        plan_revision: Option<u64>,
        limit: usize,
    ) -> Result<Vec<RuntimeTaskStepReceipt>, String> {
        let limit = limit.clamp(1, 512);
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let revision = plan_revision.map(|value| checked_sqlite_i64(value, "计划版本"));
        let revision = revision.transpose()?;
        let mut statement = connection
            .prepare(
                "SELECT receipt_id FROM runtime_task_step_receipts
                 WHERE workspace_scope=?1 AND task_id=?2
                   AND (?3 IS NULL OR plan_revision=?3)
                 ORDER BY created_at ASC, receipt_id ASC LIMIT ?4",
            )
            .map_err(|error| format!("无法查询原生任务步骤回执：{error}"))?;
        let ids = statement
            .query_map(
                params![workspace_scope, task_id, revision, limit as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("无法读取原生任务步骤回执列表：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析原生任务步骤回执列表：{error}"))?;
        ids.into_iter()
            .map(|receipt_id| {
                read_runtime_task_step_receipt(&connection, workspace_scope, &receipt_id)?
                    .ok_or_else(|| "原生任务步骤回执列表包含缺失记录".to_string())
            })
            .collect()
    }

    pub(crate) fn append_runtime_task_evidence(
        &self,
        workspace_scope: &str,
        input: &RuntimeTaskEvidenceInput,
    ) -> Result<RuntimeTaskEvidence, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("append_runtime_task_evidence")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_task_evidence_shape(input)?;
        let task_id = input.task_id.trim();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始原生任务证据事务：{error}"))?;
        let task = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        let latest_revision = transaction
            .query_row(
                "SELECT revision FROM runtime_task_plans
                 WHERE workspace_scope=?1 AND task_id=?2 ORDER BY revision DESC LIMIT 1",
                params![workspace_scope, task_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务完成契约版本：{error}"))?
            .ok_or_else(|| "原生任务尚未定义完成契约".to_string())?;
        let revision = input
            .plan_revision
            .map(|value| i64::try_from(value).map_err(|_| "原生任务计划版本无效".to_string()))
            .transpose()?
            .unwrap_or(latest_revision);
        if revision != latest_revision {
            return Err("证据必须写入当前生效的原生任务计划版本".to_string());
        }
        let requirement = transaction
            .query_row(
                "SELECT step_id, evidence_type FROM runtime_task_completion_requirements
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3 AND requirement_id=?4",
                params![workspace_scope, task_id, revision, input.requirement_id.trim()],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务完成要求：{error}"))?
            .ok_or_else(|| "原生任务完成要求不存在".to_string())?;
        if requirement.1 != input.evidence_type.trim() {
            return Err("证据类型与完成要求不匹配".to_string());
        }
        let payload_json = canonical_runtime_json_string(&input.payload, "原生任务证据")?;
        let envelope = serde_json::json!({
            "planRevision": revision,
            "requirementId": input.requirement_id.trim(),
            "evidenceType": input.evidence_type.trim(),
            "sourceKind": input.source_kind.as_str(),
            "sourceRef": input.source_ref.trim(),
            "payload": canonical_runtime_json(&input.payload),
        });
        let envelope_json = canonical_runtime_json_string(&envelope, "原生任务证据封套")?;
        if envelope_json.len() > MAX_RUNTIME_EVIDENCE_BYTES {
            return Err("原生任务证据超过 256 KB 安全上限".to_string());
        }
        let content_hash = format!("sha256:{:x}", Sha256::digest(envelope_json.as_bytes()));
        let existing = transaction
            .query_row(
                "SELECT plan_revision, requirement_id, step_id, evidence_type, source_kind,
                        source_ref, payload_json, content_hash, created_at
                 FROM runtime_task_evidence
                 WHERE workspace_scope=?1 AND task_id=?2 AND evidence_id=?3",
                params![workspace_scope, task_id, input.evidence_id.trim()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务已有证据：{error}"))?;
        if let Some(existing) = existing {
            if existing.7 != content_hash {
                return Err("证据 id 已经绑定到不同内容，拒绝覆盖不可变证据".to_string());
            }
            transaction
                .commit()
                .map_err(|error| format!("无法提交原生任务证据幂等查询：{error}"))?;
            return runtime_task_evidence_from_parts(
                task_id,
                input.evidence_id.trim(),
                existing.0,
                &existing.1,
                existing.2,
                &existing.3,
                &existing.4,
                &existing.5,
                &existing.6,
                &existing.7,
                &existing.8,
            );
        }
        crate::task_runtime::validate_runtime_task_evidence(input)?;
        if matches!(task.state.as_str(), "succeeded" | "failed" | "cancelled") {
            return Err("终态原生任务不能追加新证据".to_string());
        }
        let evidence_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM runtime_task_evidence
                 WHERE workspace_scope=?1 AND task_id=?2",
                params![workspace_scope, task_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("无法统计原生任务证据：{error}"))?;
        if evidence_count >= MAX_RUNTIME_TASK_EVIDENCE as i64 {
            return Err("单个原生任务证据超过 2048 条安全上限".to_string());
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO runtime_task_evidence
                 (workspace_scope, task_id, evidence_id, plan_revision, requirement_id, step_id,
                  evidence_type, source_kind, source_ref, payload_json, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    workspace_scope,
                    task_id,
                    input.evidence_id.trim(),
                    revision,
                    input.requirement_id.trim(),
                    requirement.0,
                    input.evidence_type.trim(),
                    input.source_kind.as_str(),
                    input.source_ref.trim(),
                    payload_json,
                    content_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存原生任务证据：{error}"))?;
        let trace_id = task
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        if task.trace_id.as_deref() != Some(trace_id.as_str()) {
            transaction
                .execute(
                    "UPDATE runtime_tasks SET trace_id=?3
                     WHERE workspace_scope=?1 AND id=?2",
                    params![workspace_scope, task_id, trace_id],
                )
                .map_err(|error| format!("无法绑定原生任务证据 Trace：{error}"))?;
        }
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: task_id,
                event_type: "task.evidence_appended",
                state: "recorded",
                payload: &serde_json::json!({
                    "evidenceId": input.evidence_id.trim(),
                    "planRevision": revision,
                    "requirementId": input.requirement_id.trim(),
                    "evidenceType": input.evidence_type.trim(),
                    "sourceKind": input.source_kind.as_str(),
                    "contentHash": content_hash,
                }),
                created_at: &now,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生任务证据事务：{error}"))?;
        Ok(RuntimeTaskEvidence {
            task_id: task_id.to_string(),
            evidence_id: input.evidence_id.trim().to_string(),
            plan_revision: revision as u64,
            requirement_id: input.requirement_id.trim().to_string(),
            step_id: requirement.0,
            evidence_type: input.evidence_type.trim().to_string(),
            source_kind: input.source_kind.clone(),
            source_ref: input.source_ref.trim().to_string(),
            payload: input.payload.clone(),
            content_hash,
            created_at: now,
        })
    }

    pub(crate) fn acknowledge_runtime_schedule_dispatch(
        &self,
        workspace_scope: &str,
        input: &RuntimeScheduleDispatchAckInput,
    ) -> Result<NativeRuntimeTask, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("acknowledge_runtime_schedule_dispatch")
            .with_threshold(self.config.slow_query_threshold_ms);

        crate::task_runtime::validate_runtime_schedule_dispatch_ack(input)?;
        let occurrence_id = input.occurrence_id.trim();
        let runtime_task_id = input.runtime_task_id.trim();
        let binding = self.runtime_schedule_dispatch_binding(
            workspace_scope,
            occurrence_id,
            runtime_task_id,
        )?;
        if binding.schedule_revision != input.schedule_revision
            || binding.schedule_payload_hash
                != canonical_schedule_payload_hash(&input.schedule_payload_hash)
        {
            return Err("日程 occurrence 的 revision 或快照哈希不匹配".to_string());
        }
        let (schedule_id, schedule_kind, scheduled_for, dispatch_task_ids) = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            let occurrence = connection
                .query_row(
                    "SELECT schedule_id, schedule_kind, scheduled_for, schedule_revision,
                            runtime_task_id
                     FROM runtime_schedule_occurrences
                     WHERE workspace_scope=?1 AND occurrence_id=?2",
                    params![workspace_scope, occurrence_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("无法读取日程派发 occurrence：{error}"))?
                .ok_or_else(|| "未找到日程派发 occurrence".to_string())?;
            if occurrence.4 != runtime_task_id
                || u64::try_from(occurrence.3).unwrap_or_default() != input.schedule_revision
            {
                return Err("日程 occurrence 与 wrapper 任务不匹配".to_string());
            }
            let schedule_revision = input.schedule_revision.to_string();
            let matches_binding = |payload_json: &str| -> Result<bool, String> {
                let payload = serde_json::from_str::<Value>(payload_json)
                    .map_err(|error| format!("日程派发子任务快照损坏：{error}"))?;
                let parameters = payload
                    .get("parameters")
                    .and_then(Value::as_object)
                    .ok_or_else(|| "日程派发子任务缺少命令参数".to_string())?;
                Ok([
                    ("schedule_occurrence_id", occurrence_id),
                    ("schedule_wrapper_task_id", runtime_task_id),
                    ("schedule_id", occurrence.0.as_str()),
                    ("schedule_kind", occurrence.1.as_str()),
                    ("schedule_scheduled_for", occurrence.2.as_str()),
                    ("schedule_revision", schedule_revision.as_str()),
                    ("schedule_payload_hash", input.schedule_payload_hash.trim()),
                ]
                .into_iter()
                .all(|(key, expected)| {
                    parameters.get(key).and_then(Value::as_str) == Some(expected)
                }))
            };
            let mut dispatch_task_ids = Vec::new();
            if input.dispatch_task_ids.is_empty() {
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT task.id, task.payload
                         FROM runtime_tasks task
                         JOIN application_commands command
                           ON command.workspace_scope=task.workspace_scope
                          AND command.task_id=task.id
                         WHERE task.workspace_scope=?1 AND task.state='succeeded'
                           AND command.state='accepted'
                           AND json_valid(task.payload)=1
                           AND json_extract(task.payload, '$.parameters.schedule_occurrence_id')=?2
                           AND json_extract(task.payload, '$.parameters.schedule_wrapper_task_id')=?3
                           AND json_extract(task.payload, '$.parameters.schedule_id')=?4
                           AND json_extract(task.payload, '$.parameters.schedule_kind')=?5
                           AND json_extract(task.payload, '$.parameters.schedule_scheduled_for')=?6
                           AND json_extract(task.payload, '$.parameters.schedule_revision')=?7
                           AND json_extract(task.payload, '$.parameters.schedule_payload_hash')=?8
                         ORDER BY task.updated_at DESC LIMIT 129",
                    )
                    .map_err(|error| format!("无法准备日程派发恢复查询：{error}"))?;
                let candidates = statement
                    .query_map(
                        params![
                            workspace_scope,
                            occurrence_id,
                            runtime_task_id,
                            occurrence.0,
                            occurrence.1,
                            occurrence.2,
                            schedule_revision,
                            input.schedule_payload_hash.trim(),
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .map_err(|error| format!("无法查询日程派发恢复子任务：{error}"))?;
                for candidate in candidates {
                    let (task_id, payload_json) = candidate
                        .map_err(|error| format!("无法读取日程派发恢复子任务：{error}"))?;
                    if matches_binding(&payload_json)? {
                        dispatch_task_ids.push(task_id);
                    }
                }
                if dispatch_task_ids.len() > 128 {
                    return Err("日程派发恢复子任务超过安全上限".to_string());
                }
            } else {
                for dispatch_task_id in &input.dispatch_task_ids {
                    let task_record = connection
                        .query_row(
                            "SELECT task.state, task.payload
                         FROM runtime_tasks task
                         JOIN application_commands command
                           ON command.workspace_scope=task.workspace_scope
                          AND command.task_id=task.id
                         WHERE task.workspace_scope=?1 AND task.id=?2
                           AND command.state='accepted'",
                            params![workspace_scope, dispatch_task_id.trim()],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()
                        .map_err(|error| format!("无法验证日程派发子任务：{error}"))?
                        .ok_or_else(|| "日程派发子任务没有已接受的策略命令".to_string())?;
                    if task_record.0 != "succeeded" {
                        return Err(format!(
                            "日程派发子任务尚未成功完成（当前状态：{}）",
                            task_record.0
                        ));
                    }
                    if !matches_binding(&task_record.1)? {
                        return Err("日程派发子任务未绑定到当前 occurrence".to_string());
                    }
                    dispatch_task_ids.push(dispatch_task_id.trim().to_string());
                }
            }
            if dispatch_task_ids.is_empty() {
                return Err("日程派发没有可验证的成功子任务".to_string());
            }
            (occurrence.0, occurrence.1, occurrence.2, dispatch_task_ids)
        };
        self.append_runtime_task_evidence(
            workspace_scope,
            &RuntimeTaskEvidenceInput {
                task_id: runtime_task_id.to_string(),
                evidence_id: format!("schedule-dispatch-{occurrence_id}"),
                plan_revision: None,
                requirement_id: "dispatch-ack".to_string(),
                evidence_type: "schedule.dispatch_ack".to_string(),
                source_kind: RuntimeTaskEvidenceSourceKind::Scheduler,
                source_ref: occurrence_id.to_string(),
                payload: serde_json::json!({
                    "scheduleId": schedule_id,
                    "scheduleKind": schedule_kind,
                    "occurrenceId": occurrence_id,
                    "scheduledFor": scheduled_for,
                    "scheduleRevision": input.schedule_revision,
                    "schedulePayloadHash": input.schedule_payload_hash,
                    "runtimeTaskId": runtime_task_id,
                    "dispatchTaskIds": dispatch_task_ids,
                    "disposition": "dispatched",
                }),
            },
        )?;
        let mut task = self.runtime_task(workspace_scope, runtime_task_id)?;
        if task.state == "succeeded" {
            return Ok(task);
        }
        if matches!(
            task.state.as_str(),
            "failed" | "cancelled" | "awaiting_approval"
        ) {
            return Err(format!(
                "日程 wrapper 任务状态 {} 不能确认派发完成",
                task.state
            ));
        }
        if task.state == "paused" {
            task = self.transition_native_runtime_task(
                workspace_scope,
                runtime_task_id,
                "queued",
                task.progress,
                "恢复日程派发确认",
                None,
            )?;
        }
        if task.state == "created" {
            task = self.transition_native_runtime_task(
                workspace_scope,
                runtime_task_id,
                "queued",
                task.progress,
                "排队日程派发确认",
                None,
            )?;
        }
        if task.state == "queued" {
            task = self.transition_native_runtime_task(
                workspace_scope,
                runtime_task_id,
                "running",
                task.progress.max(10),
                "原生已验证日程派发子任务",
                None,
            )?;
        }
        if task.state != "running" {
            return Err(format!("日程 wrapper 任务状态 {} 无法完成", task.state));
        }
        self.transition_native_runtime_task(
            workspace_scope,
            runtime_task_id,
            "succeeded",
            100,
            "到期日程已进入受策略约束的执行路径",
            Some(&serde_json::json!({
                "id": format!("schedule-complete-{occurrence_id}"),
                "occurrenceId": occurrence_id,
                "scheduledFor": scheduled_for,
                "scheduleRevision": input.schedule_revision,
                "schedulePayloadHash": input.schedule_payload_hash,
                "dispatchTaskIds": dispatch_task_ids,
            })),
        )
    }

    pub(crate) fn resolve_operation_trace_id(
        &self,
        workspace_scope: &str,
        task_id: Option<&str>,
        supplied_trace_id: Option<&str>,
    ) -> Result<String, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("resolve_operation_trace_id")
            .with_threshold(self.config.slow_query_threshold_ms);

        if let Some(trace_id) = supplied_trace_id.filter(|value| !value.trim().is_empty()) {
            return Ok(crate::trace::validate_trace_id(trace_id)?.to_string());
        }
        if let Some(trace_id) = task_id
            .and_then(|task_id| self.runtime_task(workspace_scope, task_id).ok())
            .and_then(|task| task.trace_id)
            .filter(|value| !value.trim().is_empty())
        {
            crate::trace::validate_trace_id(&trace_id)?;
            return Ok(trace_id);
        }
        Ok(crate::trace::new_trace_id())
    }

    pub(crate) fn ensure_runtime_task_authorized(
        &self,
        workspace_scope: &str,
        task_id: &str,
        capability_ids: &[&str],
        operations: &[&str],
        vault_id: Option<&str>,
        allowed_states: &[&str],
    ) -> Result<NativeRuntimeTask, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("ensure_runtime_task_authorized")
            .with_threshold(self.config.slow_query_threshold_ms);

        let task = self.runtime_task(workspace_scope, task_id)?;
        if !allowed_states.contains(&task.state.as_str()) {
            return Err(format!("原生任务状态 {} 不允许执行当前操作", task.state));
        }
        let capabilities = task
            .payload
            .get("capabilityIds")
            .and_then(Value::as_array)
            .ok_or_else(|| "原生任务缺少能力范围".to_string())?;
        if !capability_ids.iter().any(|required| {
            capabilities
                .iter()
                .filter_map(Value::as_str)
                .any(|actual| actual == *required)
        }) {
            return Err("原生任务没有当前 Obsidian 操作所需能力".to_string());
        }
        let operation = task
            .payload
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| "原生任务缺少操作类型".to_string())?;
        if !operations.contains(&operation) {
            return Err(format!("原生任务操作 {operation} 与当前执行器不匹配"));
        }
        if let Some(expected_vault_id) = vault_id {
            let scoped_vault_id = task
                .payload
                .get("vaultId")
                .and_then(Value::as_str)
                .ok_or_else(|| "原生任务缺少 Vault 范围".to_string())?;
            if scoped_vault_id != expected_vault_id && scoped_vault_id != "all" {
                return Err("原生任务 Vault 范围与目标知识库不一致".to_string());
            }
        }
        Ok(task)
    }

    pub(crate) fn list_runtime_tasks(
        &self,
        workspace_scope: &str,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NativeRuntimeTask>, String> {
        if state.is_some_and(|value| !valid_runtime_task_state(value)) {
            return Err("任务状态筛选无效".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut tasks = Vec::new();
        let max = limit.clamp(1, 1000) as i64;
        let sql = if state.is_some() {
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1 AND state=?2
             ORDER BY updated_at DESC LIMIT ?3"
        } else {
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1
             ORDER BY updated_at DESC LIMIT ?2"
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备原生任务列表：{error}"))?;
        if let Some(state) = state {
            let rows = statement
                .query_map(
                    params![workspace_scope, state, max],
                    map_native_runtime_task,
                )
                .map_err(|error| format!("无法读取原生任务列表：{error}"))?;
            tasks.extend(rows.filter_map(Result::ok));
        } else {
            let rows = statement
                .query_map(params![workspace_scope, max], map_native_runtime_task)
                .map_err(|error| format!("无法读取原生任务列表：{error}"))?;
            tasks.extend(rows.filter_map(Result::ok));
        }
        Ok(tasks)
    }

    pub(crate) fn transition_native_runtime_task(
        &self,
        workspace_scope: &str,
        task_id: &str,
        target_state: &str,
        progress: u8,
        detail: &str,
        checkpoint: Option<&Value>,
    ) -> Result<NativeRuntimeTask, String> {
        self.transition_native_runtime_task_internal(
            workspace_scope,
            task_id,
            target_state,
            progress,
            detail,
            checkpoint,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn transition_native_runtime_task_with_trusted_execution_receipt(
        &self,
        workspace_scope: &str,
        task_id: &str,
        target_state: &str,
        progress: u8,
        detail: &str,
        checkpoint: Option<&Value>,
        trusted_execution_receipt: &TrustedExecutionReceipt,
    ) -> Result<NativeRuntimeTask, String> {
        self.transition_native_runtime_task_internal(
            workspace_scope,
            task_id,
            target_state,
            progress,
            detail,
            checkpoint,
            Some(trusted_execution_receipt),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_native_runtime_task_internal(
        &self,
        workspace_scope: &str,
        task_id: &str,
        target_state: &str,
        progress: u8,
        detail: &str,
        checkpoint: Option<&Value>,
        trusted_execution_receipt: Option<&TrustedExecutionReceipt>,
    ) -> Result<NativeRuntimeTask, String> {
        if !valid_runtime_task_state(target_state) {
            return Err("目标任务状态无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始任务状态事务：{error}"))?;
        let current = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        if !crate::task_runtime::valid_task_transition(&current.state, target_state) {
            return Err(format!(
                "不允许任务从 {} 转换为 {target_state}",
                current.state
            ));
        }
        let child_execution =
            read_runtime_child_execution_expectation(&transaction, workspace_scope, task_id)?;
        if target_state == "succeeded" {
            match (child_execution.as_ref(), trusted_execution_receipt) {
                (Some(expected), Some(receipt)) => {
                    validate_live_runtime_child_execution_expectation(
                        expected,
                        &Utc::now().to_rfc3339(),
                    )?;
                    validate_trusted_execution_receipt(
                        workspace_scope,
                        &current,
                        expected,
                        receipt,
                    )?;
                    validate_trusted_execution_usage_within_reservation(expected, receipt)?;
                }
                (Some(_), None) => {
                    return Err("绑定原生任务步骤的 Runtime 子任务缺少可信处理器回执".to_string());
                }
                (None, Some(_)) => {
                    return Err("非步骤绑定任务不能附加可信处理器回执".to_string());
                }
                (None, None) => {}
            }
        } else if trusted_execution_receipt.is_some() {
            return Err("可信处理器回执只能用于 Runtime 子任务成功结算".to_string());
        }
        let completion = if target_state == "succeeded" {
            let completion =
                evaluate_runtime_task_completion(&transaction, workspace_scope, task_id)?;
            let latest_plan_revision = transaction
                .query_row(
                    "SELECT revision FROM runtime_task_plans
                     WHERE workspace_scope=?1 AND task_id=?2 ORDER BY revision DESC LIMIT 1",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| format!("无法读取成功任务的计划版本：{error}"))?;
            if let Some(plan_revision) = latest_plan_revision {
                let step_run_count: i64 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM runtime_task_step_runs
                         WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                        params![workspace_scope, task_id, plan_revision],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("无法检查原生任务步骤执行记录：{error}"))?;
                let missing_steps: i64 = if step_run_count > 0 {
                    transaction
                        .query_row(
                            "SELECT COUNT(*)
                         FROM runtime_task_plan_steps step
                         WHERE step.workspace_scope=?1 AND step.task_id=?2
                           AND step.plan_revision=?3
                           AND NOT EXISTS(
                             SELECT 1 FROM runtime_task_step_receipts receipt
                             WHERE receipt.workspace_scope=step.workspace_scope
                               AND receipt.task_id=step.task_id
                               AND receipt.plan_revision=step.plan_revision
                               AND receipt.step_id=step.step_id
                               AND receipt.state='succeeded'
                           )",
                            params![workspace_scope, task_id, plan_revision],
                            |row| row.get(0),
                        )
                        .map_err(|error| format!("无法验证原生任务步骤完成状态：{error}"))?
                } else {
                    0
                };
                if missing_steps > 0 {
                    return Err(format!(
                        "原生任务计划仍有 {missing_steps} 个步骤未形成成功回执"
                    ));
                }
            }
            if let Some(status) = completion.as_ref().filter(|status| !status.satisfied) {
                let missing = status
                    .requirements
                    .iter()
                    .filter(|requirement| !requirement.satisfied)
                    .map(|requirement| requirement.id.as_str())
                    .take(16)
                    .collect::<Vec<_>>()
                    .join("、");
                return Err(format!(
                    "原生任务完成契约尚未满足，缺少不可变证据：{missing}"
                ));
            }
            completion
        } else {
            None
        };
        let now = Utc::now().to_rfc3339();
        if target_state == "cancelled" {
            cancel_runtime_task_step_claims(&transaction, workspace_scope, task_id, &now)?;
        }
        let current_state = current.state.clone();
        let title = current.title.clone();
        let trace_id = current
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        let created_at = current.created_at.clone();
        let mut payload = current.payload;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| "原生任务负载不是 JSON 对象".to_string())?;
        object.insert("state".to_string(), Value::String(target_state.to_string()));
        object.insert("progress".to_string(), Value::from(progress));
        object.insert("updatedAt".to_string(), Value::String(now.clone()));
        object.insert("traceId".to_string(), Value::String(trace_id.clone()));
        if let Some(receipt) = trusted_execution_receipt {
            object.insert(
                "trustedExecutionReceipt".to_string(),
                serde_json::to_value(receipt)
                    .map_err(|error| format!("无法序列化可信原生执行回执：{error}"))?,
            );
        }
        if let Some(completion) = completion.as_ref() {
            object.insert(
                "completionContract".to_string(),
                serde_json::to_value(completion)
                    .map_err(|error| format!("无法序列化任务完成契约状态：{error}"))?,
            );
        }
        if !detail.trim().is_empty() {
            object.insert(
                "result".to_string(),
                Value::String(detail.chars().take(4000).collect()),
            );
        }
        let serialized = serde_json::to_string(&payload)
            .map_err(|error| format!("无法序列化任务状态：{error}"))?;
        transaction
            .execute(
                "UPDATE runtime_tasks SET state=?3, trace_id=?4, payload=?5, updated_at=?6
                 WHERE workspace_scope=?1 AND id=?2",
                params![
                    workspace_scope,
                    task_id,
                    target_state,
                    trace_id,
                    serialized,
                    now
                ],
            )
            .map_err(|error| format!("无法更新任务状态：{error}"))?;
        transaction
            .execute(
                "UPDATE runtime_task_attempts SET finished_at=?3
                 WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                params![workspace_scope, task_id, now],
            )
            .map_err(|error| format!("无法结束任务尝试：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_task_attempts
                 (id, workspace_scope, task_id, state, detail, started_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    task_id,
                    target_state,
                    detail.chars().take(1000).collect::<String>(),
                    now,
                    if matches!(target_state, "succeeded" | "failed" | "cancelled") {
                        Some(now.clone())
                    } else {
                        None
                    },
                ],
            )
            .map_err(|error| format!("无法记录任务状态转换：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_task_transitions
                 (id, workspace_scope, task_id, from_state, to_state, detail, checkpoint_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    task_id,
                    current_state,
                    target_state,
                    detail.chars().take(2000).collect::<String>(),
                    serde_json::to_string(checkpoint.unwrap_or(&Value::Null))
                        .map_err(|error| format!("无法序列化转换检查点：{error}"))?,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存任务状态转换：{error}"))?;
        if let Some(checkpoint) = checkpoint.filter(|value| value.is_object()) {
            let checkpoint_id = checkpoint
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let checkpoint_json = serde_json::to_string(checkpoint)
                .map_err(|error| format!("无法序列化任务检查点：{error}"))?;
            let sequence = transaction
                .query_row(
                    "SELECT COALESCE(MAX(sequence), -1) + 1 FROM runtime_task_checkpoints
                     WHERE workspace_scope=?1 AND task_id=?2",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法生成任务检查点序号：{error}"))?;
            let checkpoint_state = if matches!(target_state, "failed" | "cancelled") {
                "failed"
            } else if target_state == "succeeded" {
                "completed"
            } else {
                "running"
            };
            transaction
                .execute(
                    "INSERT INTO runtime_task_checkpoints
                     (workspace_scope, task_id, checkpoint_id, sequence, state, payload,
                      payload_hash, created_at, updated_at, completed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
                     ON CONFLICT(workspace_scope, task_id, checkpoint_id) DO UPDATE SET
                       state=excluded.state, payload=excluded.payload,
                       payload_hash=excluded.payload_hash, updated_at=excluded.updated_at,
                       completed_at=excluded.completed_at",
                    params![
                        workspace_scope,
                        task_id,
                        checkpoint_id,
                        sequence,
                        checkpoint_state,
                        checkpoint_json,
                        format!("{:x}", Sha256::digest(checkpoint_json.as_bytes())),
                        now,
                        if checkpoint_state == "completed" {
                            Some(now.clone())
                        } else {
                            None
                        },
                    ],
                )
                .map_err(|error| format!("无法保存任务检查点：{error}"))?;
        }
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: Some(task_id.to_string()),
            trace_id: Some(trace_id.clone()),
            event_type: "task.state_changed".to_string(),
            state: target_state.to_string(),
            created_at: now.clone(),
            vault_id: payload
                .get("vaultId")
                .and_then(Value::as_str)
                .map(str::to_string),
            relative_path: None,
            detail: detail.chars().take(2000).collect(),
        };
        insert_operation_event_in_transaction(&transaction, &event)
            .map_err(|error| format!("无法保存任务状态审计事件：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: task_id,
                event_type: "task.state_changed",
                state: target_state,
                payload: &serde_json::json!({
                    "fromState": current_state,
                    "progress": progress,
                    "detail": detail,
                    "completionContract": completion,
                }),
                created_at: &now,
            },
        )?;
        let next = NativeRuntimeTask {
            id: task_id.to_string(),
            state: target_state.to_string(),
            title,
            trace_id: Some(trace_id),
            progress,
            payload,
            created_at,
            updated_at: now,
        };
        transaction
            .commit()
            .map_err(|error| format!("无法提交任务状态事务：{error}"))?;
        Ok(next)
    }

    pub fn ensure_vault_write_allowed(
        &self,
        workspace_scope: &str,
        vault_id: &str,
        relative_path: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let client_state = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 Vault 写入策略：{error}"))?
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .and_then(|snapshot| {
                snapshot
                    .get("clientState")
                    .or_else(|| snapshot.get("client_state"))
                    .cloned()
            })
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        evaluate_vault_write_policy(&client_state, vault_id, relative_path)
    }

    pub fn ensure_vault_read_allowed(
        &self,
        workspace_scope: &str,
        vault_id: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let client_state = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 Vault 读取策略：{error}"))?
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .and_then(|snapshot| {
                snapshot
                    .get("clientState")
                    .or_else(|| snapshot.get("client_state"))
                    .cloned()
            })
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        evaluate_vault_read_policy(&client_state, vault_id)
    }

    pub(crate) fn purge_unreadable_vault_indexes(
        &self,
        workspace_scope: &str,
        vaults: &[VaultDescriptor],
    ) -> Result<Vec<String>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("purge_unreadable_vault_indexes")
            .with_threshold(self.config.slow_query_threshold_ms);

        let unreadable_vault_ids = vaults
            .iter()
            .filter(|vault| vault.connection_state == "connected")
            .filter_map(|vault| {
                self.ensure_vault_read_allowed(workspace_scope, &vault.id)
                    .is_err()
                    .then_some(vault.id.clone())
            })
            .collect::<Vec<_>>();
        if unreadable_vault_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始 Vault 读取策略清理事务：{error}"))?;
        for vault_id in &unreadable_vault_ids {
            transaction
                .execute("DELETE FROM note_fts WHERE vault_id=?1", [vault_id])
                .map_err(|error| format!("无法清理禁用 Vault 的全文索引：{error}"))?;
            transaction
                .execute("DELETE FROM note_lexical_fts WHERE vault_id=?1", [vault_id])
                .map_err(|error| format!("无法清理禁用 Vault 的中文词法索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_feature_vectors WHERE vault_id=?1",
                    [vault_id],
                )
                .map_err(|error| format!("无法清理禁用 Vault 的本地特征向量：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_neural_embeddings WHERE workspace_scope=?1 AND vault_id=?2",
                    params![workspace_scope, vault_id],
                )
                .map_err(|error| format!("无法清理禁用 Vault 的神经 Embedding 引用：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM neural_embedding_index_state WHERE workspace_scope=?1 AND vault_id=?2",
                    params![workspace_scope, vault_id],
                )
                .map_err(|error| format!("无法清理禁用 Vault 的神经 Embedding 状态：{error}"))?;
            transaction
                .execute("DELETE FROM note_index WHERE vault_id=?1", [vault_id])
                .map_err(|error| format!("无法清理禁用 Vault 的笔记索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM vault_index_changes WHERE vault_id=?1",
                    [vault_id],
                )
                .map_err(|error| format!("无法清理禁用 Vault 的索引队列：{error}"))?;
        }
        transaction
            .execute(
                "DELETE FROM neural_embedding_cache
                 WHERE workspace_scope=?1 AND NOT EXISTS (
                   SELECT 1 FROM note_neural_embeddings e
                   WHERE e.workspace_scope=neural_embedding_cache.workspace_scope
                     AND e.provider_id=neural_embedding_cache.provider_id
                     AND e.model=neural_embedding_cache.model
                     AND e.input_hash=neural_embedding_cache.input_hash
                 )",
                [workspace_scope],
            )
            .map_err(|error| format!("无法清理禁用 Vault 的孤立 Embedding 缓存：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 读取策略清理事务：{error}"))?;
        Ok(unreadable_vault_ids)
    }

    fn readable_indexed_vault_ids(&self, workspace_scope: &str) -> Result<Vec<String>, String> {
        let vault_ids = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            let mut statement = connection
                .prepare("SELECT DISTINCT vault_id FROM note_index ORDER BY vault_id")
                .map_err(|error| format!("无法准备索引 Vault 查询：{error}"))?;
            let collected = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法读取索引 Vault：{error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法解析索引 Vault：{error}"))?;
            collected
        };
        Ok(vault_ids
            .into_iter()
            .filter(|vault_id| {
                self.ensure_vault_read_allowed(workspace_scope, vault_id)
                    .is_ok()
            })
            .collect())
    }

    pub fn list_native_operation_events(
        &self,
        limit: usize,
    ) -> Result<Vec<OperationEvent>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM operation_events
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|error| format!("无法查询原生操作日志：{error}"))?;
        let rows = statement
            .query_map([limit.clamp(1, 1000) as i64], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取原生操作日志：{error}"))?;
        let mut events = rows
            .filter_map(Result::ok)
            .filter_map(|payload| serde_json::from_str::<OperationEvent>(&payload).ok())
            .collect::<Vec<_>>();
        events.reverse();
        Ok(events)
    }

    pub fn rebuild_index_for_vault_with_cancellation<F>(
        &self,
        vault_id: &str,
        is_cancelled: &F,
    ) -> Result<IndexBuildResult, String>
    where
        F: Fn() -> bool,
    {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("rebuild_index_for_vault")
            .with_threshold(self.config.slow_query_threshold_ms);

        let workspace_scope = self.local_workspace_scope()?;
        self.ensure_vault_read_allowed(&workspace_scope, vault_id)?;
        let (_, root) = resolve_vault_for_runtime(vault_id)?;
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files_for_runtime_with_cancellation(
            &root,
            &mut markdown,
            &mut attachments,
            is_cancelled,
        )?;
        ensure_index_not_cancelled(is_cancelled)?;

        log::info!(
            "开始索引重建: vault={}, 文件数={}, 批处理大小={}",
            vault_id,
            markdown.len(),
            self.config.vault_index_batch_size
        );

        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始索引事务：{error}"))?;
        transaction
            .execute("DELETE FROM note_fts WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理全文索引：{error}"))?;
        transaction
            .execute("DELETE FROM note_lexical_fts WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理中文词法索引：{error}"))?;
        transaction
            .execute(
                "DELETE FROM note_feature_vectors WHERE vault_id=?1",
                [vault_id],
            )
            .map_err(|error| format!("无法清理本地特征向量：{error}"))?;
        transaction
            .execute("DELETE FROM note_index WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理笔记索引：{error}"))?;

        let mut indexed_notes = 0;
        let mut skipped_notes = 0;

        // 使用配置的批处理大小
        for (batch_idx, batch) in markdown.chunks(self.config.vault_index_batch_size).enumerate() {
            ensure_index_not_cancelled(is_cancelled)?;

            for path in batch {
                match prepare_note_index(&root, path).and_then(|note| {
                    note.map(|note| upsert_prepared_note_index(&transaction, vault_id, &note))
                        .transpose()
                }) {
                    Ok(Some(())) => indexed_notes += 1,
                    Ok(None) | Err(_) => skipped_notes += 1,
                }
            }

            // 每批次完成后记录进度
            if batch_idx % 10 == 0 && batch_idx > 0 {
                log::debug!(
                    "索引进度: {}/{} 文件已处理",
                    indexed_notes + skipped_notes,
                    markdown.len()
                );
            }
        }

        ensure_index_not_cancelled(is_cancelled)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交索引事务：{error}"))?;

        log::info!(
            "索引重建完成: vault={}, 成功={}, 跳过={}",
            vault_id,
            indexed_notes,
            skipped_notes
        );

        Ok(IndexBuildResult {
            vault_id: vault_id.to_string(),
            indexed_notes,
            skipped_notes,
            completed_at: Utc::now().to_rfc3339(),
        })
    }

    pub(crate) fn enqueue_vault_index_path(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
    ) -> Result<(), String> {
        self.enqueue_vault_index_path_inner(vault_id, root, path, None)
    }

    pub(crate) fn enqueue_vault_index_path_with_trace(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
        trace_id: &str,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("enqueue_vault_index_path_with_trace")
            .with_threshold(self.config.slow_query_threshold_ms);

        self.enqueue_vault_index_path_inner(vault_id, root, path, Some(trace_id))
    }

    fn enqueue_vault_index_path_inner(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
        inherited_trace_id: Option<&str>,
    ) -> Result<(), String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("enqueue_vault_index_path")
            .with_threshold(self.config.slow_query_threshold_ms);

        let workspace_scope = self.local_workspace_scope()?;
        self.ensure_vault_read_allowed(&workspace_scope, vault_id)?;
        let relative_path = normalized_index_relative_path(root, path)?;
        validate_index_relative_path(&relative_path)?;
        let canonical_root = canonical_index_root(root)?;
        let change_kind = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => "upsert",
            Ok(_) => "delete",
            Err(error) if error.kind() == ErrorKind::NotFound => "delete",
            Err(error) => return Err(format!("无法读取索引变更文件：{error}")),
        };
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let now = Utc::now();
        let generated_trace_id;
        let (trace_id, replace_existing_trace) = if let Some(trace_id) = inherited_trace_id {
            (crate::trace::validate_trace_id(trace_id)?, true)
        } else {
            generated_trace_id = crate::trace::new_trace_id();
            (generated_trace_id.as_str(), false)
        };
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        ensure_registered_vault_root(&connection, vault_id, &canonical_root)?;
        enqueue_vault_index_change_in_connection(
            &connection,
            vault_id,
            root_text,
            &relative_path,
            change_kind,
            trace_id,
            replace_existing_trace,
            now.timestamp_millis() + VAULT_INDEX_DEBOUNCE_MS,
            &now.to_rfc3339(),
        )?;
        Ok(())
    }

    pub(crate) fn recover_vault_index_changes(&self) -> Result<usize, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("recover_vault_index_changes")
            .with_threshold(self.config.slow_query_threshold_ms);

        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引恢复事务：{error}"))?;
        let recovered = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state='pending', available_at_ms=?1, claimed_at_ms=NULL, updated_at=?2
                 WHERE state='processing' AND attempt_count < ?3",
                params![now.timestamp_millis(), now_text, VAULT_INDEX_MAX_ATTEMPTS,],
            )
            .map_err(|error| format!("无法恢复中断的 Vault 索引任务：{error}"))?;
        let dead_lettered = dead_letter_exhausted_vault_index_changes(
            &transaction,
            "processing",
            "应用退出前索引任务未完成",
            "startup_recovery",
            &now_text,
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引恢复事务：{error}"))?;
        Ok(recovered + dead_lettered)
    }

    pub(crate) fn claim_vault_index_changes(
        &self,
        limit: usize,
    ) -> Result<Vec<ClaimedVaultIndexChange>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("claim_vault_index_changes")
            .with_threshold(self.config.slow_query_threshold_ms);

        let now = Utc::now();
        let now_ms = now.timestamp_millis();
        let now_text = now.to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引认领事务：{error}"))?;
        dead_letter_exhausted_vault_index_changes(
            &transaction,
            "pending",
            "Vault 索引任务超过最大重试次数",
            "claim_sweep",
            &now_text,
        )?;
        let candidates = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, vault_id, canonical_root, relative_path, generation, attempt_count, trace_id
                     FROM vault_index_changes
                     WHERE state='pending' AND attempt_count < ?1 AND available_at_ms <= ?2
                     ORDER BY available_at_ms, id LIMIT ?3",
                )
                .map_err(|error| format!("无法准备 Vault 索引认领查询：{error}"))?;
            let rows = statement
                .query_map(
                    params![VAULT_INDEX_MAX_ATTEMPTS, now_ms, limit.clamp(1, 128) as i64],
                    |row| {
                        Ok(ClaimedVaultIndexChange {
                            id: row.get(0)?,
                            vault_id: row.get(1)?,
                            canonical_root: PathBuf::from(row.get::<_, String>(2)?),
                            relative_path: row.get(3)?,
                            generation: row.get(4)?,
                            attempt_count: row.get::<_, i64>(5)? + 1,
                            trace_id: row.get(6)?,
                        })
                    },
                )
                .map_err(|error| format!("无法读取待处理 Vault 索引任务：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法解析待处理 Vault 索引任务：{error}"))?
        };
        let mut claimed = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let changed = transaction
                .execute(
                    "UPDATE vault_index_changes
                     SET state='processing', attempt_count=attempt_count+1,
                         claimed_at_ms=?1, updated_at=?2
                     WHERE id=?3 AND generation=?4 AND state='pending'
                       AND attempt_count < ?5 AND available_at_ms <= ?1",
                    params![
                        now_ms,
                        now_text,
                        candidate.id,
                        candidate.generation,
                        VAULT_INDEX_MAX_ATTEMPTS
                    ],
                )
                .map_err(|error| format!("无法认领 Vault 索引任务：{error}"))?;
            if changed == 1 {
                crate::trace::record_trace_event_in_connection(
                    &transaction,
                    DEFAULT_LOCAL_WORKSPACE_SCOPE,
                    &crate::trace::TraceEventRecord {
                        trace_id: &candidate.trace_id,
                        entity_kind: "index_change",
                        entity_id: &format!("{}:{}", candidate.id, candidate.generation),
                        event_type: "index.claimed",
                        state: "processing",
                        payload: &serde_json::json!({
                            "vaultId": candidate.vault_id,
                            "relativePath": candidate.relative_path,
                            "attemptCount": candidate.attempt_count,
                        }),
                        created_at: &now_text,
                    },
                )?;
                claimed.push(candidate);
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引认领事务：{error}"))?;
        Ok(claimed)
    }

    pub(crate) fn apply_claimed_vault_index_change(
        &self,
        change: &ClaimedVaultIndexChange,
        current_root: &Path,
    ) -> Result<Option<AppliedVaultIndexChange>, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("apply_claimed_vault_index_change")
            .with_threshold(self.config.slow_query_threshold_ms);

        let canonical_root = canonical_index_root(current_root)?;
        if canonical_root != change.canonical_root {
            return Err("Vault 根目录已变化，拒绝应用旧索引任务".to_string());
        }
        let target = resolve_index_target(&canonical_root, &change.relative_path)?;
        let prepared = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => {
                prepare_note_index(&canonical_root, &target)?
            }
            Ok(_) => None,
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(format!("无法读取待索引笔记：{error}")),
        };
        if prepared
            .as_ref()
            .is_some_and(|note| note.relative_path != change.relative_path)
        {
            return Err("索引任务路径规范化后发生变化".to_string());
        }
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引提交事务：{error}"))?;
        let owns_change = transaction
            .query_row(
                "SELECT 1 FROM vault_index_changes
                 WHERE id=?1 AND vault_id=?2 AND canonical_root=?3 AND relative_path=?4
                   AND generation=?5 AND state='processing'",
                params![
                    change.id,
                    change.vault_id,
                    root_text,
                    change.relative_path,
                    change.generation
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法校验 Vault 索引任务所有权：{error}"))?
            .is_some();
        if !owns_change {
            return Ok(None);
        }
        ensure_registered_vault_root(&transaction, &change.vault_id, &canonical_root)?;
        let change_kind = if let Some(note) = prepared.as_ref() {
            upsert_prepared_note_index(&transaction, &change.vault_id, note)?;
            "upsert"
        } else {
            delete_note_index_in_transaction(
                &transaction,
                &change.vault_id,
                &change.relative_path,
            )?;
            "delete"
        };
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: None,
            trace_id: Some(change.trace_id.clone()),
            event_type: "vault.note.index".to_string(),
            state: "success".to_string(),
            created_at: Utc::now().to_rfc3339(),
            vault_id: Some(change.vault_id.clone()),
            relative_path: Some(change.relative_path.clone()),
            detail: format!("Vault 索引队列已完成 {change_kind}"),
        };
        insert_operation_event_in_transaction(&transaction, &event)?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &change.trace_id,
                entity_kind: "index_change",
                entity_id: &format!("{}:{}", change.id, change.generation),
                event_type: "index.completed",
                state: "succeeded",
                payload: &serde_json::json!({
                    "vaultId": change.vault_id,
                    "relativePath": change.relative_path,
                    "changeKind": change_kind,
                }),
                created_at: &event.created_at,
            },
        )?;
        let deleted = transaction
            .execute(
                "DELETE FROM vault_index_changes
                 WHERE id=?1 AND generation=?2 AND state='processing'",
                params![change.id, change.generation],
            )
            .map_err(|error| format!("无法完成 Vault 索引任务：{error}"))?;
        if deleted != 1 {
            return Err("Vault 索引任务在提交期间被新事件替换".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引变更：{error}"))?;
        Ok(Some(AppliedVaultIndexChange {
            vault_id: change.vault_id.clone(),
            relative_path: change.relative_path.clone(),
            change_kind: change_kind.to_string(),
        }))
    }

    pub(crate) fn discard_claimed_vault_index_change(
        &self,
        change: &ClaimedVaultIndexChange,
        reason: &str,
    ) -> Result<bool, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("discard_claimed_vault_index_change")
            .with_threshold(self.config.slow_query_threshold_ms);

        let now = Utc::now().to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引取消事务：{error}"))?;
        let deleted = transaction
            .execute(
                "DELETE FROM vault_index_changes
                 WHERE id=?1 AND generation=?2 AND state='processing'",
                params![change.id, change.generation],
            )
            .map_err(|error| format!("无法取消 Vault 索引任务：{error}"))?
            == 1;
        if deleted {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &crate::trace::TraceEventRecord {
                    trace_id: &change.trace_id,
                    entity_kind: "index_change",
                    entity_id: &format!("{}:{}", change.id, change.generation),
                    event_type: "index.cancelled",
                    state: "cancelled",
                    payload: &serde_json::json!({
                        "vaultId": change.vault_id,
                        "relativePath": change.relative_path,
                        "reason": reason,
                    }),
                    created_at: &now,
                },
            )?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引取消事务：{error}"))?;
        Ok(deleted)
    }

    pub(crate) fn fail_claimed_vault_index_change(
        &self,
        change: &ClaimedVaultIndexChange,
        error: &str,
    ) -> Result<VaultIndexFailureOutcome, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("fail_claimed_vault_index_change")
            .with_threshold(self.config.slow_query_threshold_ms);

        let terminal = change.attempt_count >= VAULT_INDEX_MAX_ATTEMPTS;
        let retry_delay = VAULT_INDEX_RETRY_BASE_MS
            .saturating_mul(1_i64 << change.attempt_count.saturating_sub(1).min(6));
        let now = Utc::now();
        let error = error.chars().take(2_000).collect::<String>();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|db_error| format!("无法开始 Vault 索引失败事务：{db_error}"))?;
        let updated = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state=?1, available_at_ms=?2, claimed_at_ms=NULL,
                     last_error=?3, updated_at=?4
                 WHERE id=?5 AND generation=?6 AND state='processing'",
                params![
                    if terminal { "dead_letter" } else { "pending" },
                    now.timestamp_millis() + retry_delay,
                    error,
                    now.to_rfc3339(),
                    change.id,
                    change.generation
                ],
            )
            .map_err(|db_error| format!("无法记录 Vault 索引失败：{db_error}"))?
            == 1;
        if updated {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &crate::trace::TraceEventRecord {
                    trace_id: &change.trace_id,
                    entity_kind: "index_change",
                    entity_id: &format!("{}:{}", change.id, change.generation),
                    event_type: if terminal {
                        "index.dead_lettered"
                    } else {
                        "index.retry_scheduled"
                    },
                    state: if terminal { "dead_letter" } else { "pending" },
                    payload: &serde_json::json!({
                        "vaultId": change.vault_id,
                        "relativePath": change.relative_path,
                        "attemptCount": change.attempt_count,
                        "error": error,
                    }),
                    created_at: &now.to_rfc3339(),
                },
            )?;
        }
        if updated && terminal {
            let event = OperationEvent {
                id: Uuid::new_v4().to_string(),
                task_id: None,
                trace_id: Some(change.trace_id.clone()),
                event_type: "vault.note.index".to_string(),
                state: "failed".to_string(),
                created_at: now.to_rfc3339(),
                vault_id: Some(change.vault_id.clone()),
                relative_path: Some(change.relative_path.clone()),
                detail: format!("Vault 索引任务重试耗尽：{error}"),
            };
            insert_operation_event_in_transaction(&transaction, &event)?;
        }
        transaction
            .commit()
            .map_err(|db_error| format!("无法提交 Vault 索引失败状态：{db_error}"))?;
        Ok(VaultIndexFailureOutcome { updated, terminal })
    }

    pub(crate) fn reconcile_vault_index(
        &self,
        vault: &VaultDescriptor,
    ) -> Result<VaultIndexReconcileResult, String> {
        if vault.connection_state != "connected" {
            return Err("只能校准已连接的 Vault".to_string());
        }
        let workspace_scope = self.local_workspace_scope()?;
        self.ensure_vault_read_allowed(&workspace_scope, &vault.id)?;
        let canonical_root = canonical_index_root(Path::new(&vault.path))?;
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files_for_runtime_with_cancellation(
            &canonical_root,
            &mut markdown,
            &mut attachments,
            &|| false,
        )?;
        let mut current_paths = HashSet::with_capacity(markdown.len());
        for path in markdown {
            let relative_path = normalized_index_relative_path(&canonical_root, &path)?;
            validate_index_relative_path(&relative_path)?;
            current_paths.insert(relative_path);
        }

        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let now_ms = now.timestamp_millis();
        let reconcile_trace_id = crate::trace::new_trace_id();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引校准事务：{error}"))?;
        ensure_registered_vault_root(&transaction, &vault.id, &canonical_root)?;
        let known_paths = {
            let mut statement = transaction
                .prepare(
                    "SELECT relative_path FROM note_index WHERE vault_id=?1
                     UNION
                     SELECT relative_path FROM vault_index_changes WHERE vault_id=?1",
                )
                .map_err(|error| format!("无法准备 Vault 索引校准查询：{error}"))?;
            let rows = statement
                .query_map([&vault.id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法读取 Vault 索引校准基线：{error}"))?;
            rows.collect::<Result<HashSet<_>, _>>()
                .map_err(|error| format!("无法解析 Vault 索引校准基线：{error}"))?
        };
        for relative_path in &current_paths {
            enqueue_vault_index_change_in_connection(
                &transaction,
                &vault.id,
                root_text,
                relative_path,
                "upsert",
                &reconcile_trace_id,
                false,
                now_ms,
                &now_text,
            )?;
        }
        let mut queued_deletes = 0;
        for relative_path in known_paths.difference(&current_paths) {
            let normalized = normalize_queued_relative_path(relative_path)?;
            validate_index_relative_path(&normalized)?;
            enqueue_vault_index_change_in_connection(
                &transaction,
                &vault.id,
                root_text,
                &normalized,
                "delete",
                &reconcile_trace_id,
                false,
                now_ms,
                &now_text,
            )?;
            queued_deletes += 1;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引校准事务：{error}"))?;
        Ok(VaultIndexReconcileResult {
            queued_upserts: current_paths.len(),
            queued_deletes,
        })
    }

    fn health(&self, workspace_scope: &str) -> Result<DatabaseHealth, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let journal_mode = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|error| format!("无法读取 WAL 状态：{error}"))?;
        let integrity = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| format!("无法执行完整性检查：{error}"))?;
        let schema_version = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("无法读取 schema 版本：{error}"))?;
        let workspace_snapshot = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区统计：{error}"))?
            .and_then(|payload| serde_json::from_str::<Value>(&payload).ok());
        let workspace_count = |key: &str| {
            workspace_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.get(key))
                .and_then(Value::as_array)
                .map(|items| items.len() as i64)
                .unwrap_or(0)
        };
        Ok(DatabaseHealth {
            path: self.path.to_string_lossy().into_owned(),
            journal_mode,
            integrity,
            schema_version,
            task_count: workspace_count("tasks"),
            approval_count: workspace_count("approvals"),
            message_count: connection
                .query_row(
                    "SELECT COUNT(*) FROM workspace_messages WHERE workspace_scope=?1",
                    [workspace_scope],
                    |row| row.get(0),
                )
                .map_err(|error| format!("无法统计独立消息记录：{error}"))?,
            operation_event_count: table_count(&connection, "operation_events")?,
            indexed_note_count: table_count(&connection, "note_index")?,
        })
    }

    fn backup(&self) -> Result<DatabaseBackupResult, String> {
        let created_at = Utc::now();
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let filename = format!("yunspire-{}.sqlite", created_at.format("%Y%m%d-%H%M%S"));
        let backup_path = backup_dir.join(filename);
        let source = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut destination = Connection::open(&backup_path)
            .map_err(|error| format!("无法创建 SQLite 备份：{error}"))?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)
            .map_err(|error| format!("无法初始化 SQLite 在线备份：{error}"))?;
        if let Err(error) = backup.run_to_completion(64, std::time::Duration::from_millis(10), None)
        {
            drop(backup);
            drop(destination);
            let _ = fs::remove_file(&backup_path);
            return Err(format!("SQLite 在线备份失败：{error}"));
        }
        drop(backup);
        drop(destination);
        #[cfg(unix)]
        fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法限制备份文件权限：{error}"))?;
        let byte_length = fs::metadata(&backup_path)
            .map_err(|error| format!("无法验证备份文件：{error}"))?
            .len();
        Ok(DatabaseBackupResult {
            path: backup_path.to_string_lossy().into_owned(),
            byte_length,
            created_at: created_at.to_rfc3339(),
        })
    }

    pub(crate) fn backup_for_runtime(&self) -> Result<DatabaseBackupResult, String> {
        self.backup()
    }

    fn backup_dir(&self) -> Result<PathBuf, String> {
        self.path
            .parent()
            .map(|parent| parent.join("backups"))
            .ok_or_else(|| "无法定位数据库备份目录".to_string())
    }

    fn validate_backup_path(&self, requested_path: &str) -> Result<PathBuf, String> {
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let canonical_dir = backup_dir
            .canonicalize()
            .map_err(|error| format!("无法规范化备份目录：{error}"))?;
        let requested = PathBuf::from(requested_path.trim());
        let canonical = requested
            .canonicalize()
            .map_err(|error| format!("无法读取指定备份：{error}"))?;
        if canonical.parent() != Some(canonical_dir.as_path())
            || canonical.extension().and_then(|value| value.to_str()) != Some("sqlite")
            || !canonical.is_file()
        {
            return Err("只能恢复 Yunspire 本地备份目录中的 SQLite 备份".to_string());
        }
        Ok(canonical)
    }

    fn preflight_restore(&self, requested_path: &str) -> Result<DatabaseRestorePreflight, String> {
        let path = self.validate_backup_path(requested_path)?;
        inspect_backup(&path)
    }

    fn list_backups(&self) -> Result<Vec<DatabaseBackupInfo>, String> {
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let mut backups = fs::read_dir(&backup_dir)
            .map_err(|error| format!("无法读取备份目录：{error}"))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
                    && entry.file_name().to_string_lossy().starts_with("yunspire-")
            })
            .filter_map(|entry| {
                let preflight = inspect_backup(&entry.path()).ok()?;
                Some(DatabaseBackupInfo {
                    path: preflight.path,
                    file_name: preflight.file_name,
                    byte_length: preflight.byte_length,
                    modified_at: entry
                        .metadata()
                        .ok()?
                        .modified()
                        .ok()
                        .map(chrono::DateTime::<Utc>::from)?
                        .to_rfc3339(),
                    schema_version: preflight.schema_version,
                    integrity: preflight.integrity,
                })
            })
            .collect::<Vec<_>>();
        backups.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
        Ok(backups)
    }

    fn restore(&self, requested_path: &str) -> Result<DatabaseRestoreResult, String> {
        let preflight = self.preflight_restore(requested_path)?;
        if !preflight.compatible {
            return Err(format!("备份恢复预检未通过：{}", preflight.reason));
        }
        let source_path = PathBuf::from(&preflight.path);
        let backup_dir = self.backup_dir()?;
        let restored_at = Utc::now();
        let safety_path = backup_dir.join(format!(
            "yunspire-before-restore-{}.sqlite",
            restored_at.format("%Y%m%d-%H%M%S-%3f")
        ));
        let mut destination = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut safety = Connection::open(&safety_path)
            .map_err(|error| format!("无法创建恢复前安全备份：{error}"))?;
        copy_database(&destination, &mut safety)
            .map_err(|error| format!("无法创建恢复前安全备份：{error}"))?;
        drop(safety);
        #[cfg(unix)]
        fs::set_permissions(&safety_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法限制恢复前备份权限：{error}"))?;

        let restore_result = (|| -> Result<(i64, String), String> {
            let source =
                Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|error| format!("无法以只读方式打开恢复来源：{error}"))?;
            copy_database(&source, &mut destination)?;
            drop(source);
            run_migrations(&destination)?;
            restore_database_runtime_configuration(&destination)
                .map_err(|error| format!("无法恢复 SQLite 运行参数：{error}"))?;
            let integrity = database_integrity(&destination)?;
            if integrity != "ok" {
                return Err(format!("恢复后的数据库完整性检查失败：{integrity}"));
            }
            let schema_version = database_schema_version(&destination)?;
            Ok((schema_version, integrity))
        })();

        let (schema_version, integrity) = match restore_result {
            Ok(result) => result,
            Err(error) => {
                let safety_source =
                    Connection::open_with_flags(&safety_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .map_err(|rollback_error| {
                            format!("{error}；同时无法打开恢复前安全备份：{rollback_error}")
                        })?;
                copy_database(&safety_source, &mut destination).map_err(|rollback_error| {
                    format!("{error}；自动回滚也失败：{rollback_error}")
                })?;
                return Err(format!("{error}；已自动回滚到恢复前状态"));
            }
        };
        Ok(DatabaseRestoreResult {
            restored_from: preflight.path,
            safety_backup: safety_path.to_string_lossy().into_owned(),
            schema_version,
            integrity,
            restored_at: restored_at.to_rfc3339(),
        })
    }

    pub(crate) fn restore_for_runtime(
        &self,
        requested_path: &str,
    ) -> Result<DatabaseRestoreResult, String> {
        // 性能监控
        let _profiler = crate::database::QueryProfiler::new("restore_for_runtime")
            .with_threshold(self.config.slow_query_threshold_ms);

        self.restore(requested_path)
    }

    fn optimization_evidence(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<OptimizationEvidenceBatch, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let cursor = connection
            .query_row(
                "SELECT revision, last_occurred_at, last_event_id
                 FROM optimization_cursors WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化游标：{error}"))?;
        let requested = limit.clamp(1, 500);
        let mut statement = connection
            .prepare(
                "SELECT id, event_type, occurred_at, payload
                 FROM long_term_memory_events e
                 WHERE e.workspace_scope=?1 AND e.state='committed'
                   AND (e.occurred_at>?2 OR (e.occurred_at=?2 AND e.id>?3))
                   AND NOT EXISTS (
                     SELECT 1 FROM long_term_memory_governance g
                     WHERE g.workspace_scope=e.workspace_scope AND g.memory_id=e.id
                       AND (g.status!='active' OR (g.expires_at IS NOT NULL AND g.expires_at<=?4))
                   )
                 ORDER BY e.occurred_at, e.id LIMIT ?5",
            )
            .map_err(|error| format!("无法准备后台优化证据查询：{error}"))?;
        let rows = statement
            .query_map(
                params![
                    workspace_scope,
                    cursor.1,
                    cursor.2,
                    Utc::now().to_rfc3339(),
                    (requested + 1) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化证据：{error}"))?;
        let mut events = Vec::new();
        let mut next_occurred_at = cursor.1.clone();
        let mut next_event_id = cursor.2.clone();
        let mut has_more = false;
        for row in rows.filter_map(Result::ok) {
            if events.len() >= requested {
                has_more = true;
                break;
            }
            let payload: Value = serde_json::from_str(&row.3)
                .map_err(|error| format!("优化证据 {} 的载荷损坏：{error}", row.0))?;
            let content = payload
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(6000)
                .collect::<String>();
            if content.trim().is_empty() {
                next_occurred_at = row.2;
                next_event_id = row.0;
                continue;
            }
            events.push(OptimizationEvidenceEvent {
                id: row.0.clone(),
                event_type: row.1,
                occurred_at: row.2.clone(),
                actor: payload
                    .get("actor")
                    .and_then(Value::as_str)
                    .unwrap_or("system")
                    .to_string(),
                content,
                metadata: payload
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
            });
            next_occurred_at = row.2;
            next_event_id = row.0;
        }
        Ok(OptimizationEvidenceBatch {
            cursor_revision: cursor.0,
            cursor_occurred_at: cursor.1,
            cursor_event_id: cursor.2,
            next_occurred_at,
            next_event_id,
            events,
            has_more,
        })
    }

    fn create_optimization_candidate(
        &self,
        workspace_scope: &str,
        input: OptimizationCandidateInput,
        mutation_key: Option<&RuntimeEffectMutationKey>,
    ) -> Result<OptimizationCandidateResult, String> {
        validate_optimization_candidate_input(&input)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始后台优化候选事务：{error}"))?;
        if let Some(key) = mutation_key {
            if let Some(result) =
                read_runtime_effect_mutation_result(&transaction, workspace_scope, key)?
            {
                transaction
                    .commit()
                    .map_err(|error| format!("无法完成后台优化候选幂等重放：{error}"))?;
                return Ok(result);
            }
        }
        let existing = transaction
            .query_row(
                "SELECT base_version, candidate_version, summary, rules_json,
                        skill_hints_json, metrics_json, evidence_count,
                        evidence_occurred_at, evidence_event_id, created_at, expires_at
                 FROM optimization_candidates
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, input.id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法检查后台优化候选重放：{error}"))?;
        if let Some(existing) = existing {
            let rules = serde_json::from_str::<Vec<String>>(&existing.3)
                .map_err(|error| format!("优化候选规则载荷损坏：{error}"))?;
            let skill_hints = serde_json::from_str::<Value>(&existing.4)
                .map_err(|error| format!("优化候选 Skill 提示载荷损坏：{error}"))?;
            let metrics = serde_json::from_str::<Value>(&existing.5)
                .map_err(|error| format!("优化候选指标载荷损坏：{error}"))?;
            if existing.2 != input.summary.trim()
                || rules != input.rules
                || skill_hints != input.skill_hints
                || metrics != input.metrics
                || existing.6 != input.evidence_count as i64
                || existing.7 != input.evidence_cursor_occurred_at
                || existing.8 != input.evidence_cursor_event_id
                || existing.10 != input.expires_at
            {
                return Err("后台优化候选 ID 已绑定到不同请求".to_string());
            }
            let result = OptimizationCandidateResult {
                id: input.id,
                base_version: existing.0,
                candidate_version: existing.1,
                state: "pending_evaluation".to_string(),
                summary: existing.2,
                rules,
                skill_hints,
                metrics,
                evidence_count: usize::try_from(existing.6).unwrap_or_default(),
                created_at: existing.9,
                evaluated_at: None,
                expires_at: existing.10,
            };
            if let Some(key) = mutation_key {
                persist_runtime_effect_mutation_result(
                    &transaction,
                    workspace_scope,
                    key,
                    &result,
                )?;
            }
            transaction
                .commit()
                .map_err(|error| format!("无法完成后台优化候选重放：{error}"))?;
            return Ok(result);
        }
        let (cursor_revision, cursor_occurred_at, cursor_event_id) = transaction
            .query_row(
                "SELECT revision, last_occurred_at, last_event_id
                 FROM optimization_cursors WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化游标：{error}"))?;
        if cursor_revision != input.expected_cursor_revision {
            return Err("后台优化证据已被另一轮复盘领取，请重新读取增量证据".to_string());
        }
        if input.evidence_cursor_occurred_at < cursor_occurred_at
            || (input.evidence_cursor_occurred_at == cursor_occurred_at
                && input.evidence_cursor_event_id < cursor_event_id)
        {
            return Err("后台优化候选游标不能回退".to_string());
        }
        let base_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        let now = Utc::now().to_rfc3339();
        let rules_json = serde_json::to_string(&input.rules)
            .map_err(|error| format!("无法序列化优化规则：{error}"))?;
        let skill_hints_json = serde_json::to_string(&input.skill_hints)
            .map_err(|error| format!("无法序列化 Skill 优化提示：{error}"))?;
        let metrics_json = serde_json::to_string(&input.metrics)
            .map_err(|error| format!("无法序列化优化指标：{error}"))?;
        transaction
            .execute(
                "INSERT INTO optimization_candidates
                 (workspace_scope, id, base_version, candidate_version, state, summary,
                  rules_json, skill_hints_json, metrics_json, evidence_count,
                  evidence_occurred_at, evidence_event_id, created_at, evaluated_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending_evaluation', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13)",
                params![
                    workspace_scope,
                    input.id,
                    base_version,
                    base_version + 1,
                    input.summary.trim(),
                    rules_json,
                    skill_hints_json,
                    metrics_json,
                    input.evidence_count as i64,
                    input.evidence_cursor_occurred_at,
                    input.evidence_cursor_event_id,
                    now,
                    input.expires_at,
                ],
            )
            .map_err(|error| format!("无法保存后台优化候选：{error}"))?;
        let advanced = transaction
            .execute(
                "UPDATE optimization_cursors
                 SET revision=revision+1, last_occurred_at=?2, last_event_id=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND revision=?5",
                params![
                    workspace_scope,
                    input.evidence_cursor_occurred_at,
                    input.evidence_cursor_event_id,
                    now,
                    input.expected_cursor_revision,
                ],
            )
            .map_err(|error| format!("无法推进后台优化游标：{error}"))?;
        if advanced != 1 {
            return Err("后台优化游标推进失败，候选没有提交".to_string());
        }
        let result = OptimizationCandidateResult {
            id: input.id,
            base_version,
            candidate_version: base_version + 1,
            state: "pending_evaluation".to_string(),
            summary: input.summary.trim().to_string(),
            rules: input.rules,
            skill_hints: input.skill_hints,
            metrics: input.metrics,
            evidence_count: input.evidence_count,
            created_at: now,
            evaluated_at: None,
            expires_at: input.expires_at,
        };
        if let Some(key) = mutation_key {
            persist_runtime_effect_mutation_result(&transaction, workspace_scope, key, &result)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交后台优化候选：{error}"))?;
        Ok(result)
    }

    fn evaluate_optimization_candidate(
        &self,
        workspace_scope: &str,
        candidate_id: &str,
        mutation_key: Option<&RuntimeEffectMutationKey>,
    ) -> Result<OptimizationEvaluationResult, String> {
        if !valid_runtime_identifier(candidate_id, 160) {
            return Err("优化候选 ID 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始优化候选评估事务：{error}"))?;
        if let Some(key) = mutation_key {
            if let Some(result) =
                read_runtime_effect_mutation_result(&transaction, workspace_scope, key)?
            {
                transaction
                    .commit()
                    .map_err(|error| format!("无法完成优化候选评估幂等重放：{error}"))?;
                return Ok(result);
            }
        }
        let row = transaction
            .query_row(
                "SELECT base_version, state, summary, rules_json, skill_hints_json,
                        evidence_count, expires_at
                 FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取优化候选：{error}"))?
            .ok_or_else(|| "优化候选不存在".to_string())?;
        if row.1 != "pending_evaluation" {
            let result = transaction
                .query_row(
                    "SELECT state, checks_json, evaluated_at
                     FROM optimization_evaluations
                     WHERE workspace_scope=?1 AND candidate_id=?2
                     ORDER BY evaluated_at DESC, id DESC LIMIT 1",
                    params![workspace_scope, candidate_id],
                    |evaluation| {
                        Ok((
                            evaluation.get::<_, String>(0)?,
                            evaluation.get::<_, String>(1)?,
                            evaluation.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("无法读取优化候选评估重放：{error}"))?
                .ok_or_else(|| format!("优化候选当前状态为 {}，但缺少评估结果", row.1))?;
            let result = OptimizationEvaluationResult {
                candidate_id: candidate_id.to_string(),
                passed: result.0 == "pending_review",
                state: result.0,
                checks: serde_json::from_str(&result.1)
                    .map_err(|error| format!("优化候选评估载荷损坏：{error}"))?,
                evaluated_at: result.2,
            };
            if let Some(key) = mutation_key {
                persist_runtime_effect_mutation_result(
                    &transaction,
                    workspace_scope,
                    key,
                    &result,
                )?;
            }
            transaction
                .commit()
                .map_err(|error| format!("无法完成优化候选评估重放：{error}"))?;
            return Ok(result);
        }
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |value| value.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        let mut checks = Vec::new();
        if row.0 != current_version {
            checks.push("候选基线版本已过期".to_string());
        }
        if row.5 < 2 {
            checks.push("证据数量少于 2 条".to_string());
        }
        if row.2.trim().is_empty() || row.2.chars().count() > 32_000 {
            checks.push("候选摘要为空或超过 32000 字".to_string());
        }
        let rules = serde_json::from_str::<Vec<String>>(&row.3)
            .map_err(|error| format!("优化规则载荷损坏：{error}"))?;
        if rules.is_empty() || rules.len() > 12 {
            checks.push("规则数量必须为 1 到 12 条".to_string());
        }
        if rules
            .iter()
            .any(|rule| contains_optimization_forbidden_instruction(rule))
        {
            checks.push("候选规则包含权限、设置或访问控制变更".to_string());
        }
        if serde_json::from_str::<Value>(&row.4)
            .ok()
            .filter(Value::is_object)
            .is_none()
        {
            checks.push("Skill 提示载荷不是 JSON 对象".to_string());
        }
        if let Some(expires_at) = &row.6 {
            if chrono::DateTime::parse_from_rfc3339(expires_at)
                .ok()
                .is_some_and(|value| value.with_timezone(&Utc) <= Utc::now())
            {
                checks.push("候选已过期".to_string());
            }
        }
        let passed = checks.is_empty();
        let state = if passed { "pending_review" } else { "rejected" };
        let evaluated_at = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE optimization_candidates SET state=?3, evaluated_at=?4
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id, state, evaluated_at],
            )
            .map_err(|error| format!("无法更新优化候选评估状态：{error}"))?;
        transaction
            .execute(
                "INSERT INTO optimization_evaluations
                 (id, workspace_scope, candidate_id, state, checks_json, evaluated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    candidate_id,
                    state,
                    serde_json::to_string(&checks).unwrap_or_else(|_| "[]".to_string()),
                    evaluated_at,
                ],
            )
            .map_err(|error| format!("无法保存优化候选评估：{error}"))?;
        let result = OptimizationEvaluationResult {
            candidate_id: candidate_id.to_string(),
            state: state.to_string(),
            passed,
            checks,
            evaluated_at,
        };
        if let Some(key) = mutation_key {
            persist_runtime_effect_mutation_result(&transaction, workspace_scope, key, &result)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化候选评估：{error}"))?;
        Ok(result)
    }

    fn optimization_candidate(
        &self,
        workspace_scope: &str,
        candidate_id: &str,
    ) -> Result<Option<OptimizationCandidateResult>, String> {
        if !valid_runtime_identifier(candidate_id, 160) {
            return Err("优化候选 ID 无效".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let row = connection
            .query_row(
                "SELECT base_version, candidate_version, state, summary, rules_json,
                        skill_hints_json, metrics_json, evidence_count, created_at,
                        evaluated_at, expires_at
                 FROM optimization_candidates
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取优化候选：{error}"))?;
        row.map(|row| {
            Ok(OptimizationCandidateResult {
                id: candidate_id.to_string(),
                base_version: row.0,
                candidate_version: row.1,
                state: row.2,
                summary: row.3,
                rules: serde_json::from_str(&row.4)
                    .map_err(|error| format!("优化候选规则载荷损坏：{error}"))?,
                skill_hints: serde_json::from_str(&row.5)
                    .map_err(|error| format!("优化候选 Skill 提示载荷损坏：{error}"))?,
                metrics: serde_json::from_str(&row.6)
                    .map_err(|error| format!("优化候选指标载荷损坏：{error}"))?,
                evidence_count: usize::try_from(row.7).unwrap_or_default(),
                created_at: row.8,
                evaluated_at: row.9,
                expires_at: row.10,
            })
        })
        .transpose()
    }

    fn load_optimization_profile(
        &self,
        workspace_scope: &str,
    ) -> Result<OptimizationProfileResult, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let row = connection
            .query_row(
                "SELECT version, candidate_id, guidance, rules_json, skill_hints_json, updated_at
                 FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取当前优化配置：{error}"))?;
        Ok(OptimizationProfileResult {
            version: row.0,
            candidate_id: row.1,
            guidance: row.2,
            rules: serde_json::from_str(&row.3).unwrap_or_default(),
            skill_hints: serde_json::from_str(&row.4)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
            updated_at: row.5,
        })
    }

    pub(crate) fn apply_optimization_candidate(
        &self,
        workspace_scope: &str,
        candidate_id: &str,
        mutation_key: Option<&RuntimeEffectMutationKey>,
    ) -> Result<OptimizationProfileResult, String> {
        if !valid_runtime_identifier(candidate_id, 160) {
            return Err("优化候选 ID 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始应用优化候选事务：{error}"))?;
        if let Some(key) = mutation_key {
            if let Some(result) =
                read_runtime_effect_mutation_result(&transaction, workspace_scope, key)?
            {
                transaction
                    .commit()
                    .map_err(|error| format!("无法完成优化候选应用幂等重放：{error}"))?;
                return Ok(result);
            }
        }
        let candidate = transaction
            .query_row(
                "SELECT base_version, candidate_version, state, summary, rules_json, skill_hints_json
                 FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取待应用优化候选：{error}"))?
            .ok_or_else(|| "优化候选不存在".to_string())?;
        if candidate.2 != "pending_review" {
            return Err(format!(
                "优化候选当前状态为 {}，未通过独立评估",
                candidate.2
            ));
        }
        let bound_reflection_job = transaction
            .query_row(
                "SELECT reflection_job_id
                 FROM memory_reflection_optimization_candidates
                 WHERE workspace_scope=?1 AND candidate_id=?2 AND state='bound'
                 LIMIT 1",
                params![workspace_scope, candidate_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法检查反思优化候选绑定：{error}"))?;
        if bound_reflection_job.is_some() {
            return Err("已绑定反思任务的优化候选必须使用原子审批命令".to_string());
        }
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        if candidate.0 != current_version || candidate.1 != current_version + 1 {
            return Err("优化候选基线已变化，需要重新生成和评估".to_string());
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO optimization_profile_revisions
                 (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                 VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, ?7, NULL)",
                params![workspace_scope, candidate.1, candidate_id, candidate.3, candidate.4, candidate.5, now],
            )
            .map_err(|error| format!("无法保存优化版本：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_profiles SET version=?2, candidate_id=?3, guidance=?4,
                 rules_json=?5, skill_hints_json=?6, updated_at=?7 WHERE workspace_scope=?1",
                params![
                    workspace_scope,
                    candidate.1,
                    candidate_id,
                    candidate.3,
                    candidate.4,
                    candidate.5,
                    now
                ],
            )
            .map_err(|error| format!("无法原子应用优化配置：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_candidates SET state='applied' WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
            )
            .map_err(|error| format!("无法更新优化候选状态：{error}"))?;
        let result = load_optimization_profile_in_connection(&transaction, workspace_scope)?;
        if let Some(key) = mutation_key {
            persist_runtime_effect_mutation_result(&transaction, workspace_scope, key, &result)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化配置：{error}"))?;
        Ok(result)
    }

    fn rollback_optimization_profile(
        &self,
        workspace_scope: &str,
        target_version: Option<i64>,
        mutation_key: Option<&RuntimeEffectMutationKey>,
    ) -> Result<OptimizationProfileResult, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始优化回滚事务：{error}"))?;
        if let Some(key) = mutation_key {
            if let Some(result) =
                read_runtime_effect_mutation_result(&transaction, workspace_scope, key)?
            {
                transaction
                    .commit()
                    .map_err(|error| format!("无法完成优化回滚幂等重放：{error}"))?;
                return Ok(result);
            }
        }
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取优化回滚版本：{error}"))?;
        let target = target_version.unwrap_or_else(|| current_version.saturating_sub(1));
        if target < 0 || target >= current_version {
            return Err("没有可回滚的上一版优化配置".to_string());
        }
        let revision = transaction
            .query_row(
                "SELECT candidate_id, guidance, rules_json, skill_hints_json
                 FROM optimization_profile_revisions
                 WHERE workspace_scope=?1 AND version=?2",
                params![workspace_scope, target],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取目标优化版本：{error}"))?
            .ok_or_else(|| "目标优化版本不存在".to_string())?;
        let now = Utc::now().to_rfc3339();
        let new_version = current_version + 1;
        transaction
            .execute(
                "INSERT INTO optimization_profile_revisions
                 (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                 VALUES (?1, ?2, ?3, 'rollback', ?4, ?5, ?6, ?7, ?8)",
                params![workspace_scope, new_version, revision.0, revision.1, revision.2, revision.3, now, target],
            )
            .map_err(|error| format!("无法保存回滚版本：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_profiles SET version=?2, candidate_id=?3, guidance=?4,
                 rules_json=?5, skill_hints_json=?6, updated_at=?7 WHERE workspace_scope=?1",
                params![
                    workspace_scope,
                    new_version,
                    revision.0,
                    revision.1,
                    revision.2,
                    revision.3,
                    now
                ],
            )
            .map_err(|error| format!("无法提交优化回滚：{error}"))?;
        let result = load_optimization_profile_in_connection(&transaction, workspace_scope)?;
        if let Some(key) = mutation_key {
            persist_runtime_effect_mutation_result(&transaction, workspace_scope, key, &result)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化回滚事务：{error}"))?;
        Ok(result)
    }

    fn list_optimization_versions(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<OptimizationVersion>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT version, candidate_id, state, guidance, created_at, rollback_target
                 FROM optimization_profile_revisions WHERE workspace_scope=?1
                 ORDER BY version DESC LIMIT ?2",
            )
            .map_err(|error| format!("无法准备优化版本查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, limit.clamp(1, 100) as i64],
                |row| {
                    Ok(OptimizationVersion {
                        version: row.get(0)?,
                        candidate_id: row.get(1)?,
                        state: row.get(2)?,
                        guidance: row.get(3)?,
                        created_at: row.get(4)?,
                        rollback_target: row.get(5)?,
                    })
                },
            )
            .map_err(|error| format!("无法读取优化版本：{error}"))?;
        Ok(rows.filter_map(Result::ok).collect())
    }
}

fn copy_database(source: &Connection, destination: &mut Connection) -> Result<(), String> {
    let backup = rusqlite::backup::Backup::new(source, destination)
        .map_err(|error| format!("无法初始化 SQLite 复制：{error}"))?;
    backup
        .run_to_completion(64, std::time::Duration::from_millis(10), None)
        .map_err(|error| format!("SQLite 复制失败：{error}"))
}

fn restore_database_runtime_configuration(connection: &Connection) -> Result<(), String> {
    let current_journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取 journal_mode：{error}"))?;
    let was_wal = current_journal_mode.eq_ignore_ascii_case("wal");
    if !was_wal {
        let applied_journal_mode = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法启用 WAL：{error}"))?;
        if !applied_journal_mode.eq_ignore_ascii_case("wal") {
            return Err(format!(
                "SQLite 未启用 WAL，当前模式为 {applied_journal_mode}"
            ));
        }
    }
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(|error| format!("无法配置同步与外键约束：{error}"))?;
    // A rollback-journal database has no WAL frames to flush. Checkpointing immediately after
    // switching that same connection to WAL can return SQLITE_LOCKED.
    if was_wal {
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(|error| format!("无法完成 WAL checkpoint：{error}"))?;
    }
    Ok(())
}

fn database_integrity(connection: &Connection) -> Result<String, String> {
    connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("无法执行数据库完整性检查：{error}"))
}

fn database_schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("无法读取数据库 schema 版本：{error}"))
}

fn inspect_backup(path: &Path) -> Result<DatabaseRestorePreflight, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取备份元数据：{error}"))?;
    if metadata.len() == 0 {
        return Err("备份文件为空".to_string());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法以只读方式打开备份：{error}"))?;
    let integrity = database_integrity(&connection)?;
    let schema_version = database_schema_version(&connection)?;
    let compatible =
        integrity == "ok" && schema_version > 0 && schema_version <= CURRENT_SCHEMA_VERSION;
    let reason = if integrity != "ok" {
        format!("完整性检查失败：{integrity}")
    } else if schema_version <= 0 {
        "不是可识别的 Yunspire 数据库".to_string()
    } else if schema_version > CURRENT_SCHEMA_VERSION {
        format!("备份 schema {schema_version} 高于当前应用支持的 {CURRENT_SCHEMA_VERSION}")
    } else {
        "预检通过".to_string()
    };
    Ok(DatabaseRestorePreflight {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("yunspire.sqlite")
            .to_string(),
        byte_length: metadata.len(),
        schema_version,
        integrity,
        compatible,
        reason,
    })
}

fn evaluate_vault_write_policy(
    client_state: &Value,
    vault_id: &str,
    relative_path: &str,
) -> Result<(), String> {
    let settings = client_state.get("settings").and_then(Value::as_object);
    let access = settings
        .and_then(|value| value.get("vaultAccess"))
        .and_then(Value::as_object)
        .and_then(|value| value.get(vault_id))
        .and_then(Value::as_str)
        .unwrap_or("readwrite");
    match access {
        "readwrite" => {}
        "readonly" => return Err("当前 Obsidian 知识库仅允许查询，已拒绝写入".to_string()),
        "disabled" => return Err("当前 Obsidian 知识库已设为不接入，已拒绝写入".to_string()),
        _ => return Err("当前 Obsidian 知识库访问策略无效，已拒绝写入".to_string()),
    }

    let write_scope = settings
        .and_then(|value| value.get("vaultWriteScope"))
        .and_then(Value::as_str)
        .unwrap_or("all-writable");
    let current_vault = client_state
        .get("currentVaultId")
        .and_then(Value::as_str)
        .unwrap_or("all");
    match write_scope {
        "all-writable" => Ok(()),
        "readonly" => Err("设置已禁止自动写入 Obsidian".to_string()),
        "current-vault" => {
            if current_vault == vault_id && current_vault != "all" {
                Ok(())
            } else {
                Err("写入目标不属于当前 Obsidian 知识库".to_string())
            }
        }
        "inbox-only" => {
            if current_vault != vault_id || current_vault == "all" {
                return Err("写入目标不属于当前 Obsidian 知识库".to_string());
            }
            if relative_path == "收件箱"
                || relative_path.starts_with("收件箱/")
                || relative_path == "00 收件箱"
                || relative_path.starts_with("00 收件箱/")
            {
                Ok(())
            } else {
                Err("当前策略只允许写入收件箱".to_string())
            }
        }
        _ => Err("知识库写入范围配置无效，已拒绝写入".to_string()),
    }
}

fn evaluate_vault_read_policy(client_state: &Value, vault_id: &str) -> Result<(), String> {
    let access = client_state
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|value| value.get("vaultAccess"))
        .and_then(Value::as_object)
        .and_then(|value| value.get(vault_id))
        .and_then(Value::as_str)
        .unwrap_or("readwrite");
    match access {
        "readwrite" | "readonly" => Ok(()),
        "disabled" => Err("当前 Obsidian 知识库已设为不接入，已拒绝读取".to_string()),
        _ => Err("当前 Obsidian 知识库访问策略无效，已拒绝读取".to_string()),
    }
}

fn valid_runtime_task_state(value: &str) -> bool {
    matches!(
        value,
        "created"
            | "queued"
            | "running"
            | "awaiting_approval"
            | "paused"
            | "succeeded"
            | "failed"
            | "cancelled"
    )
}

fn canonical_runtime_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_runtime_json).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_runtime_json(&object[key]));
            }
            Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

fn canonical_runtime_json_string(value: &Value, label: &str) -> Result<String, String> {
    serde_json::to_string(&canonical_runtime_json(value))
        .map_err(|error| format!("无法序列化{label}：{error}"))
}

pub(crate) fn runtime_effect_mutation_request_hash(
    value: &Value,
    label: &str,
) -> Result<String, String> {
    let canonical = canonical_runtime_json_string(value, label)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
}

pub(crate) fn runtime_effect_mutation_key(
    authorization: &RuntimeEffectfulHandlerAuthorization,
    handler_kind: &'static str,
    request: &Value,
) -> Result<RuntimeEffectMutationKey, String> {
    Ok(RuntimeEffectMutationKey {
        command_id: authorization.command_id.clone(),
        handler_kind,
        request_hash: runtime_effect_mutation_request_hash(request, "Runtime 副作用请求")?,
    })
}

pub(crate) fn read_runtime_effect_mutation_result<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    workspace_scope: &str,
    key: &RuntimeEffectMutationKey,
) -> Result<Option<T>, String> {
    let stored = connection
        .query_row(
            "SELECT request_hash, result_json
             FROM runtime_effect_mutation_results
             WHERE workspace_scope=?1 AND command_id=?2 AND handler_kind=?3",
            params![workspace_scope, key.command_id, key.handler_kind],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取 Runtime 副作用重放结果：{error}"))?;
    let Some((request_hash, result_json)) = stored else {
        return Ok(None);
    };
    if request_hash != key.request_hash {
        return Err("同一 Runtime command 的处理器请求已绑定到不同参数".to_string());
    }
    serde_json::from_str(&result_json)
        .map(Some)
        .map_err(|error| format!("无法解析 Runtime 副作用重放结果：{error}"))
}

pub(crate) fn persist_runtime_effect_mutation_result<T: Serialize>(
    connection: &Connection,
    workspace_scope: &str,
    key: &RuntimeEffectMutationKey,
    result: &T,
) -> Result<(), String> {
    let result_json = serde_json::to_string(result)
        .map_err(|error| format!("无法序列化 Runtime 副作用结果：{error}"))?;
    connection
        .execute(
            "INSERT INTO runtime_effect_mutation_results
             (workspace_scope, command_id, handler_kind, request_hash, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace_scope,
                key.command_id,
                key.handler_kind,
                key.request_hash,
                result_json,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|error| format!("无法保存 Runtime 副作用幂等结果：{error}"))?;
    Ok(())
}

fn runtime_budget_value(payload: &Value, key: &str) -> Option<u64> {
    payload
        .get("budget")
        .and_then(Value::as_object)
        .and_then(|budget| budget.get(key))
        .and_then(Value::as_u64)
}

fn runtime_budget_cost(payload: &Value) -> Option<f64> {
    payload
        .get("budget")
        .and_then(Value::as_object)
        .and_then(|budget| budget.get("maxCost"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn checked_sqlite_i64(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} 超出 SQLite 整数范围"))
}

fn read_runtime_task_execution_budget(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
) -> Result<RuntimeTaskExecutionBudgetStatus, String> {
    connection
        .query_row(
            "SELECT max_steps, max_tool_calls, max_runtime_seconds, max_tokens, max_cost,
                    reserved_steps, reserved_tool_calls, reserved_runtime_seconds,
                    reserved_tokens, reserved_cost, consumed_steps, consumed_tool_calls,
                    consumed_runtime_seconds, consumed_tokens, consumed_cost,
                    cancellation_fence, cancelled_at
             FROM runtime_task_execution_budgets
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
            params![workspace_scope, task_id, plan_revision],
            |row| {
                Ok(RuntimeTaskExecutionBudgetStatus {
                    runtime_task_id: task_id.to_string(),
                    plan_revision: u64::try_from(plan_revision).unwrap_or_default(),
                    max_steps: u64::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
                    max_tool_calls: u64::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                    max_runtime_seconds: u64::try_from(row.get::<_, i64>(2)?).unwrap_or_default(),
                    max_tokens: row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u64::try_from(value).ok()),
                    max_cost: row.get(4)?,
                    reserved_steps: u64::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
                    reserved_tool_calls: u64::try_from(row.get::<_, i64>(6)?).unwrap_or_default(),
                    reserved_runtime_seconds: u64::try_from(row.get::<_, i64>(7)?)
                        .unwrap_or_default(),
                    reserved_tokens: u64::try_from(row.get::<_, i64>(8)?).unwrap_or_default(),
                    reserved_cost: row.get(9)?,
                    consumed_steps: u64::try_from(row.get::<_, i64>(10)?).unwrap_or_default(),
                    consumed_tool_calls: u64::try_from(row.get::<_, i64>(11)?).unwrap_or_default(),
                    consumed_runtime_seconds: u64::try_from(row.get::<_, i64>(12)?)
                        .unwrap_or_default(),
                    consumed_tokens: u64::try_from(row.get::<_, i64>(13)?).unwrap_or_default(),
                    consumed_cost: row.get(14)?,
                    cancellation_fence: u64::try_from(row.get::<_, i64>(15)?).unwrap_or_default(),
                    cancelled_at: row.get(16)?,
                })
            },
        )
        .map_err(|error| format!("无法读取原生任务执行预算：{error}"))
}

fn ensure_runtime_task_execution_budget(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
    payload: &Value,
    explicit_budget: Option<&CommandBudget>,
    created_at: &str,
) -> Result<RuntimeTaskExecutionBudgetStatus, String> {
    let step_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM runtime_task_plan_steps
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
            params![workspace_scope, task_id, plan_revision],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法统计原生任务计划步骤：{error}"))?;
    let minimum_steps = u64::try_from(step_count.max(1)).unwrap_or(1);
    let (max_steps, max_tool_calls, max_runtime_seconds, max_tokens, max_cost) =
        if let Some(budget) = explicit_budget {
            if budget.max_steps < minimum_steps {
                return Err("原生任务计划步骤数量超过应用命令步骤预算".to_string());
            }
            (
                budget.max_steps,
                budget.max_tool_calls,
                budget.max_runtime_seconds,
                budget.max_tokens,
                budget.max_cost,
            )
        } else {
            let max_steps = runtime_budget_value(payload, "maxSteps").unwrap_or(minimum_steps);
            if max_steps < minimum_steps {
                return Err("原生任务计划步骤数量超过任务步骤预算".to_string());
            }
            (
                max_steps,
                runtime_budget_value(payload, "maxToolCalls").unwrap_or(minimum_steps),
                runtime_budget_value(payload, "maxRuntimeSeconds")
                    .unwrap_or(3_600)
                    .max(1),
                runtime_budget_value(payload, "maxTokens"),
                runtime_budget_cost(payload),
            )
        };
    if max_runtime_seconds == 0 || max_cost.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err("原生任务执行预算无效".to_string());
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO runtime_task_execution_budgets
             (workspace_scope, task_id, plan_revision, max_steps, max_tool_calls,
              max_runtime_seconds, max_tokens, max_cost, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                workspace_scope,
                task_id,
                plan_revision,
                checked_sqlite_i64(max_steps, "步骤预算")?,
                checked_sqlite_i64(max_tool_calls, "工具调用预算")?,
                checked_sqlite_i64(max_runtime_seconds, "运行时间预算")?,
                max_tokens
                    .map(|value| checked_sqlite_i64(value, "Token 预算"))
                    .transpose()?,
                max_cost,
                created_at,
            ],
        )
        .map_err(|error| format!("无法初始化原生任务执行预算：{error}"))?;
    read_runtime_task_execution_budget(connection, workspace_scope, task_id, plan_revision)
}

fn read_runtime_child_execution_expectation(
    connection: &Connection,
    workspace_scope: &str,
    child_task_id: &str,
) -> Result<Option<RuntimeChildExecutionExpectation>, String> {
    let row = connection
        .query_row(
            "SELECT binding.claim_id, binding.command_id, run.task_id, run.plan_revision,
                    run.step_id, command.trace_id, command.payload, run.state,
                    run.lease_expires_at, parent.state, run.cancellation_fence,
                    budget.cancellation_fence, budget.cancelled_at,
                    run.reserved_tool_calls, run.reserved_runtime_seconds,
                    run.reserved_tokens, run.reserved_cost, run.effect_class,
                    budget.max_tokens, budget.max_cost
             FROM runtime_task_step_command_bindings binding
             JOIN runtime_task_step_runs run
               ON run.workspace_scope=binding.workspace_scope AND run.claim_id=binding.claim_id
             JOIN runtime_task_execution_budgets budget
               ON budget.workspace_scope=run.workspace_scope
              AND budget.task_id=run.task_id
              AND budget.plan_revision=run.plan_revision
             JOIN application_commands command
               ON command.workspace_scope=binding.workspace_scope
              AND command.id=binding.command_id
              AND command.task_id=binding.child_task_id
             JOIN runtime_tasks parent
               ON parent.workspace_scope=run.workspace_scope AND parent.id=run.task_id
             WHERE binding.workspace_scope=?1 AND binding.child_task_id=?2",
            params![workspace_scope, child_task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, f64>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, Option<i64>>(18)?,
                    row.get::<_, Option<f64>>(19)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 Runtime 子任务可信执行绑定：{error}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let command = serde_json::from_str::<ApplicationCommand>(&row.6)
        .map_err(|error| format!("无法解析 Runtime 子任务应用命令：{error}"))?;
    if command.id != row.1 || !matches!(command.origin, crate::policy::CommandOrigin::Runtime) {
        return Err("Runtime 子任务绑定的应用命令身份无效".to_string());
    }
    if command.trace_id.as_deref() != Some(row.5.as_str()) {
        return Err("Runtime 子任务应用命令 Trace 快照不一致".to_string());
    }
    let command_effectful = crate::policy::command_is_effectful(&command);
    let binding = RuntimeTaskStepCommandBinding {
        runtime_task_id: row.2,
        plan_revision: u64::try_from(row.3)
            .ok()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| "Runtime 子任务绑定的计划版本无效".to_string())?,
        step_id: row.4,
        step_claim_id: row.0,
    };
    if command.step_binding.as_ref() != Some(&binding) {
        return Err("Runtime 子任务应用命令的步骤绑定快照不一致".to_string());
    }
    Ok(Some(RuntimeChildExecutionExpectation {
        binding,
        command_id: row.1,
        trace_id: row.5,
        capability_id: command.capability_id,
        operation: command.operation,
        parameters: command.parameters,
        vault_id: command.vault_id,
        command_effectful,
        effect_class: RuntimeTaskStepEffectClass::parse(&row.17)
            .ok_or_else(|| "Runtime 子任务绑定的步骤 effect class 无效".to_string())?,
        run_state: row.7,
        lease_expires_at: row.8,
        parent_state: row.9,
        cancellation_fence: u64::try_from(row.10).unwrap_or_default(),
        budget_cancellation_fence: u64::try_from(row.11).unwrap_or_default(),
        cancelled_at: row.12,
        reserved_tool_calls: u64::try_from(row.13).unwrap_or_default(),
        reserved_runtime_seconds: u64::try_from(row.14).unwrap_or_default(),
        reserved_tokens: u64::try_from(row.15).unwrap_or_default(),
        reserved_cost: row.16,
        max_tokens: row.18.map(|value| u64::try_from(value).unwrap_or_default()),
        max_cost: row.19,
    }))
}

fn validate_trusted_execution_receipt(
    workspace_scope: &str,
    child: &NativeRuntimeTask,
    expected: &RuntimeChildExecutionExpectation,
    receipt: &TrustedExecutionReceipt,
) -> Result<(), String> {
    if receipt.workspace_scope != workspace_scope
        || receipt.child_task_id != child.id
        || receipt.command_id != expected.command_id
        || receipt.trace_id != expected.trace_id
        || receipt.capability_id != expected.capability_id
        || receipt.operation != expected.operation
        || receipt.step_binding != expected.binding
    {
        return Err("可信原生执行回执与 Runtime 子任务绑定不一致".to_string());
    }
    let expected_trust_kind = match expected.effect_class {
        RuntimeTaskStepEffectClass::ReadOnly => "read_only_native_handler",
        RuntimeTaskStepEffectClass::Effectful => "effectful_native_handler",
    };
    if receipt.trust_kind != expected_trust_kind {
        return Err("可信原生执行回执类型与步骤 effect class 不一致".to_string());
    }
    if child.trace_id.as_deref() != Some(expected.trace_id.as_str())
        || child.payload.get("commandId").and_then(Value::as_str)
            != Some(expected.command_id.as_str())
        || child.payload.get("operation").and_then(Value::as_str)
            != Some(expected.operation.as_str())
        || !child
            .payload
            .get("capabilityIds")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|value| value == expected.capability_id)
            })
    {
        return Err("Runtime 子任务负载与可信原生执行回执不一致".to_string());
    }
    if !receipt.receipt_id.starts_with("native-handler:sha256:")
        || receipt.consumed_tool_calls == 0
        || receipt.consumed_runtime_seconds == 0
        || !receipt.consumed_cost.is_finite()
        || receipt.consumed_cost < 0.0
        || (!receipt.cost_measured && receipt.consumed_cost != 0.0)
        || chrono::DateTime::parse_from_rfc3339(&receipt.completed_at).is_err()
    {
        return Err("可信原生执行回执用量或时间无效".to_string());
    }
    Ok(())
}

fn validate_trusted_execution_usage_within_reservation(
    expected: &RuntimeChildExecutionExpectation,
    receipt: &TrustedExecutionReceipt,
) -> Result<(), String> {
    if receipt.consumed_tool_calls > expected.reserved_tool_calls
        || receipt.consumed_runtime_seconds > expected.reserved_runtime_seconds
        || expected
            .max_tokens
            .is_some_and(|_| receipt.consumed_tokens > expected.reserved_tokens)
        || expected.max_cost.is_some_and(|_| {
            !receipt.cost_measured || receipt.consumed_cost > expected.reserved_cost + f64::EPSILON
        })
    {
        return Err("可信原生执行回执消耗超过步骤预留预算".to_string());
    }
    Ok(())
}

fn validate_live_runtime_child_execution_expectation(
    expected: &RuntimeChildExecutionExpectation,
    now: &str,
) -> Result<(), String> {
    if expected.run_state != "claimed" {
        return Err("Runtime 子任务绑定的步骤领取已经完成或被封锁".to_string());
    }
    if expected.lease_expires_at.as_str() <= now {
        return Err("Runtime 子任务绑定的步骤领取 lease 已过期".to_string());
    }
    if !matches!(
        expected.parent_state.as_str(),
        "queued" | "running" | "awaiting_approval"
    ) || expected.cancelled_at.is_some()
        || expected.cancellation_fence != expected.budget_cancellation_fence
    {
        return Err("Runtime 子任务绑定已被父任务状态或取消栅栏封锁".to_string());
    }
    Ok(())
}

fn runtime_handler_pair_allowed(
    allowed_pairs: &[(&str, &str)],
    capability_id: &str,
    operation: &str,
) -> bool {
    allowed_pairs
        .iter()
        .any(|(allowed_capability, allowed_operation)| {
            capability_id == *allowed_capability && operation == *allowed_operation
        })
}

fn runtime_effectful_handler_authorization(
    connection: &Connection,
    workspace_scope: &str,
    operation_context: &OperationContext,
    allowed_pairs: &[(&str, &str)],
) -> Result<RuntimeEffectfulHandlerAuthorization, String> {
    if allowed_pairs.is_empty() {
        return Err("effectful 原生处理器没有声明允许的命令身份".to_string());
    }
    let execution_ticket = operation_context
        .execution_ticket
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= 240)
        .ok_or_else(|| "effectful Runtime 处理器缺少有效执行票据".to_string())?;
    let child_task_id = operation_context
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "effectful Runtime 处理器缺少 Runtime 子任务 ID".to_string())?;
    let trace_id = operation_context
        .trace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "effectful Runtime 处理器缺少 Trace".to_string())?;
    crate::trace::validate_trace_id(trace_id)?;
    let child = read_native_runtime_task(connection, workspace_scope, child_task_id)?;
    if child.payload.get("kind").and_then(Value::as_str) != Some("runtime_child")
        || child.state != "running"
    {
        return Err("只有 running 状态的 Runtime 子任务可以调用 effectful 原生处理器".to_string());
    }
    let expected =
        read_runtime_child_execution_expectation(connection, workspace_scope, child_task_id)?
            .ok_or_else(|| "Runtime 子任务缺少 effectful 步骤绑定".to_string())?;
    validate_live_runtime_child_execution_expectation(&expected, &Utc::now().to_rfc3339())?;
    if expected.effect_class != RuntimeTaskStepEffectClass::Effectful || !expected.command_effectful
    {
        return Err("只读 Runtime 能力不能调用 effectful 原生处理器".to_string());
    }
    if expected.trace_id != trace_id
        || !runtime_handler_pair_allowed(
            allowed_pairs,
            &expected.capability_id,
            &expected.operation,
        )
    {
        return Err("effectful 原生处理器与 Runtime 子命令身份不一致".to_string());
    }
    Ok(RuntimeEffectfulHandlerAuthorization {
        execution_ticket: execution_ticket.to_string(),
        child_task_id: child_task_id.to_string(),
        command_id: expected.command_id,
        trace_id: expected.trace_id,
        capability_id: expected.capability_id,
        operation: expected.operation,
        binding: expected.binding,
        reservation: TrustedHandlerReservation {
            max_tool_calls: expected.reserved_tool_calls,
            max_runtime_seconds: expected.reserved_runtime_seconds,
            max_tokens: expected.max_tokens.map(|_| expected.reserved_tokens),
            max_cost: expected.max_cost.map(|_| expected.reserved_cost),
        },
    })
}

fn runtime_read_only_result_limit(parameters: &Value) -> usize {
    parameters
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(50)
        .clamp(1, 200)
}

fn runtime_read_only_client_state(
    connection: &Connection,
    workspace_scope: &str,
) -> Result<Value, String> {
    Ok(connection
        .query_row(
            "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
            [workspace_scope],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取 Runtime 只读 Vault 策略：{error}"))?
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|snapshot| {
            snapshot
                .get("clientState")
                .or_else(|| snapshot.get("client_state"))
                .cloned()
        })
        .unwrap_or_else(|| Value::Object(serde_json::Map::new())))
}

fn runtime_read_only_search_output(
    connection: &Connection,
    workspace_scope: &str,
    expected: &RuntimeChildExecutionExpectation,
) -> Result<Value, String> {
    let query = expected
        .parameters
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Runtime 原生搜索缺少不可变 query 参数".to_string())?;
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("Runtime 原生搜索词超过 512 个字符的安全上限".to_string());
    }
    let limit = runtime_read_only_result_limit(&expected.parameters);
    let client_state = runtime_read_only_client_state(connection, workspace_scope)?;
    let explicit_vault_id = expected
        .vault_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all");
    let vault_ids = if let Some(vault_id) = explicit_vault_id {
        evaluate_vault_read_policy(&client_state, vault_id)?;
        vec![vault_id.to_string()]
    } else {
        let mut statement = connection
            .prepare("SELECT DISTINCT vault_id FROM note_index ORDER BY vault_id")
            .map_err(|error| format!("无法准备 Runtime 原生搜索 Vault 范围：{error}"))?;
        let vault_ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取 Runtime 原生搜索 Vault 范围：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Runtime 原生搜索 Vault 范围：{error}"))?;
        vault_ids
            .into_iter()
            .filter(|vault_id| evaluate_vault_read_policy(&client_state, vault_id).is_ok())
            .collect::<Vec<_>>()
    };
    let mut results = Vec::new();
    for vault_id in &vault_ids {
        results.extend(indexed_search_in_connection_with_neural(
            connection,
            Some(vault_id),
            query,
            limit,
            None,
        )?);
    }
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.vault_id.cmp(&right.vault_id))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    results.truncate(limit);
    Ok(serde_json::json!({
        "kind": "indexed_search",
        "query": query,
        "vaultId": explicit_vault_id.unwrap_or("all"),
        "searchedVaultIds": vault_ids,
        "resultCount": results.len(),
        "results": results,
    }))
}

fn runtime_task_state_counts(
    connection: &Connection,
    workspace_scope: &str,
) -> Result<Value, String> {
    let mut statement = connection
        .prepare(
            "SELECT state, COUNT(*) FROM runtime_tasks
             WHERE workspace_scope=?1 GROUP BY state ORDER BY state",
        )
        .map_err(|error| format!("无法准备 Runtime 任务状态快照：{error}"))?;
    let rows = statement
        .query_map([workspace_scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| format!("无法读取 Runtime 任务状态快照：{error}"))?;
    let mut counts = serde_json::Map::new();
    for row in rows {
        let (state, count) =
            row.map_err(|error| format!("无法解析 Runtime 任务状态快照：{error}"))?;
        counts.insert(state, Value::from(count));
    }
    Ok(Value::Object(counts))
}

fn runtime_read_only_tasks_output(
    connection: &Connection,
    workspace_scope: &str,
    limit: usize,
) -> Result<Value, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, state, title, trace_id, updated_at FROM runtime_tasks
             WHERE workspace_scope=?1 ORDER BY updated_at DESC, id DESC LIMIT ?2",
        )
        .map_err(|error| format!("无法准备 Runtime 原生任务快照：{error}"))?;
    let items = statement
        .query_map(params![workspace_scope, limit as i64], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "state": row.get::<_, String>(1)?,
                "title": row.get::<_, String>(2)?,
                "traceId": row.get::<_, Option<String>>(3)?,
                "updatedAt": row.get::<_, String>(4)?,
            }))
        })
        .map_err(|error| format!("无法读取 Runtime 原生任务快照：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Runtime 原生任务快照：{error}"))?;
    Ok(serde_json::json!({
        "kind": "runtime_tasks",
        "stateCounts": runtime_task_state_counts(connection, workspace_scope)?,
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_logs_output(connection: &Connection, limit: usize) -> Result<Value, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, task_id, event_type, state, created_at FROM operation_events
             ORDER BY created_at DESC, id DESC LIMIT ?1",
        )
        .map_err(|error| format!("无法准备 Runtime 原生日志快照：{error}"))?;
    let items = statement
        .query_map([limit as i64], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "taskId": row.get::<_, Option<String>>(1)?,
                "eventType": row.get::<_, String>(2)?,
                "state": row.get::<_, String>(3)?,
                "createdAt": row.get::<_, String>(4)?,
            }))
        })
        .map_err(|error| format!("无法读取 Runtime 原生日志快照：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Runtime 原生日志快照：{error}"))?;
    Ok(serde_json::json!({
        "kind": "operation_events",
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_dashboard_output(
    connection: &Connection,
    workspace_scope: &str,
) -> Result<Value, String> {
    let pending_approvals = connection
        .query_row(
            "SELECT COUNT(*) FROM approvals WHERE state='pending'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法读取 Runtime 待审批快照：{error}"))?;
    let pending_inbound = connection
        .query_row(
            "SELECT COUNT(*) FROM inbound_content_records
             WHERE workspace_scope=?1 AND state NOT IN ('committed', 'failed', 'cancelled')",
            [workspace_scope],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法读取 Runtime Inbox 快照：{error}"))?;
    let connected_vaults = connection
        .query_row(
            "SELECT COUNT(*) FROM vault_registry WHERE connection_state='connected'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法读取 Runtime Vault 快照：{error}"))?;
    let operation_events = connection
        .query_row("SELECT COUNT(*) FROM operation_events", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("无法读取 Runtime 日志计数：{error}"))?;
    Ok(serde_json::json!({
        "kind": "dashboard",
        "taskStateCounts": runtime_task_state_counts(connection, workspace_scope)?,
        "pendingApprovals": pending_approvals,
        "pendingInbound": pending_inbound,
        "connectedVaults": connected_vaults,
        "operationEvents": operation_events,
    }))
}

fn runtime_read_only_settings_output(
    connection: &Connection,
    workspace_scope: &str,
) -> Result<Value, String> {
    let runtime = connection
        .query_row(
            "SELECT scheduler_enabled, updated_at FROM runtime_settings WHERE workspace_scope=?1",
            [workspace_scope],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取 Runtime 设置快照：{error}"))?;
    let authorization = connection
        .query_row(
            "SELECT status, authorization_version, updated_at FROM application_authorization WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 Runtime 应用授权快照：{error}"))?;
    Ok(serde_json::json!({
        "kind": "settings",
        "schedulerEnabled": runtime.as_ref().is_none_or(|value| value.0 != 0),
        "runtimeUpdatedAt": runtime.map(|value| value.1),
        "authorization": authorization.map(|value| serde_json::json!({
            "status": value.0,
            "version": value.1,
            "updatedAt": value.2,
        })),
    }))
}

fn runtime_read_only_skills_output(
    connection: &Connection,
    workspace_scope: &str,
    limit: usize,
) -> Result<Value, String> {
    let registry_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='skill_registry'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法检查 Runtime Skill 表：{error}"))?
        .is_some();
    let items = if registry_exists {
        let mut statement = connection
            .prepare(
                "SELECT id, current_version, state, name, payload_hash, updated_at
                 FROM skill_registry WHERE workspace_scope=?1
                 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )
            .map_err(|error| format!("无法准备 Runtime Skill 快照：{error}"))?;
        let items = statement
            .query_map(params![workspace_scope, limit as i64], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "version": row.get::<_, i64>(1)?,
                    "state": row.get::<_, String>(2)?,
                    "name": row.get::<_, String>(3)?,
                    "payloadHash": row.get::<_, String>(4)?,
                    "updatedAt": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|error| format!("无法读取 Runtime Skill 快照：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Runtime Skill 快照：{error}"))?;
        items
    } else {
        let mut statement = connection
            .prepare(
                "SELECT id, revision, state, payload_hash, updated_at FROM managed_resources
                 WHERE workspace_scope=?1 AND resource_type='user_skill' AND state='active'
                 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )
            .map_err(|error| format!("无法准备 Runtime 兼容 Skill 快照：{error}"))?;
        let items = statement
            .query_map(params![workspace_scope, limit as i64], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "version": row.get::<_, i64>(1)?,
                    "state": row.get::<_, String>(2)?,
                    "payloadHash": row.get::<_, String>(3)?,
                    "updatedAt": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|error| format!("无法读取 Runtime 兼容 Skill 快照：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Runtime 兼容 Skill 快照：{error}"))?;
        items
    };
    Ok(serde_json::json!({
        "kind": "skills",
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_vaults_output(
    connection: &Connection,
    workspace_scope: &str,
    limit: usize,
) -> Result<Value, String> {
    let client_state = runtime_read_only_client_state(connection, workspace_scope)?;
    let mut statement = connection
        .prepare(
            "SELECT id, display_name, note_count, attachment_count, connection_state,
                    is_open, last_indexed_at, last_error
             FROM vault_registry ORDER BY display_name, id LIMIT ?1",
        )
        .map_err(|error| format!("无法准备 Runtime Vault 快照：{error}"))?;
    let rows = statement
        .query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|error| format!("无法读取 Runtime Vault 快照：{error}"))?;
    let mut items = Vec::new();
    for row in rows {
        let row = row.map_err(|error| format!("无法解析 Runtime Vault 快照：{error}"))?;
        if evaluate_vault_read_policy(&client_state, &row.0).is_err() {
            continue;
        }
        items.push(serde_json::json!({
            "id": row.0,
            "name": row.1,
            "noteCount": row.2,
            "attachmentCount": row.3,
            "connectionState": row.4,
            "isOpen": row.5 != 0,
            "lastIndexedAt": row.6,
            "lastError": row.7,
        }));
    }
    Ok(serde_json::json!({
        "kind": "vaults",
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_managed_resources_output(
    connection: &Connection,
    workspace_scope: &str,
    resource_types: &[&str],
    kind: &str,
    limit: usize,
) -> Result<Value, String> {
    let mut items = Vec::new();
    for resource_type in resource_types {
        let remaining = limit.saturating_sub(items.len());
        if remaining == 0 {
            break;
        }
        let mut statement = connection
            .prepare(
                "SELECT resource_type, id, revision, state, payload_hash, updated_at
                 FROM managed_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'
                 ORDER BY updated_at DESC, id DESC LIMIT ?3",
            )
            .map_err(|error| format!("无法准备 Runtime {kind} 快照：{error}"))?;
        let mut rows = statement
            .query_map(
                params![workspace_scope, resource_type, remaining as i64],
                |row| {
                    Ok(serde_json::json!({
                        "resourceType": row.get::<_, String>(0)?,
                        "id": row.get::<_, String>(1)?,
                        "revision": row.get::<_, i64>(2)?,
                        "state": row.get::<_, String>(3)?,
                        "payloadHash": row.get::<_, String>(4)?,
                        "updatedAt": row.get::<_, String>(5)?,
                    }))
                },
            )
            .map_err(|error| format!("无法读取 Runtime {kind} 快照：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Runtime {kind} 快照：{error}"))?;
        items.append(&mut rows);
    }
    items.sort_by(|left, right| {
        right
            .get("updatedAt")
            .and_then(Value::as_str)
            .cmp(&left.get("updatedAt").and_then(Value::as_str))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            })
    });
    items.truncate(limit);
    Ok(serde_json::json!({
        "kind": kind,
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_schedules_output(
    connection: &Connection,
    workspace_scope: &str,
    limit: usize,
) -> Result<Value, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, schedule_kind, enabled, next_run, revision, updated_at
             FROM runtime_schedules WHERE workspace_scope=?1
             ORDER BY updated_at DESC, id DESC LIMIT ?2",
        )
        .map_err(|error| format!("无法准备 Runtime 日程快照：{error}"))?;
    let items = statement
        .query_map(params![workspace_scope, limit as i64], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "scheduleKind": row.get::<_, String>(1)?,
                "enabled": row.get::<_, i64>(2)? != 0,
                "nextRun": row.get::<_, Option<String>>(3)?,
                "revision": row.get::<_, i64>(4)?,
                "updatedAt": row.get::<_, String>(5)?,
            }))
        })
        .map_err(|error| format!("无法读取 Runtime 日程快照：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Runtime 日程快照：{error}"))?;
    Ok(serde_json::json!({
        "kind": "schedules",
        "itemCount": items.len(),
        "items": items,
    }))
}

fn runtime_read_only_inbox_output(
    connection: &Connection,
    workspace_scope: &str,
    limit: usize,
) -> Result<Value, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, task_id, state, source_type, title, updated_at
             FROM inbound_content_records WHERE workspace_scope=?1
             ORDER BY updated_at DESC, id DESC LIMIT ?2",
        )
        .map_err(|error| format!("无法准备 Runtime Inbox 快照：{error}"))?;
    let items = statement
        .query_map(params![workspace_scope, limit as i64], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "taskId": row.get::<_, Option<String>>(1)?,
                "state": row.get::<_, String>(2)?,
                "sourceType": row.get::<_, String>(3)?,
                "title": row.get::<_, String>(4)?,
                "updatedAt": row.get::<_, String>(5)?,
            }))
        })
        .map_err(|error| format!("无法读取 Runtime Inbox 快照：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Runtime Inbox 快照：{error}"))?;
    Ok(serde_json::json!({
        "kind": "inbox",
        "itemCount": items.len(),
        "items": items,
    }))
}

fn execute_runtime_read_only_handler_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    expected: &RuntimeChildExecutionExpectation,
) -> Result<Value, String> {
    let limit = runtime_read_only_result_limit(&expected.parameters);
    match expected.capability_id.as_str() {
        "system:search" => runtime_read_only_search_output(connection, workspace_scope, expected),
        "system:tasks" => runtime_read_only_tasks_output(connection, workspace_scope, limit),
        "system:logs" => runtime_read_only_logs_output(connection, limit),
        "system:dashboard" => runtime_read_only_dashboard_output(connection, workspace_scope),
        "system:settings" => runtime_read_only_settings_output(connection, workspace_scope),
        "system:skills" => runtime_read_only_skills_output(connection, workspace_scope, limit),
        "system:vaults" => runtime_read_only_vaults_output(connection, workspace_scope, limit),
        "system:reports" => runtime_read_only_managed_resources_output(
            connection,
            workspace_scope,
            &["report", "report_subscription"],
            "reports",
            limit,
        ),
        "system:schedule" => runtime_read_only_schedules_output(connection, workspace_scope, limit),
        "system:inbox" => runtime_read_only_inbox_output(connection, workspace_scope, limit),
        _ => Err(format!(
            "Runtime 只读能力 {} 没有受信任的原生处理器",
            expected.capability_id
        )),
    }
}

fn validate_runtime_task_step_command_binding_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    command: &ApplicationCommand,
    trace_id: &str,
    now: &str,
) -> Result<Option<RuntimeTaskStepCommandBinding>, String> {
    let Some(binding) = command.step_binding.as_ref() else {
        return Ok(None);
    };
    crate::task_runtime::validate_runtime_task_step_binding(binding)?;
    let row = connection
        .query_row(
            "SELECT run.task_id, run.plan_revision, run.step_id, run.state,
                    run.lease_expires_at, run.effect_class, run.cancellation_fence,
                    budget.cancellation_fence, budget.cancelled_at,
                    run.reserved_tool_calls, run.reserved_runtime_seconds,
                    run.reserved_tokens, run.reserved_cost, task.state
             FROM runtime_task_step_runs run
             JOIN runtime_task_execution_budgets budget
               ON budget.workspace_scope=run.workspace_scope
              AND budget.task_id=run.task_id
              AND budget.plan_revision=run.plan_revision
             JOIN runtime_tasks task
               ON task.workspace_scope=run.workspace_scope AND task.id=run.task_id
             WHERE run.workspace_scope=?1 AND run.claim_id=?2",
            params![workspace_scope, binding.step_claim_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, f64>(12)?,
                    row.get::<_, String>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法验证原生任务步骤绑定：{error}"))?
        .ok_or_else(|| "原生任务步骤领取不存在".to_string())?;
    if row.0 != binding.runtime_task_id
        || u64::try_from(row.1).ok() != Some(binding.plan_revision)
        || row.2 != binding.step_id
    {
        return Err("应用命令与原生任务步骤绑定不一致".to_string());
    }
    if row.3 != "claimed" || row.4.as_str() <= now || row.8.is_some() || row.13 == "cancelled" {
        return Err("原生任务步骤领取已过期、完成或被父任务取消".to_string());
    }
    if row.6 != row.7 {
        return Err("原生任务步骤领取已被取消栅栏封锁".to_string());
    }
    let step = load_runtime_task_plan_step_records(
        connection,
        workspace_scope,
        &binding.runtime_task_id,
        row.1,
    )?
    .into_iter()
    .find(|step| step.step_id == binding.step_id)
    .ok_or_else(|| "原生任务步骤绑定引用的计划步骤不存在".to_string())?;
    let parent_task =
        read_native_runtime_task(connection, workspace_scope, &binding.runtime_task_id)?;
    validate_runtime_task_step_child_authority(command, trace_id, &step, &parent_task)?;
    let effect_class = RuntimeTaskStepEffectClass::parse(&row.5)
        .ok_or_else(|| "原生任务步骤效果分类损坏".to_string())?;
    if effect_class == RuntimeTaskStepEffectClass::ReadOnly
        && crate::policy::command_is_effectful(command)
    {
        return Err("只读任务步骤不能派发有副作用的应用命令".to_string());
    }
    let reserved_tool_calls = u64::try_from(row.9).unwrap_or_default();
    let reserved_runtime_seconds = u64::try_from(row.10).unwrap_or_default();
    let reserved_tokens = u64::try_from(row.11).unwrap_or_default();
    if command.budget.max_tool_calls > reserved_tool_calls
        || command.budget.max_runtime_seconds > reserved_runtime_seconds
    {
        return Err("应用命令预算超过原生任务步骤已预留额度".to_string());
    }
    match (reserved_tokens, command.budget.max_tokens) {
        (0, Some(tokens)) if tokens > 0 => {
            return Err("应用命令 Token 预算超过原生任务步骤已预留额度".to_string())
        }
        (reserved, Some(tokens)) if tokens > reserved => {
            return Err("应用命令 Token 预算超过原生任务步骤已预留额度".to_string())
        }
        (reserved, None) if reserved > 0 => {
            return Err("原生任务步骤已预留 Token 时应用命令必须声明 Token 上限".to_string())
        }
        _ => {}
    }
    match (row.12, command.budget.max_cost) {
        (reserved, Some(cost)) if cost > reserved + f64::EPSILON => {
            return Err("应用命令成本预算超过原生任务步骤已预留额度".to_string())
        }
        (reserved, None) if reserved > 0.0 => {
            return Err("原生任务步骤已预留成本时应用命令必须声明成本上限".to_string())
        }
        _ => {}
    }
    Ok(Some(binding.clone()))
}

fn runtime_scope_array(
    value: &Value,
    key: &str,
    label: &str,
) -> Result<Option<HashSet<String>>, String> {
    let Some(scope) = value.get(key) else {
        return Ok(None);
    };
    if scope.is_null() {
        return Ok(Some(HashSet::new()));
    }
    let items = scope
        .as_array()
        .ok_or_else(|| format!("{label} 必须是字符串数组"))?;
    let mut normalized = HashSet::new();
    for item in items {
        let item = item
            .as_str()
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .ok_or_else(|| format!("{label} 包含无效范围"))?;
        normalized.insert(item.to_string());
    }
    Ok(Some(normalized))
}

fn runtime_scope_vault(
    value: &Value,
    key: &str,
    label: &str,
) -> Result<Option<Option<String>>, String> {
    let Some(scope) = value.get(key) else {
        return Ok(None);
    };
    if scope.is_null() {
        return Ok(Some(None));
    }
    let scope = scope
        .as_str()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .ok_or_else(|| format!("{label} 必须是非空字符串或 null"))?;
    Ok(Some(Some(scope.to_string())))
}

fn ensure_runtime_child_scope_subset(
    child_values: &[String],
    parent_scope: Option<&HashSet<String>>,
    step_scope: Option<&HashSet<String>>,
    label: &str,
) -> Result<(), String> {
    for child_value in child_values {
        let child_value = child_value.trim();
        if child_value.is_empty()
            || !parent_scope.is_some_and(|allowed| allowed.contains(child_value))
        {
            return Err(format!("Runtime 子命令{label}超出父任务授权范围"));
        }
        if step_scope.is_some_and(|allowed| !allowed.contains(child_value)) {
            return Err(format!("Runtime 子命令{label}超出父步骤授权范围"));
        }
    }
    Ok(())
}

fn runtime_scope_allows_vault(scope: Option<&str>, vault_id: &str) -> bool {
    scope.is_some_and(|scope| scope == "all" || scope == vault_id)
}

fn validate_runtime_task_step_child_authority(
    command: &ApplicationCommand,
    trace_id: &str,
    step: &RuntimeTaskPlanStepRecord,
    parent_task: &NativeRuntimeTask,
) -> Result<(), String> {
    if step.step_kind != RuntimeTaskStepKind::Capability {
        return Err("只有 capability 原生任务步骤可以派发 Runtime 子命令".to_string());
    }
    let step_capability = step
        .parameters
        .get("capabilityId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "capability 原生任务步骤缺少 capabilityId 授权".to_string())?;
    if step_capability != command.capability_id {
        return Err("Runtime 子命令 capabilityId 与父步骤授权不一致".to_string());
    }
    let parent_capabilities = runtime_scope_array(
        &parent_task.payload,
        "capabilityIds",
        "父任务 capabilityIds",
    )?
    .ok_or_else(|| "父任务缺少 capabilityIds 授权".to_string())?;
    if !parent_capabilities.contains(command.capability_id.trim()) {
        return Err("Runtime 子命令 capabilityId 超出父任务授权范围".to_string());
    }

    let step_operation = step
        .parameters
        .get("operation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "capability 原生任务步骤缺少 operation 授权".to_string())?;
    if step_operation != command.operation {
        return Err("Runtime 子命令 operation 与父步骤授权不一致".to_string());
    }
    let parent_operation = parent_task
        .payload
        .get("operation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "父任务缺少 operation 授权".to_string())?;
    if parent_operation != command.operation {
        return Err("Runtime 子命令 operation 超出父任务授权范围".to_string());
    }

    let parent_trace_id = parent_task
        .trace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "父任务缺少可继承的 Trace".to_string())?;
    if trace_id != parent_trace_id {
        return Err("Runtime 子命令 Trace 与父任务不一致".to_string());
    }

    let parent_vault =
        runtime_scope_vault(&parent_task.payload, "vaultId", "父任务 vaultId")?.flatten();
    let step_vault = runtime_scope_vault(&step.parameters, "vaultId", "父步骤 vaultId")?;
    if let Some(child_vault) = command.vault_id.as_deref().map(str::trim) {
        if child_vault.is_empty()
            || !runtime_scope_allows_vault(parent_vault.as_deref(), child_vault)
        {
            return Err("Runtime 子命令 Vault 超出父任务授权范围".to_string());
        }
        if step_vault
            .as_ref()
            .is_some_and(|scope| !runtime_scope_allows_vault(scope.as_deref(), child_vault))
        {
            return Err("Runtime 子命令 Vault 超出父步骤授权范围".to_string());
        }
    }

    let parent_paths = runtime_scope_array(
        &parent_task.payload,
        "relativePaths",
        "父任务 relativePaths",
    )?;
    let step_paths =
        runtime_scope_array(&step.parameters, "relativePaths", "父步骤 relativePaths")?;
    ensure_runtime_child_scope_subset(
        &command.relative_paths,
        parent_paths.as_ref(),
        step_paths.as_ref(),
        "相对路径",
    )?;

    let parent_network = runtime_scope_array(
        &parent_task.payload,
        "networkTargets",
        "父任务 networkTargets",
    )?;
    let step_network =
        runtime_scope_array(&step.parameters, "networkTargets", "父步骤 networkTargets")?;
    ensure_runtime_child_scope_subset(
        &command.network_targets,
        parent_network.as_ref(),
        step_network.as_ref(),
        "网络目标",
    )?;

    let parent_declared_scope = runtime_scope_array(
        &parent_task.payload,
        "declaredScope",
        "父任务 declaredScope",
    )?;
    let step_declared_scope =
        runtime_scope_array(&step.parameters, "declaredScope", "父步骤 declaredScope")?;
    ensure_runtime_child_scope_subset(
        &command.declared_scope,
        parent_declared_scope.as_ref(),
        step_declared_scope.as_ref(),
        "声明范围",
    )
}

fn latest_runtime_task_plan_revision(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    requested_revision: Option<u64>,
) -> Result<i64, String> {
    let latest = connection
        .query_row(
            "SELECT revision FROM runtime_task_plans
             WHERE workspace_scope=?1 AND task_id=?2
             ORDER BY revision DESC LIMIT 1",
            params![workspace_scope, task_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务计划版本：{error}"))?
        .ok_or_else(|| "原生任务尚未定义可执行计划".to_string())?;
    if let Some(requested_revision) = requested_revision {
        let requested = checked_sqlite_i64(requested_revision, "计划版本")?;
        if requested != latest {
            return Err("只能领取当前原生任务计划版本的步骤".to_string());
        }
    }
    Ok(latest)
}

fn load_runtime_task_plan_step_records(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
) -> Result<Vec<RuntimeTaskPlanStepRecord>, String> {
    let mut statement = connection
        .prepare(
            "SELECT step_id, position, step_kind, title, depends_on_json, parameters_json
             FROM runtime_task_plan_steps
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3
             ORDER BY position ASC",
        )
        .map_err(|error| format!("无法读取原生任务计划步骤：{error}"))?;
    let rows = statement
        .query_map(params![workspace_scope, task_id, plan_revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|error| format!("无法查询原生任务计划步骤：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析原生任务计划步骤：{error}"))?;
    rows.into_iter()
        .map(
            |(step_id, _position, kind, title, depends_on_json, parameters_json)| {
                let step_kind = RuntimeTaskStepKind::parse(&kind)
                    .ok_or_else(|| format!("原生任务计划步骤类型无效：{kind}"))?;
                let depends_on = serde_json::from_str::<Vec<String>>(&depends_on_json)
                    .map_err(|error| format!("原生任务计划步骤依赖损坏：{error}"))?;
                let parameters = serde_json::from_str::<Value>(&parameters_json)
                    .map_err(|error| format!("原生任务计划步骤参数损坏：{error}"))?;
                Ok(RuntimeTaskPlanStepRecord {
                    step_id,
                    effect_class: crate::task_runtime::runtime_task_step_effect_class(
                        &step_kind,
                        &parameters,
                    ),
                    step_kind,
                    title,
                    depends_on,
                    parameters,
                })
            },
        )
        .collect()
}

fn latest_runtime_task_step_states(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
) -> Result<HashMap<String, (String, RuntimeTaskStepEffectClass)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT run.step_id, run.state, run.effect_class
             FROM runtime_task_step_runs run
             JOIN (
               SELECT step_id, MAX(attempt) AS attempt
               FROM runtime_task_step_runs
               WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3
               GROUP BY step_id
             ) latest
               ON latest.step_id=run.step_id AND latest.attempt=run.attempt
             WHERE run.workspace_scope=?1 AND run.task_id=?2 AND run.plan_revision=?3",
        )
        .map_err(|error| format!("无法读取原生任务步骤运行状态：{error}"))?;
    let rows = statement
        .query_map(params![workspace_scope, task_id, plan_revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("无法查询原生任务步骤运行状态：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析原生任务步骤运行状态：{error}"))?;
    rows.into_iter()
        .map(|(step_id, state, effect_class)| {
            Ok((
                step_id,
                (
                    state,
                    RuntimeTaskStepEffectClass::parse(&effect_class)
                        .ok_or_else(|| "原生任务步骤效果分类损坏".to_string())?,
                ),
            ))
        })
        .collect()
}

fn expire_runtime_task_step_claims(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
    now: &str,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "SELECT claim_id, step_id, reserved_tool_calls, reserved_runtime_seconds,
                    reserved_tokens, reserved_cost
             FROM runtime_task_step_runs
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3
               AND state='claimed' AND lease_expires_at<=?4",
        )
        .map_err(|error| format!("无法读取过期原生任务步骤领取：{error}"))?;
    let expired = statement
        .query_map(
            params![workspace_scope, task_id, plan_revision, now],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                ))
            },
        )
        .map_err(|error| format!("无法查询过期原生任务步骤领取：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析过期原生任务步骤领取：{error}"))?;
    drop(statement);
    for (claim_id, step_id, tool_calls, runtime_seconds, tokens, cost) in expired {
        let changed = connection
            .execute(
                "UPDATE runtime_task_step_runs
                 SET state='expired', finished_at=?4
                 WHERE workspace_scope=?1 AND claim_id=?2 AND state='claimed'",
                params![workspace_scope, claim_id, task_id, now],
            )
            .map_err(|error| format!("无法标记过期原生任务步骤领取：{error}"))?;
        if changed != 1 {
            continue;
        }
        connection
            .execute(
                "UPDATE runtime_task_execution_budgets
                 SET reserved_steps=MAX(0, reserved_steps-1),
                     reserved_tool_calls=MAX(0, reserved_tool_calls-?4),
                     reserved_runtime_seconds=MAX(0, reserved_runtime_seconds-?5),
                     reserved_tokens=MAX(0, reserved_tokens-?6),
                     reserved_cost=MAX(0, reserved_cost-?7), updated_at=?8
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                params![
                    workspace_scope,
                    task_id,
                    plan_revision,
                    tool_calls,
                    runtime_seconds,
                    tokens,
                    cost,
                    now,
                ],
            )
            .map_err(|error| format!("无法释放过期原生任务步骤预算：{error}"))?;
        let output = serde_json::json!({"reason": "lease_expired"});
        let output_json = canonical_runtime_json_string(&output, "过期步骤回执")?;
        let receipt_id = format!("receipt-expired-{claim_id}");
        let content_hash = format!(
            "sha256:{:x}",
            Sha256::digest(
                serde_json::to_string(&serde_json::json!({
                    "state": "expired",
                    "output": output,
                    "error": "lease_expired",
                    "consumedToolCalls": 0,
                    "consumedRuntimeSeconds": 0,
                    "consumedTokens": 0,
                    "consumedCost": 0.0,
                }))
                .map_err(|error| format!("无法序列化过期步骤回执：{error}"))?
                .as_bytes(),
            )
        );
        connection
            .execute(
                "INSERT INTO runtime_task_step_receipts
                 (workspace_scope, receipt_id, claim_id, task_id, plan_revision, step_id, state,
                  output_json, error, consumed_tool_calls, consumed_runtime_seconds,
                  consumed_tokens, consumed_cost, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'expired', ?7, 'lease_expired',
                         0, 0, 0, 0, ?8, ?9)",
                params![
                    workspace_scope,
                    receipt_id,
                    claim_id,
                    task_id,
                    plan_revision,
                    step_id,
                    output_json,
                    content_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存过期原生任务步骤回执：{error}"))?;
    }
    Ok(())
}

fn ensure_runtime_task_running_for_step_claim(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    now: &str,
) -> Result<(), String> {
    let current = read_native_runtime_task(connection, workspace_scope, task_id)?;
    match current.state.as_str() {
        "running" => Ok(()),
        "queued" => {
            let mut payload = current.payload;
            let object = payload
                .as_object_mut()
                .ok_or_else(|| "原生任务负载不是 JSON 对象".to_string())?;
            object.insert("state".to_string(), Value::String("running".to_string()));
            object.insert("progress".to_string(), Value::from(current.progress.max(1)));
            object.insert("updatedAt".to_string(), Value::String(now.to_string()));
            let payload_json = serde_json::to_string(&payload)
                .map_err(|error| format!("无法序列化原生任务步骤领取状态：{error}"))?;
            connection
                .execute(
                    "UPDATE runtime_tasks SET state='running', payload=?3, updated_at=?4
                     WHERE workspace_scope=?1 AND id=?2 AND state='queued'",
                    params![workspace_scope, task_id, payload_json, now],
                )
                .map_err(|error| format!("无法启动原生任务步骤领取：{error}"))?;
            connection
                .execute(
                    "UPDATE runtime_task_attempts SET finished_at=?3
                     WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                    params![workspace_scope, task_id, now],
                )
                .map_err(|error| format!("无法结束原生任务排队尝试：{error}"))?;
            connection
                .execute(
                    "INSERT INTO runtime_task_attempts
                     (id, workspace_scope, task_id, state, detail, started_at)
                     VALUES (?1, ?2, ?3, 'running', '由步骤执行器原子领取', ?4)",
                    params![Uuid::new_v4().to_string(), workspace_scope, task_id, now],
                )
                .map_err(|error| format!("无法记录原生任务步骤领取尝试：{error}"))?;
            connection
                .execute(
                    "INSERT INTO runtime_task_transitions
                     (id, workspace_scope, task_id, from_state, to_state, detail, checkpoint_json, created_at)
                     VALUES (?1, ?2, ?3, 'queued', 'running', '由步骤执行器原子领取', '{}', ?4)",
                    params![Uuid::new_v4().to_string(), workspace_scope, task_id, now],
                )
                .map_err(|error| format!("无法记录原生任务步骤领取转换：{error}"))?;
            Ok(())
        }
        "cancelled" => Err("父任务已取消，不能领取任务步骤".to_string()),
        _ => Err(format!("原生任务状态 {} 不能领取计划步骤", current.state)),
    }
}

fn read_runtime_task_step_receipt(
    connection: &Connection,
    workspace_scope: &str,
    receipt_id: &str,
) -> Result<Option<RuntimeTaskStepReceipt>, String> {
    let row = connection
        .query_row(
            "SELECT receipt_id, claim_id, task_id, plan_revision, step_id, state,
                    output_json, error, consumed_tool_calls, consumed_runtime_seconds,
                    consumed_tokens, consumed_cost, content_hash, created_at
             FROM runtime_task_step_receipts
             WHERE workspace_scope=?1 AND receipt_id=?2",
            params![workspace_scope, receipt_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, f64>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务步骤回执：{error}"))?;
    row.map(
        |(
            receipt_id,
            claim_id,
            task_id,
            plan_revision,
            step_id,
            state,
            output_json,
            error,
            consumed_tool_calls,
            consumed_runtime_seconds,
            consumed_tokens,
            consumed_cost,
            content_hash,
            created_at,
        )| {
            Ok(RuntimeTaskStepReceipt {
                receipt_id,
                step_claim_id: claim_id,
                runtime_task_id: task_id,
                plan_revision: u64::try_from(plan_revision).unwrap_or_default(),
                step_id,
                state,
                output: serde_json::from_str(&output_json)
                    .map_err(|error| format!("原生任务步骤回执输出损坏：{error}"))?,
                error,
                consumed_tool_calls: u64::try_from(consumed_tool_calls).unwrap_or_default(),
                consumed_runtime_seconds: u64::try_from(consumed_runtime_seconds)
                    .unwrap_or_default(),
                consumed_tokens: u64::try_from(consumed_tokens).unwrap_or_default(),
                consumed_cost,
                content_hash,
                created_at,
            })
        },
    )
    .transpose()
}

#[allow(clippy::too_many_arguments)]
fn append_runtime_step_receipt_evidence(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
    step_id: &str,
    claim_id: &str,
    receipt_id: &str,
    receipt_content_hash: &str,
    trace_id: &str,
    created_at: &str,
) -> Result<Vec<String>, String> {
    let mut statement = transaction
        .prepare(
            "SELECT requirement_id
             FROM runtime_task_completion_requirements
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3
               AND step_id=?4 AND evidence_type='runtime.step_receipt'
             ORDER BY position ASC",
        )
        .map_err(|error| format!("无法读取步骤回执完成要求：{error}"))?;
    let requirement_ids = statement
        .query_map(
            params![workspace_scope, task_id, plan_revision, step_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("无法查询步骤回执完成要求：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析步骤回执完成要求：{error}"))?;
    drop(statement);
    if requirement_ids.is_empty() {
        return Ok(Vec::new());
    }
    let evidence_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM runtime_task_evidence
             WHERE workspace_scope=?1 AND task_id=?2",
            params![workspace_scope, task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法统计步骤回执证据：{error}"))?;
    if evidence_count.saturating_add(requirement_ids.len() as i64)
        > MAX_RUNTIME_TASK_EVIDENCE as i64
    {
        return Err("步骤回执证据将超过单任务 2048 条安全上限".to_string());
    }
    let mut evidence_ids = Vec::with_capacity(requirement_ids.len());
    for requirement_id in requirement_ids {
        let evidence_seed = format!(
            "{workspace_scope}\0{task_id}\0{plan_revision}\0{step_id}\0{claim_id}\0{requirement_id}"
        );
        let evidence_id = format!(
            "step-receipt-{:x}",
            Sha256::digest(evidence_seed.as_bytes())
        );
        let payload = serde_json::json!({
            "receiptId": receipt_id,
            "stepClaimId": claim_id,
            "receiptContentHash": receipt_content_hash,
            "state": "succeeded",
        });
        let payload_json = canonical_runtime_json_string(&payload, "步骤回执证据")?;
        let envelope = serde_json::json!({
            "planRevision": plan_revision,
            "requirementId": requirement_id,
            "evidenceType": "runtime.step_receipt",
            "sourceKind": "runtime",
            "sourceRef": receipt_id,
            "payload": canonical_runtime_json(&payload),
        });
        let envelope_json = canonical_runtime_json_string(&envelope, "步骤回执证据封套")?;
        if envelope_json.len() > MAX_RUNTIME_EVIDENCE_BYTES {
            return Err("步骤回执证据超过 256 KB 安全上限".to_string());
        }
        let evidence_content_hash =
            format!("sha256:{:x}", Sha256::digest(envelope_json.as_bytes()));
        transaction
            .execute(
                "INSERT INTO runtime_task_evidence
                 (workspace_scope, task_id, evidence_id, plan_revision, requirement_id, step_id,
                  evidence_type, source_kind, source_ref, payload_json, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'runtime.step_receipt', 'runtime', ?7, ?8, ?9, ?10)",
                params![
                    workspace_scope,
                    task_id,
                    evidence_id,
                    plan_revision,
                    requirement_id,
                    step_id,
                    receipt_id,
                    payload_json,
                    evidence_content_hash,
                    created_at,
                ],
            )
            .map_err(|error| format!("无法保存可信步骤回执证据：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id,
                entity_kind: "runtime_task",
                entity_id: task_id,
                event_type: "task.evidence_appended",
                state: "recorded",
                payload: &serde_json::json!({
                    "evidenceId": evidence_id,
                    "planRevision": plan_revision,
                    "requirementId": requirement_id,
                    "evidenceType": "runtime.step_receipt",
                    "sourceKind": "runtime",
                    "sourceRef": receipt_id,
                    "receiptContentHash": receipt_content_hash,
                    "contentHash": evidence_content_hash,
                }),
                created_at,
            },
        )?;
        evidence_ids.push(evidence_id);
    }
    Ok(evidence_ids)
}

fn cancel_runtime_task_step_claims(
    connection: &Transaction<'_>,
    workspace_scope: &str,
    task_id: &str,
    now: &str,
) -> Result<(), String> {
    let mut revision_statement = connection
        .prepare(
            "SELECT plan_revision FROM runtime_task_execution_budgets
             WHERE workspace_scope=?1 AND task_id=?2",
        )
        .map_err(|error| format!("无法读取父任务步骤取消栅栏：{error}"))?;
    let revisions = revision_statement
        .query_map(params![workspace_scope, task_id], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("无法查询父任务步骤取消栅栏：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析父任务步骤取消栅栏：{error}"))?;
    drop(revision_statement);
    for revision in revisions {
        let mut run_statement = connection
            .prepare(
                "SELECT claim_id, step_id, reserved_tool_calls, reserved_runtime_seconds,
                        reserved_tokens, reserved_cost
                 FROM runtime_task_step_runs
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3 AND state='claimed'",
            )
            .map_err(|error| format!("无法读取待取消原生任务步骤领取：{error}"))?;
        let runs = run_statement
            .query_map(params![workspace_scope, task_id, revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                ))
            })
            .map_err(|error| format!("无法查询待取消原生任务步骤领取：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析待取消原生任务步骤领取：{error}"))?;
        drop(run_statement);
        let mut released_tool_calls = 0_i64;
        let mut released_runtime_seconds = 0_i64;
        let mut released_tokens = 0_i64;
        let mut released_cost = 0.0_f64;
        let mut released_steps = 0_i64;
        for (claim_id, step_id, tool_calls, runtime_seconds, tokens, cost) in runs {
            let changed = connection
                .execute(
                    "UPDATE runtime_task_step_runs
                     SET state='cancelled', finished_at=?4
                     WHERE workspace_scope=?1 AND claim_id=?2 AND state='claimed'",
                    params![workspace_scope, claim_id, task_id, now],
                )
                .map_err(|error| format!("无法取消原生任务步骤领取：{error}"))?;
            if changed != 1 {
                continue;
            }
            released_steps += 1;
            released_tool_calls += tool_calls;
            released_runtime_seconds += runtime_seconds;
            released_tokens += tokens;
            released_cost += cost;
            let output = serde_json::json!({"reason": "parent_task_cancelled"});
            let output_json = canonical_runtime_json_string(&output, "取消步骤回执")?;
            let receipt_id = format!("receipt-cancelled-{claim_id}");
            let content_json = canonical_runtime_json_string(
                &serde_json::json!({
                    "state": "cancelled",
                    "output": output,
                    "error": "parent_task_cancelled",
                    "consumedToolCalls": 0,
                    "consumedRuntimeSeconds": 0,
                    "consumedTokens": 0,
                    "consumedCost": 0.0,
                }),
                "取消步骤回执",
            )?;
            let content_hash = format!("sha256:{:x}", Sha256::digest(content_json.as_bytes()));
            connection
                .execute(
                    "INSERT INTO runtime_task_step_receipts
                     (workspace_scope, receipt_id, claim_id, task_id, plan_revision, step_id, state,
                      output_json, error, consumed_tool_calls, consumed_runtime_seconds,
                      consumed_tokens, consumed_cost, content_hash, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'cancelled', ?7,
                             'parent_task_cancelled', 0, 0, 0, 0, ?8, ?9)",
                    params![
                        workspace_scope,
                        receipt_id,
                        claim_id,
                        task_id,
                        revision,
                        step_id,
                        output_json,
                        content_hash,
                        now,
                    ],
                )
                .map_err(|error| format!("无法保存取消原生任务步骤回执：{error}"))?;
        }
        connection
            .execute(
                "UPDATE runtime_task_execution_budgets
                 SET reserved_steps=MAX(0, reserved_steps-?4),
                     reserved_tool_calls=MAX(0, reserved_tool_calls-?5),
                     reserved_runtime_seconds=MAX(0, reserved_runtime_seconds-?6),
                     reserved_tokens=MAX(0, reserved_tokens-?7),
                     reserved_cost=MAX(0, reserved_cost-?8),
                     cancellation_fence=cancellation_fence+1,
                     cancelled_at=COALESCE(cancelled_at, ?9), updated_at=?9
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                params![
                    workspace_scope,
                    task_id,
                    revision,
                    released_steps,
                    released_tool_calls,
                    released_runtime_seconds,
                    released_tokens,
                    released_cost,
                    now,
                ],
            )
            .map_err(|error| format!("无法提交父任务步骤取消栅栏：{error}"))?;
    }
    let mut child_statement = connection
        .prepare(
            "SELECT DISTINCT binding.child_task_id
             FROM runtime_task_step_command_bindings binding
             JOIN runtime_task_step_runs run
               ON run.workspace_scope=binding.workspace_scope AND run.claim_id=binding.claim_id
             WHERE run.workspace_scope=?1 AND run.task_id=?2",
        )
        .map_err(|error| format!("无法读取原生任务步骤子任务绑定：{error}"))?;
    let child_task_ids = child_statement
        .query_map(params![workspace_scope, task_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("无法查询原生任务步骤子任务绑定：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析原生任务步骤子任务绑定：{error}"))?;
    drop(child_statement);
    for child_task_id in child_task_ids {
        let child = read_native_runtime_task(connection, workspace_scope, &child_task_id)?;
        if matches!(child.state.as_str(), "succeeded" | "failed" | "cancelled") {
            continue;
        }
        cancel_runtime_task_step_claims(connection, workspace_scope, &child_task_id, now)?;
        let mut payload = child.payload;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| "原生任务子任务负载不是 JSON 对象".to_string())?;
        object.insert("state".to_string(), Value::String("cancelled".to_string()));
        object.insert("updatedAt".to_string(), Value::String(now.to_string()));
        object.insert(
            "result".to_string(),
            Value::String(format!("父任务 {task_id} 已取消")),
        );
        let payload_json = serde_json::to_string(&payload)
            .map_err(|error| format!("无法序列化取消的原生任务子任务：{error}"))?;
        connection
            .execute(
                "UPDATE runtime_tasks
                 SET state='cancelled', payload=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND id=?2
                   AND state IN ('created', 'queued', 'running', 'awaiting_approval', 'paused')",
                params![workspace_scope, child_task_id, payload_json, now],
            )
            .map_err(|error| format!("无法传播父任务取消到原生子任务：{error}"))?;
        connection
            .execute(
                "UPDATE runtime_task_attempts SET finished_at=?3
                 WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                params![workspace_scope, child_task_id, now],
            )
            .map_err(|error| format!("无法结束被取消的原生子任务尝试：{error}"))?;
        connection
            .execute(
                "INSERT INTO runtime_task_attempts
                 (id, workspace_scope, task_id, state, detail, started_at, finished_at)
                 VALUES (?1, ?2, ?3, 'cancelled', ?4, ?5, ?5)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    child_task_id,
                    format!("父任务 {task_id} 取消传播"),
                    now,
                ],
            )
            .map_err(|error| format!("无法记录被取消的原生子任务尝试：{error}"))?;
        connection
            .execute(
                "INSERT INTO runtime_task_transitions
                 (id, workspace_scope, task_id, from_state, to_state, detail, checkpoint_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'cancelled', ?5, '{}', ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    child_task_id,
                    child.state,
                    format!("父任务 {task_id} 取消传播"),
                    now,
                ],
            )
            .map_err(|error| format!("无法记录原生子任务取消转换：{error}"))?;
        connection
            .execute(
                "UPDATE application_commands SET state='cancelled', updated_at=?3
                 WHERE workspace_scope=?1 AND task_id=?2 AND state='accepted'",
                params![workspace_scope, child_task_id, now],
            )
            .map_err(|error| format!("无法取消原生子任务应用命令：{error}"))?;
        let trace_id = child
            .trace_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: Some(child_task_id.clone()),
            trace_id: Some(trace_id.clone()),
            event_type: "task.state_changed".to_string(),
            state: "cancelled".to_string(),
            created_at: now.to_string(),
            vault_id: payload
                .get("vaultId")
                .and_then(Value::as_str)
                .map(str::to_string),
            relative_path: None,
            detail: format!("父任务 {task_id} 取消传播"),
        };
        insert_operation_event_in_transaction(connection, &event)
            .map_err(|error| format!("无法保存原生子任务取消审计事件：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            connection,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: &child_task_id,
                event_type: "task.cancelled_by_parent",
                state: "cancelled",
                payload: &serde_json::json!({"parentTaskId": task_id}),
                created_at: now,
            },
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_runtime_task_plan_revision(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    revision: i64,
    plan: &RuntimeTaskPlanInput,
    plan_json: &str,
    content_hash: &str,
    created_at: &str,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO runtime_task_plans
             (workspace_scope, task_id, revision, schema_version, goal, plan_json, content_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace_scope,
                task_id,
                revision,
                plan.schema_version.trim(),
                plan.goal.trim(),
                plan_json,
                content_hash,
                created_at,
            ],
        )
        .map_err(|error| format!("无法保存原生任务计划：{error}"))?;
    for (position, step) in plan.steps.iter().enumerate() {
        let depends_on_json = serde_json::to_string(&step.depends_on)
            .map_err(|error| format!("无法序列化计划步骤依赖：{error}"))?;
        let parameters_json = canonical_runtime_json_string(&step.parameters, "计划步骤参数")?;
        connection
            .execute(
                "INSERT INTO runtime_task_plan_steps
                 (workspace_scope, task_id, plan_revision, step_id, position, step_kind, title,
                  depends_on_json, parameters_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    workspace_scope,
                    task_id,
                    revision,
                    step.id.trim(),
                    position as i64,
                    step.kind.as_str(),
                    step.title.trim(),
                    depends_on_json,
                    parameters_json,
                ],
            )
            .map_err(|error| format!("无法保存原生任务计划步骤：{error}"))?;
    }
    for (position, requirement) in plan.completion_contract.requirements.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO runtime_task_completion_requirements
                 (workspace_scope, task_id, plan_revision, requirement_id, position, step_id,
                  evidence_type, minimum_count, description)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    workspace_scope,
                    task_id,
                    revision,
                    requirement.id.trim(),
                    position as i64,
                    requirement.step_id.as_deref().map(str::trim),
                    requirement.evidence_type.trim(),
                    requirement.minimum_count as i64,
                    requirement.description.trim(),
                ],
            )
            .map_err(|error| format!("无法保存原生任务完成契约：{error}"))?;
    }
    Ok(())
}

fn runtime_task_plan_from_input(plan: RuntimeTaskPlanInput) -> RuntimeTaskPlan {
    RuntimeTaskPlan {
        schema_version: plan.schema_version,
        goal: plan.goal,
        steps: plan
            .steps
            .into_iter()
            .map(|step| RuntimeTaskPlanStep {
                id: step.id,
                kind: step.kind,
                title: step.title,
                depends_on: step.depends_on,
                parameters: step.parameters,
            })
            .collect(),
        completion_contract: RuntimeTaskCompletionContract {
            mode: plan.completion_contract.mode,
            requirements: plan
                .completion_contract
                .requirements
                .into_iter()
                .map(|requirement| RuntimeTaskCompletionRequirement {
                    id: requirement.id,
                    step_id: requirement.step_id,
                    evidence_type: requirement.evidence_type,
                    minimum_count: requirement.minimum_count,
                    description: requirement.description,
                })
                .collect(),
        },
        metadata: plan.metadata,
    }
}

fn evaluate_runtime_task_completion(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
) -> Result<Option<RuntimeTaskCompletionStatus>, String> {
    let revision = connection
        .query_row(
            "SELECT revision FROM runtime_task_plans
             WHERE workspace_scope=?1 AND task_id=?2 ORDER BY revision DESC LIMIT 1",
            params![workspace_scope, task_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务完成契约版本：{error}"))?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT requirement.requirement_id, requirement.description,
                    requirement.evidence_type, requirement.minimum_count,
                    COUNT(evidence.evidence_id)
             FROM runtime_task_completion_requirements requirement
             LEFT JOIN runtime_task_evidence evidence
               ON evidence.workspace_scope=requirement.workspace_scope
              AND evidence.task_id=requirement.task_id
              AND evidence.plan_revision=requirement.plan_revision
              AND evidence.requirement_id=requirement.requirement_id
             WHERE requirement.workspace_scope=?1 AND requirement.task_id=?2
               AND requirement.plan_revision=?3
             GROUP BY requirement.requirement_id, requirement.description,
                      requirement.evidence_type, requirement.minimum_count, requirement.position
             ORDER BY requirement.position",
        )
        .map_err(|error| format!("无法准备原生任务完成契约校验：{error}"))?;
    let requirements = statement
        .query_map(params![workspace_scope, task_id, revision], |row| {
            let required_count = row.get::<_, i64>(3)?.max(0) as usize;
            let observed_count = row.get::<_, i64>(4)?.max(0) as usize;
            Ok(RuntimeTaskRequirementStatus {
                id: row.get(0)?,
                description: row.get(1)?,
                evidence_type: row.get(2)?,
                required_count,
                observed_count,
                satisfied: observed_count >= required_count,
            })
        })
        .map_err(|error| format!("无法读取原生任务完成契约校验：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析原生任务完成契约校验：{error}"))?;
    let satisfied =
        !requirements.is_empty() && requirements.iter().all(|requirement| requirement.satisfied);
    Ok(Some(RuntimeTaskCompletionStatus {
        plan_revision: revision.max(0) as u64,
        satisfied,
        requirements,
    }))
}

#[allow(clippy::too_many_arguments)]
fn runtime_task_evidence_from_parts(
    task_id: &str,
    evidence_id: &str,
    plan_revision: i64,
    requirement_id: &str,
    step_id: Option<String>,
    evidence_type: &str,
    source_kind: &str,
    source_ref: &str,
    payload_json: &str,
    content_hash: &str,
    created_at: &str,
) -> Result<RuntimeTaskEvidence, String> {
    let source_kind = RuntimeTaskEvidenceSourceKind::parse(source_kind)
        .ok_or_else(|| "原生任务证据来源类型损坏".to_string())?;
    let payload = serde_json::from_str(payload_json)
        .map_err(|error| format!("原生任务证据 JSON 损坏：{error}"))?;
    Ok(RuntimeTaskEvidence {
        task_id: task_id.to_string(),
        evidence_id: evidence_id.to_string(),
        plan_revision: u64::try_from(plan_revision)
            .map_err(|_| "原生任务证据计划版本损坏".to_string())?,
        requirement_id: requirement_id.to_string(),
        step_id,
        evidence_type: evidence_type.to_string(),
        source_kind,
        source_ref: source_ref.to_string(),
        payload,
        content_hash: content_hash.to_string(),
        created_at: created_at.to_string(),
    })
}

fn read_runtime_task_contract(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
    if !valid_runtime_identifier(task_id, 180) {
        return Err("原生任务契约 taskId 无效".to_string());
    }
    read_native_runtime_task(connection, workspace_scope, task_id)?;
    let stored_plan = connection
        .query_row(
            "SELECT revision, plan_json, content_hash, created_at
             FROM runtime_task_plans
             WHERE workspace_scope=?1 AND task_id=?2 ORDER BY revision DESC LIMIT 1",
            params![workspace_scope, task_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务计划：{error}"))?;
    let Some((revision, plan_json, content_hash, created_at)) = stored_plan else {
        return Ok(None);
    };
    let plan_input = serde_json::from_str::<RuntimeTaskPlanInput>(&plan_json)
        .map_err(|error| format!("原生任务计划 JSON 损坏：{error}"))?;
    crate::task_runtime::validate_runtime_task_plan(&plan_input)
        .map_err(|error| format!("原生任务计划校验失败：{error}"))?;
    let completion = evaluate_runtime_task_completion(connection, workspace_scope, task_id)?
        .ok_or_else(|| "原生任务完成契约缺失".to_string())?;
    if completion.plan_revision != revision.max(0) as u64 {
        return Err("原生任务计划与完成契约版本不一致".to_string());
    }
    let raw_evidence = {
        let mut statement = connection
            .prepare(
                "SELECT evidence_id, plan_revision, requirement_id, step_id, evidence_type,
                        source_kind, source_ref, payload_json, content_hash, created_at
                 FROM runtime_task_evidence
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3
                 ORDER BY created_at, evidence_id",
            )
            .map_err(|error| format!("无法准备原生任务证据查询：{error}"))?;
        let rows = statement
            .query_map(params![workspace_scope, task_id, revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|error| format!("无法读取原生任务证据：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析原生任务证据：{error}"))?
    };
    let evidence = raw_evidence
        .into_iter()
        .map(|item| {
            runtime_task_evidence_from_parts(
                task_id, &item.0, item.1, &item.2, item.3, &item.4, &item.5, &item.6, &item.7,
                &item.8, &item.9,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(RuntimeTaskContractSnapshot {
        task_id: task_id.to_string(),
        plan: RuntimeTaskPlanSnapshot {
            task_id: task_id.to_string(),
            revision: revision.max(0) as u64,
            plan: runtime_task_plan_from_input(plan_input),
            content_hash,
            created_at,
        },
        completion,
        evidence,
    }))
}

fn schedule_occurrence_identifier(
    schedule_id: &str,
    schedule_kind: &str,
    scheduled_for: &str,
) -> String {
    let digest = Sha256::digest(
        format!("yunspire:schedule-occurrence:v1\0{schedule_kind}\0{schedule_id}\0{scheduled_for}")
            .as_bytes(),
    );
    format!("schedule-occurrence-{digest:x}")
}

fn schedule_task_plan(
    schedule_id: &str,
    schedule_kind: &str,
    occurrence_id: &str,
    scheduled_for: &str,
) -> RuntimeTaskPlanInput {
    RuntimeTaskPlanInput {
        schema_version: "1.0".to_string(),
        goal: format!("派发到期{schedule_kind}日程 {schedule_id}"),
        steps: vec![RuntimeTaskPlanStepInput {
            id: "dispatch".to_string(),
            kind: RuntimeTaskStepKind::ScheduleDispatch,
            title: "派发日程到受策略约束的执行路径".to_string(),
            depends_on: Vec::new(),
            parameters: serde_json::json!({
                "scheduleId": schedule_id,
                "scheduleKind": schedule_kind,
                "occurrenceId": occurrence_id,
                "scheduledFor": scheduled_for,
            }),
        }],
        completion_contract: RuntimeTaskCompletionContractInput {
            mode: RuntimeTaskCompletionMode::AllOf,
            requirements: vec![RuntimeTaskCompletionRequirementInput {
                id: "dispatch-ack".to_string(),
                step_id: Some("dispatch".to_string()),
                evidence_type: "schedule.dispatch_ack".to_string(),
                minimum_count: 1,
                description: "Renderer 已接收稳定 occurrence 并进入原有策略命令路径".to_string(),
            }],
        },
        metadata: serde_json::json!({
            "origin": "native_scheduler",
            "scheduleId": schedule_id,
            "scheduleKind": schedule_kind,
            "occurrenceId": occurrence_id,
            "scheduledFor": scheduled_for,
        }),
    }
}

fn ensure_schedule_occurrence_task(
    connection: &Connection,
    ScheduleOccurrenceClaim {
        workspace_scope,
        schedule_id,
        schedule_kind,
        scheduled_for,
        schedule_revision,
        schedule_payload,
        schedule_payload_hash,
    }: ScheduleOccurrenceClaim<'_>,
) -> Result<Option<ScheduleOccurrenceTask>, String> {
    let occurrence_id = schedule_occurrence_identifier(schedule_id, schedule_kind, scheduled_for);
    let existing = connection
        .query_row(
            "SELECT occurrence.runtime_task_id, task.state, occurrence.schedule_revision, task.payload
             FROM runtime_schedule_occurrences occurrence
             JOIN runtime_tasks task
               ON task.workspace_scope=occurrence.workspace_scope
              AND task.id=occurrence.runtime_task_id
             WHERE occurrence.workspace_scope=?1 AND occurrence.occurrence_id=?2",
            params![workspace_scope, occurrence_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取日程 occurrence：{error}"))?;
    if let Some((runtime_task_id, state, occurrence_revision, task_payload_json)) = existing {
        if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
            return Ok(None);
        }
        let mut task_payload = serde_json::from_str::<Value>(&task_payload_json)
            .map_err(|error| format!("日程 wrapper payload 无法解析：{error}"))?;
        let existing_snapshot = task_payload.as_object().and_then(|object| {
            let payload = object.get("schedulePayload")?.clone();
            let payload_hash = object
                .get("schedulePayloadHash")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())?;
            Some((payload, payload_hash.to_string()))
        });
        let (payload, payload_hash) = if let Some(snapshot) =
            read_runtime_schedule_revision_snapshot(
                connection,
                workspace_scope,
                schedule_id,
                schedule_kind,
                occurrence_revision,
            )? {
            snapshot
        } else if let Some((payload, payload_hash)) = existing_snapshot {
            verified_schedule_payload_snapshot(payload, &payload_hash, "日程 wrapper 快照")?
        } else {
            return Err(format!(
                "日程 occurrence 缺少不可变历史快照：{schedule_id}/{schedule_kind}/revision-{occurrence_revision}"
            ));
        };
        let object = task_payload
            .as_object_mut()
            .ok_or_else(|| "日程 wrapper payload 必须是 JSON 对象".to_string())?;
        let mut payload_changed = false;
        for (key, value) in [
            ("scheduleId", Value::String(schedule_id.to_string())),
            ("scheduleKind", Value::String(schedule_kind.to_string())),
            ("scheduleOccurrenceId", Value::String(occurrence_id.clone())),
            ("scheduledFor", Value::String(scheduled_for.to_string())),
            (
                "scheduleRevision",
                Value::Number(occurrence_revision.into()),
            ),
            ("schedulePayload", payload.clone()),
            ("schedulePayloadHash", Value::String(payload_hash.clone())),
        ] {
            if object.get(key) != Some(&value) {
                object.insert(key.to_string(), value);
                payload_changed = true;
            }
        }
        if payload_changed {
            let now = Utc::now().to_rfc3339();
            connection
                .execute(
                    "UPDATE runtime_tasks SET payload=?3, updated_at=?4
                     WHERE workspace_scope=?1 AND id=?2",
                    params![
                        workspace_scope,
                        runtime_task_id,
                        serde_json::to_string(&task_payload)
                            .map_err(|error| format!("无法序列化日程 wrapper 快照：{error}"))?,
                        now,
                    ],
                )
                .map_err(|error| format!("无法回填日程 wrapper 快照：{error}"))?;
        }
        return Ok(Some(ScheduleOccurrenceTask {
            occurrence_id,
            runtime_task_id,
            schedule_revision: u64::try_from(occurrence_revision).unwrap_or(1),
            payload,
            payload_hash,
        }));
    }
    let runtime_task_id = format!("task-{occurrence_id}");
    let now = Utc::now().to_rfc3339();
    let trace_id = format!("trace-{occurrence_id}");
    let title = schedule_payload
        .get("name")
        .or_else(|| schedule_payload.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("到期日程")
        .chars()
        .take(240)
        .collect::<String>();
    let task_payload = serde_json::json!({
        "id": &runtime_task_id,
        "kind": "scheduled_dispatch",
        "state": "queued",
        "title": title,
        "traceId": &trace_id,
        "progress": 0,
        "scheduleId": schedule_id,
        "scheduleKind": schedule_kind,
        "scheduleOccurrenceId": &occurrence_id,
        "scheduledFor": scheduled_for,
        "scheduleRevision": schedule_revision,
        "schedulePayload": schedule_payload,
        "schedulePayloadHash": schedule_payload_hash,
        "createdAt": now,
        "updatedAt": now,
    });
    connection
        .execute(
            "INSERT INTO runtime_tasks
             (workspace_scope, id, state, title, trace_id, payload, created_at, updated_at)
             VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?6)",
            params![
                workspace_scope,
                runtime_task_id,
                title,
                trace_id,
                serde_json::to_string(&task_payload)
                    .map_err(|error| format!("无法序列化日程原生任务：{error}"))?,
                now,
            ],
        )
        .map_err(|error| format!("无法创建日程原生任务：{error}"))?;
    connection
        .execute(
            "INSERT INTO runtime_task_attempts
             (id, workspace_scope, task_id, state, detail, started_at)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                runtime_task_id,
                "由原生调度 occurrence 创建",
                now,
            ],
        )
        .map_err(|error| format!("无法记录日程任务首次尝试：{error}"))?;
    let plan = schedule_task_plan(schedule_id, schedule_kind, &occurrence_id, scheduled_for);
    crate::task_runtime::validate_runtime_task_plan(&plan)?;
    let plan_json = canonical_runtime_json_string(
        &serde_json::to_value(&plan).map_err(|error| format!("无法序列化日程任务计划：{error}"))?,
        "日程任务计划",
    )?;
    let plan_hash = format!("sha256:{:x}", Sha256::digest(plan_json.as_bytes()));
    insert_runtime_task_plan_revision(
        connection,
        workspace_scope,
        &runtime_task_id,
        1,
        &plan,
        &plan_json,
        &plan_hash,
        &now,
    )?;
    ensure_runtime_task_execution_budget(
        connection,
        workspace_scope,
        &runtime_task_id,
        1,
        &task_payload,
        None,
        &now,
    )?;
    connection
        .execute(
            "INSERT INTO runtime_schedule_occurrences
             (workspace_scope, occurrence_id, schedule_id, schedule_kind, scheduled_for,
              schedule_revision, runtime_task_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace_scope,
                occurrence_id,
                schedule_id,
                schedule_kind,
                scheduled_for,
                schedule_revision,
                runtime_task_id,
                now,
            ],
        )
        .map_err(|error| format!("无法保存日程 occurrence：{error}"))?;
    crate::trace::record_trace_event_in_connection(
        connection,
        workspace_scope,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "runtime_task",
            entity_id: &runtime_task_id,
            event_type: "schedule.occurrence_claimed",
            state: "queued",
            payload: &serde_json::json!({
                "scheduleId": schedule_id,
                "scheduleKind": schedule_kind,
                "occurrenceId": occurrence_id,
                "scheduledFor": scheduled_for,
                "scheduleRevision": schedule_revision,
            }),
            created_at: &now,
        },
    )?;
    Ok(Some(ScheduleOccurrenceTask {
        occurrence_id,
        runtime_task_id,
        schedule_revision: u64::try_from(schedule_revision).unwrap_or(1),
        payload: schedule_payload.clone(),
        payload_hash: canonical_schedule_payload_hash(schedule_payload_hash),
    }))
}

fn map_native_runtime_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<NativeRuntimeTask> {
    let payload: String = row.get(4)?;
    let payload = serde_json::from_str::<Value>(&payload)
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
    let progress = payload
        .get("progress")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(100) as u8;
    Ok(NativeRuntimeTask {
        id: row.get(0)?,
        state: row.get(1)?,
        title: row.get(2)?,
        trace_id: row.get(3)?,
        progress,
        payload,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn read_native_runtime_task(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
) -> Result<NativeRuntimeTask, String> {
    connection
        .query_row(
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, task_id],
            map_native_runtime_task,
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务：{error}"))?
        .ok_or_else(|| "未找到原生任务".to_string())
}

fn backfill_schedule_occurrence_task_snapshots(
    connection: &Connection,
    validate_all: bool,
) -> Result<(), String> {
    if !migration_source_table_exists(connection, "runtime_schedule_occurrences")?
        || !migration_source_table_exists(connection, "runtime_schedule_revisions")?
    {
        return Ok(());
    }
    let occurrence_filter = if validate_all {
        ""
    } else {
        "WHERE CASE
           WHEN json_valid(task.payload)=0 THEN 1
           ELSE (
             json_type(task.payload, '$.scheduleId') IS NOT 'text' OR
             json_extract(task.payload, '$.scheduleId') IS NOT occurrence.schedule_id OR
             json_type(task.payload, '$.scheduleKind') IS NOT 'text' OR
             json_extract(task.payload, '$.scheduleKind') IS NOT occurrence.schedule_kind OR
             json_type(task.payload, '$.scheduleOccurrenceId') IS NOT 'text' OR
             json_extract(task.payload, '$.scheduleOccurrenceId') IS NOT occurrence.occurrence_id OR
             json_type(task.payload, '$.scheduledFor') IS NOT 'text' OR
             json_extract(task.payload, '$.scheduledFor') IS NOT occurrence.scheduled_for OR
             json_type(task.payload, '$.scheduleRevision') IS NOT 'integer' OR
             json_extract(task.payload, '$.scheduleRevision') IS NOT occurrence.schedule_revision OR
             json_type(task.payload, '$.schedulePayload') IS NOT 'object' OR
             json_extract(task.payload, '$.schedulePayload')='{}' OR
             json_type(task.payload, '$.schedulePayloadHash') IS NOT 'text' OR
             length(json_extract(task.payload, '$.schedulePayloadHash')) != 71 OR
             substr(json_extract(task.payload, '$.schedulePayloadHash'), 1, 7) != 'sha256:' OR
             substr(json_extract(task.payload, '$.schedulePayloadHash'), 8) GLOB '*[^0-9a-f]*' OR
             EXISTS(
               SELECT 1 FROM runtime_schedule_revisions revision
               WHERE revision.workspace_scope=occurrence.workspace_scope
                 AND revision.schedule_id=occurrence.schedule_id
                 AND revision.schedule_kind=occurrence.schedule_kind
                 AND revision.revision=occurrence.schedule_revision
                 AND revision.payload_hash != json_extract(task.payload, '$.schedulePayloadHash')
             )
           )
         END"
    };
    let occurrences = {
        let query = format!(
            "SELECT occurrence.workspace_scope, occurrence.occurrence_id,
                    occurrence.schedule_id, occurrence.schedule_kind,
                    occurrence.scheduled_for, occurrence.schedule_revision,
                    occurrence.runtime_task_id, task.payload
             FROM runtime_schedule_occurrences occurrence
             JOIN runtime_tasks task
               ON task.workspace_scope=occurrence.workspace_scope
              AND task.id=occurrence.runtime_task_id
             {occurrence_filter}"
        );
        let mut statement = connection
            .prepare(&query)
            .map_err(|error| format!("无法准备日程 wrapper 快照回填：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|error| format!("无法读取日程 wrapper 快照回填记录：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析日程 wrapper 快照回填记录：{error}"))?
    };
    for (
        workspace_scope,
        occurrence_id,
        schedule_id,
        schedule_kind,
        scheduled_for,
        schedule_revision,
        runtime_task_id,
        task_payload_json,
    ) in occurrences
    {
        let mut task_payload = serde_json::from_str::<Value>(&task_payload_json)
            .map_err(|error| format!("日程 wrapper {runtime_task_id} payload 无法解析：{error}"))?;
        let existing_snapshot = task_payload.as_object().and_then(|object| {
            let payload = object.get("schedulePayload")?.clone();
            let payload_hash = object
                .get("schedulePayloadHash")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())?;
            Some((payload, payload_hash.to_string()))
        });
        let (schedule_payload, schedule_payload_hash) = if let Some(snapshot) =
            read_runtime_schedule_revision_snapshot(
                connection,
                &workspace_scope,
                &schedule_id,
                &schedule_kind,
                schedule_revision,
            )? {
            snapshot
        } else if let Some((payload, payload_hash)) = existing_snapshot {
            verified_schedule_payload_snapshot(payload, &payload_hash, "日程 wrapper 快照")?
        } else {
            return Err(format!(
                "日程 wrapper {runtime_task_id} 缺少不可变历史快照：{schedule_id}/{schedule_kind}/revision-{schedule_revision}"
            ));
        };
        let object = task_payload
            .as_object_mut()
            .ok_or_else(|| format!("日程 wrapper {runtime_task_id} payload 必须是 JSON 对象"))?;
        let mut changed = false;
        for (key, value) in [
            ("scheduleId", Value::String(schedule_id)),
            ("scheduleKind", Value::String(schedule_kind)),
            ("scheduleOccurrenceId", Value::String(occurrence_id)),
            ("scheduledFor", Value::String(scheduled_for)),
            ("scheduleRevision", Value::Number(schedule_revision.into())),
            ("schedulePayload", schedule_payload),
            ("schedulePayloadHash", Value::String(schedule_payload_hash)),
        ] {
            if object.get(key) != Some(&value) {
                object.insert(key.to_string(), value);
                changed = true;
            }
        }
        if changed {
            connection
                .execute(
                    "UPDATE runtime_tasks SET payload=?3 WHERE workspace_scope=?1 AND id=?2",
                    params![
                        workspace_scope,
                        runtime_task_id,
                        serde_json::to_string(&task_payload)
                            .map_err(|error| format!("无法序列化日程 wrapper 快照：{error}"))?,
                    ],
                )
                .map_err(|error| format!("无法回填日程 wrapper 快照：{error}"))?;
        }
    }
    Ok(())
}

fn add_sqlite_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("无法检查 {table}.{column}：{error}"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("无法读取 {table} 字段：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 {table} 字段：{error}"))?
        .iter()
        .any(|name| name == column);
    drop(statement);
    if !exists {
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
            ))
            .map_err(|error| format!("无法新增 {table}.{column}：{error}"))?;
    }
    Ok(())
}

fn run_migrations(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("无法读取 schema 版本：{error}"))?;
    if version < 1 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS workspace_state (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS vault_registry (
                   id TEXT PRIMARY KEY,
                   display_name TEXT NOT NULL,
                   canonical_path TEXT NOT NULL UNIQUE,
                   note_count INTEGER NOT NULL DEFAULT 0,
                   attachment_count INTEGER NOT NULL DEFAULT 0,
                   connection_state TEXT NOT NULL,
                   is_open INTEGER NOT NULL DEFAULT 0,
                   last_indexed_at TEXT NOT NULL,
                   last_error TEXT
                 );
                 CREATE TABLE IF NOT EXISTS tasks (
                   id TEXT PRIMARY KEY,
                   state TEXT NOT NULL,
                   trace_id TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS approvals (
                   id TEXT PRIMARY KEY,
                   task_id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS secretary_messages (
                   id TEXT PRIMARY KEY,
                   conversation_id TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS operation_events (
                   id TEXT PRIMARY KEY,
                   task_id TEXT,
                   event_type TEXT NOT NULL,
                   state TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 PRAGMA user_version=1;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 1 失败：{error}"))?;
    }
    if version < 2 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS note_index (
                   vault_id TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   title TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   modified_at TEXT NOT NULL,
                   byte_length INTEGER NOT NULL,
                   tags_json TEXT NOT NULL,
                   wiki_links_json TEXT NOT NULL,
                   PRIMARY KEY (vault_id, relative_path)
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5(
                   vault_id UNINDEXED,
                   relative_path UNINDEXED,
                   title,
                   content,
                   tokenize='unicode61'
                 );
                 PRAGMA user_version=2;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 2 失败：{error}"))?;
    }
    if version < 3 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS local_workspace_scopes (
                   id TEXT PRIMARY KEY,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 INSERT OR IGNORE INTO local_workspace_scopes (id, created_at, updated_at)
                   VALUES ('local', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS legacy_api_profiles (
                   workspace_scope TEXT PRIMARY KEY,
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   selected_model TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=3;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 3 失败：{error}"))?;
    }
    if version < 4 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS workspace_snapshots (
                   workspace_scope TEXT PRIMARY KEY,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=4;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 4 失败：{error}"))?;
    }
    if version < 5 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 PRAGMA user_version=5;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 5 失败：{error}"))?;
    }
    if version < 6 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE legacy_api_profiles
                   ADD COLUMN api_key_ciphertext BLOB NOT NULL DEFAULT X'';
                 PRAGMA user_version=6;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 6 失败：{error}"))?;
    }
    if version < 7 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS legacy_model_profiles (
                   workspace_scope TEXT NOT NULL,
                   role TEXT NOT NULL CHECK(role IN ('chat', 'analysis', 'image')),
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   selected_model TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   api_key_ciphertext BLOB NOT NULL DEFAULT X'',
                   PRIMARY KEY(workspace_scope, role),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO legacy_model_profiles
                   (workspace_scope, role, provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext)
                   SELECT workspace_scope, 'chat', provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext
                   FROM legacy_api_profiles;
                 INSERT OR IGNORE INTO legacy_model_profiles
                   (workspace_scope, role, provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext)
                   SELECT workspace_scope, 'analysis', provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext
                   FROM legacy_api_profiles;
                 PRAGMA user_version=7;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 7 失败：{error}"))?;
    }
    if version < 8 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS model_providers (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   name TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   assignments_json TEXT NOT NULL,
                   defaults_json TEXT NOT NULL,
                   api_key_ciphertext BLOB NOT NULL DEFAULT X'',
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=8;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 8 失败：{error}"))?;
    }
    if version < 9 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS vault_preferences (
                   workspace_scope TEXT PRIMARY KEY,
                   defaults_initialized INTEGER NOT NULL DEFAULT 0,
                   explicit_vault_id TEXT,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=9;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 9 失败：{error}"))?;
    }
    if version < 10 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS long_term_memory_events (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   event_type TEXT NOT NULL,
                   occurred_at TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'committed', 'failed')),
                   attempt_count INTEGER NOT NULL DEFAULT 0,
                   vault_relative_path TEXT,
                   content_hash TEXT,
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   committed_at TEXT,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_user_state
                   ON long_term_memory_events(workspace_scope, state, occurred_at);
                 PRAGMA user_version=10;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 10 失败：{error}"))?;
    }
    if version < 11 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS runtime_settings (
                   workspace_scope TEXT PRIMARY KEY,
                   scheduler_enabled INTEGER NOT NULL DEFAULT 1,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_tasks (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   title TEXT NOT NULL,
                   trace_id TEXT,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_tasks_state
                   ON runtime_tasks(workspace_scope, state, updated_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_steps (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   step_id TEXT NOT NULL,
                   position INTEGER NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, step_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_task_attempts (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   started_at TEXT NOT NULL,
                   finished_at TEXT,
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_schedules (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL CHECK(schedule_kind IN ('collection', 'report')),
                   enabled INTEGER NOT NULL,
                   next_run TEXT,
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   lease_owner TEXT,
                   lease_expires_at TEXT,
                   last_claimed_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id, schedule_kind),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_schedules_due
                   ON runtime_schedules(workspace_scope, enabled, next_run, lease_expires_at);
                 CREATE TABLE IF NOT EXISTS runtime_schedule_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   schedule_id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, schedule_id, schedule_kind, revision),
                   FOREIGN KEY(workspace_scope, schedule_id, schedule_kind)
                     REFERENCES runtime_schedules(workspace_scope, id, schedule_kind) ON DELETE CASCADE
                 );
                 PRAGMA user_version=11;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 11 失败：{error}"))?;
    }
    if version < 12 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS inbound_content_records (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   task_id TEXT,
                   state TEXT NOT NULL CHECK(state IN (
                     'extracted', 'analyzing', 'analysis_pending', 'quality_rejected',
                     'ready_to_write', 'writing', 'committed', 'failed', 'cancelled'
                   )),
                   source_type TEXT NOT NULL,
                   source_ref TEXT NOT NULL,
                   title TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   content_characters INTEGER NOT NULL,
                   attachment_count INTEGER NOT NULL,
                   image_count INTEGER NOT NULL,
                   extraction_json TEXT NOT NULL,
                   analysis_json TEXT NOT NULL,
                   quality_json TEXT NOT NULL,
                   target_json TEXT NOT NULL,
                   failure_reason TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   committed_at TEXT,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_inbound_content_records_state
                   ON inbound_content_records(workspace_scope, state, updated_at);
                 CREATE TABLE IF NOT EXISTS inbound_content_transitions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   content_id TEXT NOT NULL,
                   from_state TEXT,
                   to_state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, content_id)
                     REFERENCES inbound_content_records(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_inbound_content_transitions_record
                   ON inbound_content_transitions(workspace_scope, content_id, created_at);
                 PRAGMA user_version=12;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 12 失败：{error}"))?;
    }
    if version < 13 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE runtime_task_steps ADD COLUMN checkpoint_json TEXT NOT NULL DEFAULT '{}';
                 CREATE TABLE IF NOT EXISTS runtime_task_step_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   step_id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   position INTEGER NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   checkpoint_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, task_id, step_id, revision),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_step_revisions
                   ON runtime_task_step_revisions(workspace_scope, task_id, step_id, revision);
                 CREATE TABLE IF NOT EXISTS runtime_task_checkpoints (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   checkpoint_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'completed', 'failed')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   completed_at TEXT,
                   PRIMARY KEY(workspace_scope, task_id, checkpoint_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_checkpoints
                   ON runtime_task_checkpoints(workspace_scope, task_id, sequence, updated_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_recoveries (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   interrupted_task_updated_at TEXT NOT NULL,
                   recommendation TEXT NOT NULL CHECK(recommendation IN ('completed', 'resume', 'needs_input', 'manual')),
                   resume_step_id TEXT,
                   resume_step_index INTEGER,
                   resume_checkpoint_id TEXT,
                   evidence_json TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'resolved')),
                   resolution TEXT,
                   detected_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   resolved_at TEXT,
                   PRIMARY KEY(workspace_scope, task_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=13;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 13 失败：{error}"))?;
    }
    if version < 14 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 DROP INDEX IF EXISTS idx_long_term_memory_user_state;
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_workspace_state
                   ON long_term_memory_events(workspace_scope, state, occurred_at);
                 PRAGMA user_version=14;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 14 失败：{error}"))?;
    }
    if version < 15 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS application_commands (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   command_type TEXT NOT NULL,
                   operation TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('accepted', 'denied', 'completed', 'failed', 'cancelled')),
                   task_id TEXT,
                   trace_id TEXT,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, idempotency_key),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_application_commands_task
                   ON application_commands(workspace_scope, task_id, updated_at);
                 CREATE TABLE IF NOT EXISTS policy_decisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   outcome TEXT NOT NULL CHECK(outcome IN ('allow', 'deny', 'require_approval', 'allow_with_reduced_scope')),
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, command_id)
                     REFERENCES application_commands(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_policy_decisions_command
                   ON policy_decisions(workspace_scope, command_id, created_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_transitions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   from_state TEXT NOT NULL,
                   to_state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   checkpoint_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_transitions
                   ON runtime_task_transitions(workspace_scope, task_id, created_at);
                 PRAGMA user_version=15;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 15 失败：{error}"))?;
    }
    if version < 16 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS managed_resources (
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL CHECK(resource_type IN (
                     'user_skill', 'schedule', 'report_subscription', 'report',
                     'assistant_profile', 'optimization_profile', 'optimization_candidate'
                   )),
                   id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('active', 'deleted')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, resource_type, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_managed_resources_type_state
                   ON managed_resources(workspace_scope, resource_type, state, updated_at);
                 CREATE TABLE IF NOT EXISTS managed_resource_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL,
                   resource_id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('active', 'deleted')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, resource_type, resource_id, revision),
                   FOREIGN KEY(workspace_scope, resource_type, resource_id)
                     REFERENCES managed_resources(workspace_scope, resource_type, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_managed_resource_revisions
                   ON managed_resource_revisions(workspace_scope, resource_type, resource_id, revision);
                 PRAGMA user_version=16;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 16 失败：{error}"))?;
    }
    if version < 17 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS long_term_memory_governance (
                   workspace_scope TEXT NOT NULL,
                   memory_id TEXT NOT NULL,
                   status TEXT NOT NULL CHECK(status IN ('active', 'corrected', 'expired', 'tombstoned', 'compressed')),
                   replacement_id TEXT,
                   note TEXT NOT NULL DEFAULT '',
                   expires_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, memory_id),
                   FOREIGN KEY(memory_id) REFERENCES long_term_memory_events(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_governance_state
                   ON long_term_memory_governance(workspace_scope, status, expires_at, updated_at);
                 PRAGMA user_version=17;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 17 失败：{error}"))?;
    }
    if version < 18 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS external_connectors (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   name TEXT NOT NULL,
                   connector_type TEXT NOT NULL CHECK(connector_type IN ('feishu', 'wechat', 'email_webhook', 'webhook')),
                   endpoint_ciphertext BLOB NOT NULL,
                   secret_ciphertext BLOB NOT NULL,
                   enabled INTEGER NOT NULL DEFAULT 1,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS external_delivery_receipts (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   connector_id TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   trace_id TEXT,
                   status_code INTEGER NOT NULL,
                   response_hash TEXT NOT NULL,
                   delivered_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_external_delivery_task
                   ON external_delivery_receipts(workspace_scope, task_id, delivered_at);
                 PRAGMA user_version=18;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 18 失败：{error}"))?;
    }
    if version < 19 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS optimization_cursors (
                   workspace_scope TEXT PRIMARY KEY,
                   revision INTEGER NOT NULL DEFAULT 0,
                   last_occurred_at TEXT NOT NULL DEFAULT '',
                   last_event_id TEXT NOT NULL DEFAULT '',
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_candidates (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   base_version INTEGER NOT NULL,
                   candidate_version INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending_evaluation', 'pending_review', 'rejected', 'applied', 'superseded')),
                   summary TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   metrics_json TEXT NOT NULL,
                   evidence_count INTEGER NOT NULL,
                   evidence_occurred_at TEXT NOT NULL,
                   evidence_event_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   evaluated_at TEXT,
                   expires_at TEXT,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_optimization_candidates_state
                   ON optimization_candidates(workspace_scope, state, created_at);
                 CREATE TABLE IF NOT EXISTS optimization_evaluations (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   candidate_id TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending_review', 'rejected')),
                   checks_json TEXT NOT NULL,
                   evaluated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, candidate_id)
                     REFERENCES optimization_candidates(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_profiles (
                   workspace_scope TEXT PRIMARY KEY,
                   version INTEGER NOT NULL,
                   candidate_id TEXT,
                   guidance TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_profile_revisions (
                   workspace_scope TEXT NOT NULL,
                   version INTEGER NOT NULL,
                   candidate_id TEXT,
                   state TEXT NOT NULL CHECK(state IN ('initial', 'active', 'rollback')),
                   guidance TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   rollback_target INTEGER,
                   PRIMARY KEY(workspace_scope, version),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO optimization_cursors
                   (workspace_scope, revision, last_occurred_at, last_event_id, updated_at)
                   SELECT id, 0, '', '', CURRENT_TIMESTAMP FROM local_workspace_scopes;
                 INSERT OR IGNORE INTO optimization_profiles
                   (workspace_scope, version, candidate_id, guidance, rules_json, skill_hints_json, updated_at)
                   SELECT id, 0, NULL, '', '[]', '{}', CURRENT_TIMESTAMP FROM local_workspace_scopes;
                 INSERT OR IGNORE INTO optimization_profile_revisions
                   (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                   SELECT id, 0, NULL, 'initial', '', '[]', '{}', CURRENT_TIMESTAMP, NULL
                   FROM local_workspace_scopes;
                 PRAGMA user_version=19;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 19 失败：{error}"))?;
    }
    if version < 20 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS model_usage_events (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   request_id TEXT NOT NULL UNIQUE,
                   operation TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   model TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('started', 'succeeded', 'failed', 'cancelled')),
                   prompt_tokens INTEGER NOT NULL DEFAULT 0,
                   completion_tokens INTEGER NOT NULL DEFAULT 0,
                   total_tokens INTEGER NOT NULL DEFAULT 0,
                   estimated_cost_usd REAL,
                   cost_source TEXT NOT NULL,
                   duration_ms INTEGER NOT NULL DEFAULT 0,
                   error TEXT,
                   created_at TEXT NOT NULL,
                   completed_at TEXT,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_model_usage_events_time
                   ON model_usage_events(workspace_scope, created_at);
                 PRAGMA user_version=20;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 20 失败：{error}"))?;
    }
    if version < 21 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS application_authorization (
                   id INTEGER PRIMARY KEY CHECK(id=1),
                   status TEXT NOT NULL CHECK(status IN ('granted', 'denied')),
                   authorization_version INTEGER NOT NULL,
                   decided_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 PRAGMA user_version=21;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 21 失败：{error}"))?;
    }
    if version < 22 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS vault_index_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   vault_id TEXT NOT NULL,
                   canonical_root TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                   change_kind TEXT NOT NULL CHECK(change_kind IN ('upsert', 'delete')),
                   state TEXT NOT NULL CHECK(state IN ('pending', 'processing', 'failed')),
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   available_at_ms INTEGER NOT NULL,
                   claimed_at_ms INTEGER,
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(vault_id, relative_path)
                 );
                 CREATE INDEX IF NOT EXISTS idx_vault_index_changes_ready
                   ON vault_index_changes(state, available_at_ms, updated_at);
                 PRAGMA user_version=22;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 22 失败：{error}"))?;
    }
    if version < 23 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE VIRTUAL TABLE IF NOT EXISTS note_lexical_fts USING fts5(
                   vault_id UNINDEXED,
                   relative_path UNINDEXED,
                   title,
                   content,
                   tags,
                   wiki_links,
                   cjk_terms,
                   tokenize='unicode61'
                 );
                 PRAGMA user_version=23;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 23 失败：{error}"))?;
    }
    if version < 24 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS memory_records (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   track TEXT NOT NULL CHECK(track IN ('user_episode', 'user_profile', 'agent_case', 'agent_skill')),
                   title TEXT NOT NULL,
                   content TEXT NOT NULL,
                   user_id TEXT NOT NULL,
                   agent_id TEXT NOT NULL,
                   app_id TEXT NOT NULL,
                   project_id TEXT NOT NULL,
                   session_id TEXT NOT NULL,
                   source_doc_id TEXT NOT NULL,
                   source_relative_path TEXT,
                   source_content_hash TEXT,
                   evidence_json TEXT NOT NULL,
                   confidence REAL NOT NULL CHECK(confidence >= 0 AND confidence <= 1),
                   version INTEGER NOT NULL CHECK(version > 0),
                   supersedes_id TEXT,
                   state TEXT NOT NULL CHECK(state IN ('draft', 'active', 'superseded', 'tombstone')),
                   expires_at TEXT,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_memory_records_scope_state
                   ON memory_records(workspace_scope, user_id, agent_id, app_id, project_id, session_id, state, updated_at);
                 CREATE INDEX IF NOT EXISTS idx_memory_records_source
                   ON memory_records(workspace_scope, source_doc_id, source_content_hash);
                 CREATE TABLE IF NOT EXISTS memory_record_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   memory_id TEXT NOT NULL,
                   version INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('draft', 'active', 'superseded', 'tombstone')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, memory_id, version),
                   FOREIGN KEY(workspace_scope, memory_id)
                     REFERENCES memory_records(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                   workspace_scope UNINDEXED,
                   memory_id UNINDEXED,
                   title,
                   content,
                   evidence,
                   cjk_terms,
                   tokenize='unicode61'
                 );
                 CREATE TABLE IF NOT EXISTS memory_reflection_jobs (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   task_id TEXT,
                   scope_json TEXT NOT NULL,
                   source_doc_ids_json TEXT NOT NULL,
                   source_content_hash TEXT NOT NULL,
                   metrics_json TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'awaiting_review', 'completed', 'failed', 'cancelled')),
                   proposal_memory_id TEXT,
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, idempotency_key),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_memory_reflection_state
                   ON memory_reflection_jobs(workspace_scope, state, updated_at);
                 PRAGMA user_version=24;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 24 失败：{error}"))?;
    }
    if version < 25 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS note_feature_vectors (
                   vault_id TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   representation_version INTEGER NOT NULL,
                   dimensions INTEGER NOT NULL,
                   vector_blob BLOB NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY (vault_id, relative_path),
                   FOREIGN KEY (vault_id, relative_path)
                     REFERENCES note_index(vault_id, relative_path) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO vault_index_changes
                   (vault_id, canonical_root, relative_path, generation, change_kind, state,
                    attempt_count, available_at_ms, claimed_at_ms, last_error, created_at, updated_at)
                 SELECT i.vault_id, v.canonical_path, i.relative_path, 1, 'upsert', 'pending',
                        0, 0, NULL, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
                 FROM note_index i
                 JOIN vault_registry v ON v.id=i.vault_id
                 LEFT JOIN note_feature_vectors n
                   ON n.vault_id=i.vault_id AND n.relative_path=i.relative_path
                 WHERE n.vault_id IS NULL;
                 PRAGMA user_version=25;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 25 失败：{error}"))?;
    }
    if version < 26 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS assistant_conversations (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision >= 0),
                   context_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS assistant_requests (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   conversation_id TEXT NOT NULL,
                   conversation_revision INTEGER NOT NULL CHECK(conversation_revision >= 0),
                   sequence INTEGER NOT NULL CHECK(sequence > 0),
                   state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'needs_input')),
                   payload_json TEXT NOT NULL,
                   context_json TEXT,
                   context_hash TEXT,
                   result_json TEXT,
                   has_volatile_attachments INTEGER NOT NULL DEFAULT 0 CHECK(has_volatile_attachments IN (0, 1)),
                   recovery_count INTEGER NOT NULL DEFAULT 0 CHECK(recovery_count >= 0),
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   started_at TEXT,
                   completed_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, conversation_id, sequence),
                   FOREIGN KEY(workspace_scope, conversation_id)
                     REFERENCES assistant_conversations(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_assistant_requests_lane
                   ON assistant_requests(workspace_scope, conversation_id, state, sequence);
                 CREATE INDEX IF NOT EXISTS idx_assistant_requests_recovery
                   ON assistant_requests(workspace_scope, state, updated_at);
                 PRAGMA user_version=26;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 26 失败：{error}"))?;
    }
    if version < 27 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 27 无法开始事务：{error}"))?;
        crate::trace::migrate_schema(&transaction)?;
        crate::skill_lifecycle::migrate_schema(&transaction)?;
        add_sqlite_column_if_missing(&transaction, "model_usage_events", "trace_id", "TEXT")?;
        add_sqlite_column_if_missing(&transaction, "operation_events", "trace_id", "TEXT")?;
        add_sqlite_column_if_missing(&transaction, "vault_index_changes", "trace_id", "TEXT")?;
        transaction
            .execute_batch(
                "UPDATE runtime_tasks
                    SET trace_id='trace-legacy-task-' || lower(hex(randomblob(16)))
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE model_usage_events
                    SET trace_id='trace-legacy-model-' || lower(hex(randomblob(16)))
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE operation_events
                    SET trace_id=COALESCE(
                      NULLIF(json_extract(payload, '$.traceId'), ''),
                      'trace-legacy-operation-' || id
                    )
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE vault_index_changes
                    SET trace_id='trace-legacy-index-' || id
                  WHERE trace_id IS NULL OR trim(trace_id)='';",
            )
            .map_err(|error| format!("SQLite migration 27 无法扩展 Trace 字段：{error}"))?;
        crate::trace::migrate_legacy_events(&transaction)?;
        crate::skill_lifecycle::migrate_legacy_skills(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=27;")
            .map_err(|error| format!("SQLite migration 27 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 27 失败：{error}"))?;
    }
    if version < 28 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE vault_index_changes RENAME TO vault_index_changes_v27;
                 CREATE TABLE vault_index_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   vault_id TEXT NOT NULL,
                   canonical_root TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                   change_kind TEXT NOT NULL CHECK(change_kind IN ('upsert', 'delete')),
                   state TEXT NOT NULL CHECK(state IN ('pending', 'processing', 'dead_letter')),
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   available_at_ms INTEGER NOT NULL,
                   claimed_at_ms INTEGER,
                   last_error TEXT,
                   trace_id TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(vault_id, relative_path)
                 );
                 INSERT INTO vault_index_changes
                   (id, vault_id, canonical_root, relative_path, generation, change_kind, state,
                    attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id,
                    created_at, updated_at)
                 SELECT id, vault_id, canonical_root, relative_path, generation, change_kind,
                        CASE WHEN state='failed' THEN 'dead_letter' ELSE state END,
                        attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id,
                        created_at, updated_at
                   FROM vault_index_changes_v27;
                 DROP TABLE vault_index_changes_v27;
                 CREATE INDEX idx_vault_index_changes_ready
                   ON vault_index_changes(state, available_at_ms, updated_at);
                 PRAGMA user_version=28;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 28 失败：{error}"))?;
    }
    if version < 29 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS neural_embedding_cache (
                   workspace_scope TEXT NOT NULL,
                   provider_id TEXT NOT NULL,
                   model TEXT NOT NULL,
                   input_hash TEXT NOT NULL,
                   dimensions INTEGER NOT NULL CHECK(dimensions > 0 AND dimensions <= 65536),
                   vector_blob BLOB NOT NULL,
                   created_at TEXT NOT NULL,
                   last_used_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, provider_id, model, input_hash),
                   FOREIGN KEY(workspace_scope, provider_id)
                     REFERENCES model_providers(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS note_neural_embeddings (
                   workspace_scope TEXT NOT NULL,
                   provider_id TEXT NOT NULL,
                   model TEXT NOT NULL,
                   vault_id TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   input_hash TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, provider_id, model, vault_id, relative_path),
                   FOREIGN KEY(workspace_scope, provider_id)
                     REFERENCES model_providers(workspace_scope, id) ON DELETE CASCADE,
                   FOREIGN KEY(vault_id, relative_path)
                     REFERENCES note_index(vault_id, relative_path) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_note_neural_embedding_lookup
                   ON note_neural_embeddings(workspace_scope, provider_id, model, vault_id, content_hash);
                 CREATE TABLE IF NOT EXISTS neural_embedding_index_state (
                   workspace_scope TEXT NOT NULL,
                   provider_id TEXT NOT NULL,
                   model TEXT NOT NULL,
                   vault_id TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'building', 'ready', 'degraded', 'failed')),
                   total_notes INTEGER NOT NULL DEFAULT 0 CHECK(total_notes >= 0),
                   indexed_notes INTEGER NOT NULL DEFAULT 0 CHECK(indexed_notes >= 0),
                   last_error TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, provider_id, model, vault_id),
                   FOREIGN KEY(workspace_scope, provider_id)
                     REFERENCES model_providers(workspace_scope, id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=29;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 29 失败：{error}"))?;
    }
    if version < 30 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS creation_resources (
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL CHECK(resource_type IN ('theme', 'component', 'template')),
                   id TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision > 0),
                   state TEXT NOT NULL CHECK(state IN ('active', 'archived')),
                   schema_version TEXT NOT NULL CHECK(schema_version='1.0'),
                   version TEXT NOT NULL,
                   display_name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   manifest_json TEXT NOT NULL,
                   payload_json TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   source_ref_ids_json TEXT NOT NULL,
                   model_run_ids_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, resource_type, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_creation_resources_active
                   ON creation_resources(workspace_scope, state, resource_type, updated_at DESC);
                 CREATE TABLE IF NOT EXISTS creation_resource_revisions (
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL CHECK(resource_type IN ('theme', 'component', 'template')),
                   resource_id TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision > 0),
                   state TEXT NOT NULL CHECK(state IN ('active', 'archived')),
                   schema_version TEXT NOT NULL CHECK(schema_version='1.0'),
                   version TEXT NOT NULL,
                   display_name TEXT NOT NULL,
                   description TEXT NOT NULL,
                   manifest_json TEXT NOT NULL,
                   payload_json TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   source_ref_ids_json TEXT NOT NULL,
                   model_run_ids_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, resource_type, resource_id, revision),
                   FOREIGN KEY(workspace_scope, resource_type, resource_id)
                     REFERENCES creation_resources(workspace_scope, resource_type, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_creation_resource_revisions
                   ON creation_resource_revisions(workspace_scope, resource_type, resource_id, revision DESC);
                 PRAGMA user_version=30;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 30 失败：{error}"))?;
    }
    if version < 31 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 31 无法开始事务：{error}"))?;
        crate::durable_asset::migrate_schema(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=31;")
            .map_err(|error| format!("SQLite migration 31 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 31 失败：{error}"))?;
    }
    if version < 32 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 32 无法开始事务：{error}"))?;
        crate::creation::runtime::migrate(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=32;")
            .map_err(|error| format!("SQLite migration 32 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 32 失败：{error}"))?;
    }
    if version < 33 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 33 无法开始事务：{error}"))?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS workspace_messages (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   conversation_id TEXT NOT NULL,
                   ordinal INTEGER NOT NULL DEFAULT 0 CHECK(ordinal >= 0),
                   payload_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   sync_token TEXT NOT NULL DEFAULT '',
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope)
                     REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_workspace_messages_page
                   ON workspace_messages(workspace_scope, created_at, id);
                 CREATE INDEX IF NOT EXISTS idx_workspace_messages_conversation
                   ON workspace_messages(workspace_scope, conversation_id, created_at, id);
                 CREATE TABLE IF NOT EXISTS assistant_request_messages (
                   workspace_scope TEXT NOT NULL,
                   request_id TEXT NOT NULL,
                   ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                   payload_json TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, request_id, ordinal),
                   FOREIGN KEY(workspace_scope, request_id)
                     REFERENCES assistant_requests(workspace_scope, id) ON DELETE CASCADE
                 );",
            )
            .map_err(|error| format!("SQLite migration 33 无法创建消息表：{error}"))?;
        migrate_embedded_workspace_messages(&transaction)?;
        migrate_embedded_assistant_request_messages(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=33;")
            .map_err(|error| format!("SQLite migration 33 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 33 失败：{error}"))?;
    }
    if version < 34 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 34 无法开始事务：{error}"))?;
        add_sqlite_column_if_missing(
            &transaction,
            "runtime_task_recoveries",
            "plan_revision",
            "INTEGER",
        )?;
        add_sqlite_column_if_missing(
            &transaction,
            "runtime_task_recoveries",
            "completion_satisfied",
            "INTEGER",
        )?;
        add_sqlite_column_if_missing(
            &transaction,
            "runtime_task_recoveries",
            "missing_requirement_ids_json",
            "TEXT NOT NULL DEFAULT '[]'",
        )?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS runtime_task_plans (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision > 0),
                   schema_version TEXT NOT NULL CHECK(schema_version='1.0'),
                   goal TEXT NOT NULL,
                   plan_json TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, revision),
                   FOREIGN KEY(workspace_scope, task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_plans_latest
                   ON runtime_task_plans(workspace_scope, task_id, revision DESC);
                 CREATE TABLE IF NOT EXISTS runtime_task_plan_steps (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL,
                   step_id TEXT NOT NULL,
                   position INTEGER NOT NULL CHECK(position >= 0),
                   step_kind TEXT NOT NULL CHECK(step_kind IN (
                     'model', 'capability', 'approval', 'verification', 'checkpoint', 'schedule_dispatch'
                   )),
                   title TEXT NOT NULL,
                   depends_on_json TEXT NOT NULL,
                   parameters_json TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, plan_revision, step_id),
                   UNIQUE(workspace_scope, task_id, plan_revision, position),
                   FOREIGN KEY(workspace_scope, task_id, plan_revision)
                     REFERENCES runtime_task_plans(workspace_scope, task_id, revision) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_task_completion_requirements (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL,
                   requirement_id TEXT NOT NULL,
                   position INTEGER NOT NULL CHECK(position >= 0),
                   step_id TEXT,
                   evidence_type TEXT NOT NULL,
                   minimum_count INTEGER NOT NULL CHECK(minimum_count > 0),
                   description TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, plan_revision, requirement_id),
                   UNIQUE(workspace_scope, task_id, plan_revision, position),
                   FOREIGN KEY(workspace_scope, task_id, plan_revision)
                     REFERENCES runtime_task_plans(workspace_scope, task_id, revision) ON DELETE CASCADE,
                   FOREIGN KEY(workspace_scope, task_id, plan_revision, step_id)
                     REFERENCES runtime_task_plan_steps(workspace_scope, task_id, plan_revision, step_id)
                 );
                 CREATE TABLE IF NOT EXISTS runtime_task_evidence (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   evidence_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL,
                   requirement_id TEXT NOT NULL,
                   step_id TEXT,
                   evidence_type TEXT NOT NULL,
                   source_kind TEXT NOT NULL CHECK(source_kind IN (
                     'runtime', 'operation_event', 'inbound_content', 'vault_commit',
                     'model_receipt', 'user_approval', 'scheduler', 'verification'
                   )),
                   source_ref TEXT NOT NULL,
                   payload_json TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, evidence_id),
                   FOREIGN KEY(workspace_scope, task_id, plan_revision, requirement_id)
                     REFERENCES runtime_task_completion_requirements(
                       workspace_scope, task_id, plan_revision, requirement_id
                     ) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_evidence_requirement
                   ON runtime_task_evidence(
                     workspace_scope, task_id, plan_revision, requirement_id, created_at
                   );
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plans_immutable_update
                   BEFORE UPDATE ON runtime_task_plans
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan revisions are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plan_steps_immutable_update
                   BEFORE UPDATE ON runtime_task_plan_steps
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan steps are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_requirements_immutable_update
                   BEFORE UPDATE ON runtime_task_completion_requirements
                   BEGIN SELECT RAISE(ABORT, 'runtime task completion requirements are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_evidence_immutable_update
                   BEFORE UPDATE ON runtime_task_evidence
                   BEGIN SELECT RAISE(ABORT, 'runtime task evidence is immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plans_immutable_delete
                   BEFORE DELETE ON runtime_task_plans
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan revisions are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plan_steps_immutable_delete
                   BEFORE DELETE ON runtime_task_plan_steps
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan steps are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_requirements_immutable_delete
                   BEFORE DELETE ON runtime_task_completion_requirements
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task completion requirements are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_evidence_immutable_delete
                   BEFORE DELETE ON runtime_task_evidence
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task evidence is immutable'); END;
                 CREATE TABLE IF NOT EXISTS runtime_schedule_occurrences (
                   workspace_scope TEXT NOT NULL,
                   occurrence_id TEXT NOT NULL,
                   schedule_id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL CHECK(schedule_kind IN ('collection', 'report')),
                   scheduled_for TEXT NOT NULL,
                   schedule_revision INTEGER NOT NULL CHECK(schedule_revision > 0),
                   runtime_task_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, occurrence_id),
                   UNIQUE(workspace_scope, schedule_id, schedule_kind, scheduled_for),
                   UNIQUE(workspace_scope, runtime_task_id),
                   FOREIGN KEY(workspace_scope, runtime_task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_schedule_occurrences_schedule
                   ON runtime_schedule_occurrences(
                     workspace_scope, schedule_id, schedule_kind, scheduled_for DESC
                   );",
            )
            .map_err(|error| format!("SQLite migration 34 无法创建任务契约表：{error}"))?;
        transaction
            .execute_batch("PRAGMA user_version=34;")
            .map_err(|error| format!("SQLite migration 34 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 34 失败：{error}"))?;
    }
    if version < 35 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 35 无法开始事务：{error}"))?;
        transaction
            .execute_batch(
                "CREATE TRIGGER IF NOT EXISTS runtime_task_plan_steps_immutable_update
                   BEFORE UPDATE ON runtime_task_plan_steps
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan steps are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_requirements_immutable_update
                   BEFORE UPDATE ON runtime_task_completion_requirements
                   BEGIN SELECT RAISE(ABORT, 'runtime task completion requirements are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plans_immutable_delete
                   BEFORE DELETE ON runtime_task_plans
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan revisions are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_plan_steps_immutable_delete
                   BEFORE DELETE ON runtime_task_plan_steps
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task plan steps are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_requirements_immutable_delete
                   BEFORE DELETE ON runtime_task_completion_requirements
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task completion requirements are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_evidence_immutable_delete
                   BEFORE DELETE ON runtime_task_evidence
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_tasks task
                     WHERE task.workspace_scope=OLD.workspace_scope AND task.id=OLD.task_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task evidence is immutable'); END;
                 DROP INDEX IF EXISTS idx_runtime_schedule_occurrences_schedule;
                 ALTER TABLE runtime_schedule_occurrences
                   RENAME TO runtime_schedule_occurrences_v34;
                 CREATE TABLE runtime_schedule_occurrences (
                   workspace_scope TEXT NOT NULL,
                   occurrence_id TEXT NOT NULL,
                   schedule_id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL CHECK(schedule_kind IN ('collection', 'report')),
                   scheduled_for TEXT NOT NULL,
                   schedule_revision INTEGER NOT NULL CHECK(schedule_revision > 0),
                   runtime_task_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, occurrence_id),
                   UNIQUE(workspace_scope, schedule_id, schedule_kind, scheduled_for),
                   UNIQUE(workspace_scope, runtime_task_id),
                   FOREIGN KEY(workspace_scope, runtime_task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 INSERT INTO runtime_schedule_occurrences
                   (workspace_scope, occurrence_id, schedule_id, schedule_kind, scheduled_for,
                    schedule_revision, runtime_task_id, created_at)
                 SELECT workspace_scope, occurrence_id, schedule_id, schedule_kind, scheduled_for,
                        schedule_revision, runtime_task_id, created_at
                 FROM runtime_schedule_occurrences_v34;
                 DROP TABLE runtime_schedule_occurrences_v34;
                 CREATE INDEX idx_runtime_schedule_occurrences_schedule
                   ON runtime_schedule_occurrences(
                     workspace_scope, schedule_id, schedule_kind, scheduled_for DESC
                   );
                 PRAGMA user_version=35;",
            )
            .map_err(|error| format!("SQLite migration 35 无法强化任务契约与 occurrence：{error}"))?;
        backfill_schedule_occurrence_task_snapshots(&transaction, true)?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 35 失败：{error}"))?;
    } else {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始日程 wrapper 快照回填事务：{error}"))?;
        backfill_schedule_occurrence_task_snapshots(&transaction, false)?;
        transaction
            .commit()
            .map_err(|error| format!("日程 wrapper 快照回填失败：{error}"))?;
    }
    if version < 36 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 36 无法开始事务：{error}"))?;
        transaction
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS workspace_message_fts USING fts5(
                   workspace_scope UNINDEXED,
                   conversation_id UNINDEXED,
                   message_id UNINDEXED,
                   role UNINDEXED,
                   content,
                   cjk_terms,
                   tokenize='unicode61'
                 );",
            )
            .map_err(|error| format!("SQLite migration 36 无法创建消息全文索引：{error}"))?;
        backfill_workspace_message_fts(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=36;")
            .map_err(|error| format!("SQLite migration 36 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 36 失败：{error}"))?;
    }
    if version < 37 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 37 无法开始事务：{error}"))?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS runtime_task_execution_budgets (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL CHECK(plan_revision > 0),
                   max_steps INTEGER NOT NULL CHECK(max_steps > 0),
                   max_tool_calls INTEGER NOT NULL CHECK(max_tool_calls >= 0),
                   max_runtime_seconds INTEGER NOT NULL CHECK(max_runtime_seconds > 0),
                   max_tokens INTEGER CHECK(max_tokens IS NULL OR max_tokens >= 0),
                   max_cost REAL CHECK(max_cost IS NULL OR max_cost >= 0),
                   reserved_steps INTEGER NOT NULL DEFAULT 0 CHECK(reserved_steps >= 0),
                   reserved_tool_calls INTEGER NOT NULL DEFAULT 0 CHECK(reserved_tool_calls >= 0),
                   reserved_runtime_seconds INTEGER NOT NULL DEFAULT 0 CHECK(reserved_runtime_seconds >= 0),
                   reserved_tokens INTEGER NOT NULL DEFAULT 0 CHECK(reserved_tokens >= 0),
                   reserved_cost REAL NOT NULL DEFAULT 0 CHECK(reserved_cost >= 0),
                   consumed_steps INTEGER NOT NULL DEFAULT 0 CHECK(consumed_steps >= 0),
                   consumed_tool_calls INTEGER NOT NULL DEFAULT 0 CHECK(consumed_tool_calls >= 0),
                   consumed_runtime_seconds INTEGER NOT NULL DEFAULT 0 CHECK(consumed_runtime_seconds >= 0),
                   consumed_tokens INTEGER NOT NULL DEFAULT 0 CHECK(consumed_tokens >= 0),
                   consumed_cost REAL NOT NULL DEFAULT 0 CHECK(consumed_cost >= 0),
                   cancellation_fence INTEGER NOT NULL DEFAULT 0 CHECK(cancellation_fence >= 0),
                   cancelled_at TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, plan_revision),
                   FOREIGN KEY(workspace_scope, task_id, plan_revision)
                     REFERENCES runtime_task_plans(workspace_scope, task_id, revision) ON DELETE CASCADE,
                   CHECK(reserved_steps + consumed_steps <= max_steps),
                   CHECK(reserved_tool_calls + consumed_tool_calls <= max_tool_calls),
                   CHECK(reserved_runtime_seconds + consumed_runtime_seconds <= max_runtime_seconds),
                   CHECK(max_tokens IS NULL OR reserved_tokens + consumed_tokens <= max_tokens),
                   CHECK(max_cost IS NULL OR reserved_cost + consumed_cost <= max_cost)
                 );
                 CREATE TABLE IF NOT EXISTS runtime_task_step_runs (
                   workspace_scope TEXT NOT NULL,
                   claim_id TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL CHECK(plan_revision > 0),
                   step_id TEXT NOT NULL,
                   attempt INTEGER NOT NULL CHECK(attempt > 0),
                   effect_class TEXT NOT NULL CHECK(effect_class IN ('read_only', 'effectful')),
                   state TEXT NOT NULL CHECK(state IN ('claimed', 'succeeded', 'failed', 'cancelled', 'expired')),
                   lease_owner TEXT NOT NULL,
                   lease_expires_at TEXT NOT NULL,
                   reserved_tool_calls INTEGER NOT NULL CHECK(reserved_tool_calls >= 0),
                   reserved_runtime_seconds INTEGER NOT NULL CHECK(reserved_runtime_seconds >= 0),
                   reserved_tokens INTEGER NOT NULL CHECK(reserved_tokens >= 0),
                   reserved_cost REAL NOT NULL CHECK(reserved_cost >= 0),
                   cancellation_fence INTEGER NOT NULL CHECK(cancellation_fence >= 0),
                   claimed_at TEXT NOT NULL,
                   finished_at TEXT,
                   PRIMARY KEY(workspace_scope, claim_id),
                   UNIQUE(workspace_scope, task_id, plan_revision, step_id, attempt),
                   FOREIGN KEY(workspace_scope, task_id, plan_revision, step_id)
                     REFERENCES runtime_task_plan_steps(
                       workspace_scope, task_id, plan_revision, step_id
                     ) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_step_runs_frontier
                   ON runtime_task_step_runs(
                     workspace_scope, task_id, plan_revision, state, lease_expires_at
                   );
                 CREATE TRIGGER IF NOT EXISTS runtime_task_step_runs_identity_immutable
                   BEFORE UPDATE ON runtime_task_step_runs
                   WHEN OLD.workspace_scope != NEW.workspace_scope
                     OR OLD.claim_id != NEW.claim_id
                     OR OLD.task_id != NEW.task_id
                     OR OLD.plan_revision != NEW.plan_revision
                     OR OLD.step_id != NEW.step_id
                     OR OLD.attempt != NEW.attempt
                     OR OLD.effect_class != NEW.effect_class
                     OR OLD.cancellation_fence != NEW.cancellation_fence
                   BEGIN SELECT RAISE(ABORT, 'runtime task step run identity is immutable'); END;
                 CREATE TABLE IF NOT EXISTS runtime_task_step_receipts (
                   workspace_scope TEXT NOT NULL,
                   receipt_id TEXT NOT NULL,
                   claim_id TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   plan_revision INTEGER NOT NULL CHECK(plan_revision > 0),
                   step_id TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('succeeded', 'failed', 'cancelled', 'expired')),
                   output_json TEXT NOT NULL,
                   error TEXT,
                   consumed_tool_calls INTEGER NOT NULL CHECK(consumed_tool_calls >= 0),
                   consumed_runtime_seconds INTEGER NOT NULL CHECK(consumed_runtime_seconds >= 0),
                   consumed_tokens INTEGER NOT NULL CHECK(consumed_tokens >= 0),
                   consumed_cost REAL NOT NULL CHECK(consumed_cost >= 0),
                   content_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, receipt_id),
                   UNIQUE(workspace_scope, claim_id),
                   FOREIGN KEY(workspace_scope, claim_id)
                     REFERENCES runtime_task_step_runs(workspace_scope, claim_id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_step_receipts_task
                   ON runtime_task_step_receipts(
                     workspace_scope, task_id, plan_revision, step_id, created_at
                   );
                 CREATE TRIGGER IF NOT EXISTS runtime_task_step_receipts_immutable_update
                   BEFORE UPDATE ON runtime_task_step_receipts
                   BEGIN SELECT RAISE(ABORT, 'runtime task step receipts are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_step_receipts_immutable_delete
                   BEFORE DELETE ON runtime_task_step_receipts
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_task_step_runs run
                     WHERE run.workspace_scope=OLD.workspace_scope AND run.claim_id=OLD.claim_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task step receipts are immutable'); END;
                 CREATE TABLE IF NOT EXISTS runtime_task_step_command_bindings (
                   workspace_scope TEXT NOT NULL,
                   claim_id TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   child_task_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, claim_id),
                   UNIQUE(workspace_scope, command_id),
                   FOREIGN KEY(workspace_scope, claim_id)
                     REFERENCES runtime_task_step_runs(workspace_scope, claim_id) ON DELETE CASCADE,
                   FOREIGN KEY(workspace_scope, command_id)
                     REFERENCES application_commands(workspace_scope, id) ON DELETE CASCADE,
                   FOREIGN KEY(workspace_scope, child_task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TRIGGER IF NOT EXISTS runtime_task_step_command_bindings_immutable_update
                   BEFORE UPDATE ON runtime_task_step_command_bindings
                   BEGIN SELECT RAISE(ABORT, 'runtime task step command bindings are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_task_step_command_bindings_immutable_delete
                   BEFORE DELETE ON runtime_task_step_command_bindings
                   WHEN EXISTS(
                     SELECT 1 FROM runtime_task_step_runs run
                     WHERE run.workspace_scope=OLD.workspace_scope AND run.claim_id=OLD.claim_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime task step command bindings are immutable'); END;
                 INSERT OR IGNORE INTO runtime_task_execution_budgets
                   (workspace_scope, task_id, plan_revision, max_steps, max_tool_calls,
                    max_runtime_seconds, max_tokens, max_cost, created_at, updated_at)
                 SELECT plan.workspace_scope, plan.task_id, plan.revision,
                        MAX(
                          (SELECT COUNT(*) FROM runtime_task_plan_steps step
                           WHERE step.workspace_scope=plan.workspace_scope
                             AND step.task_id=plan.task_id
                             AND step.plan_revision=plan.revision),
                          CASE WHEN json_type(task.payload, '$.budget.maxSteps')='integer'
                               THEN json_extract(task.payload, '$.budget.maxSteps') ELSE 1 END
                        ),
                        CASE WHEN json_type(task.payload, '$.budget.maxToolCalls')='integer'
                             THEN json_extract(task.payload, '$.budget.maxToolCalls')
                             ELSE (SELECT COUNT(*) FROM runtime_task_plan_steps step
                                   WHERE step.workspace_scope=plan.workspace_scope
                                     AND step.task_id=plan.task_id
                                     AND step.plan_revision=plan.revision) END,
                        CASE WHEN json_type(task.payload, '$.budget.maxRuntimeSeconds')='integer'
                             THEN json_extract(task.payload, '$.budget.maxRuntimeSeconds') ELSE 3600 END,
                        CASE WHEN json_type(task.payload, '$.budget.maxTokens')='integer'
                             THEN json_extract(task.payload, '$.budget.maxTokens') END,
                        CASE WHEN json_type(task.payload, '$.budget.maxCost') IN ('integer', 'real')
                             THEN json_extract(task.payload, '$.budget.maxCost') END,
                        plan.created_at, plan.created_at
                 FROM runtime_task_plans plan
                 JOIN runtime_tasks task
                   ON task.workspace_scope=plan.workspace_scope AND task.id=plan.task_id
                 WHERE plan.revision=(
                   SELECT MAX(latest.revision) FROM runtime_task_plans latest
                   WHERE latest.workspace_scope=plan.workspace_scope AND latest.task_id=plan.task_id
                 );
                 PRAGMA user_version=37;",
            )
            .map_err(|error| format!("SQLite migration 37 无法创建任务步骤运行表：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 37 失败：{error}"))?;
    }
    if version < 38 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 38 无法开始事务：{error}"))?;
        crate::memory::migrate_reflection_schema(&transaction)?;
        crate::skill_lifecycle::migrate_effect_schema(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=38;")
            .map_err(|error| format!("SQLite migration 38 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 38 失败：{error}"))?;
    }
    if version < 39 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 39 无法开始事务：{error}"))?;
        crate::memory::migrate_reflection_optimization_schema(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=39;")
            .map_err(|error| format!("SQLite migration 39 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 39 失败：{error}"))?;
    }
    if version < 40 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 40 无法开始事务：{error}"))?;
        add_sqlite_column_if_missing(
            &transaction,
            "runtime_task_recoveries",
            "replacement_key",
            "TEXT",
        )?;
        add_sqlite_column_if_missing(
            &transaction,
            "runtime_task_recoveries",
            "replacement_task_id",
            "TEXT",
        )?;
        transaction
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_task_recoveries_replacement_key
                   ON runtime_task_recoveries(workspace_scope, replacement_key)
                   WHERE replacement_key IS NOT NULL;
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_task_recoveries_replacement_task
                   ON runtime_task_recoveries(workspace_scope, replacement_task_id)
                   WHERE replacement_task_id IS NOT NULL;
                 PRAGMA user_version=40;",
            )
            .map_err(|error| format!("SQLite migration 40 无法创建恢复替换索引：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 40 失败：{error}"))?;
    }
    if version < 41 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS runtime_effect_mutation_results (
                   workspace_scope TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   handler_kind TEXT NOT NULL,
                   request_hash TEXT NOT NULL,
                   result_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, command_id, handler_kind),
                   FOREIGN KEY(workspace_scope, command_id)
                     REFERENCES application_commands(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TRIGGER IF NOT EXISTS runtime_effect_mutation_results_immutable_update
                   BEFORE UPDATE ON runtime_effect_mutation_results
                   BEGIN SELECT RAISE(ABORT, 'runtime effect mutation results are immutable'); END;
                 CREATE TRIGGER IF NOT EXISTS runtime_effect_mutation_results_immutable_delete
                   BEFORE DELETE ON runtime_effect_mutation_results
                   WHEN EXISTS(
                     SELECT 1 FROM application_commands command
                     WHERE command.workspace_scope=OLD.workspace_scope
                       AND command.id=OLD.command_id
                   )
                   BEGIN SELECT RAISE(ABORT, 'runtime effect mutation results are immutable'); END;
                 PRAGMA user_version=41;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 41 失败：{error}"))?;
    }
    if version < 42 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 UPDATE memory_records
                    SET state='draft'
                  WHERE state='active'
                    AND (
                      (
                        track='user_episode'
                        AND length(id)=45
                        AND id LIKE 'user-episode-%'
                        AND substr(id, 14) NOT GLOB '*[^0-9a-f]*'
                      )
                      OR (
                        track='agent_case'
                        AND length(id)=43
                        AND id LIKE 'agent-case-%'
                        AND substr(id, 12) NOT GLOB '*[^0-9a-f]*'
                      )
                    );
                 PRAGMA user_version=42;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 42 无法收紧自动对话记忆状态：{error}"))?;
    }
    if version < 43 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS content_fingerprints (
                   workspace_scope TEXT NOT NULL,
                   content_id TEXT NOT NULL,
                   content_type TEXT NOT NULL,
                   exact_hash TEXT NOT NULL,
                   structure_hash TEXT NOT NULL,
                   simhash INTEGER NOT NULL,
                   source_fingerprint TEXT,
                   title TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY (workspace_scope, content_id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_fingerprints_exact
                   ON content_fingerprints(workspace_scope, exact_hash);
                 CREATE INDEX IF NOT EXISTS idx_fingerprints_structure
                   ON content_fingerprints(workspace_scope, structure_hash);
                 CREATE INDEX IF NOT EXISTS idx_fingerprints_simhash
                   ON content_fingerprints(workspace_scope, simhash);
                 PRAGMA user_version=43;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 43 失败：{error}"))?;
    }
    if version < 44 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE assistant_requests ADD COLUMN execution_plan_json TEXT;
                 ALTER TABLE assistant_requests ADD COLUMN current_step INTEGER;
                 PRAGMA user_version=44;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 44 失败：{error}"))?;
    }
    if version < 45 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS user_activity_events (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   event_type TEXT NOT NULL CHECK(event_type IN ('note_view', 'note_edit', 'search', 'capture', 'creation')),
                   vault_id TEXT,
                   note_path TEXT,
                   entity_id TEXT,
                   occurred_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_activity_events_time
                   ON user_activity_events(workspace_scope, event_type, occurred_at);
                 PRAGMA user_version=45;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 45 失败：{error}"))?;
    }

    // Migration 46: 间隔重复（Spaced Repetition）
    if version < 46 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS spaced_repetition_records (
                   workspace_scope TEXT NOT NULL,
                   vault_id TEXT NOT NULL,
                   note_path TEXT NOT NULL,
                   review_count INTEGER NOT NULL DEFAULT 0,
                   last_reviewed_at TEXT NOT NULL,
                   next_review_at TEXT NOT NULL,
                   interval_days INTEGER NOT NULL,
                   memory_strength REAL NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY (workspace_scope, vault_id, note_path),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_spaced_repetition_next_review
                   ON spaced_repetition_records(workspace_scope, vault_id, next_review_at);
                 PRAGMA user_version=46;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 46 失败：{error}"))?;
    }

    Ok(())
}

fn workspace_message_fields(
    message: &Value,
    ordinal: usize,
    fallback_created_at: &str,
) -> Result<(String, String, String, String), String> {
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "消息缺少字段 id".to_string())?
        .to_string();
    let conversation_id = message
        .get("conversationId")
        .or_else(|| message.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("local-conversation")
        .to_string();
    let created_at = message
        .get("createdAt")
        .or_else(|| message.get("created_at"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_created_at)
        .to_string();
    let payload_json = serde_json::to_string(message)
        .map_err(|error| format!("无法序列化消息 {ordinal}：{error}"))?;
    Ok((id, conversation_id, created_at, payload_json))
}

fn workspace_message_search_fields(message: &Value) -> (String, String) {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .nfc()
        .collect::<String>();
    (role, content)
}

fn workspace_message_fts_exists(connection: &Connection) -> Result<bool, String> {
    migration_source_table_exists(connection, "workspace_message_fts")
}

fn refresh_workspace_message_fts_row(
    connection: &Connection,
    workspace_scope: &str,
    message_id: &str,
    conversation_id: &str,
    message: &Value,
) -> Result<(), String> {
    let (role, content) = workspace_message_search_fields(message);
    let cjk_terms = cjk_lexical_terms(&content);
    connection
        .execute(
            "DELETE FROM workspace_message_fts
             WHERE workspace_scope=?1 AND message_id=?2",
            params![workspace_scope, message_id],
        )
        .map_err(|error| format!("无法刷新消息全文索引：{error}"))?;
    connection
        .execute(
            "INSERT INTO workspace_message_fts
             (workspace_scope, conversation_id, message_id, role, content, cjk_terms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace_scope,
                conversation_id,
                message_id,
                role,
                content,
                cjk_terms,
            ],
        )
        .map_err(|error| format!("无法写入消息全文索引：{error}"))?;
    Ok(())
}

fn backfill_workspace_message_fts(connection: &Connection) -> Result<(), String> {
    connection
        .execute("DELETE FROM workspace_message_fts", [])
        .map_err(|error| format!("无法清空消息全文索引回填：{error}"))?;
    let records = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, id, conversation_id, payload_json
                 FROM workspace_messages",
            )
            .map_err(|error| format!("无法准备消息全文索引回填：{error}"))?;
        let records = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| format!("无法读取消息全文索引回填：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析消息全文索引回填：{error}"))?;
        records
    };
    for (workspace_scope, message_id, conversation_id, payload_json) in records {
        let message = serde_json::from_str::<Value>(&payload_json)
            .map_err(|error| format!("独立消息记录已损坏，无法回填全文索引：{error}"))?;
        refresh_workspace_message_fts_row(
            connection,
            &workspace_scope,
            &message_id,
            &conversation_id,
            &message,
        )?;
    }
    Ok(())
}

fn upsert_workspace_message_rows(
    connection: &Connection,
    workspace_scope: &str,
    messages: &[Value],
    sync_token: Option<&str>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let fts_available = workspace_message_fts_exists(connection)?;
    for (ordinal, message) in messages.iter().enumerate() {
        let (id, conversation_id, created_at, payload_json) =
            workspace_message_fields(message, ordinal, &now)?;
        connection
            .execute(
                "INSERT INTO workspace_messages
                 (workspace_scope, id, conversation_id, ordinal, payload_json,
                  created_at, updated_at, sync_token)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   conversation_id=excluded.conversation_id,
                   ordinal=excluded.ordinal,
                   payload_json=excluded.payload_json,
                   created_at=CASE
                     WHEN json_extract(excluded.payload_json, '$.createdAt') IS NULL
                      AND json_extract(excluded.payload_json, '$.created_at') IS NULL
                     THEN workspace_messages.created_at
                     ELSE excluded.created_at
                   END,
                   updated_at=excluded.updated_at,
                   sync_token=CASE WHEN excluded.sync_token=''
                     THEN workspace_messages.sync_token ELSE excluded.sync_token END",
                params![
                    workspace_scope,
                    id,
                    conversation_id,
                    ordinal as i64,
                    payload_json,
                    created_at,
                    now,
                    sync_token.unwrap_or("")
                ],
            )
            .map_err(|error| format!("无法保存独立消息记录：{error}"))?;
        if fts_available {
            refresh_workspace_message_fts_row(
                connection,
                workspace_scope,
                &id,
                &conversation_id,
                message,
            )?;
        }
    }
    Ok(())
}

fn migration_source_table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
             )",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(|error| format!("无法检查旧运行时数据表 {table}：{error}"))
}

fn migrate_embedded_workspace_messages(connection: &Connection) -> Result<(), String> {
    if !migration_source_table_exists(connection, "workspace_snapshots")? {
        return Ok(());
    }
    let snapshots = {
        let mut statement = connection
            .prepare("SELECT workspace_scope, payload FROM workspace_snapshots")
            .map_err(|error| format!("无法准备旧工作区消息迁移：{error}"))?;
        let snapshots = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("无法读取旧工作区消息：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析旧工作区消息：{error}"))?;
        snapshots
    };
    for (workspace_scope, payload_json) in snapshots {
        let mut payload = serde_json::from_str::<Value>(&payload_json)
            .map_err(|error| format!("旧工作区快照损坏，无法迁移消息：{error}"))?;
        let messages = payload
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if messages.is_empty() {
            continue;
        }
        upsert_workspace_message_rows(connection, &workspace_scope, &messages, Some("migration"))?;
        if let Some(payload) = payload.as_object_mut() {
            payload.insert("messages".to_string(), Value::Array(Vec::new()));
        }
        let compact = serde_json::to_string(&payload)
            .map_err(|error| format!("无法压缩迁移后的工作区快照：{error}"))?;
        connection
            .execute(
                "UPDATE workspace_snapshots SET payload=?2 WHERE workspace_scope=?1",
                params![workspace_scope, compact],
            )
            .map_err(|error| format!("无法提交旧工作区消息迁移：{error}"))?;
    }
    Ok(())
}

fn migrate_embedded_assistant_request_messages(connection: &Connection) -> Result<(), String> {
    if !migration_source_table_exists(connection, "assistant_requests")? {
        return Ok(());
    }
    let requests = {
        let mut statement = connection
            .prepare("SELECT workspace_scope, id, payload_json FROM assistant_requests")
            .map_err(|error| format!("无法准备旧 AI助手请求消息迁移：{error}"))?;
        let requests = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("无法读取旧 AI助手请求消息：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析旧 AI助手请求消息：{error}"))?;
        requests
    };
    for (workspace_scope, request_id, payload_json) in requests {
        let mut payload = serde_json::from_str::<Value>(&payload_json)
            .map_err(|error| format!("旧 AI助手请求恢复信息损坏：{error}"))?;
        let messages = payload
            .as_object_mut()
            .and_then(|payload| {
                payload
                    .remove("conversationMessages")
                    .or_else(|| payload.remove("conversation_messages"))
            })
            .and_then(|messages| messages.as_array().cloned())
            .unwrap_or_default();
        if messages.is_empty() {
            continue;
        }
        for (ordinal, message) in messages.iter().enumerate() {
            connection
                .execute(
                    "INSERT OR REPLACE INTO assistant_request_messages
                     (workspace_scope, request_id, ordinal, payload_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        workspace_scope,
                        request_id,
                        ordinal as i64,
                        serde_json::to_string(message)
                            .map_err(|error| format!("无法序列化旧 AI助手请求消息：{error}"))?
                    ],
                )
                .map_err(|error| format!("无法迁移旧 AI助手请求消息：{error}"))?;
        }
        connection
            .execute(
                "UPDATE assistant_requests SET payload_json=?3
                 WHERE workspace_scope=?1 AND id=?2",
                params![
                    workspace_scope,
                    request_id,
                    serde_json::to_string(&payload)
                        .map_err(|error| format!("无法压缩旧 AI助手请求恢复信息：{error}"))?
                ],
            )
            .map_err(|error| format!("无法提交旧 AI助手请求消息迁移：{error}"))?;
    }
    Ok(())
}

fn table_count(connection: &Connection, table: &str) -> Result<i64, String> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| format!("无法统计 {table}：{error}"))
}

fn valid_runtime_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn contains_sensitive_memory_value(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("authorization:")
        || lower.contains("api_key=")
        || lower.contains("api-key=")
        || lower.contains("password=")
        || lower.contains("cookie:")
        || Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b")
            .expect("valid credential pattern")
            .is_match(value)
}

fn contains_optimization_forbidden_instruction(value: &str) -> bool {
    let normalized = value.to_lowercase().replace(char::is_whitespace, "");
    [
        "绕过审批",
        "关闭审批",
        "禁用审批",
        "修改系统提示",
        "覆盖系统指令",
        "扩大权限",
        "授予工具权限",
        "读取密钥",
        "导出密钥",
        "打开设置",
        "修改设置",
        "绕过访问控制",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
}

fn validate_optimization_candidate_input(input: &OptimizationCandidateInput) -> Result<(), String> {
    if !valid_runtime_identifier(&input.id, 160) {
        return Err("优化候选 ID 无效".to_string());
    }
    let summary = input.summary.trim();
    if summary.is_empty()
        || summary.chars().count() > 32_000
        || contains_sensitive_memory_value(summary)
    {
        return Err("优化候选摘要为空、过长或包含疑似凭据".to_string());
    }
    if input.rules.is_empty() || input.rules.len() > 12 {
        return Err("优化候选必须包含 1 到 12 条规则".to_string());
    }
    if input.rules.iter().any(|rule| {
        rule.trim().is_empty()
            || rule.chars().count() > 2000
            || contains_sensitive_memory_value(rule)
    }) {
        return Err("优化规则为空、过长或包含疑似凭据".to_string());
    }
    if !input.skill_hints.is_object() || !input.metrics.is_object() {
        return Err("优化候选的 Skill 提示和指标必须是 JSON 对象".to_string());
    }
    if serde_json::to_vec(&input.skill_hints)
        .map_err(|error| format!("无法校验 Skill 优化提示：{error}"))?
        .len()
        > 128 * 1024
        || serde_json::to_vec(&input.metrics)
            .map_err(|error| format!("无法校验优化指标：{error}"))?
            .len()
            > 64 * 1024
    {
        return Err("优化候选结构化数据超过安全上限".to_string());
    }
    if input.evidence_count < 2
        || chrono::DateTime::parse_from_rfc3339(&input.evidence_cursor_occurred_at).is_err()
        || !valid_runtime_identifier(&input.evidence_cursor_event_id, 160)
    {
        return Err("优化候选缺少足够的增量证据或有效证据游标".to_string());
    }
    if let Some(expires_at) = input.expires_at.as_deref() {
        let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| "优化候选过期时间必须是 RFC3339".to_string())?;
        if expires_at.with_timezone(&Utc) <= Utc::now() {
            return Err("优化候选过期时间必须晚于当前时间".to_string());
        }
    }
    Ok(())
}

fn value_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("运行记录缺少字段 {key}"))
}

fn validate_records(records: &[Value], label: &str) -> Result<(), String> {
    if records.len() > MAX_SNAPSHOT_RECORDS {
        return Err(format!("{label} 数量超过安全上限"));
    }
    for record in records {
        let serialized =
            serde_json::to_vec(record).map_err(|error| format!("无法序列化 {label}：{error}"))?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err(format!("单条 {label} 超过 2 MB 安全上限"));
        }
        value_string(record, "id")?;
    }
    Ok(())
}

fn validate_workspace_messages(records: &[Value]) -> Result<(), String> {
    for record in records {
        serde_json::to_vec(record).map_err(|error| format!("无法序列化 消息：{error}"))?;
        value_string(record, "id")?;
    }
    Ok(())
}

fn validate_workspace_snapshot_value(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if lower.starts_with("data:") && lower.contains(";base64,") {
                return Err(format!("工作区快照 `{path}` 不能保存 Base64 数据 URL"));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_workspace_snapshot_value(item, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                let lower_key = key.to_ascii_lowercase();
                if matches!(
                    lower_key.as_str(),
                    "contentbase64" | "database64" | "base64" | "dataurl"
                ) && !item.is_null()
                {
                    return Err(format!("工作区快照 `{path}.{key}` 不能保存 Base64 数据"));
                }
                if matches!(
                    lower_key.as_str(),
                    "canonicalmarkdown" | "creationdocument" | "candidatedocument" | "basedocument"
                ) && !item.is_null()
                {
                    return Err(format!(
                        "工作区快照 `{path}.{key}` 不能保存创作完整正文；请仅保存耐久资产描述符"
                    ));
                }
                validate_workspace_snapshot_value(item, &format!("{path}.{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_workspace_snapshot_client_state(client_state: &Value) -> Result<(), String> {
    if let Some(object) = client_state.as_object() {
        for key in ["documents", "creationDocuments"] {
            if object
                .get(key)
                .is_some_and(|value| !value.as_object().is_some_and(|object| object.is_empty()))
            {
                return Err(format!(
                    "工作区快照 clientState.{key} 必须为空；正文由耐久资产或 Vault 承载"
                ));
            }
        }
    }
    validate_workspace_snapshot_value(client_state, "clientState")
}

fn validate_creation_resource_type(value: &str) -> Result<String, String> {
    let value = value.trim();
    if matches!(value, "theme" | "component" | "template") {
        Ok(value.to_string())
    } else {
        Err("创作资源类型只允许 theme、component 或 template".to_string())
    }
}

fn validate_creation_resource_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    let valid = Regex::new(r"^[a-z][a-z0-9-]{0,79}$").expect("valid creation resource id");
    if valid.is_match(value) {
        Ok(value.to_string())
    } else {
        Err("创作资源 ID 必须是最多 80 位的小写字母、数字或连字符".to_string())
    }
}

fn validate_creation_resource_version(value: &str) -> Result<String, String> {
    let value = value.trim();
    let valid = Regex::new(r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$")
        .expect("valid creation resource version");
    if value.len() <= 40 && valid.is_match(value) {
        Ok(value.to_string())
    } else {
        Err("创作资源 version 必须是规范的三段语义版本".to_string())
    }
}

fn validate_creation_resource_text(
    value: &str,
    label: &str,
    required: bool,
    maximum: usize,
) -> Result<String, String> {
    let value = value.trim();
    if (required && value.is_empty())
        || value.chars().count() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(format!("{label}为空、过长或包含控制字符"));
    }
    Ok(value.to_string())
}

fn normalize_creation_resource_ids(values: &[String], label: &str) -> Result<Vec<String>, String> {
    if values.len() > 100 {
        return Err(format!("{label}超过 100 项安全上限"));
    }
    let mut normalized = values
        .iter()
        .map(|value| value.trim())
        .map(|value| {
            if valid_runtime_identifier(value, 160) {
                Ok(value.to_string())
            } else {
                Err(format!("{label}包含无效标识符"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn canonical_creation_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_creation_json).collect()),
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_creation_json(&map[key]));
            }
            Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

fn canonical_creation_json_string(value: &Value, label: &str) -> Result<String, String> {
    serde_json::to_string(&canonical_creation_json(value))
        .map_err(|error| format!("无法序列化创作资源 {label}：{error}"))
}

fn is_creation_resource_path_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "path" | "relativepath" | "filepath" | "assetpath" | "entrypoint"
    ) || key.ends_with("_path")
}

fn validate_creation_resource_path(value: &str) -> Result<(), String> {
    let value = value.trim();
    let safe_characters =
        Regex::new(r"^[A-Za-z0-9._/-]+$").expect("valid creation resource path pattern");
    let executable_extensions = [
        ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".wasm", ".sh", ".bash", ".cmd", ".bat",
        ".ps1", ".exe", ".dll", ".dylib", ".so", ".py", ".rb",
    ];
    let lower = value.to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 2048
        || value.starts_with(['/', '\\', '~'])
        || value.as_bytes().get(1) == Some(&b':')
        || value.contains('\\')
        || !safe_characters.is_match(value)
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || executable_extensions
            .iter()
            .any(|extension| lower.ends_with(extension))
    {
        return Err(format!("创作资源路径不安全：{value}"));
    }
    Ok(())
}

fn creation_resource_string_is_unsafe(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let executable_fragments = [
        "<script",
        "</script",
        "<iframe",
        "<object",
        "<embed",
        "<base",
        "<link",
        "javascript:",
        "vbscript:",
        "data:text",
        "data:application",
        "data:image",
        "@import",
        "expression(",
        "eval(",
        "new function",
        "document.cookie",
        "window.location",
        "child_process",
        "process.env",
        "#!/",
    ];
    let event_handler = Regex::new(
        r"(?i)\bon(?:abort|blur|change|click|error|focus|input|key(?:down|press|up)|load|message|mouse(?:down|enter|leave|move|out|over|up)|reset|resize|scroll|submit|touch(?:end|move|start)|unload)\s*=",
    )
    .expect("valid event handler pattern");
    let protocol_relative = Regex::new(r#"(?i)(?:^|[\s\"'(=])//[a-z0-9]"#)
        .expect("valid protocol-relative URL pattern");
    executable_fragments
        .iter()
        .any(|fragment| lower.contains(fragment))
        || lower.contains("://")
        || lower.contains("mailto:")
        || lower.contains("tel:")
        || lower.contains("www.")
        || lower.contains("../")
        || lower.contains("..\\")
        || matches!(
            lower.trim(),
            "javascript" | "script" | "executable" | "shell"
        )
        || event_handler.is_match(value)
        || protocol_relative.is_match(value)
        || contains_sensitive_memory_value(value)
}

fn creation_resource_key_is_executable(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    matches!(
        key.as_str(),
        "script"
            | "scripts"
            | "javascript"
            | "executable"
            | "command"
            | "commandline"
            | "eval"
            | "functionbody"
            | "sourcecode"
    )
}

fn creation_resource_permission_must_be_false(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    matches!(
        key.as_str(),
        "network"
            | "shell"
            | "vaultwrite"
            | "allowtopnavigation"
            | "allowpopups"
            | "allowexternalscripts"
            | "allowscripts"
            | "allowexternalstyles"
    )
}

fn validate_declarative_creation_value(
    value: &Value,
    path: &str,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_CREATION_RESOURCE_JSON_DEPTH {
        return Err(format!("创作资源 {path} 超过 JSON 深度安全上限"));
    }
    *nodes += 1;
    if *nodes > MAX_CREATION_RESOURCE_JSON_NODES {
        return Err("创作资源 JSON 节点数量超过安全上限".to_string());
    }
    match value {
        Value::String(text) => {
            if text
                .chars()
                .any(|character| character == '\0' || character == '\u{7f}')
                || creation_resource_string_is_unsafe(text)
            {
                return Err(format!(
                    "创作资源 {path} 包含脚本、外链、路径穿越或疑似凭据"
                ));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_declarative_creation_value(
                    item,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    nodes,
                )?;
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if key.is_empty()
                    || key.len() > 128
                    || key.chars().any(char::is_control)
                    || creation_resource_key_is_executable(key)
                {
                    return Err(format!("创作资源 {path} 包含不可执行的危险字段 `{key}`"));
                }
                if creation_resource_permission_must_be_false(key)
                    && !matches!(item, Value::Bool(false))
                {
                    return Err(format!("创作资源权限字段 `{key}` 必须显式为 false"));
                }
                if is_creation_resource_path_key(key) {
                    let path_value = item
                        .as_str()
                        .ok_or_else(|| format!("创作资源路径字段 `{key}` 必须是字符串"))?;
                    validate_creation_resource_path(path_value)?;
                }
                validate_declarative_creation_value(
                    item,
                    &format!("{path}.{key}"),
                    depth + 1,
                    nodes,
                )?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn creation_manifest_string<'a>(manifest: &'a Value, key: &str) -> Result<&'a str, String> {
    manifest
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("创作资源 manifest 缺少字符串字段 {key}"))
}

fn validate_template_creation_resource(manifest: &Value, payload: &Value) -> Result<(), String> {
    let entrypoint = manifest
        .get("entrypoint")
        .or_else(|| payload.get("entrypoint"))
        .and_then(Value::as_str)
        .ok_or_else(|| "模板资源必须声明 entrypoint".to_string())?;
    validate_creation_resource_path(entrypoint)?;
    let files = payload
        .get("files")
        .or_else(|| manifest.get("files"))
        .and_then(Value::as_array)
        .ok_or_else(|| "模板资源必须声明 files 数组".to_string())?;
    if files.is_empty() || files.len() > 200 {
        return Err("模板资源 files 必须包含 1 到 200 项".to_string());
    }
    let allowed_kinds = [
        "html", "css", "json", "image", "font", "data", "markdown", "text",
    ];
    for (index, file) in files.iter().enumerate() {
        let file = file
            .as_object()
            .ok_or_else(|| format!("模板资源 files[{index}] 必须是对象"))?;
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("模板资源 files[{index}] 缺少 path"))?;
        validate_creation_resource_path(path)?;
        let kind = file
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("模板资源 files[{index}] 缺少 kind"))?;
        if !allowed_kinds.contains(&kind) {
            return Err(format!(
                "模板资源 files[{index}] 使用了不安全的文件类型 `{kind}`"
            ));
        }
    }
    if !files.iter().any(|file| {
        file.get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path == entrypoint)
    }) {
        return Err("模板资源 entrypoint 必须存在于 files 中".to_string());
    }
    Ok(())
}

fn validate_creation_resource_input(
    input: &CreationResourceInput,
) -> Result<ValidatedCreationResource, String> {
    if input.schema_version.trim() != "1.0" {
        return Err("创作资源 schemaVersion 必须是 1.0".to_string());
    }
    let resource_type = validate_creation_resource_type(&input.resource_type)?;
    let id = validate_creation_resource_id(&input.id)?;
    let version = validate_creation_resource_version(&input.version)?;
    let display_name =
        validate_creation_resource_text(&input.display_name, "创作资源名称", true, 80)?;
    let description =
        validate_creation_resource_text(&input.description, "创作资源描述", false, 1000)?;
    if !input.manifest.is_object() || !input.payload.is_object() {
        return Err("创作资源 manifest 和 payload 必须是 JSON 对象".to_string());
    }
    if creation_manifest_string(&input.manifest, "schemaVersion")? != "1.0"
        || creation_manifest_string(&input.manifest, "manifestType")? != resource_type
        || creation_manifest_string(&input.manifest, "id")? != id
        || creation_manifest_string(&input.manifest, "version")? != version
    {
        return Err("创作资源 manifest 的版本、类型或 ID 与资源封套不一致".to_string());
    }
    let mut nodes = 0;
    validate_declarative_creation_value(&input.manifest, "manifest", 0, &mut nodes)?;
    validate_declarative_creation_value(&input.payload, "payload", 0, &mut nodes)?;
    if resource_type == "template" {
        validate_template_creation_resource(&input.manifest, &input.payload)?;
    }
    let manifest = canonical_creation_json(&input.manifest);
    let payload = canonical_creation_json(&input.payload);
    let manifest_json = canonical_creation_json_string(&manifest, "manifest")?;
    let payload_json = canonical_creation_json_string(&payload, "payload")?;
    if manifest_json.len() > MAX_CREATION_MANIFEST_BYTES {
        return Err("创作资源 manifest 超过 256 KiB 安全上限".to_string());
    }
    let source_ref_ids =
        normalize_creation_resource_ids(&input.source_ref_ids, "创作资源来源引用")?;
    let model_run_ids = normalize_creation_resource_ids(&input.model_run_ids, "模型运行引用")?;
    let source_ref_ids_json = serde_json::to_string(&source_ref_ids)
        .map_err(|error| format!("无法序列化创作资源来源引用：{error}"))?;
    let model_run_ids_json = serde_json::to_string(&model_run_ids)
        .map_err(|error| format!("无法序列化模型运行引用：{error}"))?;
    let hash_envelope = serde_json::json!({
        "schemaVersion": "1.0",
        "resourceType": resource_type.clone(),
        "id": id.clone(),
        "version": version.clone(),
        "displayName": display_name.clone(),
        "description": description.clone(),
        "manifest": manifest.clone(),
        "payload": payload.clone(),
        "sourceRefIds": source_ref_ids.clone(),
        "modelRunIds": model_run_ids.clone(),
    });
    let canonical_envelope = canonical_creation_json_string(&hash_envelope, "封套")?;
    let content_hash = format!("sha256:{:x}", Sha256::digest(canonical_envelope.as_bytes()));
    if let Some(claimed) = input.content_hash.as_deref() {
        if claimed.trim().to_ascii_lowercase() != content_hash {
            return Err("客户端声明的创作资源 contentHash 与后端计算结果不一致".to_string());
        }
    }
    Ok(ValidatedCreationResource {
        schema_version: "1.0".to_string(),
        resource_type,
        id,
        version,
        display_name,
        description,
        manifest,
        payload,
        manifest_json,
        payload_json,
        content_hash,
        source_ref_ids,
        model_run_ids,
        source_ref_ids_json,
        model_run_ids_json,
    })
}

fn creation_resource_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCreationResourceRow> {
    Ok(StoredCreationResourceRow {
        resource_type: row.get(0)?,
        id: row.get(1)?,
        revision: row.get(2)?,
        state: row.get(3)?,
        schema_version: row.get(4)?,
        version: row.get(5)?,
        display_name: row.get(6)?,
        description: row.get(7)?,
        manifest_json: row.get(8)?,
        payload_json: row.get(9)?,
        content_hash: row.get(10)?,
        source_ref_ids_json: row.get(11)?,
        model_run_ids_json: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn managed_resource_id(payload: &Value, label: &str) -> Result<String, String> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{label}缺少 id"))?;
    if id.chars().count() > 180 || id.chars().any(char::is_control) {
        return Err(format!("{label} id 无效"));
    }
    Ok(id.to_string())
}

fn validate_report_resource(payload: &Value) -> Result<(), String> {
    if payload.get("markdown").is_some() {
        return Err("报告正文必须保存在耐久资产中，不能写入 SQLite 资源记录".to_string());
    }
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| "报告资源缺少 state".to_string())?;
    if !matches!(
        state,
        "preview" | "awaiting_approval" | "writing" | "persisted" | "failed" | "cancelled"
    ) {
        return Err("报告资源 state 无效".to_string());
    }
    let body_asset = payload
        .get("bodyAsset")
        .and_then(Value::as_object)
        .ok_or_else(|| "报告资源缺少 bodyAsset 耐久正文描述符".to_string())?;
    let asset_id = body_asset
        .get("assetId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "报告耐久正文缺少 assetId".to_string())?;
    if asset_id.chars().count() > 180 || asset_id.chars().any(char::is_control) {
        return Err("报告耐久正文 assetId 无效".to_string());
    }
    if body_asset.get("state").and_then(Value::as_str) != Some("ready") {
        return Err("报告耐久正文尚未就绪".to_string());
    }
    if body_asset
        .get("byteLength")
        .and_then(Value::as_u64)
        .is_none()
    {
        return Err("报告耐久正文缺少 byteLength".to_string());
    }
    Ok(())
}

fn upsert_managed_resource(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    id: &str,
    payload: &Value,
) -> Result<(), String> {
    if !payload.is_object() {
        return Err(format!("{resource_type}/{id} 的资源负载必须是 JSON 对象"));
    }
    let serialized =
        serde_json::to_string(payload).map_err(|error| format!("无法序列化独立资源：{error}"))?;
    if serialized.len() > MAX_RECORD_BYTES {
        return Err(format!("{resource_type}/{id} 超过 2 MB 安全上限"));
    }
    let payload_hash = format!("{:x}", Sha256::digest(serialized.as_bytes()));
    let existing = transaction
        .query_row(
            "SELECT revision, state, payload_hash, created_at FROM managed_resources
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取独立资源当前修订：{error}"))?;
    if existing
        .as_ref()
        .is_some_and(|(_, state, hash, _)| state == "active" && hash == &payload_hash)
    {
        return Ok(());
    }
    let revision = existing
        .as_ref()
        .map_or(1, |(revision, _, _, _)| revision + 1);
    let now = Utc::now().to_rfc3339();
    let created_at = existing
        .as_ref()
        .map(|(_, _, _, created_at)| created_at.as_str())
        .unwrap_or(now.as_str());
    transaction
        .execute(
            "INSERT INTO managed_resources
             (workspace_scope, resource_type, id, revision, state, payload, payload_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?7, ?8)
             ON CONFLICT(workspace_scope, resource_type, id) DO UPDATE SET
               revision=excluded.revision, state='active', payload=excluded.payload,
               payload_hash=excluded.payload_hash, updated_at=excluded.updated_at",
            params![
                workspace_scope,
                resource_type,
                id,
                revision,
                serialized,
                payload_hash,
                created_at,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源：{error}"))?;
    transaction
        .execute(
            "INSERT INTO managed_resource_revisions
             (id, workspace_scope, resource_type, resource_id, revision, state, payload, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                resource_type,
                id,
                revision,
                serialized,
                payload_hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源修订：{error}"))?;
    Ok(())
}

fn tombstone_managed_resource(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    id: &str,
) -> Result<(), String> {
    let existing = transaction
        .query_row(
            "SELECT revision, state, payload, payload_hash FROM managed_resources
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取待删除独立资源：{error}"))?;
    let Some((revision, state, payload, payload_hash)) = existing else {
        return Ok(());
    };
    if state == "deleted" {
        return Ok(());
    }
    let revision = revision + 1;
    let now = Utc::now().to_rfc3339();
    transaction
        .execute(
            "UPDATE managed_resources SET revision=?4, state='deleted', updated_at=?5
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id, revision, now],
        )
        .map_err(|error| format!("无法标记独立资源已删除：{error}"))?;
    transaction
        .execute(
            "INSERT INTO managed_resource_revisions
             (id, workspace_scope, resource_type, resource_id, revision, state, payload, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'deleted', ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                resource_type,
                id,
                revision,
                payload,
                payload_hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源删除修订：{error}"))?;
    Ok(())
}

fn sync_managed_resource_group(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    resources: &[Value],
) -> Result<(), String> {
    let mut incoming_ids = HashSet::new();
    for payload in resources {
        let id = managed_resource_id(payload, resource_type)?;
        if !incoming_ids.insert(id.clone()) {
            return Err(format!("{resource_type} 包含重复 id：{id}"));
        }
        upsert_managed_resource(transaction, workspace_scope, resource_type, &id, payload)?;
    }
    let existing_ids = {
        let mut statement = transaction
            .prepare(
                "SELECT id FROM managed_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'",
            )
            .map_err(|error| format!("无法准备独立资源清理查询：{error}"))?;
        let ids = statement
            .query_map(params![workspace_scope, resource_type], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("无法读取独立资源清理列表：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        ids
    };
    for id in existing_ids {
        if !incoming_ids.contains(&id) {
            tombstone_managed_resource(transaction, workspace_scope, resource_type, &id)?;
        }
    }
    Ok(())
}

fn validate_inbound_identifier(value: &str, label: &str, max_chars: usize) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(format!("{label}为空或超过 {max_chars} 个字符"));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(format!("{label}包含控制字符"));
    }
    Ok(())
}

fn serialize_inbound_record_section(value: &Value, label: &str) -> Result<String, String> {
    if !value.is_object() {
        return Err(format!("内容处理记录的{label}必须是 JSON 对象"));
    }
    let serialized = serde_json::to_string(value)
        .map_err(|error| format!("无法序列化内容处理记录的{label}：{error}"))?;
    if serialized.len() > MAX_INBOUND_RECORD_BYTES / 2 {
        return Err(format!("内容处理记录的{label}超过 256 KB 安全上限"));
    }
    Ok(serialized)
}

fn validate_inbound_content_record(record: &InboundContentRecordInput) -> Result<(), String> {
    validate_inbound_identifier(&record.id, "内容记录 ID", 180)?;
    validate_inbound_identifier(&record.source_type, "来源类型", 32)?;
    validate_inbound_identifier(&record.source_ref, "来源引用", 4096)?;
    validate_inbound_identifier(&record.title, "内容标题", 240)?;
    if !record.content_hash.starts_with("sha256:")
        || record.content_hash.len() != 71
        || !record.content_hash[7..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err("正文哈希必须是小写 sha256:<64位十六进制>".to_string());
    }
    if record.content_characters > i64::MAX as usize
        || record.attachment_count > i64::MAX as usize
        || record.image_count > i64::MAX as usize
    {
        return Err("内容处理记录的计数字段超出 SQLite 整数表示范围".to_string());
    }
    if !matches!(
        record.state.as_str(),
        "extracted"
            | "analyzing"
            | "analysis_pending"
            | "quality_rejected"
            | "ready_to_write"
            | "writing"
            | "committed"
            | "failed"
            | "cancelled"
    ) {
        return Err("内容处理记录状态无效".to_string());
    }
    if let Some(task_id) = record.task_id.as_deref() {
        validate_inbound_identifier(task_id, "内容处理任务 ID", 180)?;
    }
    if let Some(reason) = record.failure_reason.as_deref() {
        if reason.chars().count() > 4000 {
            return Err("内容处理失败原因超过 4000 个字符".to_string());
        }
    }
    let serialized =
        serde_json::to_vec(record).map_err(|error| format!("无法序列化内容处理记录：{error}"))?;
    if serialized.len() > MAX_INBOUND_RECORD_BYTES {
        return Err("单条内容处理记录超过 512 KB 安全上限".to_string());
    }
    Ok(())
}

fn inbound_content_transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match from {
        "extracted" => matches!(
            to,
            "analyzing" | "analysis_pending" | "quality_rejected" | "failed" | "cancelled"
        ),
        "analyzing" | "analysis_pending" => {
            matches!(
                to,
                "ready_to_write" | "quality_rejected" | "failed" | "cancelled"
            )
        }
        "quality_rejected" => matches!(to, "failed" | "cancelled"),
        "ready_to_write" => matches!(to, "writing" | "failed" | "cancelled"),
        "writing" => matches!(to, "committed" | "failed"),
        "committed" | "failed" | "cancelled" => false,
        _ => false,
    }
}

fn runtime_value_string(value: &Value, key: &str, label: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| item.len() <= 180)
        .map(str::to_string)
        .ok_or_else(|| format!("{label}缺少有效字段 {key}"))
}

fn normalize_runtime_time(value: Option<&str>) -> Option<String> {
    value
        .and_then(|item| chrono::DateTime::parse_from_rfc3339(item).ok())
        .map(|time| time.with_timezone(&Utc).to_rfc3339())
}

fn sync_runtime_tasks(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    tasks: &[Value],
) -> Result<(), String> {
    for task in tasks {
        let id = runtime_value_string(task, "id", "原生任务")?;
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("created");
        if !valid_runtime_task_state(state) {
            return Err("同步的原生任务状态无效".to_string());
        }
        let title = task
            .get("title")
            .or_else(|| task.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("未命名任务")
            .chars()
            .take(240)
            .collect::<String>();
        let incoming_trace_id = task
            .get("traceId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let previous_task = transaction
            .query_row(
                "SELECT state, trace_id FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务状态：{error}"))?;
        if previous_task.is_some()
            && transaction
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM runtime_task_plans
                       WHERE workspace_scope=?1 AND task_id=?2
                     )",
                    params![workspace_scope, id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法检查同步任务完成契约：{error}"))?
                != 0
        {
            return Err("客户端快照不能覆盖由原生完成契约管理的任务".to_string());
        }
        let previous_trace_id = previous_task
            .as_ref()
            .and_then(|(_, trace_id)| trace_id.as_deref())
            .filter(|value| !value.trim().is_empty());
        if incoming_trace_id
            .as_deref()
            .zip(previous_trace_id)
            .is_some_and(|(incoming, previous)| incoming != previous)
        {
            return Err("同一原生任务不能重新绑定其他 Trace".to_string());
        }
        let trace_id = incoming_trace_id
            .or_else(|| previous_trace_id.map(str::to_string))
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        let mut task_payload = task.clone();
        task_payload
            .as_object_mut()
            .ok_or_else(|| "原生任务必须是 JSON 对象".to_string())?
            .insert("traceId".to_string(), Value::String(trace_id.clone()));
        let payload = serde_json::to_string(&task_payload)
            .map_err(|error| format!("无法序列化原生任务：{error}"))?;
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO runtime_tasks (workspace_scope, id, state, title, trace_id, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   state=excluded.state, title=excluded.title, trace_id=excluded.trace_id,
                   payload=excluded.payload, updated_at=excluded.updated_at",
                params![workspace_scope, id, state, title, trace_id, payload, now],
            )
            .map_err(|error| format!("无法保存原生任务：{error}"))?;
        if previous_task.as_ref().map(|(state, _)| state.as_str()) != Some(state) {
            transaction
                .execute(
                    "UPDATE runtime_task_attempts SET finished_at=?3
                     WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                    params![workspace_scope, id, now],
                )
                .map_err(|error| format!("无法结束原生任务上一次尝试：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_task_attempts
                     (id, workspace_scope, task_id, state, detail, started_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        id,
                        state,
                        task.get("result")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(1000)
                            .collect::<String>(),
                        now,
                    ],
                )
                .map_err(|error| format!("无法记录原生任务状态变更：{error}"))?;
            crate::trace::record_trace_event_in_connection(
                transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id: &trace_id,
                    entity_kind: "runtime_task",
                    entity_id: &id,
                    event_type: "task.synced",
                    state,
                    payload: &serde_json::json!({
                        "previousState": previous_task.as_ref().map(|(state, _)| state),
                        "source": "runtime-state-sync",
                    }),
                    created_at: &now,
                },
            )?;
        }
        let mut current_step_ids = HashSet::new();
        if let Some(steps) = task.get("steps").and_then(Value::as_array) {
            for (position, step) in steps.iter().enumerate() {
                let step_id = step
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|item| !item.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{id}:{position}"));
                current_step_ids.insert(step_id.clone());
                let step_state = step
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("created");
                let detail = step.get("detail").and_then(Value::as_str).unwrap_or("");
                let checkpoint_json = serde_json::to_string(
                    step.get("checkpoint")
                        .filter(|checkpoint| checkpoint.is_object())
                        .unwrap_or(&Value::Null),
                )
                .map_err(|error| format!("无法序列化任务步骤检查点：{error}"))?;
                let previous = transaction
                    .query_row(
                        "SELECT state, detail, checkpoint_json FROM runtime_task_steps
                         WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                        params![workspace_scope, id, step_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(|error| format!("无法读取任务步骤当前状态：{error}"))?;
                transaction
                    .execute(
                        "INSERT INTO runtime_task_steps
                         (workspace_scope, task_id, step_id, position, state, detail, checkpoint_json, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                         ON CONFLICT(workspace_scope, task_id, step_id) DO UPDATE SET
                           position=excluded.position, state=excluded.state, detail=excluded.detail,
                           checkpoint_json=excluded.checkpoint_json, updated_at=excluded.updated_at",
                        params![
                            workspace_scope,
                            id,
                            step_id,
                            position as i64,
                            step_state,
                            detail.chars().take(4000).collect::<String>(),
                            checkpoint_json,
                            now
                        ],
                    )
                    .map_err(|error| format!("无法保存原生任务步骤：{error}"))?;
                let changed = match previous.as_ref() {
                    Some((old_state, old_detail, old_checkpoint)) => {
                        old_state != step_state
                            || old_detail != detail
                            || old_checkpoint != &checkpoint_json
                    }
                    None => true,
                };
                if changed {
                    let revision = transaction
                        .query_row(
                            "SELECT COALESCE(MAX(revision), 0) + 1 FROM runtime_task_step_revisions
                             WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                            params![workspace_scope, id, step_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|error| format!("无法计算任务步骤修订号：{error}"))?;
                    transaction
                        .execute(
                            "INSERT INTO runtime_task_step_revisions
                             (id, workspace_scope, task_id, step_id, revision, position, state, detail,
                              checkpoint_json, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                Uuid::new_v4().to_string(),
                                workspace_scope,
                                id,
                                step_id,
                                revision,
                                position as i64,
                                step_state,
                                detail.chars().take(4000).collect::<String>(),
                                checkpoint_json,
                                now,
                            ],
                        )
                        .map_err(|error| format!("无法记录任务步骤修订：{error}"))?;
                }
            }
        }
        let stale_step_ids = {
            let mut statement = transaction
                .prepare("SELECT step_id FROM runtime_task_steps WHERE workspace_scope=?1 AND task_id=?2")
                .map_err(|error| format!("无法读取任务现有步骤：{error}"))?;
            let rows = statement
                .query_map(params![workspace_scope, id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法枚举任务现有步骤：{error}"))?
                .filter_map(Result::ok)
                .filter(|step_id| !current_step_ids.contains(step_id))
                .collect::<Vec<_>>();
            rows
        };
        for step_id in stale_step_ids {
            transaction
                .execute(
                    "DELETE FROM runtime_task_steps WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                    params![workspace_scope, id, step_id],
                )
                .map_err(|error| format!("无法移除过期任务步骤：{error}"))?;
        }
        sync_runtime_task_checkpoints(transaction, workspace_scope, &id, task)?;
    }
    Ok(())
}

fn sync_runtime_task_checkpoints(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    task_id: &str,
    task: &Value,
) -> Result<(), String> {
    let checkpoints = task
        .get("checkpoints")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if checkpoints.len() > 512 {
        return Err("单个任务的检查点超过 512 个安全上限".to_string());
    }
    for (sequence, checkpoint) in checkpoints.iter().enumerate() {
        let checkpoint_id = runtime_value_string(checkpoint, "id", "任务检查点")?;
        let state = checkpoint
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if !matches!(state, "pending" | "running" | "completed" | "failed") {
            return Err("任务检查点状态无效".to_string());
        }
        let payload = serde_json::to_string(checkpoint)
            .map_err(|error| format!("无法序列化任务检查点：{error}"))?;
        if payload.len() > MAX_INBOUND_RECORD_BYTES {
            return Err("单个任务检查点超过 512 KB 安全上限".to_string());
        }
        let payload_hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let now = Utc::now().to_rfc3339();
        let completed_at = (state == "completed").then_some(now.as_str());
        transaction
            .execute(
                "INSERT INTO runtime_task_checkpoints
                 (workspace_scope, task_id, checkpoint_id, sequence, state, payload, payload_hash,
                  created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
                 ON CONFLICT(workspace_scope, task_id, checkpoint_id) DO UPDATE SET
                   sequence=excluded.sequence, state=excluded.state, payload=excluded.payload,
                   payload_hash=excluded.payload_hash, updated_at=excluded.updated_at,
                   completed_at=COALESCE(excluded.completed_at, runtime_task_checkpoints.completed_at)",
                params![
                    workspace_scope,
                    task_id,
                    checkpoint_id,
                    sequence as i64,
                    state,
                    payload,
                    payload_hash,
                    now,
                    completed_at,
                ],
            )
            .map_err(|error| format!("无法保存任务检查点：{error}"))?;
    }
    Ok(())
}

fn sync_runtime_schedule_group(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    schedules: &[Value],
    schedule_kind: &str,
) -> Result<(), String> {
    let current_ids = schedules
        .iter()
        .map(|schedule| runtime_value_string(schedule, "id", "原生日程"))
        .collect::<Result<HashSet<_>, _>>()?;
    let existing = {
        let mut statement = transaction
            .prepare(
                "SELECT id FROM runtime_schedules WHERE workspace_scope=?1 AND schedule_kind=?2",
            )
            .map_err(|error| format!("无法读取已登记原生日程：{error}"))?;
        let ids = statement
            .query_map(params![workspace_scope, schedule_kind], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("无法枚举已登记原生日程：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        ids
    };
    for id in existing.into_iter().filter(|id| !current_ids.contains(id)) {
        transaction
            .execute(
                "DELETE FROM runtime_schedules WHERE workspace_scope=?1 AND id=?2 AND schedule_kind=?3",
                params![workspace_scope, id, schedule_kind],
            )
            .map_err(|error| format!("无法移除已删除原生日程：{error}"))?;
    }
    for schedule in schedules {
        upsert_runtime_schedule_record(transaction, workspace_scope, schedule, schedule_kind)?;
    }
    Ok(())
}

fn canonical_schedule_payload_hash(hash: &str) -> String {
    let trimmed = hash.trim();
    let digest = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    format!("sha256:{}", digest.to_ascii_lowercase())
}

fn verified_schedule_payload_snapshot(
    payload: Value,
    payload_hash: &str,
    label: &str,
) -> Result<(Value, String), String> {
    if !payload.is_object() || payload.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(format!("{label} payload 必须是非空 JSON 对象"));
    }
    let supplied_hash = canonical_schedule_payload_hash(payload_hash);
    let digest = supplied_hash
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("{label} payload hash 格式无效"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} payload hash 格式无效"));
    }
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| format!("无法序列化{label} payload：{error}"))?;
    let computed_hash = format!("sha256:{:x}", Sha256::digest(payload_json.as_bytes()));
    if supplied_hash != computed_hash {
        return Err(format!("{label} payload 与 hash 不匹配"));
    }
    Ok((payload, computed_hash))
}

fn read_runtime_schedule_revision_snapshot(
    connection: &Connection,
    workspace_scope: &str,
    schedule_id: &str,
    schedule_kind: &str,
    schedule_revision: i64,
) -> Result<Option<(Value, String)>, String> {
    let snapshot = connection
        .query_row(
            "SELECT payload, payload_hash
             FROM runtime_schedule_revisions
             WHERE workspace_scope=?1 AND schedule_id=?2 AND schedule_kind=?3 AND revision=?4",
            params![
                workspace_scope,
                schedule_id,
                schedule_kind,
                schedule_revision
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取日程 occurrence 历史快照：{error}"))?;
    let Some((payload_json, payload_hash)) = snapshot else {
        return Ok(None);
    };
    let payload = serde_json::from_str::<Value>(&payload_json)
        .map_err(|error| format!("日程 occurrence 历史 payload 无法解析：{error}"))?;
    verified_schedule_payload_snapshot(payload, &payload_hash, "日程 occurrence 历史快照").map(Some)
}

fn upsert_runtime_schedule_record(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    schedule: &Value,
    schedule_kind: &str,
) -> Result<(), String> {
    let id = runtime_value_string(schedule, "id", "原生日程")?;
    let enabled = schedule
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let next_run = normalize_runtime_time(schedule.get("nextRun").and_then(Value::as_str));
    let payload =
        serde_json::to_string(schedule).map_err(|error| format!("无法序列化原生日程：{error}"))?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err("单条原生日程超过 2 MB 安全上限".to_string());
    }
    let payload_hash = format!("sha256:{:x}", Sha256::digest(payload.as_bytes()));
    let previous = transaction
        .query_row(
            "SELECT payload_hash, revision FROM runtime_schedules
             WHERE workspace_scope=?1 AND id=?2 AND schedule_kind=?3",
            params![workspace_scope, id, schedule_kind],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取原生日程修订：{error}"))?;
    let payload_changed = previous
        .as_ref()
        .map(|(hash, _)| hash.as_str() != payload_hash.as_str())
        .unwrap_or(true);
    let revision = previous
        .as_ref()
        .map(|(_, value)| if payload_changed { value + 1 } else { *value })
        .unwrap_or(1);
    let now = Utc::now().to_rfc3339();
    transaction
        .execute(
            "INSERT INTO runtime_schedules
             (workspace_scope, id, schedule_kind, enabled, next_run, payload, payload_hash, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(workspace_scope, id, schedule_kind) DO UPDATE SET
               enabled=excluded.enabled, next_run=excluded.next_run, payload=excluded.payload,
               payload_hash=excluded.payload_hash, revision=excluded.revision,
               lease_owner=NULL, lease_expires_at=NULL, updated_at=excluded.updated_at",
            params![workspace_scope, id, schedule_kind, i64::from(enabled), next_run, payload, payload_hash, revision, now],
        )
        .map_err(|error| format!("无法保存原生日程：{error}"))?;
    if payload_changed {
        transaction
            .execute(
                "INSERT INTO runtime_schedule_revisions
                 (id, workspace_scope, schedule_id, schedule_kind, revision, payload, payload_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![Uuid::new_v4().to_string(), workspace_scope, id, schedule_kind, revision, payload, payload_hash, now],
            )
            .map_err(|error| format!("无法保存原生日程修订：{error}"))?;
    }
    Ok(())
}

fn read_payloads(
    connection: &Connection,
    sql: &str,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备快照查询：{error}"))?;
    let mut payloads = Vec::new();
    if let Some(limit) = limit {
        let rows = statement
            .query_map([limit as i64], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取快照：{error}"))?;
        for payload in rows.filter_map(Result::ok) {
            if let Ok(value) = serde_json::from_str(&payload) {
                payloads.push(value);
            }
        }
    } else {
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取快照：{error}"))?;
        for payload in rows.filter_map(Result::ok) {
            if let Ok(value) = serde_json::from_str(&payload) {
                payloads.push(value);
            }
        }
    }
    Ok(payloads)
}

fn markdown_metadata(content: &str) -> (String, Vec<String>, Vec<String>) {
    let title = content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .unwrap_or("无标题笔记")
        .nfc()
        .collect::<String>();
    let tag_regex = Regex::new(r"(?:^|\s)#([\p{L}\p{N}_/-]+)").expect("valid tag regex");
    let link_regex = Regex::new(r"\[\[([^\]|#]+)").expect("valid wiki link regex");
    let mut tags = tag_regex
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().nfc().collect::<String>())
        })
        .collect::<Vec<_>>();
    let mut links = link_regex
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().trim().nfc().collect::<String>())
        })
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    links.sort();
    links.dedup();
    (title, tags, links)
}

fn fts_match_query(query: &str) -> Result<String, String> {
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    Ok(terms.join(" AND "))
}

fn ensure_index_not_cancelled<F>(is_cancelled: &F) -> Result<(), String>
where
    F: Fn() -> bool,
{
    if is_cancelled() {
        Err("Vault 索引已取消".to_string())
    } else {
        Ok(())
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2FA1F}'
    )
}

fn cjk_lexical_terms(value: &str) -> String {
    let mut terms = Vec::new();
    let mut run = Vec::new();
    let flush = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        terms.extend(run.iter().map(char::to_string));
        terms.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
        run.clear();
    };
    for character in value.nfc() {
        if is_cjk(character) {
            run.push(character);
        } else {
            flush(&mut run, &mut terms);
        }
    }
    flush(&mut run, &mut terms);
    terms.sort();
    terms.dedup();
    terms.join(" ")
}

// Local feature vectors are deterministic hashed bags of words and CJK 1-3 grams.
// They are rebuildable search features, not model embeddings.
fn stable_feature_hash(namespace: u8, bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64 ^ u64::from(namespace);
    for byte in bytes {
        hash = stable_feature_hash_step(hash, byte);
    }
    hash
}

fn stable_feature_hash_step(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
}

fn add_hashed_feature(vector: &mut [f32], hash: u64, weight: f32) {
    let index = (hash as usize) % vector.len();
    let sign = if hash & (1_u64 << 63) == 0 { 1.0 } else { -1.0 };
    vector[index] += weight * sign;
}

fn add_text_feature(vector: &mut [f32], namespace: u8, text: &str, weight: f32) {
    if !text.is_empty() {
        add_hashed_feature(vector, stable_feature_hash(namespace, text.bytes()), weight);
    }
}

fn add_cjk_feature(vector: &mut [f32], namespace: u8, characters: &[char], weight: f32) {
    let mut hash = 0xcbf29ce484222325_u64 ^ u64::from(namespace);
    for character in characters {
        let mut encoded = [0_u8; 4];
        for byte in character.encode_utf8(&mut encoded).bytes() {
            hash = stable_feature_hash_step(hash, byte);
        }
    }
    add_hashed_feature(vector, hash, weight);
}

fn flush_vector_word(vector: &mut [f32], word: &mut String, weight: f32) {
    if !word.is_empty() {
        add_text_feature(vector, b'w', word, weight * 1.4);
        word.clear();
    }
}

fn add_local_vector_text(vector: &mut [f32], value: &str, weight: f32, max_chars: usize) {
    let mut word = String::new();
    let mut previous = None;
    let mut previous_previous = None;
    for character in value.nfc().flat_map(char::to_lowercase).take(max_chars) {
        if is_cjk(character) {
            flush_vector_word(vector, &mut word, weight);
            add_cjk_feature(vector, b'1', &[character], weight * 0.35);
            if let Some(left) = previous {
                add_cjk_feature(vector, b'2', &[left, character], weight);
            }
            if let (Some(left), Some(middle)) = (previous_previous, previous) {
                add_cjk_feature(vector, b'3', &[left, middle, character], weight * 1.25);
            }
            previous_previous = previous;
            previous = Some(character);
        } else {
            previous = None;
            previous_previous = None;
            if character.is_alphanumeric() {
                if word.len() >= 128 {
                    flush_vector_word(vector, &mut word, weight);
                }
                word.push(character);
            } else {
                flush_vector_word(vector, &mut word, weight);
            }
        }
    }
    flush_vector_word(vector, &mut word, weight);
}

fn normalize_local_feature_vector(vector: &mut [f32]) -> bool {
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON {
        return false;
    }
    for value in vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    true
}

fn note_local_feature_vector(
    relative_path: &str,
    title: &str,
    content: &str,
    tags: &[String],
    wiki_links: &[String],
) -> Vec<u8> {
    let mut vector = vec![0_f32; LOCAL_FEATURE_VECTOR_DIMENSIONS];
    add_local_vector_text(&mut vector, title, 3.0, usize::MAX);
    add_local_vector_text(&mut vector, relative_path, 1.5, usize::MAX);
    for tag in tags {
        add_local_vector_text(&mut vector, tag, 2.5, usize::MAX);
    }
    for link in wiki_links {
        add_local_vector_text(&mut vector, link, 2.0, usize::MAX);
    }
    add_local_vector_text(&mut vector, content, 1.0, MAX_LOCAL_VECTOR_CONTENT_CHARS);
    normalize_local_feature_vector(&mut vector);
    vector
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>()
}

fn query_local_feature_vector(query: &str) -> Option<Vec<f32>> {
    let mut vector = vec![0_f32; LOCAL_FEATURE_VECTOR_DIMENSIONS];
    add_local_vector_text(&mut vector, query, 1.0, MAX_SEARCH_QUERY_CHARS);
    normalize_local_feature_vector(&mut vector).then_some(vector)
}

fn decode_local_feature_vector(version: i64, dimensions: i64, blob: &[u8]) -> Option<Vec<f32>> {
    if version != LOCAL_FEATURE_VECTOR_VERSION
        || dimensions != LOCAL_FEATURE_VECTOR_DIMENSIONS as i64
        || blob.len() != LOCAL_FEATURE_VECTOR_DIMENSIONS * std::mem::size_of::<f32>()
    {
        return None;
    }
    let vector = blob
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    (vector.iter().all(|value| value.is_finite()) && (0.99..=1.01).contains(&norm))
        .then_some(vector)
}

fn local_vector_similarity(query: &[f32], candidate: &[f32]) -> Option<f64> {
    if query.len() != candidate.len() {
        return None;
    }
    let similarity = query
        .iter()
        .zip(candidate)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum::<f64>();
    similarity
        .is_finite()
        .then_some(similarity.clamp(-1.0, 1.0))
}

fn normalize_neural_embedding(mut vector: Vec<f32>) -> Result<Vec<f32>, String> {
    if vector.is_empty() || vector.len() > 65_536 || vector.iter().any(|value| !value.is_finite()) {
        return Err("神经 Embedding 向量为空、过大或包含非有限数值".to_string());
    }
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON {
        return Err("神经 Embedding 向量不能是零向量".to_string());
    }
    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(vector)
}

fn encode_neural_embedding(vector: Vec<f32>) -> Result<(i64, Vec<u8>), String> {
    let vector = normalize_neural_embedding(vector)?;
    let dimensions = vector.len() as i64;
    let blob = vector
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    Ok((dimensions, blob))
}

fn decode_neural_embedding(dimensions: i64, blob: &[u8]) -> Option<Vec<f32>> {
    let dimensions = usize::try_from(dimensions).ok()?;
    if dimensions == 0
        || dimensions > 65_536
        || blob.len() != dimensions * std::mem::size_of::<f32>()
    {
        return None;
    }
    let vector = blob
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    (vector.iter().all(|value| value.is_finite()) && (0.99..=1.01).contains(&norm))
        .then_some(vector)
}

fn neural_embedding_input_hash(input: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(input.as_bytes()))
}

fn neural_note_embedding_input(
    relative_path: &str,
    title: &str,
    tags_json: &str,
    wiki_links_json: &str,
    content: &str,
) -> String {
    let content = content
        .nfc()
        .take(MAX_NEURAL_EMBEDDING_INPUT_CHARS)
        .collect::<String>();
    render_prompt_template(
        NEURAL_NOTE_EMBEDDING_PROMPT_TEMPLATE,
        &[
            ("title", title),
            ("relative_path", relative_path),
            ("tags_json", tags_json),
            ("wiki_links_json", wiki_links_json),
            ("content", &content),
        ],
    )
    .expect("bundled neural note embedding Prompt must be valid")
    .nfc()
    .take(crate::model_provider::MAX_EMBEDDING_INPUT_CHARS)
    .collect()
}

fn lexical_fts_match_query(query: &str) -> Result<String, String> {
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let normalized = query.trim().nfc().collect::<String>();
    if normalized.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    let mut groups = Vec::new();
    for raw_term in normalized.split_whitespace() {
        let cjk = raw_term
            .chars()
            .filter(|character| is_cjk(*character))
            .collect::<Vec<_>>();
        if cjk.len() >= 2 {
            let pairs = cjk
                .windows(2)
                .map(|pair| format!("\"{}\"", pair.iter().collect::<String>()))
                .collect::<Vec<_>>();
            groups.push(format!("({})", pairs.join(" AND ")));
        } else {
            groups.push(format!("\"{}\"", raw_term.replace('"', "\"\"")));
        }
    }
    if groups.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    Ok(groups.join(" AND "))
}

fn strict_path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("{label}不是有效 UTF-8"))
}

fn canonical_index_root(root: &Path) -> Result<PathBuf, String> {
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("无法规范化 Vault 根目录：{error}"))?;
    if !canonical.is_dir() {
        return Err("Vault 根路径不是目录".to_string());
    }
    strict_path_text(&canonical, "Vault 根目录")?;
    Ok(canonical)
}

fn normalize_queued_relative_path(value: &str) -> Result<String, String> {
    if value.is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
        return Err("索引相对路径无效".to_string());
    }
    Ok(value.replace('\\', "/").nfc().collect())
}

fn validate_index_relative_path(value: &str) -> Result<PathBuf, String> {
    let normalized = normalize_queued_relative_path(value)?;
    if normalized != value {
        return Err("索引相对路径尚未规范化".to_string());
    }
    let relative = Path::new(value);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err("索引路径必须是 Vault 内的相对路径".to_string());
    }
    if relative.components().any(|component| {
        !matches!(component, std::path::Component::Normal(_))
            || component
                .as_os_str()
                .to_str()
                .is_none_or(|value| value.starts_with('.'))
    }) {
        return Err("索引路径包含隐藏目录、无效 UTF-8 或目录跳转".to_string());
    }
    if !relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err("索引队列只接受 Markdown 笔记".to_string());
    }
    Ok(relative.to_path_buf())
}

fn normalized_index_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "索引文件越过 Vault 边界".to_string())?;
    let text = strict_path_text(relative, "索引文件路径")?;
    normalize_queued_relative_path(text)
}

fn resolve_index_target(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let relative = validate_index_relative_path(relative_path)?;
    let mut target = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("索引路径包含无效目录组件".to_string());
        };
        let direct = target.join(name);
        match fs::symlink_metadata(&direct) {
            Ok(_) => {
                target = direct;
                continue;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法解析索引目标：{error}")),
        }
        if !target.is_dir() {
            target = direct;
            continue;
        }
        let expected = name
            .to_str()
            .ok_or_else(|| "索引路径组件不是有效 UTF-8".to_string())?;
        let mut normalized_match = None;
        for entry in
            fs::read_dir(&target).map_err(|error| format!("无法读取索引目标目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取索引目标目录项：{error}"))?;
            let Some(candidate) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if candidate.nfc().eq(expected.chars()) {
                if normalized_match.is_some() {
                    return Err("Vault 中存在规范化后重名的笔记路径".to_string());
                }
                normalized_match = Some(entry.path());
            }
        }
        target = normalized_match.unwrap_or(direct);
    }
    Ok(target)
}

fn ensure_registered_vault_root(
    connection: &Connection,
    vault_id: &str,
    canonical_root: &Path,
) -> Result<(), String> {
    let registered = connection
        .query_row(
            "SELECT canonical_path FROM vault_registry WHERE id=?1",
            [vault_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法校验 Vault 注册路径：{error}"))?
        .ok_or_else(|| "Vault 尚未登记到本地索引注册表".to_string())?;
    if Path::new(&registered) != canonical_root {
        return Err("Vault 注册路径已变化，拒绝应用旧索引任务".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn enqueue_vault_index_change_in_connection(
    connection: &Connection,
    vault_id: &str,
    canonical_root: &str,
    relative_path: &str,
    change_kind: &str,
    trace_id: &str,
    replace_existing_trace: bool,
    available_at_ms: i64,
    now: &str,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO vault_index_changes
             (vault_id, canonical_root, relative_path, generation, change_kind, state,
              attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, 'pending', 0, ?6, NULL, NULL, ?5, ?7, ?7)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               canonical_root=excluded.canonical_root,
               generation=vault_index_changes.generation+1,
               change_kind=excluded.change_kind,
               state='pending', attempt_count=0,
               available_at_ms=excluded.available_at_ms,
               claimed_at_ms=NULL, last_error=NULL,
               trace_id=CASE WHEN ?8 THEN excluded.trace_id
                             ELSE COALESCE(vault_index_changes.trace_id, excluded.trace_id) END,
               updated_at=excluded.updated_at",
            params![
                vault_id,
                canonical_root,
                relative_path,
                change_kind,
                trace_id,
                available_at_ms,
                now,
                replace_existing_trace
            ],
        )
        .map_err(|error| format!("无法持久化 Vault 索引变更：{error}"))?;
    let (change_id, generation, persisted_trace_id) = connection
        .query_row(
            "SELECT id, generation, trace_id FROM vault_index_changes
             WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|error| format!("无法读取 Vault 索引入队结果：{error}"))?;
    crate::trace::record_trace_event_in_connection(
        connection,
        DEFAULT_LOCAL_WORKSPACE_SCOPE,
        &crate::trace::TraceEventRecord {
            trace_id: &persisted_trace_id,
            entity_kind: "index_change",
            entity_id: &format!("{change_id}:{generation}"),
            event_type: "index.enqueued",
            state: "pending",
            payload: &serde_json::json!({
                "vaultId": vault_id,
                "relativePath": relative_path,
                "changeKind": change_kind,
                "generation": generation,
            }),
            created_at: now,
        },
    )?;
    Ok(())
}

fn dead_letter_exhausted_vault_index_changes(
    transaction: &Transaction<'_>,
    source_state: &str,
    default_error: &str,
    source: &str,
    now: &str,
) -> Result<usize, String> {
    if !matches!(source_state, "pending" | "processing") {
        return Err("Vault 索引死信来源状态无效".to_string());
    }
    let exhausted = {
        let mut statement = transaction
            .prepare(
                "SELECT id, vault_id, relative_path, generation, attempt_count, trace_id, last_error
                 FROM vault_index_changes
                 WHERE state=?1 AND attempt_count >= ?2
                 ORDER BY id",
            )
            .map_err(|error| format!("无法准备 Vault 索引死信查询：{error}"))?;
        let rows = statement
            .query_map(params![source_state, VAULT_INDEX_MAX_ATTEMPTS], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|error| format!("无法读取 Vault 索引死信任务：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Vault 索引死信任务：{error}"))?
    };

    let mut dead_lettered = 0;
    for (id, vault_id, relative_path, generation, attempt_count, trace_id, last_error) in exhausted
    {
        let error = last_error
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default_error.to_string());
        let updated = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state='dead_letter', claimed_at_ms=NULL, last_error=?1, updated_at=?2
                 WHERE id=?3 AND generation=?4 AND state=?5 AND attempt_count >= ?6",
                params![
                    error,
                    now,
                    id,
                    generation,
                    source_state,
                    VAULT_INDEX_MAX_ATTEMPTS,
                ],
            )
            .map_err(|db_error| format!("无法写入 Vault 索引死信状态：{db_error}"))?;
        if updated != 1 {
            continue;
        }
        crate::trace::record_trace_event_in_connection(
            transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "index_change",
                entity_id: &format!("{id}:{generation}"),
                event_type: "index.dead_lettered",
                state: "dead_letter",
                payload: &serde_json::json!({
                    "vaultId": vault_id,
                    "relativePath": relative_path,
                    "attemptCount": attempt_count,
                    "error": error,
                    "source": source,
                }),
                created_at: now,
            },
        )?;
        insert_operation_event_in_transaction(
            transaction,
            &OperationEvent {
                id: Uuid::new_v4().to_string(),
                task_id: None,
                trace_id: Some(trace_id),
                event_type: "vault.note.index".to_string(),
                state: "failed".to_string(),
                created_at: now.to_string(),
                vault_id: Some(vault_id),
                relative_path: Some(relative_path),
                detail: format!("Vault 索引任务进入死信：{error}"),
            },
        )?;
        dead_lettered += 1;
    }
    Ok(dead_lettered)
}

fn insert_operation_event_in_transaction(
    transaction: &Transaction<'_>,
    event: &OperationEvent,
) -> Result<(), String> {
    let mut event = event.clone();
    let trace_id = event
        .trace_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(crate::trace::new_trace_id);
    crate::trace::validate_trace_id(&trace_id)?;
    event.trace_id = Some(trace_id.clone());
    let payload = serde_json::to_string(&event)
        .map_err(|error| format!("无法序列化 Vault 索引事件：{error}"))?;
    transaction
        .execute(
            "INSERT INTO operation_events
             (id, task_id, trace_id, event_type, state, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.id,
                event.task_id,
                trace_id,
                event.event_type,
                event.state,
                payload,
                event.created_at
            ],
        )
        .map_err(|error| format!("无法写入 Vault 索引操作日志：{error}"))?;
    crate::trace::record_trace_event_in_connection(
        transaction,
        DEFAULT_LOCAL_WORKSPACE_SCOPE,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "operation_event",
            entity_id: &event.id,
            event_type: &event.event_type,
            state: &event.state,
            payload: &serde_json::json!({
                "taskId": event.task_id,
                "vaultId": event.vault_id,
                "relativePath": event.relative_path,
                "detail": event.detail,
            }),
            created_at: &event.created_at,
        },
    )?;
    if let Some(vault_id) = event.vault_id.as_deref() {
        crate::trace::record_trace_event_in_connection(
            transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "vault_operation",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "vaultId": vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
    }
    Ok(())
}

fn prepare_note_index(root: &Path, path: &Path) -> Result<Option<PreparedNoteIndex>, String> {
    let relative_path = normalized_index_relative_path(root, path)?;
    validate_index_relative_path(&relative_path)?;
    let canonical_root = canonical_index_root(root)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取索引文件元数据：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Ok(None);
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("无法规范化索引文件：{error}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("索引文件越过 Vault 边界".to_string());
    }
    let bytes = read_file_limited_for_runtime(&canonical_path)?;
    let Ok(content) = String::from_utf8(bytes.clone()) else {
        return Ok(None);
    };
    let (fallback_title, tags, links) = markdown_metadata(&content);
    let title = if fallback_title == "无标题笔记" {
        canonical_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("无标题笔记")
            .nfc()
            .collect()
    } else {
        fallback_title
    };
    let modified_at = metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<Utc>::from)
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    let feature_vector = note_local_feature_vector(&relative_path, &title, &content, &tags, &links);
    Ok(Some(PreparedNoteIndex {
        relative_path,
        title,
        content_hash: format!("{:x}", Sha256::digest(&bytes)),
        modified_at,
        byte_length: metadata.len(),
        tags_json: serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string()),
        wiki_links_json: serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string()),
        content,
        feature_vector,
    }))
}

fn upsert_prepared_note_index(
    transaction: &Transaction<'_>,
    vault_id: &str,
    note: &PreparedNoteIndex,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO note_index
             (vault_id, relative_path, title, content_hash, modified_at, byte_length, tags_json, wiki_links_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               title=excluded.title,
               content_hash=excluded.content_hash,
               modified_at=excluded.modified_at,
               byte_length=excluded.byte_length,
               tags_json=excluded.tags_json,
               wiki_links_json=excluded.wiki_links_json",
            params![
                vault_id,
                note.relative_path,
                note.title,
                note.content_hash,
                note.modified_at,
                note.byte_length,
                note.tags_json,
                note.wiki_links_json,
            ],
        )
        .map_err(|error| format!("无法更新笔记索引：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note.relative_path],
        )
        .map_err(|error| format!("无法刷新全文索引：{error}"))?;
    transaction
        .execute(
            "INSERT INTO note_fts (vault_id, relative_path, title, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![vault_id, note.relative_path, note.title, note.content],
        )
        .map_err(|error| format!("无法写入全文索引：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_lexical_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note.relative_path],
        )
        .map_err(|error| format!("无法刷新中文词法索引：{error}"))?;
    let searchable = format!(
        "{}\n{}\n{}\n{}\n{}",
        note.relative_path, note.title, note.content, note.tags_json, note.wiki_links_json
    );
    let cjk_terms = cjk_lexical_terms(&searchable);
    transaction
        .execute(
            "INSERT INTO note_lexical_fts
             (vault_id, relative_path, title, content, tags, wiki_links, cjk_terms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                vault_id,
                note.relative_path,
                note.title,
                note.content,
                note.tags_json,
                note.wiki_links_json,
                cjk_terms,
            ],
        )
        .map_err(|error| format!("无法写入中文词法索引：{error}"))?;
    transaction
        .execute(
            "INSERT INTO note_feature_vectors
             (vault_id, relative_path, content_hash, representation_version,
              dimensions, vector_blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               content_hash=excluded.content_hash,
               representation_version=excluded.representation_version,
               dimensions=excluded.dimensions,
               vector_blob=excluded.vector_blob,
               updated_at=excluded.updated_at",
            params![
                vault_id,
                note.relative_path,
                note.content_hash,
                LOCAL_FEATURE_VECTOR_VERSION,
                LOCAL_FEATURE_VECTOR_DIMENSIONS as i64,
                note.feature_vector,
                note.modified_at,
            ],
        )
        .map_err(|error| format!("无法写入本地特征向量：{error}"))?;
    transaction
        .execute(
            "UPDATE neural_embedding_index_state
             SET state='pending', last_error=NULL, updated_at=?2
             WHERE vault_id=?1",
            params![vault_id, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法标记神经向量索引待更新：{error}"))?;
    Ok(())
}

fn delete_note_index_in_transaction(
    transaction: &Transaction<'_>,
    vault_id: &str,
    relative_path: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除全文索引项：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_lexical_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除中文词法索引项：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_feature_vectors WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除本地特征向量：{error}"))?;
    transaction
        .execute(
            "UPDATE neural_embedding_index_state
             SET state='pending', last_error=NULL, updated_at=?2
             WHERE vault_id=?1",
            params![vault_id, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法标记神经向量索引待更新：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除笔记索引项：{error}"))?;
    Ok(())
}

fn save_workspace_snapshot_in(
    database: &RuntimeDatabase,
    mut snapshot: WorkspaceSnapshot,
) -> Result<(), String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("save_workspace_snapshot")
        .with_threshold(database.config.slow_query_threshold_ms);

    let workspace_scope = database.local_workspace_scope()?;
    validate_records(&snapshot.tasks, "任务")?;
    // Conversation articles are user content, not control-plane records. They can
    // exceed the generic 2 MB record guard and remain persistable in SQLite.
    validate_workspace_messages(&snapshot.messages)?;
    validate_records(&snapshot.approvals, "审批")?;
    validate_records(&snapshot.operation_logs, "操作日志")?;
    for (label, records) in [
        ("tasks", snapshot.tasks.as_slice()),
        ("messages", snapshot.messages.as_slice()),
        ("approvals", snapshot.approvals.as_slice()),
        ("operationLogs", snapshot.operation_logs.as_slice()),
    ] {
        for (index, record) in records.iter().enumerate() {
            validate_workspace_snapshot_value(record, &format!("{label}[{index}]"))?;
        }
    }
    validate_workspace_snapshot_client_state(&snapshot.client_state)?;
    let messages = std::mem::take(&mut snapshot.messages);
    let payload = serde_json::to_string(&snapshot)
        .map_err(|error| format!("无法序列化本地工作区：{error}"))?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始工作区快照事务：{error}"))?;
    // The workspace shell is not an authoritative full-message replacement.
    // Older renderers may still include messages, so accept them as compatible
    // upserts, but deletion is only performed by the explicit delete commands.
    // This makes an empty shell save safe while messages are persisted page by page.
    if !messages.is_empty() {
        upsert_workspace_message_rows(&transaction, &workspace_scope, &messages, None)?;
    }
    transaction
        .execute(
            "INSERT INTO workspace_snapshots (workspace_scope, payload, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_scope) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
            params![workspace_scope, payload, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法保存本地工作区：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交本地工作区：{error}"))
}

#[tauri::command]
pub fn save_workspace_snapshot(
    database: State<'_, RuntimeDatabase>,
    snapshot: WorkspaceSnapshot,
) -> Result<(), String> {
    save_workspace_snapshot_in(database.inner(), snapshot)
}

fn upsert_workspace_messages_page_in(
    database: &RuntimeDatabase,
    messages: Vec<Value>,
) -> Result<(), String> {
    if messages.len() > 512 {
        return Err("单次消息持久化页最多包含 512 条；完整历史可继续分多页提交".to_string());
    }
    validate_workspace_messages(&messages)?;
    for (index, message) in messages.iter().enumerate() {
        validate_workspace_snapshot_value(message, &format!("messages[{index}]"))?;
    }
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始消息页持久化事务：{error}"))?;
    upsert_workspace_message_rows(&transaction, &workspace_scope, &messages, None)?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交消息页：{error}"))
}

#[tauri::command]
pub fn upsert_workspace_messages_page(
    database: State<'_, RuntimeDatabase>,
    messages: Vec<Value>,
) -> Result<(), String> {
    upsert_workspace_messages_page_in(database.inner(), messages)
}

fn list_workspace_messages_page_in(
    database: &RuntimeDatabase,
    conversation_id: Option<&str>,
    cursor_created_at: Option<&str>,
    cursor_id: Option<&str>,
    limit: Option<usize>,
) -> Result<WorkspaceMessagePage, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("list_workspace_messages_page")
        .with_threshold(database.config.slow_query_threshold_ms);

    if cursor_created_at.is_some() != cursor_id.is_some() {
        return Err("消息分页游标必须同时包含 cursorCreatedAt 和 cursorId".to_string());
    }
    let conversation_id = conversation_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if conversation_id.is_some_and(|value| value.chars().count() > 200) {
        return Err("消息分页 conversationId 无效".to_string());
    }
    let page_size = limit.unwrap_or(256).clamp(1, 512);
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT payload_json, created_at, id
             FROM workspace_messages
             WHERE workspace_scope=?1
               AND (?2 IS NULL OR conversation_id=?2)
               AND (?3 IS NULL OR created_at>?3 OR (created_at=?3 AND id>?4))
             ORDER BY created_at ASC, id ASC
             LIMIT ?5",
        )
        .map_err(|error| format!("无法准备消息分页读取：{error}"))?;
    let rows = statement
        .query_map(
            params![
                workspace_scope,
                conversation_id,
                cursor_created_at,
                cursor_id,
                (page_size + 1) as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|error| format!("无法读取消息页：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析消息页：{error}"))?;
    let has_more = rows.len() > page_size;
    let visible = rows.into_iter().take(page_size).collect::<Vec<_>>();
    let next_cursor = has_more.then(|| visible.last().cloned()).flatten();
    let items = visible
        .into_iter()
        .map(|(payload_json, _, _)| {
            serde_json::from_str::<Value>(&payload_json)
                .map_err(|error| format!("独立消息记录已经损坏：{error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkspaceMessagePage {
        items,
        next_cursor_created_at: next_cursor
            .as_ref()
            .map(|(_, created_at, _)| created_at.clone()),
        next_cursor_id: next_cursor.map(|(_, _, id)| id),
    })
}

#[tauri::command]
pub fn list_workspace_messages_page(
    database: State<'_, RuntimeDatabase>,
    conversation_id: Option<String>,
    cursor_created_at: Option<String>,
    cursor_id: Option<String>,
    limit: Option<usize>,
) -> Result<WorkspaceMessagePage, String> {
    list_workspace_messages_page_in(
        database.inner(),
        conversation_id.as_deref(),
        cursor_created_at.as_deref(),
        cursor_id.as_deref(),
        limit,
    )
}

fn search_workspace_messages_in(
    database: &RuntimeDatabase,
    query: &str,
    limit: Option<usize>,
) -> Result<Vec<WorkspaceMessageSearchResult>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("search_workspace_messages")
        .with_threshold(database.config.slow_query_threshold_ms);

    let match_query = lexical_fts_match_query(query)?;
    let result_limit = limit.unwrap_or(50).clamp(1, 100);
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT workspace_message_fts.conversation_id,
                    workspace_message_fts.message_id,
                    workspace_message_fts.role,
                    message.created_at,
                    snippet(workspace_message_fts, 4, '', '', '…', 24),
                    bm25(workspace_message_fts)
             FROM workspace_message_fts
             JOIN workspace_messages message
               ON message.workspace_scope=workspace_message_fts.workspace_scope
              AND message.id=workspace_message_fts.message_id
             WHERE workspace_message_fts MATCH ?1
               AND workspace_message_fts.workspace_scope=?2
             ORDER BY bm25(workspace_message_fts), message.created_at DESC,
                      workspace_message_fts.message_id DESC
             LIMIT ?3",
        )
        .map_err(|error| format!("无法准备会话消息搜索：{error}"))?;
    let results = statement
        .query_map(
            params![match_query, workspace_scope, result_limit as i64],
            |row| {
                Ok(WorkspaceMessageSearchResult {
                    conversation_id: row.get(0)?,
                    message_id: row.get(1)?,
                    role: row.get(2)?,
                    created_at: row.get(3)?,
                    snippet: row.get(4)?,
                    score: -row.get::<_, f64>(5)?,
                })
            },
        )
        .map_err(|error| format!("会话消息搜索失败：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析会话消息搜索结果：{error}"))?;
    Ok(results)
}

#[tauri::command]
pub fn search_workspace_messages(
    database: State<'_, RuntimeDatabase>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<WorkspaceMessageSearchResult>, String> {
    search_workspace_messages_in(database.inner(), &query, limit)
}

fn delete_workspace_messages_in(
    database: &RuntimeDatabase,
    message_ids: Vec<String>,
) -> Result<usize, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("delete_workspace_messages")
        .with_threshold(database.config.slow_query_threshold_ms);

    if message_ids.len() > 512 {
        return Err("单次最多删除 512 条消息；可继续分批删除".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始消息删除事务：{error}"))?;
    let mut deleted = 0usize;
    for message_id in message_ids {
        let message_id = message_id.trim();
        if !valid_runtime_identifier(message_id, 200) {
            return Err("待删除消息 ID 无效".to_string());
        }
        transaction
            .execute(
                "DELETE FROM workspace_message_fts
                 WHERE workspace_scope=?1 AND message_id=?2",
                params![workspace_scope, message_id],
            )
            .map_err(|error| format!("无法删除消息全文索引：{error}"))?;
        deleted = deleted.saturating_add(
            transaction
                .execute(
                    "DELETE FROM workspace_messages WHERE workspace_scope=?1 AND id=?2",
                    params![workspace_scope, message_id],
                )
                .map_err(|error| format!("无法删除消息：{error}"))?,
        );
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交消息删除：{error}"))?;
    Ok(deleted)
}

#[tauri::command]
pub fn delete_workspace_messages(
    database: State<'_, RuntimeDatabase>,
    message_ids: Vec<String>,
) -> Result<usize, String> {
    delete_workspace_messages_in(database.inner(), message_ids)
}

fn delete_workspace_conversation_messages_in(
    database: &RuntimeDatabase,
    conversation_id: String,
) -> Result<usize, String> {
    let conversation_id = conversation_id.trim();
    if conversation_id.is_empty() || conversation_id.chars().count() > 200 {
        return Err("待删除对话 ID 无效".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始对话消息删除事务：{error}"))?;
    transaction
        .execute(
            "DELETE FROM workspace_message_fts
             WHERE workspace_scope=?1 AND conversation_id=?2",
            params![workspace_scope, conversation_id],
        )
        .map_err(|error| format!("无法删除对话消息全文索引：{error}"))?;
    let deleted = transaction
        .execute(
            "DELETE FROM workspace_messages
             WHERE workspace_scope=?1 AND conversation_id=?2",
            params![workspace_scope, conversation_id],
        )
        .map_err(|error| format!("无法删除对话消息：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交对话消息删除：{error}"))?;
    Ok(deleted)
}

#[tauri::command]
pub fn delete_workspace_conversation_messages(
    database: State<'_, RuntimeDatabase>,
    conversation_id: String,
) -> Result<usize, String> {
    delete_workspace_conversation_messages_in(database.inner(), conversation_id)
}

fn validate_optional_runtime_handler_pair(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: Option<&OperationContext>,
    allowed_pairs: &[(&str, &str)],
) -> Result<Option<(String, String)>, String> {
    operation_context
        .map(|context| {
            database
                .validate_runtime_effectful_handler_pairs(
                    ticket_state,
                    workspace_scope,
                    context,
                    allowed_pairs,
                )
                .map(|authorization| (authorization.capability_id, authorization.operation))
        })
        .transpose()
}

fn record_runtime_database_handler_completion(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: Option<&OperationContext>,
    runtime_identity: Option<&(String, String)>,
    handler_started: Instant,
) -> Result<(), String> {
    if let (Some(context), Some((capability_id, operation))) = (operation_context, runtime_identity)
    {
        database.record_runtime_effectful_handler_completion(
            ticket_state,
            workspace_scope,
            context,
            capability_id,
            operation,
            TrustedHandlerUsage {
                tool_calls: 1,
                runtime_seconds: handler_started.elapsed().as_secs().max(1),
                tokens: 0,
                cost: Some(0.0),
            },
        )?;
    }
    Ok(())
}

#[tauri::command]
pub fn sync_runtime_state(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    tasks: Vec<Value>,
    schedules: Vec<Value>,
    report_subscriptions: Vec<Value>,
    scheduler_enabled: bool,
    operation_context: Option<OperationContext>,
) -> Result<(), String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_identity = validate_optional_runtime_handler_pair(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SCHEDULE_RUNTIME_HANDLER_PAIRS,
    )?;
    database.sync_runtime_state(
        &workspace_scope,
        &tasks,
        &schedules,
        &report_subscriptions,
        scheduler_enabled,
    )?;
    record_runtime_database_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_identity.as_ref(),
        handler_started,
    )
}

#[tauri::command]
pub fn sync_managed_resources(
    database: State<'_, RuntimeDatabase>,
    snapshot: ManagedResourceSnapshotInput,
) -> Result<ManagedResourceSnapshot, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.sync_managed_resources(&workspace_scope, &snapshot)
}

#[tauri::command]
pub fn load_managed_resources(
    database: State<'_, RuntimeDatabase>,
) -> Result<ManagedResourceSnapshot, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.load_managed_resources(&workspace_scope)
}

#[tauri::command]
pub fn upsert_report_record(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    report: Value,
    operation_context: Option<OperationContext>,
) -> Result<Value, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_identity = validate_optional_runtime_handler_pair(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        REPORT_RECORD_UPSERT_RUNTIME_HANDLER_PAIRS,
    )?;
    let result = database.upsert_report_resource(&workspace_scope, "report", &report)?;
    record_runtime_database_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_identity.as_ref(),
        handler_started,
    )?;
    Ok(result)
}

#[tauri::command]
pub fn delete_report_record(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    report_id: String,
    operation_context: OperationContext,
) -> Result<(), String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_identity = validate_optional_runtime_handler_pair(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        Some(&operation_context),
        REPORT_RESOURCE_DELETE_RUNTIME_HANDLER_PAIRS,
    )?;
    database.delete_report_resource(&workspace_scope, "report", &report_id)?;
    record_runtime_database_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        Some(&operation_context),
        runtime_identity.as_ref(),
        handler_started,
    )
}

#[tauri::command]
pub fn list_report_records_page(
    database: State<'_, RuntimeDatabase>,
    cursor_updated_at: Option<String>,
    cursor_id: Option<String>,
    limit: Option<usize>,
) -> Result<ManagedResourcePage, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_report_resources_page(
        &workspace_scope,
        "report",
        cursor_updated_at.as_deref(),
        cursor_id.as_deref(),
        limit.unwrap_or(128),
    )
}

#[tauri::command]
pub fn upsert_report_subscription(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    subscription: Value,
    operation_context: Option<OperationContext>,
) -> Result<Value, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_identity = validate_optional_runtime_handler_pair(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        REPORT_SUBSCRIPTION_UPSERT_RUNTIME_HANDLER_PAIRS,
    )?;
    let result =
        database.upsert_report_resource(&workspace_scope, "report_subscription", &subscription)?;
    record_runtime_database_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_identity.as_ref(),
        handler_started,
    )?;
    Ok(result)
}

#[tauri::command]
pub fn delete_report_subscription(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    subscription_id: String,
    operation_context: OperationContext,
) -> Result<(), String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_identity = validate_optional_runtime_handler_pair(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        Some(&operation_context),
        REPORT_RESOURCE_DELETE_RUNTIME_HANDLER_PAIRS,
    )?;
    database.delete_report_resource(&workspace_scope, "report_subscription", &subscription_id)?;
    record_runtime_database_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        Some(&operation_context),
        runtime_identity.as_ref(),
        handler_started,
    )
}

#[tauri::command]
pub fn list_report_subscriptions_page(
    database: State<'_, RuntimeDatabase>,
    cursor_updated_at: Option<String>,
    cursor_id: Option<String>,
    limit: Option<usize>,
) -> Result<ManagedResourcePage, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_report_resources_page(
        &workspace_scope,
        "report_subscription",
        cursor_updated_at.as_deref(),
        cursor_id.as_deref(),
        limit.unwrap_or(128),
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn read_report_source_page(
    database: State<'_, RuntimeDatabase>,
    source_kind: String,
    start_at: String,
    end_at: String,
    cursor_occurred_at: Option<String>,
    cursor_id: Option<String>,
    limit: Option<usize>,
) -> Result<ReportSourcePage, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.read_report_source_page(
        &workspace_scope,
        source_kind.trim(),
        start_at.trim(),
        end_at.trim(),
        cursor_occurred_at.as_deref(),
        cursor_id.as_deref(),
        limit.unwrap_or(128),
    )
}

#[tauri::command]
pub fn upsert_creation_resource(
    database: State<'_, RuntimeDatabase>,
    resource: CreationResourceInput,
) -> Result<CreationResource, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.upsert_creation_resource(&workspace_scope, resource)
}

#[tauri::command]
pub fn list_creation_resources(
    database: State<'_, RuntimeDatabase>,
    include_archived: Option<bool>,
) -> Result<Vec<CreationResource>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_creation_resources(&workspace_scope, include_archived.unwrap_or(false))
}

#[tauri::command]
pub fn list_creation_resource_revisions(
    database: State<'_, RuntimeDatabase>,
    resource_type: String,
    id: String,
) -> Result<Vec<CreationResource>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_creation_resource_revisions(&workspace_scope, &resource_type, &id)
}

#[tauri::command]
pub fn restore_creation_resource_revision(
    database: State<'_, RuntimeDatabase>,
    input: CreationResourceRestoreInput,
) -> Result<CreationResource, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.restore_creation_resource_revision(&workspace_scope, input)
}

#[tauri::command]
pub fn archive_creation_resource(
    database: State<'_, RuntimeDatabase>,
    resource_type: String,
    id: String,
) -> Result<CreationResourceArchiveReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.archive_creation_resource(&workspace_scope, &resource_type, &id)
}

#[tauri::command]
pub fn recover_interrupted_runtime_tasks(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<RuntimeTaskRecovery>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.recover_interrupted_runtime_tasks(&workspace_scope)
}

#[tauri::command]
pub fn supersede_runtime_task_for_recovery(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, crate::execution_ticket::ExecutionTicketState>,
    task_id: String,
    replacement_key: String,
) -> Result<RuntimeTaskRecoveryReplacement, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let replacement = database.supersede_runtime_task_for_recovery(
        &workspace_scope,
        task_id.trim(),
        replacement_key.trim(),
    )?;
    ticket_state.cancel_runtime_task_bindings(task_id.trim())?;
    Ok(replacement)
}

#[tauri::command]
pub fn bind_runtime_task_recovery_replacement(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
    replacement_task_id: String,
    replacement_key: String,
) -> Result<RuntimeTaskRecoveryReplacement, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.bind_runtime_task_recovery_replacement(
        &workspace_scope,
        task_id.trim(),
        replacement_task_id.trim(),
        replacement_key.trim(),
    )
}

#[tauri::command]
pub fn resolve_runtime_task_recovery(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
    resolution: String,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.resolve_runtime_task_recovery(&workspace_scope, task_id.trim(), resolution.trim())
}

#[tauri::command]
pub fn upsert_inbound_content_record(
    database: State<'_, RuntimeDatabase>,
    record: InboundContentRecordInput,
) -> Result<InboundContentRecordReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.upsert_inbound_content_record(&workspace_scope, &record)
}

#[tauri::command]
pub fn load_workspace_snapshot(
    database: State<'_, RuntimeDatabase>,
) -> Result<Option<WorkspaceSnapshot>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_workspace_snapshot")
        .with_threshold(database.config.slow_query_threshold_ms);

    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let scoped = connection
        .query_row(
            "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
            [&workspace_scope],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取本地工作区：{error}"))?;
    if let Some(payload) = scoped {
        let mut snapshot = serde_json::from_str::<WorkspaceSnapshot>(&payload)
            .map_err(|error| format!("本地工作区快照损坏：{error}"))?;
        // Conversation messages are loaded through list_workspace_messages_page.
        // Keeping them out of this shell prevents startup from re-materializing
        // an unbounded history as one IPC response.
        snapshot.messages.clear();
        return Ok(Some(snapshot));
    }
    let legacy_claimed = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='legacy_workspace_claimed_by'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取旧工作区归属：{error}"))?;
    if legacy_claimed.is_some() {
        return Ok(None);
    }
    let selected_task_id = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='selected_task_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取当前任务：{error}"))?;
    let client_state = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='client_state'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取客户端工作区状态：{error}"))?
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let has_records: i64 = connection
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM tasks) +
               (SELECT COUNT(*) FROM approvals) +
               (SELECT COUNT(*) FROM secretary_messages)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法检查工作区快照：{error}"))?;
    if selected_task_id.is_none() && has_records == 0 {
        return Ok(None);
    }
    let mut legacy = WorkspaceSnapshot {
        tasks: read_payloads(&connection, "SELECT payload FROM tasks ORDER BY updated_at", None)?,
        messages: read_payloads(
            &connection,
            "SELECT payload FROM secretary_messages ORDER BY created_at",
            None,
        )?,
        approvals: read_payloads(
            &connection,
            "SELECT payload FROM approvals ORDER BY updated_at",
            None,
        )?,
        operation_logs: read_payloads(
            &connection,
            "SELECT payload FROM operation_events WHERE event_type='workspace.operation' ORDER BY created_at DESC LIMIT ?1",
            Some(1000),
        )?,
        selected_task_id: selected_task_id.unwrap_or_default(),
        client_state,
    };
    let legacy_messages = std::mem::take(&mut legacy.messages);
    upsert_workspace_message_rows(
        &connection,
        &workspace_scope,
        &legacy_messages,
        Some("legacy-migration"),
    )?;
    let payload =
        serde_json::to_string(&legacy).map_err(|error| format!("无法迁移旧工作区：{error}"))?;
    connection
        .execute(
            "INSERT INTO workspace_snapshots (workspace_scope, payload, updated_at) VALUES (?1, ?2, ?3)",
            params![workspace_scope, payload, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法保存迁移后的本地工作区：{error}"))?;
    connection
        .execute(
            "INSERT INTO workspace_state (key, value, updated_at)
             VALUES ('legacy_workspace_claimed_by', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![workspace_scope, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法登记旧工作区归属：{error}"))?;
    Ok(Some(legacy))
}

#[tauri::command]
pub fn load_application_authorization(
    database: State<'_, RuntimeDatabase>,
) -> Result<ApplicationAuthorizationState, String> {
    database.application_authorization()
}

#[tauri::command]
pub fn database_health(database: State<'_, RuntimeDatabase>) -> Result<DatabaseHealth, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.health(&workspace_scope)
}

#[tauri::command]
pub fn backup_local_database(
    database: State<'_, RuntimeDatabase>,
) -> Result<DatabaseBackupResult, String> {
    database.backup()
}

#[tauri::command]
pub fn list_database_backups(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<DatabaseBackupInfo>, String> {
    database.list_backups()
}

#[tauri::command]
pub fn preflight_database_restore(
    database: State<'_, RuntimeDatabase>,
    backup_path: String,
) -> Result<DatabaseRestorePreflight, String> {
    database.preflight_restore(&backup_path)
}

#[tauri::command]
pub fn restore_local_database(
    database: State<'_, RuntimeDatabase>,
    backup_path: String,
) -> Result<DatabaseRestoreResult, String> {
    database.restore(&backup_path)
}

#[tauri::command]
pub fn query_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    query: Option<String>,
    include_inactive: Option<bool>,
    limit: Option<usize>,
) -> Result<Vec<LongTermMemoryRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.query_long_term_memory(
        &workspace_scope,
        query.as_deref().unwrap_or(""),
        include_inactive.unwrap_or(false),
        limit.unwrap_or(100),
    )
}

#[tauri::command]
pub fn govern_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    input: LongTermMemoryGovernanceInput,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.govern_long_term_memory(&workspace_scope, &input)
}

#[tauri::command]
pub fn export_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    include_inactive: Option<bool>,
) -> Result<String, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let records = database.query_long_term_memory(
        &workspace_scope,
        "",
        include_inactive.unwrap_or(true),
        1000,
    )?;
    serde_json::to_string_pretty(&serde_json::json!({
        "format": "yunspire-long-term-memory-v1",
        "exportedAt": Utc::now().to_rfc3339(),
        "records": records,
    }))
    .map_err(|error| format!("无法导出长期记忆：{error}"))
}

#[tauri::command]
pub fn long_term_memory_metrics(
    database: State<'_, RuntimeDatabase>,
) -> Result<LongTermMemoryMetrics, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.long_term_memory_metrics(&workspace_scope)
}

#[tauri::command]
pub fn read_optimization_evidence(
    database: State<'_, RuntimeDatabase>,
    limit: Option<usize>,
) -> Result<OptimizationEvidenceBatch, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.optimization_evidence(&workspace_scope, limit.unwrap_or(240))
}

pub(crate) fn validate_optimization_runtime_handler(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: &OperationContext,
) -> Result<RuntimeEffectfulHandlerAuthorization, String> {
    database.validate_runtime_effectful_handler(
        ticket_state,
        workspace_scope,
        operation_context,
        OPTIMIZATION_RUNTIME_CAPABILITY_ID,
        OPTIMIZATION_RUNTIME_OPERATION,
    )
}

pub(crate) fn record_optimization_runtime_handler_completion(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: &OperationContext,
    mutation_key: &RuntimeEffectMutationKey,
    handler_started: Instant,
) -> Result<(), String> {
    database
        .record_runtime_effectful_handler_completion_once(
            ticket_state,
            workspace_scope,
            operation_context,
            OPTIMIZATION_RUNTIME_CAPABILITY_ID,
            OPTIMIZATION_RUNTIME_OPERATION,
            &mutation_key.completion_key(),
            TrustedHandlerUsage {
                tool_calls: 1,
                runtime_seconds: handler_started.elapsed().as_secs().max(1),
                tokens: 0,
                cost: Some(0.0),
            },
        )
        .map(|_| ())
}

#[tauri::command]
pub fn create_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: OptimizationCandidateInput,
    operation_context: OperationContext,
) -> Result<OptimizationCandidateResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let authorization = validate_optimization_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
    )?;
    let request =
        serde_json::to_value(&input).map_err(|error| format!("无法序列化优化候选请求：{error}"))?;
    let mutation_key =
        runtime_effect_mutation_key(&authorization, "optimization.create_candidate", &request)?;
    let result =
        database.create_optimization_candidate(&workspace_scope, input, Some(&mutation_key))?;
    record_optimization_runtime_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        &mutation_key,
        handler_started,
    )?;
    Ok(result)
}

#[tauri::command]
pub fn evaluate_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    candidate_id: String,
    operation_context: OperationContext,
) -> Result<OptimizationEvaluationResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let authorization = validate_optimization_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
    )?;
    let candidate_id = candidate_id.trim();
    let mutation_key = runtime_effect_mutation_key(
        &authorization,
        "optimization.evaluate_candidate",
        &serde_json::json!({ "candidateId": candidate_id }),
    )?;
    let result = database.evaluate_optimization_candidate(
        &workspace_scope,
        candidate_id,
        Some(&mutation_key),
    )?;
    record_optimization_runtime_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        &mutation_key,
        handler_started,
    )?;
    Ok(result)
}

#[tauri::command]
pub fn get_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    candidate_id: String,
) -> Result<Option<OptimizationCandidateResult>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.optimization_candidate(&workspace_scope, candidate_id.trim())
}

#[tauri::command]
pub fn load_optimization_profile(
    database: State<'_, RuntimeDatabase>,
) -> Result<OptimizationProfileResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.load_optimization_profile(&workspace_scope)
}

#[tauri::command]
pub fn apply_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    candidate_id: String,
    operation_context: OperationContext,
) -> Result<OptimizationProfileResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let authorization = validate_optimization_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
    )?;
    let candidate_id = candidate_id.trim();
    let mutation_key = runtime_effect_mutation_key(
        &authorization,
        "optimization.apply_candidate",
        &serde_json::json!({ "candidateId": candidate_id }),
    )?;
    let profile = database.apply_optimization_candidate(
        &workspace_scope,
        candidate_id,
        Some(&mutation_key),
    )?;
    record_optimization_runtime_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        &mutation_key,
        handler_started,
    )?;
    Ok(profile)
}

#[tauri::command]
pub fn rollback_optimization_profile(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    target_version: Option<i64>,
    operation_context: OperationContext,
) -> Result<OptimizationProfileResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let authorization = validate_optimization_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
    )?;
    let mutation_key = runtime_effect_mutation_key(
        &authorization,
        "optimization.rollback_profile",
        &serde_json::json!({ "targetVersion": target_version }),
    )?;
    let profile = database.rollback_optimization_profile(
        &workspace_scope,
        target_version,
        Some(&mutation_key),
    )?;
    record_optimization_runtime_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        &mutation_key,
        handler_started,
    )?;
    Ok(profile)
}

#[tauri::command]
pub fn list_optimization_versions(
    database: State<'_, RuntimeDatabase>,
    limit: Option<usize>,
) -> Result<Vec<OptimizationVersion>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_optimization_versions(&workspace_scope, limit.unwrap_or(30))
}

#[tauri::command]
pub fn poll_due_runtime_schedules(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<DueRuntimeSchedule>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.claim_due_runtime_schedules(&workspace_scope, 32)
}

fn indexed_search_candidate_signals(
    candidate: &IndexedSearchCandidate,
    normalized_query: &str,
    now: &chrono::DateTime<Utc>,
) -> (f64, f64, f64) {
    let title = candidate.title.nfc().collect::<String>().to_lowercase();
    let path = candidate
        .relative_path
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let tags = candidate.tags.join(" ").to_lowercase();
    let links = candidate.wiki_links.join(" ").to_lowercase();
    let title_path_bonus = if title == normalized_query {
        12.0
    } else if title.contains(normalized_query) {
        8.0
    } else if path.contains(normalized_query) {
        6.0
    } else {
        0.0
    };
    let relation_bonus = if tags.contains(normalized_query) {
        4.0
    } else {
        0.0
    } + if links.contains(normalized_query) {
        3.0
    } else {
        0.0
    };
    let recency_bonus = chrono::DateTime::parse_from_rfc3339(&candidate.modified_at)
        .ok()
        .map(|modified| {
            let age_days = now
                .signed_duration_since(modified.with_timezone(&Utc))
                .num_days()
                .max(0) as f64;
            1.0 / (1.0 + age_days / 30.0)
        })
        .unwrap_or(0.0);
    (title_path_bonus, relation_bonus, recency_bonus)
}

fn cached_neural_embedding_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    provider_id: &str,
    model: &str,
    input_hash: &str,
) -> Result<Option<Vec<f32>>, String> {
    let cached = connection
        .query_row(
            "SELECT dimensions, vector_blob FROM neural_embedding_cache
             WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND input_hash=?4",
            params![workspace_scope, provider_id, model, input_hash],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取神经 Embedding 缓存：{error}"))?;
    Ok(cached.and_then(|(dimensions, blob)| decode_neural_embedding(dimensions, &blob)))
}

fn load_cached_neural_embedding(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    input_hash: &str,
) -> Result<Option<Vec<f32>>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    cached_neural_embedding_in_connection(
        &connection,
        workspace_scope,
        &configured.provider_id,
        &configured.model,
        input_hash,
    )
}

fn persist_neural_embedding_and_bindings(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    input_hash: &str,
    vector: Option<Vec<f32>>,
    notes: &[NeuralEmbeddingNoteInput],
) -> Result<(), String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始神经 Embedding 缓存事务：{error}"))?;
    let now = Utc::now().to_rfc3339();
    if let Some(vector) = vector {
        let (dimensions, vector_blob) = encode_neural_embedding(vector)?;
        transaction
            .execute(
                "INSERT INTO neural_embedding_cache
                 (workspace_scope, provider_id, model, input_hash, dimensions, vector_blob,
                  created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(workspace_scope, provider_id, model, input_hash) DO UPDATE SET
                   dimensions=excluded.dimensions,
                   vector_blob=excluded.vector_blob,
                   last_used_at=excluded.last_used_at",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    input_hash,
                    dimensions,
                    vector_blob,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存神经 Embedding 缓存：{error}"))?;
    } else {
        transaction
            .execute(
                "UPDATE neural_embedding_cache SET last_used_at=?5
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND input_hash=?4",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    input_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法更新神经 Embedding 缓存访问时间：{error}"))?;
    }
    for note in notes {
        transaction
            .execute(
                "INSERT INTO note_neural_embeddings
                 (workspace_scope, provider_id, model, vault_id, relative_path, content_hash,
                  input_hash, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(workspace_scope, provider_id, model, vault_id, relative_path)
                 DO UPDATE SET content_hash=excluded.content_hash,
                               input_hash=excluded.input_hash,
                               updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    note.vault_id,
                    note.relative_path,
                    note.content_hash,
                    input_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法绑定笔记神经 Embedding：{error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交神经 Embedding 缓存：{error}"))
}

fn load_missing_neural_embedding_inputs(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
    limit: usize,
) -> Result<Vec<NeuralEmbeddingNoteInput>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_missing_neural_embedding_inputs")
        .with_threshold(database.config.slow_query_threshold_ms);

    let scoped = vault_id.filter(|value| *value != "all");
    let sql = if scoped.is_some() {
        "SELECT i.vault_id, i.relative_path, i.title, i.content_hash,
                i.tags_json, i.wiki_links_json,
                COALESCE((
                  SELECT f.content FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), '')
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         LEFT JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         WHERE i.vault_id=?4 AND (
           e.relative_path IS NULL OR c.input_hash IS NULL
           OR c.dimensions <= 0 OR c.dimensions > 65536
           OR length(c.vector_blob) != c.dimensions * 4
         )
         ORDER BY i.modified_at DESC, i.relative_path
         LIMIT ?5"
    } else {
        "SELECT i.vault_id, i.relative_path, i.title, i.content_hash,
                i.tags_json, i.wiki_links_json,
                COALESCE((
                  SELECT f.content FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), '')
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         LEFT JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         WHERE e.relative_path IS NULL OR c.input_hash IS NULL
           OR c.dimensions <= 0 OR c.dimensions > 65536
           OR length(c.vector_blob) != c.dimensions * 4
         ORDER BY i.modified_at DESC, i.vault_id, i.relative_path
         LIMIT ?4"
    };
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备神经 Embedding 缺口查询：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let vault_id = row.get::<_, String>(0)?;
        let relative_path = row.get::<_, String>(1)?;
        let title = row.get::<_, String>(2)?;
        let content_hash = row.get::<_, String>(3)?;
        let tags_json = row.get::<_, String>(4)?;
        let wiki_links_json = row.get::<_, String>(5)?;
        let content = row.get::<_, String>(6)?;
        let input = neural_note_embedding_input(
            &relative_path,
            &title,
            &tags_json,
            &wiki_links_json,
            &content,
        );
        Ok(NeuralEmbeddingNoteInput {
            vault_id,
            relative_path,
            content_hash,
            input_hash: neural_embedding_input_hash(&input),
            input,
        })
    };
    let inputs = if let Some(vault_id) = scoped {
        statement
            .query_map(
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    vault_id,
                    limit.clamp(1, MAX_NEURAL_EMBEDDING_REFRESH_NOTES) as i64,
                ],
                map_row,
            )
            .map_err(|error| format!("无法读取神经 Embedding 缺口：{error}"))?
            .collect::<Result<Vec<_>, _>>()
    } else {
        statement
            .query_map(
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    limit.clamp(1, MAX_NEURAL_EMBEDDING_REFRESH_NOTES) as i64,
                ],
                map_row,
            )
            .map_err(|error| format!("无法读取神经 Embedding 缺口：{error}"))?
            .collect::<Result<Vec<_>, _>>()
    }
    .map_err(|error| format!("无法解析神经 Embedding 缺口：{error}"))?;
    Ok(inputs)
}

fn update_neural_embedding_index_state(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
    last_error: Option<&str>,
) -> Result<String, String> {
    let scoped = vault_id.filter(|value| *value != "all");
    let sql = if scoped.is_some() {
        "SELECT i.vault_id, COUNT(*),
                SUM(CASE WHEN e.relative_path IS NOT NULL AND c.input_hash IS NOT NULL THEN 1 ELSE 0 END)
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         LEFT JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         WHERE i.vault_id=?4
         GROUP BY i.vault_id"
    } else {
        "SELECT i.vault_id, COUNT(*),
                SUM(CASE WHEN e.relative_path IS NOT NULL AND c.input_hash IS NOT NULL THEN 1 ELSE 0 END)
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         LEFT JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         GROUP BY i.vault_id"
    };
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let rows = {
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备神经 Embedding 索引状态查询：{error}"))?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        };
        if let Some(vault_id) = scoped {
            statement
                .query_map(
                    params![
                        workspace_scope,
                        configured.provider_id,
                        configured.model,
                        vault_id,
                    ],
                    map_row,
                )
                .map_err(|error| format!("无法读取神经 Embedding 索引状态：{error}"))?
                .collect::<Result<Vec<_>, _>>()
        } else {
            statement
                .query_map(
                    params![workspace_scope, configured.provider_id, configured.model],
                    map_row,
                )
                .map_err(|error| format!("无法读取神经 Embedding 索引状态：{error}"))?
                .collect::<Result<Vec<_>, _>>()
        }
        .map_err(|error| format!("无法解析神经 Embedding 索引状态：{error}"))?
    };
    let rows = if rows.is_empty() {
        scoped
            .map(|vault_id| vec![(vault_id.to_string(), 0_i64, 0_i64)])
            .unwrap_or_default()
    } else {
        rows
    };
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始神经 Embedding 索引状态事务：{error}"))?;
    let now = Utc::now().to_rfc3339();
    let error = last_error.map(|value| value.chars().take(1_000).collect::<String>());
    let mut aggregate = "ready".to_string();
    for (vault_id, total_notes, indexed_notes) in rows {
        let state = if error.is_some() {
            if indexed_notes > 0 {
                "degraded"
            } else {
                "failed"
            }
        } else if indexed_notes >= total_notes {
            "ready"
        } else if indexed_notes > 0 {
            "building"
        } else {
            "pending"
        };
        if matches!(state, "failed" | "degraded") || (aggregate == "ready" && state != "ready") {
            aggregate = state.to_string();
        }
        transaction
            .execute(
                "INSERT INTO neural_embedding_index_state
                 (workspace_scope, provider_id, model, vault_id, state, total_notes,
                  indexed_notes, last_error, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(workspace_scope, provider_id, model, vault_id) DO UPDATE SET
                   state=excluded.state,
                   total_notes=excluded.total_notes,
                   indexed_notes=excluded.indexed_notes,
                   last_error=excluded.last_error,
                   updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    vault_id,
                    state,
                    total_notes,
                    indexed_notes,
                    error,
                    now,
                ],
            )
            .map_err(|db_error| format!("无法保存神经 Embedding 索引状态：{db_error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交神经 Embedding 索引状态：{error}"))?;
    Ok(aggregate)
}

async fn refresh_neural_embedding_notes(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
    limit: usize,
) -> Result<NeuralEmbeddingRefreshOutcome, String> {
    let missing = load_missing_neural_embedding_inputs(
        database,
        configured,
        workspace_scope,
        vault_id,
        limit,
    )?;
    let mut outcome = NeuralEmbeddingRefreshOutcome {
        loaded_notes: missing.len(),
        ..NeuralEmbeddingRefreshOutcome::default()
    };
    let mut pending = HashMap::<String, (String, Vec<NeuralEmbeddingNoteInput>)>::new();
    for note in missing {
        if load_cached_neural_embedding(database, configured, workspace_scope, &note.input_hash)?
            .is_some()
        {
            persist_neural_embedding_and_bindings(
                database,
                configured,
                workspace_scope,
                &note.input_hash,
                None,
                std::slice::from_ref(&note),
            )?;
            outcome.indexed_notes += 1;
        } else {
            let entry = pending
                .entry(note.input_hash.clone())
                .or_insert_with(|| (note.input.clone(), Vec::new()));
            entry.1.push(note);
        }
    }

    let mut pending = pending.into_iter().collect::<Vec<_>>();
    pending.sort_by(|left, right| left.0.cmp(&right.0));
    let mut batch_start = 0;
    while batch_start < pending.len() {
        let mut batch_end = batch_start;
        let mut batch_characters = 0_usize;
        while batch_end < pending.len() && batch_end - batch_start < NEURAL_EMBEDDING_BATCH_SIZE {
            let (_, (input, _)) = &pending[batch_end];
            let input_characters = input.chars().count();
            if batch_end > batch_start
                && batch_characters.saturating_add(input_characters)
                    > crate::model_provider::MAX_EMBEDDING_TOTAL_CHARS
            {
                break;
            }
            batch_characters = batch_characters.saturating_add(input_characters);
            batch_end += 1;
        }
        let chunk = &pending[batch_start..batch_end];
        let inputs = chunk
            .iter()
            .map(|(_, (input, _))| input.clone())
            .collect::<Vec<_>>();
        let vectors =
            match request_embeddings_with_usage(database, configured, &inputs, "embedding.index")
                .await
            {
                Ok(vectors) => vectors,
                Err(error) => {
                    outcome.error = Some(error);
                    break;
                }
            };
        for ((input_hash, (_, notes)), vector) in chunk.iter().zip(vectors) {
            persist_neural_embedding_and_bindings(
                database,
                configured,
                workspace_scope,
                input_hash,
                Some(vector),
                notes,
            )?;
            outcome.indexed_notes += notes.len();
        }
        batch_start = batch_end;
    }
    Ok(outcome)
}

async fn prepare_neural_search_context(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    vault_id: Option<&str>,
    query: &str,
) -> Result<Option<NeuralSearchContext>, String> {
    let Some(configured) =
        crate::model_provider::configured_embedding_model(database, workspace_scope)?
    else {
        return Ok(None);
    };
    let query_input = query.trim().nfc().collect::<String>();
    let query_hash = neural_embedding_input_hash(&query_input);
    let query_vector = if let Some(vector) =
        load_cached_neural_embedding(database, &configured, workspace_scope, &query_hash)?
    {
        persist_neural_embedding_and_bindings(
            database,
            &configured,
            workspace_scope,
            &query_hash,
            None,
            &[],
        )?;
        vector
    } else {
        let vectors = match request_embeddings_with_usage(
            database,
            &configured,
            std::slice::from_ref(&query_input),
            "embedding.search",
        )
        .await
        {
            Ok(vectors) => vectors,
            Err(error) => {
                let _ = update_neural_embedding_index_state(
                    database,
                    &configured,
                    workspace_scope,
                    vault_id,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let vector = vectors
            .into_iter()
            .next()
            .ok_or_else(|| "Embedding 查询响应为空".to_string())?;
        persist_neural_embedding_and_bindings(
            database,
            &configured,
            workspace_scope,
            &query_hash,
            Some(vector.clone()),
            &[],
        )?;
        vector
    };

    let refresh = refresh_neural_embedding_notes(
        database,
        &configured,
        workspace_scope,
        vault_id,
        MAX_NEURAL_EMBEDDING_REFRESH_NOTES,
    )
    .await?;
    let refresh_error = refresh.error;
    let index_state = update_neural_embedding_index_state(
        database,
        &configured,
        workspace_scope,
        vault_id,
        refresh_error.as_deref(),
    )?;
    if let Some(error) = refresh_error {
        log::warn!("神经 Embedding 索引补齐失败，继续使用已有向量与本地搜索：{error}");
    }
    Ok(Some(NeuralSearchContext {
        workspace_scope: workspace_scope.to_string(),
        provider_id: configured.provider_id,
        provider: configured.provider,
        model: configured.model,
        query_vector,
        index_state,
    }))
}

async fn request_embeddings_with_usage(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    inputs: &[String],
    operation: &str,
) -> Result<Vec<Vec<f32>>, String> {
    let request_id = format!("embedding-request-{}", Uuid::new_v4());
    let trace_id = crate::trace::new_trace_id();
    let prompt_tokens = inputs
        .iter()
        .map(|input| input.chars().count().div_ceil(4) as u64)
        .sum::<u64>();
    let started_at = Instant::now();
    let result = crate::model_provider::request_embeddings(configured, inputs).await;
    let duration_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let error = result.as_ref().err().map(String::as_str);
    if let Err(record_error) = database.record_model_usage(&ModelUsageRecord {
        request_id: &request_id,
        trace_id: &trace_id,
        operation,
        provider: &configured.provider,
        model: &configured.model,
        state: if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        },
        prompt_tokens,
        completion_tokens: 0,
        total_tokens: prompt_tokens,
        estimated_cost_usd: None,
        cost_source: "estimated_input_characters",
        duration_ms,
        error,
    }) {
        log::warn!("无法记录 Embedding 模型用量：{record_error}");
    }
    result
}

fn normalize_neural_embedding_vault_id(vault_id: Option<&str>) -> Result<Option<String>, String> {
    let Some(vault_id) = vault_id.map(str::trim) else {
        return Ok(None);
    };
    if vault_id.is_empty() || vault_id == "all" {
        return Ok(None);
    }
    if vault_id.chars().count() > 160 || vault_id.contains('\0') {
        return Err("Vault ID 无效或超过 160 个字符".to_string());
    }
    Ok(Some(vault_id.to_string()))
}

fn neural_embedding_state_priority(state: &str) -> u8 {
    match state {
        "failed" => 5,
        "degraded" => 4,
        "building" => 3,
        "pending" => 2,
        "ready" => 1,
        _ => 0,
    }
}

fn load_neural_embedding_index_status(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    vault_id: Option<&str>,
    configured: Option<&crate::model_provider::ConfiguredEmbeddingModel>,
    configuration_error: Option<String>,
) -> Result<NeuralEmbeddingIndexStatus, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_neural_embedding_index_status")
        .with_threshold(database.config.slow_query_threshold_ms);

    let scoped = normalize_neural_embedding_vault_id(vault_id)?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut vault_rows = {
        let sql = if scoped.is_some() {
            "SELECT v.id, COUNT(i.relative_path)
             FROM vault_registry v
             LEFT JOIN note_index i ON i.vault_id=v.id
             WHERE v.id=?1
             GROUP BY v.id ORDER BY v.id"
        } else {
            "SELECT v.id, COUNT(i.relative_path)
             FROM vault_registry v
             LEFT JOIN note_index i ON i.vault_id=v.id
             GROUP BY v.id ORDER BY v.id"
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备神经 Embedding 状态查询：{error}"))?;
        if let Some(vault_id) = scoped.as_deref() {
            statement
                .query_map([vault_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|error| format!("无法读取 Vault 神经 Embedding 状态：{error}"))?
                .collect::<Result<Vec<_>, _>>()
        } else {
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|error| format!("无法读取神经 Embedding 状态：{error}"))?
                .collect::<Result<Vec<_>, _>>()
        }
        .map_err(|error| format!("无法解析神经 Embedding 状态：{error}"))?
    };
    if vault_rows.is_empty() {
        if let Some(vault_id) = scoped.as_ref() {
            vault_rows.push((vault_id.clone(), 0));
        }
    }

    let mut vaults = Vec::with_capacity(vault_rows.len());
    for (vault_id, total_notes) in vault_rows {
        let (indexed_notes, stored_error, updated_at) = if let Some(configured) = configured {
            let indexed_notes = connection
                .query_row(
                    "SELECT COUNT(*)
                     FROM note_index i
                     JOIN note_neural_embeddings e
                       ON e.vault_id=i.vault_id AND e.relative_path=i.relative_path
                      AND e.content_hash=i.content_hash
                     JOIN neural_embedding_cache c
                       ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
                      AND c.model=e.model AND c.input_hash=e.input_hash
                     WHERE i.vault_id=?1 AND e.workspace_scope=?2 AND e.provider_id=?3
                       AND e.model=?4 AND c.dimensions > 0
                       AND length(c.vector_blob)=c.dimensions * 4",
                    params![
                        vault_id,
                        workspace_scope,
                        configured.provider_id,
                        configured.model,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法统计已索引神经 Embedding：{error}"))?;
            let stored = connection
                .query_row(
                    "SELECT last_error, updated_at FROM neural_embedding_index_state
                     WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND vault_id=?4",
                    params![
                        workspace_scope,
                        configured.provider_id,
                        configured.model,
                        vault_id,
                    ],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("无法读取神经 Embedding 索引状态记录：{error}"))?;
            let (stored_error, updated_at) = stored
                .map(|(error, updated_at)| (error, Some(updated_at)))
                .unwrap_or((None, None));
            (indexed_notes, stored_error, updated_at)
        } else {
            (0, configuration_error.clone(), None)
        };
        let pending_notes = total_notes.saturating_sub(indexed_notes);
        let state = if configured.is_none() {
            "unconfigured"
        } else if stored_error.is_some() {
            if indexed_notes > 0 {
                "degraded"
            } else {
                "failed"
            }
        } else if pending_notes == 0 {
            "ready"
        } else if indexed_notes > 0 {
            "building"
        } else {
            "pending"
        };
        vaults.push(NeuralEmbeddingVaultIndexStatus {
            vault_id,
            state: state.to_string(),
            total_notes,
            indexed_notes,
            pending_notes,
            last_error: stored_error,
            updated_at,
        });
    }

    let cache_entries = if let Some(configured) = configured {
        if let Some(vault_id) = scoped.as_deref() {
            connection
                .query_row(
                    "SELECT COUNT(DISTINCT c.input_hash)
                     FROM neural_embedding_cache c
                     JOIN note_neural_embeddings e
                       ON e.workspace_scope=c.workspace_scope AND e.provider_id=c.provider_id
                      AND e.model=c.model AND e.input_hash=c.input_hash
                     WHERE c.workspace_scope=?1 AND c.provider_id=?2 AND c.model=?3
                       AND e.vault_id=?4",
                    params![
                        workspace_scope,
                        configured.provider_id,
                        configured.model,
                        vault_id,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法统计 Vault 神经 Embedding 缓存：{error}"))?
        } else {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM neural_embedding_cache
                     WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3",
                    params![workspace_scope, configured.provider_id, configured.model,],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法统计神经 Embedding 缓存：{error}"))?
        }
    } else {
        0
    };
    drop(connection);

    let total_notes = vaults.iter().map(|vault| vault.total_notes).sum();
    let indexed_notes = vaults.iter().map(|vault| vault.indexed_notes).sum();
    let pending_notes = vaults.iter().map(|vault| vault.pending_notes).sum();
    let state = if configured.is_none() {
        "unconfigured".to_string()
    } else {
        vaults
            .iter()
            .max_by_key(|vault| neural_embedding_state_priority(&vault.state))
            .map(|vault| vault.state.clone())
            .unwrap_or_else(|| "ready".to_string())
    };
    let last_error = if configured.is_none() {
        configuration_error
    } else {
        vaults
            .iter()
            .filter_map(|vault| {
                vault
                    .last_error
                    .as_ref()
                    .map(|error| (neural_embedding_state_priority(&vault.state), error.clone()))
            })
            .max_by_key(|(priority, _)| *priority)
            .map(|(_, error)| error)
    };
    let updated_at = vaults
        .iter()
        .filter_map(|vault| vault.updated_at.clone())
        .max();
    Ok(NeuralEmbeddingIndexStatus {
        workspace_scope: workspace_scope.to_string(),
        vault_id: scoped,
        configured: configured.is_some(),
        provider_id: configured.map(|value| value.provider_id.clone()),
        provider: configured.map(|value| value.provider.clone()),
        model: configured.map(|value| value.model.clone()),
        state,
        total_notes,
        indexed_notes,
        pending_notes,
        cache_entries,
        last_error,
        updated_at,
        vaults,
    })
}

fn reset_neural_embedding_index(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
) -> Result<(), String> {
    let scoped = normalize_neural_embedding_vault_id(vault_id)?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始神经 Embedding 重建事务：{error}"))?;
    if let Some(vault_id) = scoped.as_deref() {
        transaction
            .execute(
                "DELETE FROM note_neural_embeddings
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND vault_id=?4",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    vault_id,
                ],
            )
            .map_err(|error| format!("无法清理 Vault 神经 Embedding 绑定：{error}"))?;
        transaction
            .execute(
                "DELETE FROM neural_embedding_index_state
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND vault_id=?4",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    vault_id,
                ],
            )
            .map_err(|error| format!("无法清理 Vault 神经 Embedding 状态：{error}"))?;
        transaction
            .execute(
                "DELETE FROM neural_embedding_cache
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3
                   AND NOT EXISTS (
                     SELECT 1 FROM note_neural_embeddings e
                     WHERE e.workspace_scope=neural_embedding_cache.workspace_scope
                       AND e.provider_id=neural_embedding_cache.provider_id
                       AND e.model=neural_embedding_cache.model
                       AND e.input_hash=neural_embedding_cache.input_hash
                   )",
                params![workspace_scope, configured.provider_id, configured.model,],
            )
            .map_err(|error| format!("无法回收未引用的神经 Embedding 缓存：{error}"))?;
    } else {
        transaction
            .execute(
                "DELETE FROM note_neural_embeddings
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3",
                params![workspace_scope, configured.provider_id, configured.model,],
            )
            .map_err(|error| format!("无法清理神经 Embedding 绑定：{error}"))?;
        transaction
            .execute(
                "DELETE FROM neural_embedding_cache
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3",
                params![workspace_scope, configured.provider_id, configured.model,],
            )
            .map_err(|error| format!("无法清理神经 Embedding 缓存：{error}"))?;
        transaction
            .execute(
                "DELETE FROM neural_embedding_index_state
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3",
                params![workspace_scope, configured.provider_id, configured.model,],
            )
            .map_err(|error| format!("无法清理神经 Embedding 状态：{error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交神经 Embedding 重建清理：{error}"))?;
    drop(connection);
    update_neural_embedding_index_state(
        database,
        configured,
        workspace_scope,
        scoped.as_deref(),
        None,
    )?;
    Ok(())
}

fn get_neural_embedding_index_status_inner(
    database: &RuntimeDatabase,
    vault_id: Option<&str>,
) -> Result<NeuralEmbeddingIndexStatus, String> {
    let workspace_scope = database.local_workspace_scope()?;
    match crate::model_provider::configured_embedding_model(database, &workspace_scope) {
        Ok(configured) => load_neural_embedding_index_status(
            database,
            &workspace_scope,
            vault_id,
            configured.as_ref(),
            None,
        ),
        Err(error) => load_neural_embedding_index_status(
            database,
            &workspace_scope,
            vault_id,
            None,
            Some(error),
        ),
    }
}

async fn rebuild_neural_embedding_index_inner(
    database: &RuntimeDatabase,
    vault_id: Option<&str>,
) -> Result<NeuralEmbeddingIndexStatus, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let configured =
        match crate::model_provider::configured_embedding_model(database, &workspace_scope) {
            Ok(Some(configured)) => configured,
            Ok(None) => {
                return load_neural_embedding_index_status(
                    database,
                    &workspace_scope,
                    vault_id,
                    None,
                    None,
                );
            }
            Err(error) => {
                return load_neural_embedding_index_status(
                    database,
                    &workspace_scope,
                    vault_id,
                    None,
                    Some(error),
                );
            }
        };
    let scoped = normalize_neural_embedding_vault_id(vault_id)?;
    reset_neural_embedding_index(database, &configured, &workspace_scope, scoped.as_deref())?;

    let mut rebuild_error = None;
    loop {
        let refresh = refresh_neural_embedding_notes(
            database,
            &configured,
            &workspace_scope,
            scoped.as_deref(),
            MAX_NEURAL_EMBEDDING_REFRESH_NOTES,
        )
        .await?;
        if let Some(error) = refresh.error {
            rebuild_error = Some(error);
            break;
        }
        if refresh.loaded_notes == 0 {
            break;
        }
        if refresh.indexed_notes == 0 {
            rebuild_error =
                Some("神经 Embedding 重建未能推进，已停止并保留本地搜索回退".to_string());
            break;
        }
        update_neural_embedding_index_state(
            database,
            &configured,
            &workspace_scope,
            scoped.as_deref(),
            None,
        )?;
    }
    update_neural_embedding_index_state(
        database,
        &configured,
        &workspace_scope,
        scoped.as_deref(),
        rebuild_error.as_deref(),
    )?;
    if let Some(error) = rebuild_error {
        log::warn!("手动重建神经 Embedding 索引未完整完成，继续使用本地搜索回退：{error}");
    }
    load_neural_embedding_index_status(
        database,
        &workspace_scope,
        scoped.as_deref(),
        Some(&configured),
        None,
    )
}

fn load_lexical_search_candidates(
    connection: &Connection,
    scoped: Option<&str>,
    query: &str,
    candidate_limit: i64,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_lexical_search_candidates")
        .with_threshold(100);

    let lexical_match_query = lexical_fts_match_query(query)?;
    let legacy_match_query = fts_match_query(query)?;
    let lexical_sql = if scoped.is_some() {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_lexical_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_lexical_fts), i.tags_json, i.wiki_links_json
         FROM note_lexical_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_lexical_fts MATCH ?1 AND f.vault_id=?2
         ORDER BY bm25(note_lexical_fts) LIMIT ?3"
    } else {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_lexical_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_lexical_fts), i.tags_json, i.wiki_links_json
         FROM note_lexical_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_lexical_fts MATCH ?1
         ORDER BY bm25(note_lexical_fts) LIMIT ?2"
    };
    let mut statement = connection
        .prepare(lexical_sql)
        .map_err(|error| format!("无法准备中文混合搜索：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let tags_json = row.get::<_, String>(6)?;
        let wiki_links_json = row.get::<_, String>(7)?;
        Ok(IndexedSearchCandidate {
            vault_id: row.get(0)?,
            relative_path: row.get(1)?,
            title: row.get(2)?,
            excerpt: row.get(3)?,
            modified_at: row.get(4)?,
            lexical_score: Some(row.get(5)?),
            vector_similarity: None,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            wiki_links: serde_json::from_str(&wiki_links_json).unwrap_or_default(),
        })
    };
    let mut candidates = if let Some(vault_id) = scoped {
        statement
            .query_map(
                params![lexical_match_query, vault_id, candidate_limit],
                map_row,
            )
            .map_err(|error| format!("中文混合搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    } else {
        statement
            .query_map(params![lexical_match_query, candidate_limit], map_row)
            .map_err(|error| format!("中文混合搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    };
    drop(statement);

    // Existing installations may search before the startup reindex has populated the new
    // lexical table. Keep the original FTS index as a temporary, read-only fallback.
    if candidates.is_empty() {
        let legacy_sql = if scoped.is_some() {
            "SELECT f.vault_id, f.relative_path, f.title,
                    snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                    bm25(note_fts), i.tags_json, i.wiki_links_json
             FROM note_fts f
             JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
             WHERE note_fts MATCH ?1 AND f.vault_id=?2
             ORDER BY bm25(note_fts) LIMIT ?3"
        } else {
            "SELECT f.vault_id, f.relative_path, f.title,
                    snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                    bm25(note_fts), i.tags_json, i.wiki_links_json
             FROM note_fts f
             JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
             WHERE note_fts MATCH ?1
             ORDER BY bm25(note_fts) LIMIT ?2"
        };
        let mut legacy = connection
            .prepare(legacy_sql)
            .map_err(|error| format!("无法准备兼容全文搜索：{error}"))?;
        candidates = if let Some(vault_id) = scoped {
            legacy
                .query_map(
                    params![legacy_match_query, vault_id, candidate_limit],
                    map_row,
                )
                .map_err(|error| format!("兼容全文搜索失败：{error}"))?
                .filter_map(Result::ok)
                .collect()
        } else {
            legacy
                .query_map(params![legacy_match_query, candidate_limit], map_row)
                .map_err(|error| format!("兼容全文搜索失败：{error}"))?
                .filter_map(Result::ok)
                .collect()
        };
    }
    Ok(candidates)
}

fn load_vector_search_candidates(
    connection: &Connection,
    scoped: Option<&str>,
    query: &str,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_vector_search_candidates")
        .with_threshold(100);

    let Some(query_vector) = query_local_feature_vector(query) else {
        return Ok(Vec::new());
    };
    let vector_sql = if scoped.is_some() {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                v.representation_version, v.dimensions, v.vector_blob
         FROM note_feature_vectors v
         JOIN note_index i
           ON i.vault_id=v.vault_id AND i.relative_path=v.relative_path
          AND i.content_hash=v.content_hash
         WHERE v.vault_id=?1"
    } else {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                v.representation_version, v.dimensions, v.vector_blob
         FROM note_feature_vectors v
         JOIN note_index i
           ON i.vault_id=v.vault_id AND i.relative_path=v.relative_path
          AND i.content_hash=v.content_hash"
    };
    let mut statement = connection
        .prepare(vector_sql)
        .map_err(|error| format!("无法准备本地特征向量搜索：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let vault_id = row.get::<_, String>(0)?;
        let relative_path = row.get::<_, String>(1)?;
        let title = row.get::<_, String>(2)?;
        let excerpt = row.get::<_, String>(3)?;
        let modified_at = row.get::<_, String>(4)?;
        let tags_json = row.get::<_, String>(5)?;
        let wiki_links_json = row.get::<_, String>(6)?;
        let version = row.get::<_, i64>(7)?;
        let dimensions = row.get::<_, i64>(8)?;
        let blob = row.get::<_, Vec<u8>>(9)?;
        let similarity = decode_local_feature_vector(version, dimensions, &blob)
            .and_then(|candidate| local_vector_similarity(&query_vector, &candidate));
        Ok(similarity
            .filter(|score| *score >= MIN_LOCAL_VECTOR_SIMILARITY)
            .map(|score| IndexedSearchCandidate {
                vault_id,
                relative_path,
                title,
                excerpt,
                modified_at,
                lexical_score: None,
                vector_similarity: Some(score),
                tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                wiki_links: serde_json::from_str(&wiki_links_json).unwrap_or_default(),
            }))
    };
    let candidates = if let Some(vault_id) = scoped {
        statement
            .query_map([vault_id], map_row)
            .map_err(|error| format!("本地特征向量搜索失败：{error}"))?
            .filter_map(Result::ok)
            .flatten()
            .collect()
    } else {
        statement
            .query_map([], map_row)
            .map_err(|error| format!("本地特征向量搜索失败：{error}"))?
            .filter_map(Result::ok)
            .flatten()
            .collect()
    };
    Ok(candidates)
}

fn load_neural_search_candidates(
    connection: &Connection,
    scoped: Option<&str>,
    context: &NeuralSearchContext,
) -> Result<(Vec<IndexedSearchCandidate>, bool), String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("load_neural_search_candidates")
        .with_threshold(100);

    let sql = if scoped.is_some() {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                c.dimensions, c.vector_blob, c.input_hash
         FROM note_neural_embeddings e
         JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         JOIN note_index i
           ON i.vault_id=e.vault_id AND i.relative_path=e.relative_path
          AND i.content_hash=e.content_hash
         WHERE e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3 AND e.vault_id=?4"
    } else {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                c.dimensions, c.vector_blob, c.input_hash
         FROM note_neural_embeddings e
         JOIN neural_embedding_cache c
           ON c.workspace_scope=e.workspace_scope AND c.provider_id=e.provider_id
          AND c.model=e.model AND c.input_hash=e.input_hash
         JOIN note_index i
           ON i.vault_id=e.vault_id AND i.relative_path=e.relative_path
          AND i.content_hash=e.content_hash
         WHERE e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备神经 Embedding 搜索：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let vault_id = row.get::<_, String>(0)?;
        let relative_path = row.get::<_, String>(1)?;
        let title = row.get::<_, String>(2)?;
        let excerpt = row.get::<_, String>(3)?;
        let modified_at = row.get::<_, String>(4)?;
        let dimensions = row.get::<_, i64>(7)?;
        let blob = row.get::<_, Vec<u8>>(8)?;
        let input_hash = row.get::<_, String>(9)?;
        let Some(candidate_vector) = decode_neural_embedding(dimensions, &blob) else {
            return Ok((None, Some(input_hash)));
        };
        if candidate_vector.len() != context.query_vector.len() {
            return Ok((None, Some(input_hash)));
        }
        let similarity = local_vector_similarity(&context.query_vector, &candidate_vector);
        let tags_json = row.get::<_, String>(5)?;
        let wiki_links_json = row.get::<_, String>(6)?;
        Ok((
            similarity
                .filter(|score| *score >= MIN_NEURAL_EMBEDDING_SIMILARITY)
                .map(|score| IndexedSearchCandidate {
                    vault_id,
                    relative_path,
                    title,
                    excerpt,
                    modified_at,
                    lexical_score: None,
                    vector_similarity: Some(score),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    wiki_links: serde_json::from_str(&wiki_links_json).unwrap_or_default(),
                }),
            None,
        ))
    };
    let rows = if let Some(vault_id) = scoped {
        statement
            .query_map(
                params![
                    context.workspace_scope,
                    context.provider_id,
                    context.model,
                    vault_id,
                ],
                map_row,
            )
            .map_err(|error| format!("神经 Embedding 搜索失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
    } else {
        statement
            .query_map(
                params![context.workspace_scope, context.provider_id, context.model,],
                map_row,
            )
            .map_err(|error| format!("神经 Embedding 搜索失败：{error}"))?
            .collect::<Result<Vec<_>, _>>()
    }
    .map_err(|error| format!("无法解析神经 Embedding 搜索结果：{error}"))?;
    drop(statement);

    let mut candidates = Vec::new();
    let mut corrupt_hashes = Vec::new();
    for (candidate, corrupt_hash) in rows {
        candidates.extend(candidate);
        corrupt_hashes.extend(corrupt_hash);
    }
    corrupt_hashes.sort();
    corrupt_hashes.dedup();
    if !corrupt_hashes.is_empty() {
        for input_hash in &corrupt_hashes {
            connection
                .execute(
                    "DELETE FROM note_neural_embeddings
                     WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND input_hash=?4",
                    params![
                        context.workspace_scope,
                        context.provider_id,
                        context.model,
                        input_hash,
                    ],
                )
                .map_err(|error| format!("无法移除损坏的神经 Embedding 绑定：{error}"))?;
            connection
                .execute(
                    "DELETE FROM neural_embedding_cache
                     WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND input_hash=?4",
                    params![
                        context.workspace_scope,
                        context.provider_id,
                        context.model,
                        input_hash,
                    ],
                )
                .map_err(|error| format!("无法移除损坏的神经 Embedding 缓存：{error}"))?;
        }
        connection
            .execute(
                "UPDATE neural_embedding_index_state
                 SET state='degraded', last_error=?4, updated_at=?5
                 WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3",
                params![
                    context.workspace_scope,
                    context.provider_id,
                    context.model,
                    "检测到损坏或维度不兼容的 Embedding 缓存，已移除并等待重建",
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|error| format!("无法标记神经 Embedding 索引降级：{error}"))?;
    }
    Ok((candidates, !corrupt_hashes.is_empty()))
}

fn indexed_search_in_connection_with_neural(
    connection: &Connection,
    vault_id: Option<&str>,
    query: &str,
    max_results: usize,
    neural: Option<&NeuralSearchContext>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // 性能监控
    let _profiler = crate::database::QueryProfiler::new("indexed_search_in_connection_with_neural")
        .with_threshold(100);

    let query = query.trim();
    if query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let scoped = vault_id.filter(|value| *value != "all");
    let candidate_limit = (max_results * 5).min(1_000);
    let normalized_query = query.nfc().collect::<String>().to_lowercase();
    let now = Utc::now();
    let mut lexical_candidates =
        load_lexical_search_candidates(connection, scoped, query, candidate_limit as i64)?;
    lexical_candidates.sort_by(|left, right| {
        let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
        let right_signals = indexed_search_candidate_signals(right, &normalized_query, &now);
        let left_score = -left.lexical_score.unwrap_or_default()
            + left_signals.0
            + left_signals.1
            + left_signals.2;
        let right_score = -right.lexical_score.unwrap_or_default()
            + right_signals.0
            + right_signals.1
            + right_signals.2;
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    lexical_candidates.truncate(candidate_limit);

    let mut local_vector_candidates = match load_vector_search_candidates(connection, scoped, query)
    {
        Ok(candidates) => candidates,
        Err(error) => {
            log::warn!("本地特征向量不可用，继续使用 FTS：{error}");
            Vec::new()
        }
    };
    local_vector_candidates.sort_by(|left, right| {
        right
            .vector_similarity
            .unwrap_or_default()
            .partial_cmp(&left.vector_similarity.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
                let right_signals =
                    indexed_search_candidate_signals(right, &normalized_query, &now);
                (right_signals.0 + right_signals.1 + right_signals.2)
                    .partial_cmp(&(left_signals.0 + left_signals.1 + left_signals.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    local_vector_candidates.truncate(candidate_limit);

    let (mut neural_candidates, neural_cache_degraded) = match neural {
        Some(context) => match load_neural_search_candidates(connection, scoped, context) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("神经 Embedding 候选不可用，继续使用 FTS 与本地向量：{error}");
                (Vec::new(), true)
            }
        },
        None => (Vec::new(), false),
    };
    neural_candidates.sort_by(|left, right| {
        right
            .vector_similarity
            .unwrap_or_default()
            .partial_cmp(&left.vector_similarity.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    neural_candidates.truncate(candidate_limit);

    let lexical_ranks = lexical_candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect::<HashMap<_, _>>();
    let local_vector_ranks = local_vector_candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect::<HashMap<_, _>>();
    let neural_ranks = neural_candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect::<HashMap<_, _>>();
    let local_vector_similarities = local_vector_candidates
        .iter()
        .filter_map(|candidate| {
            candidate.vector_similarity.map(|similarity| {
                (
                    (candidate.vault_id.clone(), candidate.relative_path.clone()),
                    similarity,
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let neural_similarities = neural_candidates
        .iter()
        .filter_map(|candidate| {
            candidate.vector_similarity.map(|similarity| {
                (
                    (candidate.vault_id.clone(), candidate.relative_path.clone()),
                    similarity,
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let neural_active = !neural_ranks.is_empty();
    let mut fused = HashMap::new();
    for candidate in lexical_candidates {
        fused.insert(
            (candidate.vault_id.clone(), candidate.relative_path.clone()),
            candidate,
        );
    }
    for candidate in local_vector_candidates {
        let key = (candidate.vault_id.clone(), candidate.relative_path.clone());
        fused
            .entry(key)
            .and_modify(|existing: &mut IndexedSearchCandidate| {
                if existing.excerpt.is_empty() {
                    existing.excerpt.clone_from(&candidate.excerpt);
                }
            })
            .or_insert(candidate);
    }
    for candidate in neural_candidates {
        let key = (candidate.vault_id.clone(), candidate.relative_path.clone());
        fused
            .entry(key)
            .and_modify(|existing: &mut IndexedSearchCandidate| {
                if existing.excerpt.is_empty() {
                    existing.excerpt.clone_from(&candidate.excerpt);
                }
            })
            .or_insert(candidate);
    }

    let mut results = fused
        .into_iter()
        .map(|(key, candidate)| {
            let lexical_rank = lexical_ranks.get(&key).copied();
            let neural_rank = neural_ranks.get(&key).copied();
            let local_vector_rank = local_vector_ranks.get(&key).copied();
            let lexical_rrf = lexical_rank
                .map(|rank| 1.0 / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let neural_rrf = neural_rank
                .map(|rank| NEURAL_RRF_WEIGHT / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let local_vector_rrf = local_vector_rank
                .map(|rank| {
                    let weight = if neural_active {
                        LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL
                    } else {
                        1.0
                    };
                    weight / (RRF_K + rank as f64)
                })
                .unwrap_or(0.0);
            let vector_rank = neural_rank.or(local_vector_rank);
            let vector_rrf = if neural_rank.is_some() {
                neural_rrf
            } else {
                local_vector_rrf
            };
            let neural_similarity = neural_similarities.get(&key).copied();
            let local_vector_similarity = local_vector_similarities.get(&key).copied();
            let vector_similarity = neural_similarity.or(local_vector_similarity);
            let (title_path_bonus, relation_bonus, recency_bonus) =
                indexed_search_candidate_signals(&candidate, &normalized_query, &now);
            IndexedSearchResult {
                vault_id: candidate.vault_id,
                relative_path: candidate.relative_path,
                title: candidate.title,
                excerpt: candidate.excerpt,
                modified_at: candidate.modified_at,
                score: lexical_rrf + neural_rrf + local_vector_rrf,
                tags: candidate.tags,
                wiki_links: candidate.wiki_links,
                source_kind: "obsidian_markdown".to_string(),
                ranking_signals: IndexedSearchSignals {
                    lexical_rank,
                    vector_rank,
                    neural_rank,
                    local_vector_rank,
                    lexical_rrf,
                    vector_rrf,
                    neural_rrf,
                    local_vector_rrf,
                    vector_similarity,
                    neural_similarity,
                    local_vector_similarity,
                    title_path_bonus,
                    relation_bonus,
                    recency_bonus,
                    vector_kind: if neural_rank.is_some() {
                        "neural_embedding_v1".to_string()
                    } else {
                        "local_feature_hash_v1".to_string()
                    },
                    embedding_provider: neural.map(|context| context.provider.clone()),
                    embedding_model: neural.map(|context| context.model.clone()),
                    embedding_index_state: neural.map(|context| {
                        if neural_cache_degraded {
                            "degraded".to_string()
                        } else {
                            context.index_state.clone()
                        }
                    }),
                },
            }
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.ranking_signals
                    .neural_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&right.ranking_signals.neural_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                right
                    .ranking_signals
                    .neural_similarity
                    .unwrap_or(f64::NEG_INFINITY)
                    .partial_cmp(
                        &left
                            .ranking_signals
                            .neural_similarity
                            .unwrap_or(f64::NEG_INFINITY),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                let left_signals = &left.ranking_signals;
                let right_signals = &right.ranking_signals;
                (right_signals.title_path_bonus
                    + right_signals.relation_bonus
                    + right_signals.recency_bonus)
                    .partial_cmp(
                        &(left_signals.title_path_bonus
                            + left_signals.relation_bonus
                            + left_signals.recency_bonus),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    results.truncate(max_results.clamp(1, 200));
    Ok(results)
}

#[tauri::command]
pub fn get_neural_embedding_index_status(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
) -> Result<NeuralEmbeddingIndexStatus, String> {
    get_neural_embedding_index_status_inner(&database, vault_id.as_deref())
}

#[tauri::command]
pub async fn rebuild_neural_embedding_index(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    consent: Option<bool>,
) -> Result<NeuralEmbeddingIndexStatus, String> {
    if consent != Some(true) {
        return Err("重建神经 Embedding 前必须明确确认会向已配置供应商发送 Vault 内容".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    if let Some(vault_id) = normalize_neural_embedding_vault_id(vault_id.as_deref())? {
        database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
        return rebuild_neural_embedding_index_inner(&database, Some(&vault_id)).await;
    }
    for readable_vault_id in database.readable_indexed_vault_ids(&workspace_scope)? {
        rebuild_neural_embedding_index_inner(&database, Some(&readable_vault_id)).await?;
    }
    get_neural_embedding_index_status_inner(&database, None)
}

#[tauri::command]
pub async fn indexed_search(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    query: String,
    limit: Option<usize>,
    allow_neural_embedding: Option<bool>,
) -> Result<Vec<IndexedSearchResult>, String> {
    let normalized_query = query.trim();
    if normalized_query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    if normalized_query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let scoped_vault_id = normalize_neural_embedding_vault_id(vault_id.as_deref())?;
    let readable_vault_ids = if let Some(vault_id) = scoped_vault_id {
        database.ensure_vault_read_allowed(&workspace_scope, &vault_id)?;
        vec![vault_id]
    } else {
        database.readable_indexed_vault_ids(&workspace_scope)?
    };
    let max_results = limit.unwrap_or(50).clamp(1, 200);
    let mut results = Vec::new();
    for readable_vault_id in readable_vault_ids {
        let neural = if allow_neural_embedding == Some(true) {
            match prepare_neural_search_context(
                &database,
                &workspace_scope,
                Some(&readable_vault_id),
                normalized_query,
            )
            .await
            {
                Ok(context) => context,
                Err(error) => {
                    log::warn!(
                        "Vault {readable_vault_id} 的神经 Embedding 搜索不可用，回退到本地混合搜索：{error}"
                    );
                    None
                }
            }
        } else {
            None
        };
        let mut vault_results = {
            let connection = database
                .connection
                .lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;
            indexed_search_in_connection_with_neural(
                &connection,
                Some(&readable_vault_id),
                normalized_query,
                max_results,
                neural.as_ref(),
            )?
        };
        results.append(&mut vault_results);
    }
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.vault_id.cmp(&right.vault_id))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    results.truncate(max_results);

    // 记录搜索事件
    let _ = crate::metrics::record_activity_event(database.inner(), "search", None, None, None);

    Ok(results)
}

/// 检测内容重复
pub fn detect_content_duplicate(
    connection: &Connection,
    workspace_scope: &str,
    fingerprint: &crate::content_fingerprint::ContentFingerprint,
) -> Result<Option<crate::content_fingerprint::DuplicateDetectionResult>, String> {
    use crate::content_fingerprint::{ContentFingerprint, DuplicateDetectionResult, DuplicateLevel};

    // L1: 精确匹配
    if let Some((existing_id, existing_title)) = connection
        .query_row(
            "SELECT content_id, title FROM content_fingerprints
             WHERE workspace_scope=?1 AND exact_hash=?2
             LIMIT 1",
            params![workspace_scope, &fingerprint.exact_hash],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| format!("查询精确匹配失败：{e}"))?
    {
        return Ok(Some(DuplicateDetectionResult {
            level: DuplicateLevel::Exact,
            existing_content_id: existing_id,
            existing_title,
            similarity_score: 1.0,
        }));
    }

    // L2: 结构匹配
    if let Some((existing_id, existing_title)) = connection
        .query_row(
            "SELECT content_id, title FROM content_fingerprints
             WHERE workspace_scope=?1 AND structure_hash=?2
             LIMIT 1",
            params![workspace_scope, &fingerprint.structure_hash],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| format!("查询结构匹配失败：{e}"))?
    {
        return Ok(Some(DuplicateDetectionResult {
            level: DuplicateLevel::StructuralSimilar,
            existing_content_id: existing_id,
            existing_title,
            similarity_score: 0.9,
        }));
    }

    // L3: SimHash 相似匹配（汉明距离 < 3）
    let mut stmt = connection
        .prepare(
            "SELECT content_id, title, simhash FROM content_fingerprints
             WHERE workspace_scope=?1
             LIMIT 1000",
        )
        .map_err(|e| format!("准备 SimHash 查询失败：{e}"))?;

    let candidates: Vec<(String, String, u64)> = stmt
        .query_map(params![workspace_scope], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
            ))
        })
        .map_err(|e| format!("查询 SimHash 失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    for (content_id, title, existing_simhash) in candidates {
        let distance = ContentFingerprint::hamming_distance(fingerprint.simhash, existing_simhash);
        if distance < 3 {
            return Ok(Some(DuplicateDetectionResult {
                level: DuplicateLevel::SemanticSimilar,
                existing_content_id: content_id,
                existing_title: title,
                similarity_score: 1.0 - (distance as f64 / 64.0),
            }));
        }
    }

    // L4: 来源指纹匹配
    if let Some(ref source_fp) = fingerprint.source_fingerprint {
        if let Some((existing_id, existing_title)) = connection
            .query_row(
                "SELECT content_id, title FROM content_fingerprints
                 WHERE workspace_scope=?1 AND source_fingerprint=?2
                 LIMIT 1",
                params![workspace_scope, source_fp],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| format!("查询来源指纹失败：{e}"))?
        {
            return Ok(Some(DuplicateDetectionResult {
                level: DuplicateLevel::UpdatedVersion,
                existing_content_id: existing_id,
                existing_title,
                similarity_score: 0.8,
            }));
        }
    }

    Ok(None)
}

/// 存储内容指纹
pub fn store_content_fingerprint(
    connection: &Connection,
    workspace_scope: &str,
    content_id: &str,
    content_type: &str,
    fingerprint: &crate::content_fingerprint::ContentFingerprint,
    title: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();

    connection
        .execute(
            "INSERT INTO content_fingerprints
             (workspace_scope, content_id, content_type, exact_hash, structure_hash,
              simhash, source_fingerprint, title, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(workspace_scope, content_id) DO UPDATE SET
               exact_hash=excluded.exact_hash,
               structure_hash=excluded.structure_hash,
               simhash=excluded.simhash,
               source_fingerprint=excluded.source_fingerprint,
               title=excluded.title,
               created_at=excluded.created_at",
            params![
                workspace_scope,
                content_id,
                content_type,
                &fingerprint.exact_hash,
                &fingerprint.structure_hash,
                fingerprint.simhash as i64,
                &fingerprint.source_fingerprint,
                title,
                now,
            ],
        )
        .map_err(|e| format!("存储指纹失败：{e}"))?;

    Ok(())
}
