use crate::{
    execution_ticket::{ExecutionTicketRenewalReceipt, ExecutionTicketState},
    runtime_db::RuntimeDatabase,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
use tauri::State;

const RUNTIME_PLAN_SCHEMA_VERSION: &str = "1.0";
const MAX_RUNTIME_PLAN_STEPS: usize = 128;
const MAX_RUNTIME_PLAN_REQUIREMENTS: usize = 128;
pub(crate) const MAX_RUNTIME_TASK_EVIDENCE: usize = 2_048;
const MAX_RUNTIME_STEP_CLAIMS_PER_REQUEST: usize = 32;
const MAX_RUNTIME_STEP_LEASE_SECONDS: u64 = 3_600;
const MAX_RUNTIME_TICKET_RENEWAL_SECONDS: u64 = 300;
const MAX_RUNTIME_STEP_RECEIPT_BYTES: usize = 256 * 1024;

fn default_json_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationContext {
    pub(crate) task_id: Option<String>,
    pub(crate) trace_id: Option<String>,
    #[serde(default)]
    pub(crate) execution_ticket: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskStepKind {
    Model,
    Capability,
    Approval,
    Verification,
    Checkpoint,
    ScheduleDispatch,
}

impl RuntimeTaskStepKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Capability => "capability",
            Self::Approval => "approval",
            Self::Verification => "verification",
            Self::Checkpoint => "checkpoint",
            Self::ScheduleDispatch => "schedule_dispatch",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "capability" => Some(Self::Capability),
            "approval" => Some(Self::Approval),
            "verification" => Some(Self::Verification),
            "checkpoint" => Some(Self::Checkpoint),
            "schedule_dispatch" => Some(Self::ScheduleDispatch),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanStepInput {
    pub id: String,
    pub kind: RuntimeTaskStepKind,
    pub title: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_json_object")]
    pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskCompletionMode {
    AllOf,
}

fn default_requirement_count() -> usize {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskCompletionRequirementInput {
    pub id: String,
    #[serde(default)]
    pub step_id: Option<String>,
    pub evidence_type: String,
    #[serde(default = "default_requirement_count")]
    pub minimum_count: usize,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskCompletionContractInput {
    pub mode: RuntimeTaskCompletionMode,
    pub requirements: Vec<RuntimeTaskCompletionRequirementInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanInput {
    pub schema_version: String,
    pub goal: String,
    pub steps: Vec<RuntimeTaskPlanStepInput>,
    pub completion_contract: RuntimeTaskCompletionContractInput,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanBindingInput {
    pub task_id: String,
    pub plan: RuntimeTaskPlanInput,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepCommandBinding {
    pub runtime_task_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub step_claim_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskStepEffectClass {
    ReadOnly,
    Effectful,
}

impl RuntimeTaskStepEffectClass {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Effectful => "effectful",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "read_only" => Some(Self::ReadOnly),
            "effectful" => Some(Self::Effectful),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepBudgetReservation {
    #[serde(default)]
    pub max_tool_calls: u64,
    #[serde(default)]
    pub max_runtime_seconds: u64,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_cost: Option<f64>,
}

fn default_step_claim_limit() -> usize {
    1
}

fn default_step_lease_seconds() -> u64 {
    300
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepClaimInput {
    pub task_id: String,
    #[serde(default)]
    pub plan_revision: Option<u64>,
    pub worker_id: String,
    #[serde(default = "default_step_claim_limit")]
    pub max_claims: usize,
    #[serde(default = "default_step_lease_seconds")]
    pub lease_seconds: u64,
    #[serde(default)]
    pub reservation: RuntimeTaskStepBudgetReservation,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepLeaseRenewalInput {
    pub task_id: String,
    pub step_claim_id: String,
    pub worker_id: String,
    #[serde(default = "default_step_lease_seconds")]
    pub lease_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepLeaseRenewalReceipt {
    pub runtime_task_id: String,
    pub step_claim_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub lease_owner: String,
    pub previous_lease_expires_at: String,
    pub lease_expires_at: String,
    pub cancellation_fence: u64,
}

fn default_ticket_renewal_seconds() -> u64 {
    300
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeExecutionTicketRenewalInput {
    pub execution_ticket: String,
    pub task_id: String,
    pub step_binding: RuntimeTaskStepCommandBinding,
    #[serde(default = "default_ticket_renewal_seconds")]
    pub extension_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReadOnlyCapabilityInput {
    pub execution_ticket: String,
    pub task_id: String,
    pub step_binding: RuntimeTaskStepCommandBinding,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReadOnlyCapabilityResult {
    pub task_id: String,
    pub command_id: String,
    pub trace_id: String,
    pub capability_id: String,
    pub operation: String,
    pub trust_kind: String,
    pub output: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepClaim {
    pub claim_id: String,
    pub runtime_task_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub step_kind: RuntimeTaskStepKind,
    pub title: String,
    pub depends_on: Vec<String>,
    pub parameters: Value,
    pub effect_class: RuntimeTaskStepEffectClass,
    pub attempt: u64,
    pub lease_owner: String,
    pub lease_expires_at: String,
    pub reserved_tool_calls: u64,
    pub reserved_runtime_seconds: u64,
    pub reserved_tokens: Option<u64>,
    pub reserved_cost: Option<f64>,
    pub cancellation_fence: u64,
    pub claimed_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskExecutionBudgetStatus {
    pub runtime_task_id: String,
    pub plan_revision: u64,
    pub max_steps: u64,
    pub max_tool_calls: u64,
    pub max_runtime_seconds: u64,
    pub max_tokens: Option<u64>,
    pub max_cost: Option<f64>,
    pub reserved_steps: u64,
    pub reserved_tool_calls: u64,
    pub reserved_runtime_seconds: u64,
    pub reserved_tokens: u64,
    pub reserved_cost: f64,
    pub consumed_steps: u64,
    pub consumed_tool_calls: u64,
    pub consumed_runtime_seconds: u64,
    pub consumed_tokens: u64,
    pub consumed_cost: f64,
    pub cancellation_fence: u64,
    pub cancelled_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepClaimBatch {
    pub claims: Vec<RuntimeTaskStepClaim>,
    pub budget: RuntimeTaskExecutionBudgetStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepFrontierItem {
    pub runtime_task_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub step_kind: RuntimeTaskStepKind,
    pub title: String,
    pub depends_on: Vec<String>,
    pub parameters: Value,
    pub effect_class: RuntimeTaskStepEffectClass,
    pub ready: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepCompletionInput {
    pub task_id: String,
    pub step_claim_id: String,
    pub receipt_id: String,
    #[serde(default)]
    pub consumed_tool_calls: u64,
    #[serde(default)]
    pub consumed_runtime_seconds: u64,
    #[serde(default)]
    pub consumed_tokens: u64,
    #[serde(default)]
    pub consumed_cost: f64,
    #[serde(default = "default_json_object")]
    pub output: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepFailureInput {
    pub task_id: String,
    pub step_claim_id: String,
    pub receipt_id: String,
    #[serde(default)]
    pub consumed_tool_calls: u64,
    #[serde(default)]
    pub consumed_runtime_seconds: u64,
    #[serde(default)]
    pub consumed_tokens: u64,
    #[serde(default)]
    pub consumed_cost: f64,
    pub error: String,
    #[serde(default = "default_json_object")]
    pub output: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskStepReceipt {
    pub receipt_id: String,
    pub step_claim_id: String,
    pub runtime_task_id: String,
    pub plan_revision: u64,
    pub step_id: String,
    pub state: String,
    pub output: Value,
    pub error: Option<String>,
    pub consumed_tool_calls: u64,
    pub consumed_runtime_seconds: u64,
    pub consumed_tokens: u64,
    pub consumed_cost: f64,
    pub content_hash: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskEvidenceSourceKind {
    Runtime,
    OperationEvent,
    InboundContent,
    VaultCommit,
    ModelReceipt,
    UserApproval,
    Scheduler,
    Verification,
}

impl RuntimeTaskEvidenceSourceKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::OperationEvent => "operation_event",
            Self::InboundContent => "inbound_content",
            Self::VaultCommit => "vault_commit",
            Self::ModelReceipt => "model_receipt",
            Self::UserApproval => "user_approval",
            Self::Scheduler => "scheduler",
            Self::Verification => "verification",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "runtime" => Some(Self::Runtime),
            "operation_event" => Some(Self::OperationEvent),
            "inbound_content" => Some(Self::InboundContent),
            "vault_commit" => Some(Self::VaultCommit),
            "model_receipt" => Some(Self::ModelReceipt),
            "user_approval" => Some(Self::UserApproval),
            "scheduler" => Some(Self::Scheduler),
            "verification" => Some(Self::Verification),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskEvidenceInput {
    pub task_id: String,
    pub evidence_id: String,
    #[serde(default)]
    pub plan_revision: Option<u64>,
    pub requirement_id: String,
    pub evidence_type: String,
    pub source_kind: RuntimeTaskEvidenceSourceKind,
    pub source_ref: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeScheduleDispatchAckInput {
    pub occurrence_id: String,
    pub runtime_task_id: String,
    pub schedule_revision: u64,
    pub schedule_payload_hash: String,
    pub dispatch_task_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanStep {
    pub id: String,
    pub kind: RuntimeTaskStepKind,
    pub title: String,
    pub depends_on: Vec<String>,
    pub parameters: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskCompletionRequirement {
    pub id: String,
    pub step_id: Option<String>,
    pub evidence_type: String,
    pub minimum_count: usize,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskCompletionContract {
    pub mode: RuntimeTaskCompletionMode,
    pub requirements: Vec<RuntimeTaskCompletionRequirement>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlan {
    pub schema_version: String,
    pub goal: String,
    pub steps: Vec<RuntimeTaskPlanStep>,
    pub completion_contract: RuntimeTaskCompletionContract,
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanSnapshot {
    pub task_id: String,
    pub revision: u64,
    pub plan: RuntimeTaskPlan,
    pub content_hash: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskEvidence {
    pub task_id: String,
    pub evidence_id: String,
    pub plan_revision: u64,
    pub requirement_id: String,
    pub step_id: Option<String>,
    pub evidence_type: String,
    pub source_kind: RuntimeTaskEvidenceSourceKind,
    pub source_ref: String,
    pub payload: Value,
    pub content_hash: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRequirementStatus {
    pub id: String,
    pub description: String,
    pub evidence_type: String,
    pub required_count: usize,
    pub observed_count: usize,
    pub satisfied: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskCompletionStatus {
    pub plan_revision: u64,
    pub satisfied: bool,
    pub requirements: Vec<RuntimeTaskRequirementStatus>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskContractSnapshot {
    pub task_id: String,
    pub plan: RuntimeTaskPlanSnapshot,
    pub completion: RuntimeTaskCompletionStatus,
    pub evidence: Vec<RuntimeTaskEvidence>,
}

fn valid_contract_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

pub(crate) fn validate_runtime_task_step_binding(
    binding: &RuntimeTaskStepCommandBinding,
) -> Result<(), String> {
    if !valid_contract_identifier(&binding.runtime_task_id, 180)
        || binding.plan_revision == 0
        || !valid_contract_identifier(&binding.step_id, 128)
        || !valid_contract_identifier(&binding.step_claim_id, 180)
    {
        return Err("原生任务步骤绑定标识符无效".to_string());
    }
    Ok(())
}

pub(crate) fn runtime_task_step_effect_class(
    kind: &RuntimeTaskStepKind,
    parameters: &Value,
) -> RuntimeTaskStepEffectClass {
    let explicitly_effectful = parameters
        .get("effectClass")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("effectful"))
        || parameters
            .get("externalEffect")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || parameters
            .get("networkTargets")
            .and_then(Value::as_array)
            .is_some_and(|targets| !targets.is_empty());
    if explicitly_effectful {
        return RuntimeTaskStepEffectClass::Effectful;
    }
    match kind {
        RuntimeTaskStepKind::Verification | RuntimeTaskStepKind::Checkpoint => {
            RuntimeTaskStepEffectClass::ReadOnly
        }
        RuntimeTaskStepKind::Capability => {
            let explicit_read_only = parameters
                .get("effectClass")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("read_only"))
                || parameters
                    .get("readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let operation_is_read_only = parameters
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|operation| {
                    matches!(
                        operation,
                        "read" | "query" | "search" | "list" | "open" | "preview"
                    )
                });
            if explicit_read_only && operation_is_read_only {
                RuntimeTaskStepEffectClass::ReadOnly
            } else {
                RuntimeTaskStepEffectClass::Effectful
            }
        }
        RuntimeTaskStepKind::Model
        | RuntimeTaskStepKind::Approval
        | RuntimeTaskStepKind::ScheduleDispatch => RuntimeTaskStepEffectClass::Effectful,
    }
}

pub(crate) fn validate_runtime_task_step_claim(
    input: &RuntimeTaskStepClaimInput,
) -> Result<(), String> {
    if !valid_contract_identifier(&input.task_id, 180)
        || input.plan_revision.is_some_and(|revision| revision == 0)
        || !valid_contract_identifier(&input.worker_id, 180)
        || input.max_claims == 0
        || input.max_claims > MAX_RUNTIME_STEP_CLAIMS_PER_REQUEST
        || input.lease_seconds == 0
        || input.lease_seconds > MAX_RUNTIME_STEP_LEASE_SECONDS
        || input.reservation.max_tool_calls > 2_048
        || input.reservation.max_runtime_seconds > 86_400
        || input
            .reservation
            .max_cost
            .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
    {
        return Err("原生任务步骤领取参数无效".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_step_lease_renewal(
    input: &RuntimeTaskStepLeaseRenewalInput,
) -> Result<(), String> {
    if !valid_contract_identifier(&input.task_id, 180)
        || !valid_contract_identifier(&input.step_claim_id, 180)
        || !valid_contract_identifier(&input.worker_id, 180)
        || input.lease_seconds == 0
        || input.lease_seconds > MAX_RUNTIME_STEP_LEASE_SECONDS
    {
        return Err("原生任务步骤 lease 续租参数无效".to_string());
    }
    Ok(())
}

fn validate_runtime_execution_ticket_renewal(
    input: &RuntimeExecutionTicketRenewalInput,
) -> Result<(), String> {
    if input.execution_ticket.trim().is_empty()
        || input.execution_ticket.chars().count() > 240
        || !valid_contract_identifier(&input.task_id, 180)
        || input.extension_seconds == 0
        || input.extension_seconds > MAX_RUNTIME_TICKET_RENEWAL_SECONDS
    {
        return Err("Runtime 执行票据续期参数无效".to_string());
    }
    validate_runtime_task_step_binding(&input.step_binding)
}

fn validate_runtime_read_only_capability_input(
    input: &RuntimeReadOnlyCapabilityInput,
) -> Result<(), String> {
    if input.execution_ticket.trim().is_empty()
        || input.execution_ticket.chars().count() > 240
        || !valid_contract_identifier(&input.task_id, 180)
    {
        return Err("Runtime 只读能力处理器输入无效".to_string());
    }
    validate_runtime_task_step_binding(&input.step_binding)
}

fn validate_runtime_task_step_receipt_fields(
    task_id: &str,
    step_claim_id: &str,
    receipt_id: &str,
    output: &Value,
) -> Result<(), String> {
    if !valid_contract_identifier(task_id, 180)
        || !valid_contract_identifier(step_claim_id, 180)
        || !valid_contract_identifier(receipt_id, 180)
        || !output.is_object()
    {
        return Err("原生任务步骤回执字段无效".to_string());
    }
    let encoded = serde_json::to_vec(output)
        .map_err(|error| format!("无法序列化原生任务步骤回执：{error}"))?;
    if encoded.len() > MAX_RUNTIME_STEP_RECEIPT_BYTES {
        return Err("原生任务步骤回执超过 256 KB 安全上限".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_step_completion(
    input: &RuntimeTaskStepCompletionInput,
) -> Result<(), String> {
    validate_runtime_task_step_receipt_fields(
        &input.task_id,
        &input.step_claim_id,
        &input.receipt_id,
        &input.output,
    )?;
    if input.consumed_tool_calls > 2_048
        || input.consumed_runtime_seconds > 86_400
        || !input.consumed_cost.is_finite()
        || input.consumed_cost < 0.0
    {
        return Err("原生任务步骤回执预算消耗无效".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_step_failure(
    input: &RuntimeTaskStepFailureInput,
) -> Result<(), String> {
    validate_runtime_task_step_receipt_fields(
        &input.task_id,
        &input.step_claim_id,
        &input.receipt_id,
        &input.output,
    )?;
    if input.error.trim().is_empty()
        || input.error.chars().count() > 4_000
        || input.error.chars().any(char::is_control)
        || input.consumed_tool_calls > 2_048
        || input.consumed_runtime_seconds > 86_400
        || !input.consumed_cost.is_finite()
        || input.consumed_cost < 0.0
    {
        return Err("原生任务步骤失败回执无效".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_plan(plan: &RuntimeTaskPlanInput) -> Result<(), String> {
    if plan.schema_version.trim() != RUNTIME_PLAN_SCHEMA_VERSION {
        return Err(format!(
            "原生任务计划 schemaVersion 必须是 {RUNTIME_PLAN_SCHEMA_VERSION}"
        ));
    }
    if plan.goal.trim().is_empty() || plan.goal.chars().count() > 4_000 {
        return Err("原生任务计划 goal 无效".to_string());
    }
    if plan.steps.is_empty() || plan.steps.len() > MAX_RUNTIME_PLAN_STEPS {
        return Err(format!(
            "原生任务计划步骤数量必须在 1 到 {MAX_RUNTIME_PLAN_STEPS} 之间"
        ));
    }
    if !(plan.metadata.is_object() || plan.metadata.is_null()) {
        return Err("原生任务计划 metadata 必须是 JSON 对象".to_string());
    }
    let mut ids = HashSet::new();
    let mut dependencies = HashMap::new();
    for step in &plan.steps {
        if !valid_contract_identifier(&step.id, 128) || !ids.insert(step.id.clone()) {
            return Err(format!("原生任务计划步骤 id 无效或重复：{}", step.id));
        }
        if step.title.trim().is_empty() || step.title.chars().count() > 240 {
            return Err(format!("原生任务计划步骤标题无效：{}", step.id));
        }
        if !(step.parameters.is_object() || step.parameters.is_null()) {
            return Err(format!("原生任务计划步骤参数必须是 JSON 对象：{}", step.id));
        }
        let mut seen_dependencies = HashSet::new();
        for dependency in &step.depends_on {
            if !valid_contract_identifier(dependency, 128)
                || dependency == &step.id
                || !seen_dependencies.insert(dependency.clone())
            {
                return Err(format!(
                    "原生任务计划步骤依赖无效：{} -> {}",
                    step.id, dependency
                ));
            }
        }
        dependencies.insert(step.id.clone(), step.depends_on.clone());
    }
    for (step_id, depends_on) in &dependencies {
        if depends_on
            .iter()
            .any(|dependency| !ids.contains(dependency))
        {
            return Err(format!("原生任务计划步骤引用了不存在的依赖：{step_id}"));
        }
    }
    let mut indegree = dependencies
        .iter()
        .map(|(id, depends_on)| (id.clone(), depends_on.len()))
        .collect::<HashMap<_, _>>();
    let mut ready = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(id.clone()))
        .collect::<Vec<_>>();
    let mut visited = 0;
    while let Some(step_id) = ready.pop() {
        visited += 1;
        for (candidate, candidate_dependencies) in &dependencies {
            if candidate_dependencies.iter().any(|item| item == &step_id) {
                let count = indegree
                    .get_mut(candidate)
                    .expect("candidate exists in plan indegree");
                *count -= 1;
                if *count == 0 {
                    ready.push(candidate.clone());
                }
            }
        }
    }
    if visited != plan.steps.len() {
        return Err("原生任务计划步骤依赖存在环".to_string());
    }
    if plan.completion_contract.mode != RuntimeTaskCompletionMode::AllOf {
        return Err("原生任务完成契约只支持 all_of".to_string());
    }
    if plan.completion_contract.requirements.is_empty()
        || plan.completion_contract.requirements.len() > MAX_RUNTIME_PLAN_REQUIREMENTS
    {
        return Err(format!(
            "原生任务完成契约要求数量必须在 1 到 {MAX_RUNTIME_PLAN_REQUIREMENTS} 之间"
        ));
    }
    let mut requirement_ids = HashSet::new();
    let mut required_evidence_count = 0usize;
    for requirement in &plan.completion_contract.requirements {
        if !valid_contract_identifier(&requirement.id, 128)
            || !requirement_ids.insert(requirement.id.clone())
        {
            return Err(format!(
                "原生任务完成契约要求 id 无效或重复：{}",
                requirement.id
            ));
        }
        if let Some(step_id) = requirement.step_id.as_deref() {
            if !ids.contains(step_id) {
                return Err(format!("完成契约引用了不存在的步骤：{step_id}"));
            }
        }
        if !valid_contract_identifier(&requirement.evidence_type, 160) {
            return Err(format!(
                "原生任务完成契约 evidenceType 无效：{}",
                requirement.evidence_type
            ));
        }
        if requirement.evidence_type.trim() == "runtime.step_receipt"
            && (requirement.step_id.is_none() || requirement.minimum_count != 1)
        {
            return Err(format!(
                "步骤回执完成要求必须绑定一个步骤且 minimumCount=1：{}",
                requirement.id
            ));
        }
        if requirement.minimum_count == 0 || requirement.minimum_count > MAX_RUNTIME_TASK_EVIDENCE {
            return Err(format!("完成契约要求数量无效：{}", requirement.id));
        }
        required_evidence_count = required_evidence_count
            .checked_add(requirement.minimum_count)
            .ok_or_else(|| "完成契约要求数量溢出".to_string())?;
        if requirement.description.trim().is_empty()
            || requirement.description.chars().count() > 500
        {
            return Err(format!("完成契约要求描述无效：{}", requirement.id));
        }
    }
    if required_evidence_count > MAX_RUNTIME_TASK_EVIDENCE {
        return Err(format!(
            "完成契约要求的证据总数不能超过 {MAX_RUNTIME_TASK_EVIDENCE}"
        ));
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_evidence_shape(
    input: &RuntimeTaskEvidenceInput,
) -> Result<(), String> {
    if !valid_contract_identifier(&input.task_id, 180)
        || !valid_contract_identifier(&input.evidence_id, 180)
        || !valid_contract_identifier(&input.requirement_id, 128)
        || !valid_contract_identifier(&input.evidence_type, 160)
    {
        return Err("原生任务证据标识符无效".to_string());
    }
    if input.source_ref.trim().is_empty()
        || input.source_ref.chars().count() > 2048
        || input.source_ref.chars().any(char::is_control)
    {
        return Err("原生任务证据 sourceRef 无效".to_string());
    }
    if !input.payload.is_object() {
        return Err("原生任务证据 payload 必须是 JSON 对象".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_task_evidence(
    input: &RuntimeTaskEvidenceInput,
) -> Result<(), String> {
    validate_runtime_task_evidence_shape(input)?;
    if input.evidence_type.trim() == "verification.result"
        && input.payload.get("valid").and_then(Value::as_bool) != Some(true)
    {
        return Err("验证结果证据必须明确包含 valid=true".to_string());
    }
    Ok(())
}

pub(crate) fn validate_public_runtime_task_evidence(
    input: &RuntimeTaskEvidenceInput,
) -> Result<(), String> {
    validate_runtime_task_evidence(input)?;
    let evidence_type = input.evidence_type.trim();
    if evidence_type.starts_with("runtime.") || evidence_type == "schedule.dispatch_ack" {
        return Err("该原生任务证据类型只允许 Rust 运行时生成".to_string());
    }
    if matches!(
        &input.source_kind,
        RuntimeTaskEvidenceSourceKind::Runtime | RuntimeTaskEvidenceSourceKind::Scheduler
    ) {
        return Err("Renderer 不能声明 runtime 或 scheduler 证据来源".to_string());
    }
    Ok(())
}

pub(crate) fn validate_runtime_schedule_dispatch_ack(
    input: &RuntimeScheduleDispatchAckInput,
) -> Result<(), String> {
    let schedule_payload_digest = input.schedule_payload_hash.strip_prefix("sha256:");
    if !valid_contract_identifier(&input.occurrence_id, 180)
        || !valid_contract_identifier(&input.runtime_task_id, 180)
        || input.schedule_revision == 0
        || !schedule_payload_digest.is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        || input.dispatch_task_ids.len() > 128
    {
        return Err("日程派发确认标识符无效".to_string());
    }
    let mut task_ids = HashSet::new();
    for task_id in &input.dispatch_task_ids {
        if !valid_contract_identifier(task_id, 180)
            || task_id == &input.runtime_task_id
            || !task_ids.insert(task_id)
        {
            return Err("日程派发确认包含无效或重复的子任务".to_string());
        }
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskControlAction {
    Queue,
    Start,
    AwaitApproval,
    Pause,
    Resume,
    Cancel,
    Retry,
    Checkpoint,
    Succeed,
    Fail,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTransitionInput {
    pub task_id: String,
    pub action: TaskControlAction,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub checkpoint: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeTask {
    pub id: String,
    pub state: String,
    pub title: String,
    pub trace_id: Option<String>,
    pub progress: u8,
    pub payload: Value,
    pub created_at: String,
    pub updated_at: String,
}

fn target_state(action: &TaskControlAction) -> &'static str {
    match action {
        TaskControlAction::Queue | TaskControlAction::Retry | TaskControlAction::Resume => "queued",
        TaskControlAction::Start | TaskControlAction::Checkpoint => "running",
        TaskControlAction::AwaitApproval => "awaiting_approval",
        TaskControlAction::Pause => "paused",
        TaskControlAction::Cancel => "cancelled",
        TaskControlAction::Succeed => "succeeded",
        TaskControlAction::Fail => "failed",
    }
}

pub(crate) fn valid_task_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match from {
        "created" => matches!(to, "queued" | "cancelled" | "failed"),
        "queued" => matches!(to, "running" | "paused" | "cancelled" | "failed"),
        "running" => matches!(
            to,
            "awaiting_approval" | "paused" | "succeeded" | "failed" | "cancelled"
        ),
        "awaiting_approval" => matches!(to, "queued" | "running" | "cancelled" | "failed"),
        "paused" => matches!(to, "queued" | "cancelled" | "failed"),
        "failed" => matches!(to, "queued" | "cancelled"),
        "succeeded" | "cancelled" => false,
        _ => false,
    }
}

fn default_progress(action: &TaskControlAction, current: u8) -> u8 {
    match action {
        TaskControlAction::Queue | TaskControlAction::Retry | TaskControlAction::Resume => current,
        TaskControlAction::Start => current.max(1),
        TaskControlAction::AwaitApproval => current,
        TaskControlAction::Pause | TaskControlAction::Checkpoint => current,
        TaskControlAction::Cancel | TaskControlAction::Fail => current,
        TaskControlAction::Succeed => 100,
    }
}

#[tauri::command]
pub fn transition_runtime_task(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: TaskTransitionInput,
) -> Result<NativeRuntimeTask, String> {
    transition_runtime_task_inner(&database, &ticket_state, input)
}

fn transition_runtime_task_inner(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    input: TaskTransitionInput,
) -> Result<NativeRuntimeTask, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let current = database.runtime_task(&workspace_scope, &input.task_id)?;
    let target = if matches!(&input.action, TaskControlAction::Checkpoint) {
        current.state.as_str()
    } else {
        target_state(&input.action)
    };
    let progress = input
        .progress
        .unwrap_or_else(|| default_progress(&input.action, current.progress))
        .min(100);
    let trusted_execution_receipt = if matches!(&input.action, TaskControlAction::Succeed)
        && current.payload.get("kind").and_then(Value::as_str) == Some("runtime_child")
    {
        let command_id = current
            .payload
            .get("commandId")
            .and_then(Value::as_str)
            .ok_or_else(|| "Runtime 子任务缺少 command ID，拒绝成功结算".to_string())?;
        let trace_id = current
            .trace_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "Runtime 子任务缺少 Trace，拒绝成功结算".to_string())?;
        Some(ticket_state.trusted_execution_receipt_for_child(
            &workspace_scope,
            &input.task_id,
            command_id,
            trace_id,
        )?)
    } else {
        None
    };
    let task = if let Some(receipt) = trusted_execution_receipt.as_ref() {
        database.transition_native_runtime_task_with_trusted_execution_receipt(
            &workspace_scope,
            &input.task_id,
            target,
            progress,
            &input.detail,
            input.checkpoint.as_ref(),
            receipt,
        )?
    } else {
        database.transition_native_runtime_task(
            &workspace_scope,
            &input.task_id,
            target,
            progress,
            &input.detail,
            input.checkpoint.as_ref(),
        )?
    };
    if matches!(&input.action, TaskControlAction::Cancel) {
        ticket_state.cancel_runtime_task_bindings(&task.id)?;
    }
    Ok(task)
}

#[tauri::command]
pub fn get_runtime_task(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
) -> Result<NativeRuntimeTask, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.runtime_task(&workspace_scope, &task_id)
}

#[tauri::command]
pub fn list_runtime_tasks(
    database: State<'_, RuntimeDatabase>,
    state: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<NativeRuntimeTask>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_runtime_tasks(&workspace_scope, state.as_deref(), limit.unwrap_or(200))
}

#[tauri::command]
pub fn define_runtime_task_plan(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskPlanBindingInput,
) -> Result<RuntimeTaskContractSnapshot, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.define_runtime_task_plan(&workspace_scope, &input.task_id, &input.plan)
}

#[tauri::command]
pub fn acknowledge_runtime_schedule_dispatch(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeScheduleDispatchAckInput,
) -> Result<NativeRuntimeTask, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.acknowledge_runtime_schedule_dispatch(&workspace_scope, &input)
}

#[tauri::command]
pub fn get_runtime_task_contract(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.runtime_task_contract(&workspace_scope, task_id.trim())
}

#[tauri::command]
pub fn append_runtime_task_evidence(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskEvidenceInput,
) -> Result<RuntimeTaskEvidence, String> {
    validate_public_runtime_task_evidence(&input)?;
    let workspace_scope = database.local_workspace_scope()?;
    database.append_runtime_task_evidence(&workspace_scope, &input)
}

#[tauri::command]
pub fn get_runtime_task_step_frontier(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
    plan_revision: Option<u64>,
) -> Result<Vec<RuntimeTaskStepFrontierItem>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.runtime_task_step_frontier(&workspace_scope, task_id.trim(), plan_revision)
}

#[tauri::command]
pub fn claim_runtime_task_plan_steps(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskStepClaimInput,
) -> Result<RuntimeTaskStepClaimBatch, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.claim_runtime_task_plan_steps(&workspace_scope, &input)
}

#[tauri::command]
pub fn renew_runtime_task_step_lease(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskStepLeaseRenewalInput,
) -> Result<RuntimeTaskStepLeaseRenewalReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.renew_runtime_task_step_lease(&workspace_scope, &input)
}

#[tauri::command]
pub fn renew_runtime_execution_ticket(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: RuntimeExecutionTicketRenewalInput,
) -> Result<ExecutionTicketRenewalReceipt, String> {
    validate_runtime_execution_ticket_renewal(&input)?;
    let workspace_scope = database.local_workspace_scope()?;
    database.validate_runtime_execution_ticket_renewal(
        &workspace_scope,
        &input.task_id,
        &input.step_binding,
    )?;
    ticket_state.renew_step_bound_ticket(
        &input.execution_ticket,
        &workspace_scope,
        &input.task_id,
        &input.step_binding,
        Duration::from_secs(input.extension_seconds),
    )
}

#[tauri::command]
pub fn execute_runtime_read_only_capability(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: RuntimeReadOnlyCapabilityInput,
) -> Result<RuntimeReadOnlyCapabilityResult, String> {
    validate_runtime_read_only_capability_input(&input)?;
    let workspace_scope = database.local_workspace_scope()?;
    let started_at = Instant::now();
    let result = database.execute_runtime_read_only_capability(
        &workspace_scope,
        &input.task_id,
        &input.step_binding,
    )?;
    ticket_state.record_read_only_handler_completion(
        &input.execution_ticket,
        &workspace_scope,
        &input.task_id,
        &result.command_id,
        &result.trace_id,
        &result.capability_id,
        &result.operation,
        &input.step_binding,
        started_at.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn complete_runtime_task_plan_step(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskStepCompletionInput,
) -> Result<RuntimeTaskStepReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.complete_runtime_task_plan_step(&workspace_scope, &input)
}

#[tauri::command]
pub fn fail_runtime_task_plan_step(
    database: State<'_, RuntimeDatabase>,
    input: RuntimeTaskStepFailureInput,
) -> Result<RuntimeTaskStepReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.fail_runtime_task_plan_step(&workspace_scope, &input)
}

#[tauri::command]
pub fn list_runtime_task_step_receipts(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
    plan_revision: Option<u64>,
    limit: Option<usize>,
) -> Result<Vec<RuntimeTaskStepReceipt>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_runtime_task_step_receipts(
        &workspace_scope,
        task_id.trim(),
        plan_revision,
        limit.unwrap_or(200),
    )
}
