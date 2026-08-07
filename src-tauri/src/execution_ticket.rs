use crate::{
    policy::{ApplicationCommand, PolicyDecision, PolicyOutcome},
    task_runtime::RuntimeTaskStepCommandBinding,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

const EXECUTION_TICKET_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_EXECUTION_TICKET_RENEWAL: Duration = Duration::from_secs(5 * 60);
const MAX_EXECUTION_TICKETS: usize = 1_024;
const MAX_TICKET_APPROVAL_BINDINGS: usize = 2_048;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTicketReceipt {
    pub token: String,
    pub task_id: String,
    pub command_id: String,
    pub parameter_digest: String,
    pub step_binding: Option<RuntimeTaskStepCommandBinding>,
    pub expires_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTicketRenewalReceipt {
    pub task_id: String,
    pub command_id: String,
    pub step_binding: RuntimeTaskStepCommandBinding,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrustedExecutionReceipt {
    pub receipt_id: String,
    pub workspace_scope: String,
    pub child_task_id: String,
    pub command_id: String,
    pub trace_id: String,
    pub capability_id: String,
    pub operation: String,
    pub trust_kind: String,
    pub step_binding: RuntimeTaskStepCommandBinding,
    pub consumed_tool_calls: u64,
    pub consumed_runtime_seconds: u64,
    pub consumed_tokens: u64,
    pub consumed_cost: f64,
    #[serde(default)]
    pub cost_measured: bool,
    pub completed_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrustedHandlerUsage {
    pub tool_calls: u64,
    pub runtime_seconds: u64,
    pub tokens: u64,
    pub cost: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrustedHandlerReservation {
    pub max_tool_calls: u64,
    pub max_runtime_seconds: u64,
    pub max_tokens: Option<u64>,
    pub max_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TicketPhase {
    Ready,
    InFlight,
    Consumed,
}

#[derive(Clone, Debug)]
struct StoredExecutionTicket {
    workspace_scope: String,
    task_id: String,
    command_id: String,
    trace_id: String,
    capability_id: String,
    operation: String,
    allowed_vault_ids: HashSet<String>,
    allow_all_vaults: bool,
    relative_paths: HashSet<String>,
    allow_dynamic_paths: bool,
    allow_multiple_commits: bool,
    policy_outcome: PolicyOutcome,
    step_binding: Option<RuntimeTaskStepCommandBinding>,
    approval_bindings: HashMap<String, String>,
    in_flight_approvals: HashSet<String>,
    committed_approvals: HashSet<String>,
    handler_started_at: Option<SystemTime>,
    trusted_handler_completions: u64,
    trusted_handler_completion_keys: HashSet<String>,
    trusted_runtime_seconds: u64,
    trusted_tokens: u64,
    trusted_cost: f64,
    trusted_cost_measured: bool,
    trusted_completed_at: Option<SystemTime>,
    trusted_execution_kind: Option<String>,
    uncertain_side_effect: bool,
    trusted_completion_sealed: bool,
    renewal_used: bool,
    issued_at: SystemTime,
    expires_at: SystemTime,
    phase: TicketPhase,
}

#[derive(Default)]
pub struct ExecutionTicketState {
    tickets: Mutex<HashMap<String, StoredExecutionTicket>>,
}

struct TicketTiming {
    issued_at: SystemTime,
    ttl: Duration,
}

pub(crate) struct TicketScope<'a> {
    pub workspace_scope: &'a str,
    pub task_id: &'a str,
    pub trace_id: Option<&'a str>,
    pub allowed_capability_ids: &'a [&'a str],
    pub allowed_operations: &'a [&'a str],
    pub vault_id: &'a str,
    pub relative_path: &'a str,
    pub require_declared_path: bool,
}

fn digest_json(value: &serde_json::Value) -> Result<String, String> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| format!("无法生成执行参数摘要：{error}"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn normalized_relative_path(value: &str) -> String {
    value.trim().replace('\\', "/")
}

impl ExecutionTicketState {
    pub(crate) fn issue(
        &self,
        workspace_scope: &str,
        task_id: &str,
        trace_id: &str,
        command: &ApplicationCommand,
        decision: &PolicyDecision,
    ) -> Result<ExecutionTicketReceipt, String> {
        self.issue_at(
            workspace_scope,
            task_id,
            trace_id,
            command,
            decision,
            TicketTiming {
                issued_at: SystemTime::now(),
                ttl: EXECUTION_TICKET_TTL,
            },
        )
    }

    fn issue_at(
        &self,
        workspace_scope: &str,
        task_id: &str,
        trace_id: &str,
        command: &ApplicationCommand,
        decision: &PolicyDecision,
        timing: TicketTiming,
    ) -> Result<ExecutionTicketReceipt, String> {
        if matches!(decision.outcome, PolicyOutcome::Deny) {
            return Err("策略拒绝的命令不能签发执行票据".to_string());
        }
        if let Some(binding) = command.step_binding.as_ref() {
            crate::task_runtime::validate_runtime_task_step_binding(binding)?;
        }
        let expires_at = timing
            .issued_at
            .checked_add(timing.ttl)
            .ok_or_else(|| "无法计算执行票据过期时间".to_string())?;
        let parameter_digest = digest_json(&command.parameters)?;
        let mut allowed_vault_ids = command
            .declared_scope
            .iter()
            .filter_map(|scope| scope.strip_prefix("vault:"))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if let Some(vault_id) = command.vault_id.as_deref() {
            allowed_vault_ids.insert(vault_id.to_string());
        }
        let allow_all_vaults = allowed_vault_ids.remove("all");
        let is_capture_run =
            command.capability_id == "system:capture" && command.operation == "run";
        if is_capture_run && !allow_all_vaults && allowed_vault_ids.is_empty() {
            return Err("采集执行票据必须声明至少一个 Vault 范围".to_string());
        }
        let token = format!("ticket-{}", Uuid::new_v4());
        let stored = StoredExecutionTicket {
            workspace_scope: workspace_scope.to_string(),
            task_id: task_id.to_string(),
            command_id: command.id.clone(),
            trace_id: trace_id.to_string(),
            capability_id: command.capability_id.clone(),
            operation: command.operation.clone(),
            allowed_vault_ids,
            allow_all_vaults,
            relative_paths: command
                .relative_paths
                .iter()
                .map(|path| normalized_relative_path(path))
                .collect(),
            allow_dynamic_paths: is_capture_run,
            allow_multiple_commits: is_capture_run,
            policy_outcome: decision.outcome.clone(),
            step_binding: command.step_binding.clone(),
            approval_bindings: HashMap::new(),
            in_flight_approvals: HashSet::new(),
            committed_approvals: HashSet::new(),
            handler_started_at: None,
            trusted_handler_completions: 0,
            trusted_handler_completion_keys: HashSet::new(),
            trusted_runtime_seconds: 0,
            trusted_tokens: 0,
            trusted_cost: 0.0,
            trusted_cost_measured: true,
            trusted_completed_at: None,
            trusted_execution_kind: None,
            uncertain_side_effect: false,
            trusted_completion_sealed: false,
            renewal_used: false,
            issued_at: timing.issued_at,
            expires_at,
            phase: TicketPhase::Ready,
        };
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        tickets.retain(|_, ticket| {
            ticket.expires_at > timing.issued_at
                || (ticket.trusted_handler_completions > 0 && !ticket.trusted_completion_sealed)
        });
        if tickets.len() >= MAX_EXECUTION_TICKETS {
            return Err("待执行票据数量已达到上限，请稍后重试".to_string());
        }
        tickets.insert(token.clone(), stored);
        Ok(ExecutionTicketReceipt {
            token,
            task_id: task_id.to_string(),
            command_id: command.id.clone(),
            parameter_digest,
            step_binding: command.step_binding.clone(),
            expires_at: DateTime::<Utc>::from(expires_at).to_rfc3339(),
        })
    }

    pub(crate) fn bind_approval(
        &self,
        token: &str,
        scope: TicketScope<'_>,
        approval_id: &str,
        effect_digest: &str,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        validate_ticket(ticket, &scope, now)?;
        if ticket.phase != TicketPhase::Ready {
            return Err("执行票据正在使用或已经消费".to_string());
        }
        if ticket.committed_approvals.contains(approval_id) {
            return Err("审批 ID 已经提交，不能重放".to_string());
        }
        match ticket.approval_bindings.get(approval_id) {
            Some(bound) if bound == effect_digest => Ok(()),
            Some(_) => Err("审批 ID 已绑定到不同的副作用参数".to_string()),
            None => {
                if ticket.approval_bindings.len() >= MAX_TICKET_APPROVAL_BINDINGS {
                    return Err("执行票据绑定的待提交审批过多".to_string());
                }
                ticket
                    .approval_bindings
                    .insert(approval_id.to_string(), effect_digest.to_string());
                Ok(())
            }
        }
    }

    // Every identity and digest is intentionally explicit before consuming a one-time ticket.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bind_operation_approval(
        &self,
        token: &str,
        workspace_scope: &str,
        task_id: &str,
        trace_id: Option<&str>,
        allowed_capability_ids: &[&str],
        allowed_operations: &[&str],
        approval_id: &str,
        effect_digest: &str,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.expires_at <= now {
            return Err("执行票据已过期，请重新提交应用命令".to_string());
        }
        if matches!(ticket.policy_outcome, PolicyOutcome::Deny) {
            return Err("执行票据对应的策略决定不允许执行".to_string());
        }
        if ticket.workspace_scope != workspace_scope || ticket.task_id != task_id {
            return Err("执行票据与当前工作区或任务不匹配".to_string());
        }
        if trace_id.is_some_and(|trace_id| trace_id != ticket.trace_id) {
            return Err("执行票据与当前 trace ID 不匹配".to_string());
        }
        if !allowed_capability_ids.contains(&ticket.capability_id.as_str()) {
            return Err("执行票据没有当前处理器所需能力".to_string());
        }
        if !allowed_operations.contains(&ticket.operation.as_str()) {
            return Err(format!(
                "执行票据 operation {} 与当前处理器不匹配",
                ticket.operation
            ));
        }
        if ticket.phase != TicketPhase::Ready {
            return Err("执行票据正在使用或已经消费".to_string());
        }
        if ticket.committed_approvals.contains(approval_id) {
            return Err("审批 ID 已经提交，不能重放".to_string());
        }
        match ticket.approval_bindings.get(approval_id) {
            Some(bound) if bound == effect_digest => Ok(()),
            Some(_) => Err("审批 ID 已绑定到不同的副作用参数".to_string()),
            None => {
                if ticket.approval_bindings.len() >= MAX_TICKET_APPROVAL_BINDINGS {
                    return Err("执行票据绑定的待提交审批过多".to_string());
                }
                ticket
                    .approval_bindings
                    .insert(approval_id.to_string(), effect_digest.to_string());
                Ok(())
            }
        }
    }

    pub(crate) fn begin_commit(
        &self,
        token: &str,
        workspace_scope: &str,
        task_id: &str,
        approvals: &[(&str, &str)],
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.expires_at <= now {
            return Err("执行票据已过期，请重新提交应用命令".to_string());
        }
        if ticket.workspace_scope != workspace_scope || ticket.task_id != task_id {
            return Err("执行票据与当前工作区或任务不匹配".to_string());
        }
        if ticket.phase != TicketPhase::Ready {
            return Err("执行票据正在使用或已经消费".to_string());
        }
        if approvals.is_empty() {
            return Err("执行票据没有绑定任何待提交审批".to_string());
        }
        let mut approval_ids = HashSet::with_capacity(approvals.len());
        for (approval_id, digest) in approvals {
            if !approval_ids.insert((*approval_id).to_string()) {
                return Err(format!("执行票据提交包含重复审批：{approval_id}"));
            }
            if ticket.committed_approvals.contains(*approval_id) {
                return Err(format!("执行票据不允许重放已提交审批：{approval_id}"));
            }
            if ticket
                .approval_bindings
                .get(*approval_id)
                .is_none_or(|bound| bound != digest)
            {
                return Err(format!("执行票据未绑定当前审批：{approval_id}"));
            }
        }
        ticket.in_flight_approvals = approval_ids;
        ticket.handler_started_at = Some(now);
        ticket.phase = TicketPhase::InFlight;
        Ok(())
    }

    pub(crate) fn release_commit(&self, token: &str) {
        if let Ok(mut tickets) = self.tickets.lock() {
            if let Some(ticket) = tickets.get_mut(token.trim()) {
                if ticket.phase == TicketPhase::InFlight {
                    ticket.in_flight_approvals.clear();
                    ticket.handler_started_at = None;
                    ticket.phase = TicketPhase::Ready;
                }
            }
        }
    }

    pub(crate) fn fail_commit(&self, token: &str) -> Result<(), String> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.phase != TicketPhase::InFlight {
            return Err("执行票据没有处于提交状态".to_string());
        }
        if ticket
            .trusted_execution_kind
            .as_deref()
            .is_some_and(|kind| kind != "effectful_native_handler")
        {
            return Err("执行票据混合了不同种类的可信处理器事实".to_string());
        }
        // An uncertain side effect must never become executable again.
        ticket.in_flight_approvals.clear();
        ticket.handler_started_at = None;
        ticket.uncertain_side_effect = true;
        ticket.phase = TicketPhase::Consumed;
        Ok(())
    }

    pub(crate) fn complete_commit(&self, token: &str) -> Result<(), String> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.phase != TicketPhase::InFlight {
            return Err("执行票据没有处于提交状态".to_string());
        }
        if ticket
            .trusted_execution_kind
            .as_deref()
            .is_some_and(|kind| kind != "effectful_native_handler")
        {
            return Err("执行票据混合了不同种类的可信处理器事实".to_string());
        }
        let completed_at = SystemTime::now();
        let started_at = ticket
            .handler_started_at
            .take()
            .ok_or_else(|| "执行票据缺少可信处理器开始时间".to_string())?;
        let runtime_seconds = completed_at
            .duration_since(started_at)
            .unwrap_or_default()
            .as_secs()
            .max(1);
        ticket.trusted_handler_completions = ticket.trusted_handler_completions.saturating_add(1);
        ticket.trusted_runtime_seconds = ticket
            .trusted_runtime_seconds
            .saturating_add(runtime_seconds);
        ticket.trusted_completed_at = Some(completed_at);
        ticket.trusted_execution_kind = Some("effectful_native_handler".to_string());
        if ticket.allow_multiple_commits {
            let completed_approvals = std::mem::take(&mut ticket.in_flight_approvals);
            for approval_id in completed_approvals {
                ticket.approval_bindings.remove(&approval_id);
                ticket.committed_approvals.insert(approval_id);
            }
            ticket.phase = TicketPhase::Ready;
        } else {
            ticket.in_flight_approvals.clear();
            ticket.phase = TicketPhase::Consumed;
        }
        Ok(())
    }

    pub(crate) fn retire(&self, token: &str, task_id: &str) -> Result<bool, String> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.task_id != task_id.trim() {
            return Err("执行票据与当前任务不匹配".to_string());
        }
        match ticket.phase {
            TicketPhase::Ready => {
                ticket.approval_bindings.clear();
                ticket.in_flight_approvals.clear();
                ticket.phase = TicketPhase::Consumed;
                Ok(true)
            }
            TicketPhase::Consumed => Ok(false),
            TicketPhase::InFlight => Err("执行票据正在提交副作用，无法退役".to_string()),
        }
    }

    pub(crate) fn validate_step_binding(
        &self,
        token: &str,
        binding: &RuntimeTaskStepCommandBinding,
    ) -> Result<(), String> {
        crate::task_runtime::validate_runtime_task_step_binding(binding)?;
        let tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.step_binding.as_ref() != Some(binding) {
            return Err("执行票据与当前任务步骤绑定不一致".to_string());
        }
        Ok(())
    }

    pub(crate) fn renew_step_bound_ticket(
        &self,
        token: &str,
        workspace_scope: &str,
        task_id: &str,
        binding: &RuntimeTaskStepCommandBinding,
        extension: Duration,
    ) -> Result<ExecutionTicketRenewalReceipt, String> {
        crate::task_runtime::validate_runtime_task_step_binding(binding)?;
        if extension.is_zero() || extension > MAX_EXECUTION_TICKET_RENEWAL {
            return Err("执行票据续期时长必须在 1 到 300 秒之间".to_string());
        }
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.expires_at <= now {
            return Err("执行票据已过期，不能续期或复活".to_string());
        }
        if ticket.workspace_scope != workspace_scope || ticket.task_id != task_id.trim() {
            return Err("执行票据与当前工作区或 Runtime 子任务不匹配".to_string());
        }
        if ticket.step_binding.as_ref() != Some(binding) {
            return Err("执行票据续期步骤绑定不一致".to_string());
        }
        if ticket.phase != TicketPhase::Ready
            || ticket.trusted_completion_sealed
            || ticket.uncertain_side_effect
        {
            return Err("只有未消费且未执行中的 Ready 票据可以续期".to_string());
        }
        if ticket.approval_bindings.is_empty() {
            if ticket.trusted_handler_completions == 0
                || ticket.trusted_execution_kind.as_deref() != Some("effectful_native_handler")
            {
                return Err("执行票据既没有待提交审批，也没有可信 effectful 完成事实".to_string());
            }
        } else if ticket.trusted_handler_completions > 0 {
            return Err("执行票据不能混合待提交审批与已记录的 effectful 完成事实".to_string());
        }
        if ticket.renewal_used {
            return Err("执行票据的单次安全续期已经使用".to_string());
        }
        let requested_expiry = ticket
            .expires_at
            .checked_add(extension)
            .ok_or_else(|| "无法计算执行票据续期时间".to_string())?;
        let hard_expiry = ticket
            .issued_at
            .checked_add(EXECUTION_TICKET_TTL + MAX_EXECUTION_TICKET_RENEWAL)
            .ok_or_else(|| "无法计算执行票据续期上限".to_string())?;
        ticket.expires_at = requested_expiry.min(hard_expiry);
        ticket.renewal_used = true;
        Ok(ExecutionTicketRenewalReceipt {
            task_id: ticket.task_id.clone(),
            command_id: ticket.command_id.clone(),
            step_binding: binding.clone(),
            expires_at: DateTime::<Utc>::from(ticket.expires_at).to_rfc3339(),
        })
    }

    pub(crate) fn trusted_execution_receipt_for_child(
        &self,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
    ) -> Result<TrustedExecutionReceipt, String> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let matching_tokens = tickets
            .iter()
            .filter(|(_, ticket)| {
                ticket.workspace_scope == workspace_scope && ticket.task_id == child_task_id
            })
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        if matching_tokens.is_empty() {
            return Err("Runtime 子任务没有可验证的执行票据".to_string());
        }
        if matching_tokens.len() > 1 {
            return Err("Runtime 子任务关联了多个执行票据，拒绝生成歧义回执".to_string());
        }
        let token = &matching_tokens[0];
        let ticket = tickets
            .get_mut(token)
            .ok_or_else(|| "Runtime 子任务执行票据在结算前失效".to_string())?;
        if ticket.command_id != command_id {
            return Err("执行票据 command ID 与 Runtime 子任务不匹配".to_string());
        }
        if ticket.trace_id != trace_id {
            return Err("执行票据 Trace 与 Runtime 子任务不匹配".to_string());
        }
        let step_binding = ticket
            .step_binding
            .clone()
            .ok_or_else(|| "Runtime 子任务执行票据缺少步骤绑定".to_string())?;
        if ticket.phase == TicketPhase::InFlight {
            return Err("Runtime 子任务处理器仍在执行，不能结算成功".to_string());
        }
        if ticket.trusted_handler_completions == 0 {
            return Err("Runtime 子任务没有可信原生处理器完成事实".to_string());
        }
        if ticket.uncertain_side_effect {
            return Err("Runtime 子任务存在结果不确定的原生副作用，不能结算成功".to_string());
        }
        let completed_at = ticket
            .trusted_completed_at
            .ok_or_else(|| "Runtime 子任务缺少可信处理器完成时间".to_string())?;
        let completed_at = DateTime::<Utc>::from(completed_at).to_rfc3339();
        let trust_kind = ticket
            .trusted_execution_kind
            .clone()
            .ok_or_else(|| "Runtime 子任务缺少可信处理器类型".to_string())?;
        let receipt_payload = serde_json::json!({
            "workspaceScope": workspace_scope,
            "childTaskId": child_task_id,
            "commandId": command_id,
            "traceId": trace_id,
            "capabilityId": ticket.capability_id,
            "operation": ticket.operation,
            "trustKind": trust_kind.clone(),
            "stepBinding": step_binding,
            "consumedToolCalls": ticket.trusted_handler_completions,
            "consumedRuntimeSeconds": ticket.trusted_runtime_seconds,
            "consumedTokens": ticket.trusted_tokens,
            "consumedCost": ticket.trusted_cost,
            "costMeasured": ticket.trusted_cost_measured,
            "completedAt": completed_at,
        });
        let encoded = serde_json::to_vec(&receipt_payload)
            .map_err(|error| format!("无法序列化可信执行回执：{error}"))?;
        let mut digest = Sha256::new();
        digest.update(token.as_bytes());
        digest.update(&encoded);
        let receipt = TrustedExecutionReceipt {
            receipt_id: format!("native-handler:sha256:{:x}", digest.finalize()),
            workspace_scope: workspace_scope.to_string(),
            child_task_id: child_task_id.to_string(),
            command_id: command_id.to_string(),
            trace_id: trace_id.to_string(),
            capability_id: ticket.capability_id.clone(),
            operation: ticket.operation.clone(),
            trust_kind,
            step_binding,
            consumed_tool_calls: ticket.trusted_handler_completions,
            consumed_runtime_seconds: ticket.trusted_runtime_seconds,
            consumed_tokens: ticket.trusted_tokens,
            consumed_cost: ticket.trusted_cost,
            cost_measured: ticket.trusted_cost_measured,
            completed_at,
        };
        ticket.approval_bindings.clear();
        ticket.in_flight_approvals.clear();
        ticket.handler_started_at = None;
        ticket.phase = TicketPhase::Consumed;
        ticket.trusted_completion_sealed = true;
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_read_only_handler_completion(
        &self,
        token: &str,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
        capability_id: &str,
        operation: &str,
        binding: &RuntimeTaskStepCommandBinding,
        elapsed: Duration,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        if ticket.expires_at <= now {
            return Err("执行票据已过期，不能记录只读处理器完成事实".to_string());
        }
        if matches!(ticket.policy_outcome, PolicyOutcome::Deny)
            || ticket.workspace_scope != workspace_scope
            || ticket.task_id != child_task_id
            || ticket.command_id != command_id
            || ticket.trace_id != trace_id
            || ticket.capability_id != capability_id
            || ticket.operation != operation
            || ticket.step_binding.as_ref() != Some(binding)
        {
            return Err("执行票据与只读 Runtime 处理器身份或步骤绑定不一致".to_string());
        }
        if ticket.phase != TicketPhase::Ready
            || ticket.trusted_handler_completions != 0
            || ticket.uncertain_side_effect
            || ticket.trusted_completion_sealed
        {
            return Err("执行票据不能重复或混合记录只读处理器完成事实".to_string());
        }
        ticket.trusted_handler_completions = 1;
        ticket.trusted_runtime_seconds = elapsed.as_secs().max(1);
        ticket.trusted_completed_at = Some(now);
        ticket.trusted_execution_kind = Some("read_only_native_handler".to_string());
        ticket.approval_bindings.clear();
        ticket.in_flight_approvals.clear();
        ticket.handler_started_at = None;
        ticket.phase = TicketPhase::Consumed;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_effectful_handler_authorization(
        &self,
        token: &str,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
        capability_id: &str,
        operation: &str,
        binding: &RuntimeTaskStepCommandBinding,
    ) -> Result<(), String> {
        crate::task_runtime::validate_runtime_task_step_binding(binding)?;
        let tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        validate_effectful_handler_ticket(
            ticket,
            SystemTime::now(),
            workspace_scope,
            child_task_id,
            command_id,
            trace_id,
            capability_id,
            operation,
            binding,
        )
    }

    /// Records a completion reported by a Rust-owned effectful handler.
    ///
    /// This is deliberately crate-private: a renderer must never be able to
    /// turn a self-reported result into a trusted execution receipt. The
    /// caller must have already validated the runtime child's live step and
    /// effect class against SQLite; this method only records the immutable
    /// ticket identity and handler timing under the ticket mutex.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_effectful_handler_completion(
        &self,
        token: &str,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
        capability_id: &str,
        operation: &str,
        binding: &RuntimeTaskStepCommandBinding,
        usage: TrustedHandlerUsage,
        reservation: TrustedHandlerReservation,
    ) -> Result<(), String> {
        self.record_effectful_handler_completion_internal(
            token,
            workspace_scope,
            child_task_id,
            command_id,
            trace_id,
            capability_id,
            operation,
            binding,
            None,
            usage,
            reservation,
        )
        .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_effectful_handler_completion_once(
        &self,
        token: &str,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
        capability_id: &str,
        operation: &str,
        binding: &RuntimeTaskStepCommandBinding,
        completion_key: &str,
        usage: TrustedHandlerUsage,
        reservation: TrustedHandlerReservation,
    ) -> Result<bool, String> {
        let completion_key = completion_key.trim();
        if completion_key.is_empty() || completion_key.chars().count() > 512 {
            return Err("effectful Runtime 处理器完成键无效".to_string());
        }
        self.record_effectful_handler_completion_internal(
            token,
            workspace_scope,
            child_task_id,
            command_id,
            trace_id,
            capability_id,
            operation,
            binding,
            Some(completion_key),
            usage,
            reservation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_effectful_handler_completion_internal(
        &self,
        token: &str,
        workspace_scope: &str,
        child_task_id: &str,
        command_id: &str,
        trace_id: &str,
        capability_id: &str,
        operation: &str,
        binding: &RuntimeTaskStepCommandBinding,
        completion_key: Option<&str>,
        usage: TrustedHandlerUsage,
        reservation: TrustedHandlerReservation,
    ) -> Result<bool, String> {
        crate::task_runtime::validate_runtime_task_step_binding(binding)?;
        if usage.tool_calls == 0
            || usage.runtime_seconds == 0
            || usage
                .cost
                .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
            || reservation
                .max_cost
                .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
        {
            return Err("effectful Runtime 处理器用量或预留预算无效".to_string());
        }
        let now = SystemTime::now();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let ticket = tickets
            .get(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        validate_effectful_handler_ticket(
            ticket,
            now,
            workspace_scope,
            child_task_id,
            command_id,
            trace_id,
            capability_id,
            operation,
            binding,
        )?;
        if completion_key.is_some_and(|key| ticket.trusted_handler_completion_keys.contains(key)) {
            return Ok(false);
        }
        let ticket = tickets
            .get_mut(token.trim())
            .ok_or_else(|| "执行票据不存在或已经失效".to_string())?;
        let consumed_cost = usage.cost.unwrap_or_default();
        let next_tool_calls = ticket
            .trusted_handler_completions
            .checked_add(usage.tool_calls)
            .ok_or_else(|| "effectful Runtime 处理器工具调用用量溢出".to_string())?;
        let next_runtime_seconds = ticket
            .trusted_runtime_seconds
            .checked_add(usage.runtime_seconds)
            .ok_or_else(|| "effectful Runtime 处理器运行时间用量溢出".to_string())?;
        let next_tokens = ticket
            .trusted_tokens
            .checked_add(usage.tokens)
            .ok_or_else(|| "effectful Runtime 处理器 Token 用量溢出".to_string())?;
        let next_cost = ticket.trusted_cost + consumed_cost;
        if next_tool_calls > reservation.max_tool_calls
            || next_runtime_seconds > reservation.max_runtime_seconds
            || !next_cost.is_finite()
            || reservation
                .max_tokens
                .is_some_and(|max_tokens| next_tokens > max_tokens)
            || reservation.max_cost.is_some_and(|max_cost| {
                usage.cost.is_none()
                    || !next_cost.is_finite()
                    || next_cost > max_cost + f64::EPSILON
            })
        {
            return Err("effectful Runtime 处理器用量超过步骤预留预算".to_string());
        }
        ticket.trusted_handler_completions = next_tool_calls;
        ticket.trusted_runtime_seconds = next_runtime_seconds;
        ticket.trusted_tokens = next_tokens;
        ticket.trusted_cost = next_cost;
        ticket.trusted_cost_measured &= usage.cost.is_some();
        ticket.trusted_completed_at = Some(now);
        ticket.trusted_execution_kind = Some("effectful_native_handler".to_string());
        if let Some(key) = completion_key {
            ticket
                .trusted_handler_completion_keys
                .insert(key.to_string());
        }
        Ok(true)
    }

    pub(crate) fn cancel_runtime_task_bindings(
        &self,
        runtime_task_id: &str,
    ) -> Result<usize, String> {
        let runtime_task_id = runtime_task_id.trim();
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        let mut cancelled = 0;
        let mut cancelled_task_ids = HashSet::from([runtime_task_id.to_string()]);
        loop {
            let mut discovered = Vec::new();
            for ticket in tickets.values_mut() {
                if ticket
                    .step_binding
                    .as_ref()
                    .is_some_and(|binding| cancelled_task_ids.contains(&binding.runtime_task_id))
                    && ticket.phase != TicketPhase::Consumed
                {
                    ticket.approval_bindings.clear();
                    ticket.in_flight_approvals.clear();
                    ticket.phase = TicketPhase::Consumed;
                    discovered.push(ticket.task_id.clone());
                    cancelled += 1;
                }
            }
            let mut added = false;
            for task_id in discovered {
                added |= cancelled_task_ids.insert(task_id);
            }
            if !added {
                break;
            }
        }
        Ok(cancelled)
    }
}

#[tauri::command]
pub fn retire_execution_ticket(
    state: tauri::State<'_, ExecutionTicketState>,
    execution_ticket: String,
    task_id: String,
) -> Result<bool, String> {
    state.retire(&execution_ticket, &task_id)
}

fn validate_ticket(
    ticket: &StoredExecutionTicket,
    scope: &TicketScope<'_>,
    now: SystemTime,
) -> Result<(), String> {
    if ticket.expires_at <= now {
        return Err("执行票据已过期，请重新提交应用命令".to_string());
    }
    if matches!(ticket.policy_outcome, PolicyOutcome::Deny) {
        return Err("执行票据对应的策略决定不允许执行".to_string());
    }
    if ticket.workspace_scope != scope.workspace_scope || ticket.task_id != scope.task_id {
        return Err("执行票据与当前工作区或任务不匹配".to_string());
    }
    if scope
        .trace_id
        .is_some_and(|trace_id| trace_id != ticket.trace_id)
    {
        return Err("执行票据与当前 trace ID 不匹配".to_string());
    }
    if !scope
        .allowed_capability_ids
        .contains(&ticket.capability_id.as_str())
    {
        return Err("执行票据没有当前处理器所需能力".to_string());
    }
    if !scope
        .allowed_operations
        .contains(&ticket.operation.as_str())
    {
        return Err(format!(
            "执行票据 operation {} 与当前处理器不匹配",
            ticket.operation
        ));
    }
    if !ticket.allow_all_vaults && !ticket.allowed_vault_ids.contains(scope.vault_id) {
        return Err("执行票据的 Vault 范围与目标知识库不一致".to_string());
    }
    let relative_path = normalized_relative_path(scope.relative_path);
    if scope.require_declared_path
        && ticket.relative_paths.is_empty()
        && !ticket.allow_dynamic_paths
    {
        return Err("执行票据没有声明当前处理器要求的文件路径".to_string());
    }
    if !ticket.relative_paths.is_empty() && !ticket.relative_paths.contains(&relative_path) {
        return Err("执行票据的路径范围与目标文件不一致".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_effectful_handler_ticket(
    ticket: &StoredExecutionTicket,
    now: SystemTime,
    workspace_scope: &str,
    child_task_id: &str,
    command_id: &str,
    trace_id: &str,
    capability_id: &str,
    operation: &str,
    binding: &RuntimeTaskStepCommandBinding,
) -> Result<(), String> {
    if ticket.expires_at <= now {
        return Err("执行票据已过期，不能授权有副作用处理器".to_string());
    }
    if !matches!(ticket.policy_outcome, PolicyOutcome::Allow) {
        return Err("只有明确 Allow 的 effectful Runtime 命令可以授权原生处理器".to_string());
    }
    if ticket.workspace_scope != workspace_scope
        || ticket.task_id != child_task_id
        || ticket.command_id != command_id
        || ticket.trace_id != trace_id
        || ticket.capability_id != capability_id
        || ticket.operation != operation
        || ticket.step_binding.as_ref() != Some(binding)
    {
        return Err("执行票据与 effectful Runtime 处理器身份或步骤绑定不一致".to_string());
    }
    if ticket.phase != TicketPhase::Ready
        || ticket.uncertain_side_effect
        || ticket.trusted_completion_sealed
        || ticket
            .trusted_execution_kind
            .as_deref()
            .is_some_and(|kind| kind != "effectful_native_handler")
    {
        return Err("执行票据不能重复或在封存后授权 effectful 处理器".to_string());
    }
    Ok(())
}
