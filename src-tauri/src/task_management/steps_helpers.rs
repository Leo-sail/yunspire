/// 任务步骤管理 - 辅助函数
///
/// 从 runtime_db.rs 提取的步骤管理私有辅助函数

use crate::task_management::types::RuntimeTaskPlanStepRecord;
use crate::task_runtime::{RuntimeTaskStepEffectClass, RuntimeTaskStepKind};
use rusqlite::{params, Connection, OptionalExtension};

/// 验证运行时标识符
pub(crate) fn valid_runtime_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

/// 检查 SQLite i64 转换
pub(crate) fn checked_sqlite_i64(value: u64, context: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{} 超出 SQLite INTEGER 范围", context))
}

/// 获取最新的任务计划版本
pub(crate) fn latest_runtime_task_plan_revision(
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

/// 加载任务计划步骤记录
pub(crate) fn load_runtime_task_plan_step_records(
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
        .map_err(|error| format!("无法读取原生任务计划步骤行：{error}"))?;

    let mut records = Vec::new();
    for row in rows {
        let (step_id, _position, step_kind_str, title, depends_on_json, parameters_json) =
            row.map_err(|error| format!("无法解析原生任务计划步骤行：{error}"))?;

        let depends_on: Option<Vec<String>> = serde_json::from_str(&depends_on_json)
            .map_err(|error| format!("无法解析步骤依赖：{error}"))?;

        let parameters: serde_json::Value = serde_json::from_str(&parameters_json)
            .map_err(|error| format!("无法解析步骤参数：{error}"))?;

        let step_kind: RuntimeTaskStepKind = serde_json::from_str(&format!("\"{}\"", step_kind_str))
            .unwrap_or(RuntimeTaskStepKind::Model);

        records.push(RuntimeTaskPlanStepRecord {
            step_id,
            step_kind,
            title,
            depends_on,
            parameters,
            effect_class: RuntimeTaskStepEffectClass::Effectful,
        });
    }

    Ok(records)
}

/// 获取最新的任务步骤状态
pub(crate) fn latest_runtime_task_step_states(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: i64,
) -> Result<std::collections::HashMap<String, (String, RuntimeTaskStepEffectClass)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT step_id, state, effect_class
             FROM runtime_task_step_states
             WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
        )
        .map_err(|error| format!("无法读取原生任务步骤状态：{error}"))?;

    let rows = statement
        .query_map(params![workspace_scope, task_id, plan_revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("无法读取原生任务步骤状态行：{error}"))?;

    let mut states = std::collections::HashMap::new();
    for row in rows {
        let (step_id, state, effect_class_str) =
            row.map_err(|error| format!("无法解析原生任务步骤状态行：{error}"))?;

        let effect_class: RuntimeTaskStepEffectClass =
            serde_json::from_str(&format!("\"{}\"", effect_class_str))
                .unwrap_or(RuntimeTaskStepEffectClass::Effectful);

        states.insert(step_id, (state, effect_class));
    }

    Ok(states)
}

/// 过期任务步骤认领
pub(crate) fn expire_runtime_task_step_claims(
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
                     reserved_tool_calls=MAX(0, reserved_tool_calls-?5),
                     reserved_runtime_seconds=MAX(0, reserved_runtime_seconds-?6),
                     reserved_tokens=MAX(0, reserved_tokens-?7),
                     reserved_cost=MAX(0.0, reserved_cost-?8)
                 WHERE workspace_scope=?1 AND task_id=?2 AND plan_revision=?3",
                params![
                    workspace_scope,
                    task_id,
                    plan_revision,
                    now,
                    tool_calls,
                    runtime_seconds,
                    tokens,
                    cost
                ],
            )
            .map_err(|error| format!("无法释放过期原生任务步骤预算：{error}"))?;

        let _ = step_id; // 使用变量避免警告
    }

    Ok(())
}
