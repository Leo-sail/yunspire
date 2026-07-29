use crate::{
    execution_ticket::{ExecutionTicketReceipt, ExecutionTicketState},
    model_provider::ModelIntentState,
    policy::{self, ApplicationCommand, PolicyDecision, PolicyOutcome},
    runtime_db::RuntimeDatabase,
};
use chrono::Utc;
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    command_id: String,
    task_id: Option<String>,
    trace_id: String,
    duplicate: bool,
    decision: PolicyDecision,
    execution_ticket: Option<ExecutionTicketReceipt>,
    accepted_at: String,
}

#[tauri::command]
pub fn submit_application_command(
    database: State<'_, RuntimeDatabase>,
    intent_state: State<'_, ModelIntentState>,
    ticket_state: State<'_, ExecutionTicketState>,
    command: ApplicationCommand,
) -> Result<CommandReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let decision = policy::evaluate(&command);
    let trace_id = command
        .trace_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(crate::trace::new_trace_id);
    crate::trace::validate_trace_id(&trace_id)?;
    let accepted_at = Utc::now().to_rfc3339();
    let persist = || -> Result<(Option<String>, bool), String> {
        let result = database.persist_application_command(
            &workspace_scope,
            &command,
            &decision,
            &trace_id,
            &accepted_at,
        )?;
        Ok(result)
    };
    let (task_id, duplicate) = if matches!(command.origin, crate::policy::CommandOrigin::Assistant)
        && !matches!(decision.outcome, PolicyOutcome::Deny)
    {
        let receipt = command
            .model_decision_receipt
            .as_deref()
            .ok_or_else(|| "执行前缺少模型意图凭证".to_string())?;
        intent_state.consume_after(
            "local",
            receipt,
            &command.intent,
            &command.capability_id,
            &command.operation,
            &command.parameters,
            persist,
        )?
    } else {
        persist()?
    };
    // A duplicate command already owns the original execution grant. Minting a
    // second token would turn an idempotent retry into a replay primitive.
    let execution_ticket = if duplicate {
        None
    } else {
        task_id
            .as_deref()
            .map(|task_id| {
                ticket_state.issue(&workspace_scope, task_id, &trace_id, &command, &decision)
            })
            .transpose()?
    };
    Ok(CommandReceipt {
        command_id: command.id,
        task_id,
        trace_id,
        duplicate,
        decision,
        execution_ticket,
        accepted_at,
    })
}

#[tauri::command]
pub fn evaluate_application_command(command: ApplicationCommand) -> PolicyDecision {
    policy::evaluate(&command)
}
