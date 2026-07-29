use crate::policy::{ApplicationCommand, PolicyDecision, PolicyOutcome};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

const EXECUTION_TICKET_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_EXECUTION_TICKETS: usize = 1_024;
const MAX_TICKET_APPROVAL_BINDINGS: usize = 2_048;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTicketReceipt {
    pub token: String,
    pub task_id: String,
    pub command_id: String,
    pub parameter_digest: String,
    pub expires_at: String,
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
    trace_id: String,
    capability_id: String,
    operation: String,
    allowed_vault_ids: HashSet<String>,
    allow_all_vaults: bool,
    relative_paths: HashSet<String>,
    allow_dynamic_paths: bool,
    allow_multiple_commits: bool,
    policy_outcome: PolicyOutcome,
    approval_bindings: HashMap<String, String>,
    in_flight_approvals: HashSet<String>,
    committed_approvals: HashSet<String>,
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
            approval_bindings: HashMap::new(),
            in_flight_approvals: HashSet::new(),
            committed_approvals: HashSet::new(),
            expires_at,
            phase: TicketPhase::Ready,
        };
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| "执行票据状态不可用".to_string())?;
        tickets.retain(|_, ticket| ticket.expires_at > timing.issued_at);
        if tickets.len() >= MAX_EXECUTION_TICKETS {
            return Err("待执行票据数量已达到上限，请稍后重试".to_string());
        }
        tickets.insert(token.clone(), stored);
        Ok(ExecutionTicketReceipt {
            token,
            task_id: task_id.to_string(),
            command_id: command.id.clone(),
            parameter_digest,
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
        ticket.phase = TicketPhase::InFlight;
        Ok(())
    }

    pub(crate) fn release_commit(&self, token: &str) {
        if let Ok(mut tickets) = self.tickets.lock() {
            if let Some(ticket) = tickets.get_mut(token.trim()) {
                if ticket.phase == TicketPhase::InFlight {
                    ticket.in_flight_approvals.clear();
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
        // An uncertain side effect must never become executable again.
        ticket.in_flight_approvals.clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{CommandBudget, CommandOrigin};
    use serde_json::json;

    fn command() -> ApplicationCommand {
        ApplicationCommand {
            id: "command-ticket".to_string(),
            command_type: "assistant.operation".to_string(),
            origin: CommandOrigin::Assistant,
            intent: "capture".to_string(),
            capability_id: "system:capture".to_string(),
            operation: "run".to_string(),
            parameters: json!({"relative_path": "分析/笔记.md"}),
            vault_id: Some("vault-agent".to_string()),
            relative_paths: vec!["分析/笔记.md".to_string()],
            network_targets: Vec::new(),
            declared_scope: vec!["vault:vault-agent".to_string()],
            budget: CommandBudget {
                max_steps: 8,
                max_runtime_seconds: 300,
                max_tool_calls: 16,
                max_tokens: None,
                max_cost: None,
            },
            idempotency_key: "ticket-idempotency".to_string(),
            trace_id: Some("trace-ticket".to_string()),
            model_decision_receipt: Some("receipt-ticket".to_string()),
        }
    }

    fn decision() -> PolicyDecision {
        crate::policy::evaluate(&command())
    }

    fn scope<'a>(task_id: &'a str, vault_id: &'a str, path: &'a str) -> TicketScope<'a> {
        TicketScope {
            workspace_scope: "local",
            task_id,
            trace_id: Some("trace-ticket"),
            allowed_capability_ids: &["system:capture", "system:create"],
            allowed_operations: &["run", "create"],
            vault_id,
            relative_path: path,
            require_declared_path: true,
        }
    }

    #[test]
    fn ticket_binds_scope_approval_and_prevents_replay() {
        let state = ExecutionTicketState::default();
        let receipt = state
            .issue(
                "local",
                "task-ticket",
                "trace-ticket",
                &command(),
                &decision(),
            )
            .expect("issue ticket");
        state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "分析/笔记.md"),
                "approval-1",
                "effect-1",
            )
            .expect("bind approval");
        state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-1")],
            )
            .expect("begin commit");
        state
            .complete_commit(&receipt.token)
            .expect("complete commit");
        assert!(state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-1")],
            )
            .is_err());
    }

    #[test]
    fn ticket_rejects_task_vault_path_and_digest_substitution() {
        let state = ExecutionTicketState::default();
        let receipt = state
            .issue(
                "local",
                "task-ticket",
                "trace-ticket",
                &command(),
                &decision(),
            )
            .expect("issue ticket");
        for invalid_scope in [
            scope("task-other", "vault-agent", "分析/笔记.md"),
            scope("task-ticket", "vault-personal", "分析/笔记.md"),
            scope("task-ticket", "vault-agent", "分析/替换.md"),
        ] {
            assert!(state
                .bind_approval(&receipt.token, invalid_scope, "approval-1", "effect-1")
                .is_err());
        }
        state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "分析/笔记.md"),
                "approval-1",
                "effect-1",
            )
            .expect("bind valid approval");
        assert!(state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-substituted")],
            )
            .is_err());
    }

    #[test]
    fn expired_ticket_is_rejected() {
        let state = ExecutionTicketState::default();
        let receipt = state
            .issue_at(
                "local",
                "task-ticket",
                "trace-ticket",
                &command(),
                &decision(),
                TicketTiming {
                    issued_at: SystemTime::UNIX_EPOCH,
                    ttl: Duration::from_secs(1),
                },
            )
            .expect("issue expired ticket fixture");
        assert!(state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "分析/笔记.md"),
                "approval-1",
                "effect-1",
            )
            .expect_err("expired ticket must fail")
            .contains("过期"));
    }

    #[test]
    fn path_bound_handler_rejects_a_wildcard_ticket() {
        let state = ExecutionTicketState::default();
        let mut wildcard = command();
        wildcard.intent = "create".to_string();
        wildcard.capability_id = "system:create".to_string();
        wildcard.operation = "create".to_string();
        wildcard.relative_paths.clear();
        let decision = crate::policy::evaluate(&wildcard);
        let receipt = state
            .issue("local", "task-ticket", "trace-ticket", &wildcard, &decision)
            .expect("issue wildcard ticket");
        assert!(state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "分析/笔记.md"),
                "approval-1",
                "effect-1",
            )
            .expect_err("path-bound handler must reject wildcard")
            .contains("没有声明"));
    }

    #[test]
    fn capture_ticket_binds_dynamic_paths_across_batches_and_retires() {
        let state = ExecutionTicketState::default();
        let mut capture = command();
        capture.relative_paths.clear();
        capture.vault_id = Some("vault-personal".to_string());
        capture.declared_scope = vec![
            "vault:vault-personal".to_string(),
            "vault:vault-agent".to_string(),
        ];
        let decision = crate::policy::evaluate(&capture);
        let receipt = state
            .issue("local", "task-ticket", "trace-ticket", &capture, &decision)
            .expect("issue capture ticket");

        state
            .bind_approval(
                &receipt.token,
                scope(
                    "task-ticket",
                    "vault-personal",
                    "资料库/原文/hash/来源一.md",
                ),
                "approval-1",
                "effect-1",
            )
            .expect("bind first source");
        state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-1")],
            )
            .expect("begin first source");
        state
            .complete_commit(&receipt.token)
            .expect("complete first source");

        state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "资料库/原文/hash/来源二.md"),
                "approval-2",
                "effect-2",
            )
            .expect("bind second source");
        state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-2", "effect-2")],
            )
            .expect("begin second source");
        state
            .complete_commit(&receipt.token)
            .expect("complete second source");

        assert!(state
            .bind_approval(
                &receipt.token,
                scope(
                    "task-ticket",
                    "vault-personal",
                    "资料库/原文/hash/来源一.md"
                ),
                "approval-1",
                "effect-1",
            )
            .expect_err("committed approval must not replay")
            .contains("重放"));
        assert!(state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-other", "资料库/原文/hash/越界.md"),
                "approval-3",
                "effect-3",
            )
            .expect_err("dynamic path must remain inside declared vaults")
            .contains("Vault 范围"));

        assert!(state
            .retire(&receipt.token, "task-ticket")
            .expect("retire capture ticket"));
        assert!(state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "资料库/原文/hash/退役后.md"),
                "approval-4",
                "effect-4",
            )
            .is_err());
    }

    #[test]
    fn uncertain_commit_is_terminal() {
        let state = ExecutionTicketState::default();
        let receipt = state
            .issue(
                "local",
                "task-ticket",
                "trace-ticket",
                &command(),
                &decision(),
            )
            .expect("issue ticket");
        state
            .bind_approval(
                &receipt.token,
                scope("task-ticket", "vault-agent", "分析/笔记.md"),
                "approval-1",
                "effect-1",
            )
            .expect("bind approval");
        state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-1")],
            )
            .expect("begin commit");
        state.fail_commit(&receipt.token).expect("seal ticket");
        assert!(state
            .begin_commit(
                &receipt.token,
                "local",
                "task-ticket",
                &[("approval-1", "effect-1")],
            )
            .is_err());
    }
}
