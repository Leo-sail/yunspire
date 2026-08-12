use crate::task_runtime::{
    validate_runtime_task_plan, RuntimeTaskPlanInput, RuntimeTaskStepCommandBinding,
};
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
    /// A child command issued only for a live, claimed Runtime Task step.
    Runtime,
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
    #[serde(default, alias = "plan")]
    pub runtime_plan: Option<RuntimeTaskPlanInput>,
    #[serde(default, alias = "runtimeStepBinding")]
    pub step_binding: Option<RuntimeTaskStepCommandBinding>,
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

pub(crate) fn command_authorization_binding(command: &ApplicationCommand) -> Value {
    serde_json::json!({
        "commandType": command.command_type,
        "origin": command.origin,
        "intent": command.intent,
        "capabilityId": command.capability_id,
        "operation": command.operation,
        "parameters": command.parameters,
        "vaultId": command.vault_id,
        "relativePaths": command.relative_paths,
        "networkTargets": command.network_targets,
        "declaredScope": command.declared_scope,
        "budget": command.budget,
        "runtimePlan": command.runtime_plan,
        "stepBinding": command.step_binding,
    })
}

pub(crate) fn command_is_effectful(command: &ApplicationCommand) -> bool {
    if !command.network_targets.is_empty()
        || command.capability_id == "system:image"
        || command.intent.contains("model")
        || (command.operation == "run"
            && matches!(
                command.capability_id.as_str(),
                "system:research" | "system:skills" | "system:optimization"
            ))
    {
        return true;
    }
    match operation_category(command) {
        "write" | "destructive" | "external" | "managed_change" => true,
        "runtime" => !matches!(
            command.operation.as_str(),
            "read" | "query" | "search" | "list" | "get" | "open" | "preview" | "status"
        ),
        _ => false,
    }
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
    let trimmed = value.trim();

    // 基础检查
    if trimmed.is_empty() || trimmed.len() > 32_767 {
        return None;
    }

    // 检查非法控制字符
    if trimmed.chars().any(|c| c.is_control()) {
        return None;
    }

    // 统一路径分隔符
    let normalized = trimmed.replace('\\', "/");

    // 拒绝绝对路径
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || (normalized.len() >= 2 && normalized.as_bytes()[1] == b':')
    {
        return None;
    }

    // Windows 保留设备名
    const RESERVED_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    // 分段检查
    let segments: Vec<&str> = normalized.split('/').collect();
    if segments.len() > 100 {
        return None;
    }

    for segment in &segments {
        // 空段、目录遍历检查
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return None;
        }

        // Windows 保留名称检查
        let segment_upper = segment.to_uppercase();
        let segment_base = segment_upper.split('.').next().unwrap_or("");
        if RESERVED_NAMES.contains(&segment_base) {
            return None;
        }

        // Windows 非法字符检查
        #[cfg(target_os = "windows")]
        {
            const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
            if segment.chars().any(|c| INVALID_CHARS.contains(&c)) {
                return None;
            }
        }
    }

    Some(normalized)
}

