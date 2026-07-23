use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

const MAX_COMMAND_BYTES: usize = 512 * 1024;
const MAX_DECLARED_TARGETS: usize = 128;
const MAX_BUDGET_STEPS: u64 = 512;
const MAX_BUDGET_TOOL_CALLS: u64 = 2_048;
const MAX_BUDGET_RUNTIME_SECONDS: u64 = 86_400;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOrigin {
    DirectUser,
    Assistant,
    Schedule,
    SystemMaintenance,
    Evolution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandBudget {
    pub max_steps: u64,
    pub max_runtime_seconds: u64,
    pub max_tool_calls: u64,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_cost: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCommand {
    pub id: String,
    pub command_type: String,
    pub origin: CommandOrigin,
    pub intent: String,
    pub capability_id: String,
    pub operation: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub relative_paths: Vec<String>,
    #[serde(default)]
    pub network_targets: Vec<String>,
    #[serde(default)]
    pub declared_scope: Vec<String>,
    pub budget: CommandBudget,
    pub idempotency_key: String,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub model_decision_receipt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allow,
    Deny,
    RequireApproval,
    AllowWithReducedScope,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub outcome: PolicyOutcome,
    pub reason_codes: Vec<String>,
    pub normalized_scope: Vec<String>,
    pub requires_checkpoint: bool,
    pub approval_type: Option<String>,
}

fn valid_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn normalize_relative_path(value: &str) -> Option<String> {
    let normalized = value.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains('\0')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(normalized)
}

fn valid_network_target(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 2048
        || value
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return false;
    }
    value.starts_with("https://")
        || value.starts_with("http://127.0.0.1:")
        || value.starts_with("http://localhost:")
}

fn operation_category(command: &ApplicationCommand) -> &'static str {
    let operation = command.operation.as_str();
    if operation.starts_with("settings.") {
        "settings"
    } else if operation.contains("send")
        || operation.contains("deliver")
        || operation.contains("publish")
    {
        "external"
    } else if matches!(
        command.capability_id.as_str(),
        "system:schedule"
            | "system:skills"
            | "system:tasks"
            | "system:logs"
            | "system:dashboard"
            | "system:reports"
    ) {
        "runtime"
    } else if command.intent == "delete"
        || command.capability_id == "system:delete"
        || operation.contains("delete")
    {
        "destructive"
    } else if operation.contains("write")
        || operation.contains("create")
        || operation.contains("update")
        || operation.contains("move")
        || operation.contains("rename")
        || operation.contains("save")
    {
        "write"
    } else {
        "read"
    }
}

pub fn evaluate(command: &ApplicationCommand) -> PolicyDecision {
    let mut reasons = Vec::new();
    let mut normalized_scope = Vec::new();
    let encoded_len = serde_json::to_vec(command)
        .map(|value| value.len())
        .unwrap_or(usize::MAX);
    if encoded_len > MAX_COMMAND_BYTES {
        reasons.push("command_too_large".to_string());
    }
    for (label, value) in [
        ("command_id", command.id.as_str()),
        ("command_type", command.command_type.as_str()),
        ("intent", command.intent.as_str()),
        ("capability_id", command.capability_id.as_str()),
        ("operation", command.operation.as_str()),
        ("idempotency_key", command.idempotency_key.as_str()),
    ] {
        if !valid_identifier(value, 180) {
            reasons.push(format!("invalid_{label}"));
        }
    }
    if command.budget.max_steps == 0 || command.budget.max_steps > MAX_BUDGET_STEPS {
        reasons.push("invalid_max_steps".to_string());
    }
    if command.budget.max_tool_calls > MAX_BUDGET_TOOL_CALLS {
        reasons.push("invalid_max_tool_calls".to_string());
    }
    if command.budget.max_runtime_seconds == 0
        || command.budget.max_runtime_seconds > MAX_BUDGET_RUNTIME_SECONDS
    {
        reasons.push("invalid_max_runtime_seconds".to_string());
    }
    if command
        .budget
        .max_cost
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        reasons.push("invalid_max_cost".to_string());
    }
    if command.relative_paths.len() > MAX_DECLARED_TARGETS
        || command.network_targets.len() > MAX_DECLARED_TARGETS
        || command.declared_scope.len() > MAX_DECLARED_TARGETS
    {
        reasons.push("too_many_declared_targets".to_string());
    }
    let mut unique = HashSet::new();
    for path in &command.relative_paths {
        match normalize_relative_path(path) {
            Some(path) if unique.insert(format!("path:{path}")) => normalized_scope.push(path),
            Some(_) => {}
            None => reasons.push("invalid_relative_path".to_string()),
        }
    }
    for target in &command.network_targets {
        if valid_network_target(target) {
            if unique.insert(format!("network:{target}")) {
                normalized_scope.push(target.trim().to_string());
            }
        } else {
            reasons.push("invalid_network_target".to_string());
        }
    }
    if matches!(command.origin, CommandOrigin::Assistant)
        && command
            .model_decision_receipt
            .as_deref()
            .is_none_or(str::is_empty)
    {
        reasons.push("missing_model_decision_receipt".to_string());
    }
    if !matches!(command.origin, CommandOrigin::DirectUser)
        && (command.capability_id == "system:settings"
            || command.operation.starts_with("settings."))
    {
        reasons.push("assistant_settings_forbidden".to_string());
    }
    let category = operation_category(command);
    if matches!(category, "write" | "destructive")
        && command.vault_id.as_deref().is_none_or(str::is_empty)
    {
        reasons.push("missing_vault_scope".to_string());
    }
    let denied = reasons.iter().any(|reason| {
        reason.starts_with("invalid_")
            || matches!(
                reason.as_str(),
                "command_too_large"
                    | "too_many_declared_targets"
                    | "missing_model_decision_receipt"
                    | "assistant_settings_forbidden"
                    | "missing_vault_scope"
            )
    });
    if denied {
        return PolicyDecision {
            outcome: PolicyOutcome::Deny,
            reason_codes: reasons,
            normalized_scope,
            requires_checkpoint: false,
            approval_type: None,
        };
    }
    let approval_type = match category {
        "destructive" => Some("destructive_change".to_string()),
        "external" => Some("external_delivery".to_string()),
        _ => None,
    };
    PolicyDecision {
        outcome: if approval_type.is_some() {
            PolicyOutcome::RequireApproval
        } else {
            PolicyOutcome::Allow
        },
        reason_codes: if reasons.is_empty() {
            vec!["policy_validated".to_string()]
        } else {
            reasons
        },
        normalized_scope,
        requires_checkpoint: matches!(category, "write" | "destructive"),
        approval_type,
    }
}