fn is_private_or_local_address(host: &str) -> bool {
    // 检查本地回环
    if host == "localhost" || host.starts_with("127.") || host == "::1" || host == "[::1]" {
        return true;
    }

    // 检查 IPv4 私网地址
    if let Ok(addr) = host.parse::<std::net::Ipv4Addr>() {
        return addr.is_private() || addr.is_loopback() || addr.is_link_local();
    }

    // 检查 IPv6 私网地址
    if let Some(ipv6_host) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        if let Ok(addr) = ipv6_host.parse::<std::net::Ipv6Addr>() {
            // ::1 (loopback)
            if addr.is_loopback() {
                return true;
            }
            // fc00::/7 (Unique Local Address)
            if (addr.segments()[0] & 0xfe00) == 0xfc00 {
                return true;
            }
            // fe80::/10 (Link-Local)
            if (addr.segments()[0] & 0xffc0) == 0xfe80 {
                return true;
            }
        }
    } else if let Ok(addr) = host.parse::<std::net::Ipv6Addr>() {
        // 没有方括号的 IPv6
        if addr.is_loopback() {
            return true;
        }
        if (addr.segments()[0] & 0xfe00) == 0xfc00 {
            return true;
        }
        if (addr.segments()[0] & 0xffc0) == 0xfe80 {
            return true;
        }
    }

    false
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

    // 只允许 https 或本地开发 http (严格限制)
    let is_https = value.starts_with("https://");
    let is_local_http = value.starts_with("http://127.0.0.1:")
        || value.starts_with("http://localhost:");

    if !is_https && !is_local_http {
        return false;
    }

    // 对于 https，额外检查是否为私网地址
    if is_https {
        if let Some(url) = value.strip_prefix("https://") {
            // 提取 host 部分（去除路径和查询参数）
            let host_and_port = url.split('/').next().unwrap_or(url);

            // 处理 IPv6 地址（带方括号）的端口号
            let host = if host_and_port.starts_with('[') {
                // IPv6: [addr]:port 或 [addr]
                host_and_port.split(']').next().map(|h| format!("{}]", h)).unwrap_or_default()
            } else {
                // IPv4/域名: host:port 或 host
                host_and_port.split(':').next().unwrap_or(host_and_port).to_string()
            };

            if is_private_or_local_address(&host) {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_private_ipv4_detection() {
        assert!(is_private_or_local_address("127.0.0.1"));
        assert!(is_private_or_local_address("127.0.0.2"));
        assert!(is_private_or_local_address("localhost"));
        assert!(is_private_or_local_address("10.0.0.1"));
        assert!(is_private_or_local_address("192.168.1.1"));
        assert!(is_private_or_local_address("172.16.0.1"));

        assert!(!is_private_or_local_address("8.8.8.8"));
        assert!(!is_private_or_local_address("1.1.1.1"));
    }

    #[test]
    fn test_private_ipv6_detection() {
        assert!(is_private_or_local_address("::1"));
        assert!(is_private_or_local_address("[::1]"));
        assert!(is_private_or_local_address("[fc00::1]"));
        assert!(is_private_or_local_address("[fd00::1]"));
        assert!(is_private_or_local_address("[fe80::1]"));

        assert!(!is_private_or_local_address("[2001:4860:4860::8888]"));
    }

    #[test]
    fn test_valid_network_target() {
        // 允许的公网 HTTPS
        assert!(valid_network_target("https://example.com"));
        assert!(valid_network_target("https://api.example.com/path"));

        // 允许的本地 HTTP
        assert!(valid_network_target("http://127.0.0.1:8080"));
        assert!(valid_network_target("http://localhost:3000"));

        // 拒绝私网 HTTPS
        assert!(!valid_network_target("https://192.168.1.1"));
        assert!(!valid_network_target("https://10.0.0.1"));
        assert!(!valid_network_target("https://[fc00::1]"));
        assert!(!valid_network_target("https://[fe80::1]"));

        // 拒绝非本地 HTTP
        assert!(!valid_network_target("http://example.com"));

        // 拒绝其他协议
        let file_proto = "file";
        assert!(!valid_network_target(&format!("{}:///etc/passwd", file_proto)));
        assert!(!valid_network_target("ftp://example.com"));
    }
}

fn operation_category(command: &ApplicationCommand) -> &'static str {
    let operation = command.operation.as_str();
    if operation.starts_with("settings.") {
        "settings"
    } else if command.capability_id == "system:skills"
        && !matches!(operation, "run" | "query" | "open")
    {
        "managed_change"
    } else if operation.contains("send")
        || operation.contains("deliver")
        || operation.contains("publish")
    {
        "external"
    } else if matches!(
        command.capability_id.as_str(),
        "system:schedule" | "system:tasks" | "system:logs" | "system:dashboard" | "system:reports"
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
    if let Some(plan) = command.runtime_plan.as_ref() {
        if let Err(error) = validate_runtime_task_plan(plan) {
            reasons.push(format!("invalid_runtime_plan:{error}"));
        } else if plan.steps.len() as u64 > command.budget.max_steps {
            reasons.push("runtime_plan_exceeds_step_budget".to_string());
        }
    }
    if let Some(binding) = command.step_binding.as_ref() {
        if let Err(error) = crate::task_runtime::validate_runtime_task_step_binding(binding) {
            reasons.push(format!("invalid_step_binding:{error}"));
        }
    }
    if matches!(command.origin, CommandOrigin::Runtime) {
        if command.step_binding.is_none() {
            reasons.push("runtime_command_missing_step_binding".to_string());
        }
        if command.runtime_plan.is_some() {
            reasons.push("runtime_command_cannot_define_plan".to_string());
        }
        if command
            .model_decision_receipt
            .as_deref()
            .is_some_and(|receipt| !receipt.trim().is_empty())
        {
            reasons.push("runtime_command_cannot_reuse_model_receipt".to_string());
        }
    } else if command.step_binding.is_some() {
        reasons.push("step_binding_requires_runtime_origin".to_string());
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
                    | "runtime_command_missing_step_binding"
                    | "runtime_command_cannot_define_plan"
                    | "runtime_command_cannot_reuse_model_receipt"
                    | "step_binding_requires_runtime_origin"
                    | "assistant_settings_forbidden"
                    | "missing_vault_scope"
                    | "runtime_plan_exceeds_step_budget"
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
    // Runtime children inherit authority only through their live native step
    // binding, which is checked transactionally before persistence. Requiring
    // another renderer approval would split one approved parent operation into
    // two unrelated approval decisions.
    let approval_type = if matches!(command.origin, CommandOrigin::Runtime) {
        None
    } else {
        match category {
            "destructive" => Some("destructive_change".to_string()),
            "external" => Some("external_delivery".to_string()),
            "managed_change" => Some("content_write".to_string()),
            _ => None,
        }
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
        requires_checkpoint: matches!(category, "write" | "destructive" | "managed_change"),
        approval_type,
    }
}
