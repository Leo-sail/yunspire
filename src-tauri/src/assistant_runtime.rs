use crate::{
    memory,
    model_config::{assistant_context_budget_from_snapshot, AssistantContextBudget},
    prompt::{prompt_text, render_prompt_template},
    runtime_db::RuntimeDatabase,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::State;

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODEL_CONFIG_BYTES: usize = 64 * 1024;
const DEFAULT_CONTEXT_PAGE_BYTES: usize = 16 * 1024 * 1024;
const CONTEXT_PAGE_TOKEN_RESERVE: usize = 16_384;
const ATTACHMENT_UNNAMED_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-unnamed.txt");
const ATTACHMENT_DEFAULT_KIND_PROMPT: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-default-kind.txt");
const ATTACHMENT_UNREAD_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-unread.template.txt");
const ATTACHMENT_IMAGE_HEADER_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-image-header.template.txt");
const ATTACHMENT_VISIBLE_TEXT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-visible-text.template.txt");
const ATTACHMENT_RECORDS_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/assistant-runtime/attachment-records.template.txt");

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequestInput {
    request_id: String,
    conversation_id: String,
    conversation_revision: i64,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    model_config: Value,
    #[serde(default)]
    has_volatile_attachments: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContextInput {
    request_id: String,
    conversation_revision: i64,
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    attachment_context: Value,
    #[serde(default)]
    latest_user_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequestCompletionInput {
    request_id: String,
    state: String,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantConversationRevisionInput {
    conversation_id: String,
    expected_revision: i64,
    next_revision: i64,
    #[serde(default)]
    keep_request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequestRecord {
    request_id: String,
    conversation_id: String,
    conversation_revision: i64,
    sequence: i64,
    state: String,
    payload: Value,
    context_hash: Option<String>,
    has_volatile_attachments: bool,
    recovery_count: i64,
    last_error: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRequestClaim {
    claim_granted: bool,
    request: AssistantRequestRecord,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContextReceipt {
    request_id: String,
    conversation_id: String,
    conversation_revision: i64,
    context_hash: String,
    messages: Vec<Value>,
    omitted_message_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantConversationRevisionReceipt {
    conversation_id: String,
    revision: i64,
    cancelled_requests: usize,
}

fn valid_id(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn validate_ids(request_id: &str, conversation_id: &str) -> Result<(), String> {
    if !valid_id(request_id, 160) {
        return Err("AI助手请求 ID 格式无效".to_string());
    }
    if !valid_id(conversation_id, 200) {
        return Err("AI助手对话 ID 格式无效".to_string());
    }
    Ok(())
}

fn serialize_bounded(value: &Value, limit: usize, label: &str) -> Result<String, String> {
    let serialized =
        serde_json::to_string(value).map_err(|error| format!("无法序列化{label}：{error}"))?;
    if serialized.len() > limit {
        return Err(format!("{label}超过本地安全上限"));
    }
    Ok(serialized)
}

fn contains_model_secret(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
            matches!(
                normalized.as_str(),
                "apikey" | "authorization" | "password" | "secret" | "token"
            ) || contains_model_secret(value)
        }),
        Value::Array(values) => values.iter().any(contains_model_secret),
        _ => false,
    }
}

struct FrozenRequestPayload {
    payload_json: String,
    messages: Vec<Value>,
}

fn assistant_memory_scope(conversation_id: &str) -> memory::MemoryScope {
    memory::MemoryScope {
        session_id: conversation_id.to_string(),
        ..memory::MemoryScope::default()
    }
}

fn freeze_assistant_memory_scope(
    payload: &mut serde_json::Map<String, Value>,
    conversation_id: &str,
) -> Result<(), String> {
    let expected = assistant_memory_scope(conversation_id);
    if let Some(declared) = payload
        .get("memoryScope")
        .or_else(|| payload.get("memory_scope"))
    {
        let declared = serde_json::from_value::<memory::MemoryScope>(declared.clone())
            .map_err(|error| format!("AI助手记忆作用域无效：{error}"))?;
        if declared != expected {
            return Err("AI助手记忆作用域必须绑定当前对话，不能跨会话提交".to_string());
        }
    }
    payload.remove("memory_scope");
    payload.insert(
        "memoryScope".to_string(),
        serde_json::to_value(expected)
            .map_err(|error| format!("无法冻结 AI助手记忆作用域：{error}"))?,
    );
    Ok(())
}

fn frozen_request_payload(
    payload: &Value,
    model_config: &Value,
    trace_id: &str,
    conversation_id: &str,
) -> Result<FrozenRequestPayload, String> {
    let mut payload = payload
        .as_object()
        .cloned()
        .ok_or_else(|| "AI助手请求恢复信息必须是对象".to_string())?;
    freeze_assistant_memory_scope(&mut payload, conversation_id)?;
    if !payload.contains_key("traceId") && !payload.contains_key("trace_id") {
        payload.insert("traceId".to_string(), Value::String(trace_id.to_string()));
    }
    let snapshot = if model_config.is_null() {
        payload
            .get("modelConfig")
            .or_else(|| payload.get("model_config"))
    } else {
        Some(model_config)
    };
    if let Some(snapshot) = snapshot {
        if contains_model_secret(snapshot) {
            return Err("AI助手模型快照不能包含密钥或令牌".to_string());
        }
        serialize_bounded(snapshot, MAX_MODEL_CONFIG_BYTES, "AI助手模型快照")?;
    }
    if !model_config.is_null() {
        payload.insert("modelConfig".to_string(), model_config.clone());
    }
    let messages = if let Some(messages) = payload
        .remove("conversationMessages")
        .or_else(|| payload.remove("conversation_messages"))
    {
        let messages = messages
            .as_array()
            .ok_or_else(|| "AI助手持久化对话快照必须是消息数组".to_string())?;
        messages.clone()
    } else {
        Vec::new()
    };
    serialize_bounded(
        &Value::Object(payload.clone()),
        MAX_REQUEST_BYTES,
        "AI助手请求恢复元数据",
    )?;
    Ok(FrozenRequestPayload {
        payload_json: serde_json::to_string(&Value::Object(payload))
            .map_err(|error| format!("无法序列化 AI助手请求恢复信息：{error}"))?,
        messages,
    })
}

fn request_trace_id(payload: &Value, request_id: &str) -> Result<String, String> {
    let trace_id = payload
        .get("traceId")
        .or_else(|| payload.get("trace_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let digest = format!("{:x}", Sha256::digest(request_id.as_bytes()));
            format!("trace-assistant-{}", &digest[..32])
        });
    crate::trace::validate_trace_id(&trace_id)?;
    Ok(trace_id)
}

fn request_from_row(row: &Row<'_>) -> rusqlite::Result<AssistantRequestRecord> {
    let payload_json = row.get::<_, String>(5)?;
    Ok(AssistantRequestRecord {
        request_id: row.get(0)?,
        conversation_id: row.get(1)?,
        conversation_revision: row.get(2)?,
        sequence: row.get(3)?,
        state: row.get(4)?,
        payload: serde_json::from_str(&payload_json).unwrap_or(Value::Null),
        context_hash: row.get(6)?,
        has_volatile_attachments: row.get::<_, i64>(7)? != 0,
        recovery_count: row.get(8)?,
        last_error: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn read_request(
    connection: &Connection,
    scope: &str,
    request_id: &str,
) -> Result<AssistantRequestRecord, String> {
    connection
        .query_row(
            "SELECT id, conversation_id, conversation_revision, sequence, state,
                    payload_json, context_hash, has_volatile_attachments, recovery_count,
                    last_error, created_at, updated_at
             FROM assistant_requests WHERE workspace_scope=?1 AND id=?2",
            params![scope, request_id],
            request_from_row,
        )
        .optional()
        .map_err(|error| format!("无法读取 AI助手请求：{error}"))?
        .ok_or_else(|| "AI助手请求不存在".to_string())
}

fn load_request_messages(
    connection: &Connection,
    scope: &str,
    request_id: &str,
) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare(
            "SELECT payload_json FROM assistant_request_messages
             WHERE workspace_scope=?1 AND request_id=?2 ORDER BY ordinal",
        )
        .map_err(|error| format!("无法准备 AI助手请求消息读取：{error}"))?;
    let messages = statement
        .query_map(params![scope, request_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取 AI助手请求消息：{error}"))?
        .map(|row| {
            let payload = row.map_err(|error| format!("无法解析 AI助手请求消息行：{error}"))?;
            serde_json::from_str::<Value>(&payload)
                .map_err(|error| format!("AI助手请求消息已经损坏：{error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !messages.is_empty() {
        return Ok(messages);
    }
    let payload = connection
        .query_row(
            "SELECT payload_json FROM assistant_requests
             WHERE workspace_scope=?1 AND id=?2",
            params![scope, request_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取旧版 AI助手请求恢复信息：{error}"))?;
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let payload = serde_json::from_str::<Value>(&payload)
        .map_err(|error| format!("旧版 AI助手请求恢复信息已经损坏：{error}"))?;
    Ok(payload
        .get("conversationMessages")
        .or_else(|| payload.get("conversation_messages"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn store_request_messages(
    connection: &Connection,
    scope: &str,
    request_id: &str,
    messages: &[Value],
) -> Result<(), String> {
    for (ordinal, message) in messages.iter().enumerate() {
        let payload_json = serde_json::to_string(message)
            .map_err(|error| format!("无法序列化 AI助手请求消息：{error}"))?;
        connection
            .execute(
                "INSERT INTO assistant_request_messages
                 (workspace_scope, request_id, ordinal, payload_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(workspace_scope, request_id, ordinal) DO UPDATE SET
                   payload_json=excluded.payload_json",
                params![scope, request_id, ordinal as i64, payload_json],
            )
            .map_err(|error| format!("无法持久化 AI助手请求消息：{error}"))?;
    }
    Ok(())
}

fn compact_request_payload_json(payload_json: &str) -> Result<String, String> {
    let mut payload = serde_json::from_str::<Value>(payload_json)
        .map_err(|error| format!("AI助手请求恢复信息已经损坏：{error}"))?;
    if let Some(payload) = payload.as_object_mut() {
        payload.remove("conversationMessages");
        payload.remove("conversation_messages");
    }
    serde_json::to_string(&payload)
        .map_err(|error| format!("无法规范化 AI助手请求恢复信息：{error}"))
}

fn load_frozen_context(
    connection: &Connection,
    scope: &str,
    request_id: &str,
) -> Result<Option<(String, Vec<Value>)>, String> {
    let frozen = connection
        .query_row(
            "SELECT context_json, context_hash FROM assistant_requests
             WHERE workspace_scope=?1 AND id=?2",
            params![scope, request_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .map_err(|error| format!("无法读取 AI助手冻结上下文：{error}"))?;
    let (context_json, context_hash) = match frozen {
        (None, None) => return Ok(None),
        (Some(context_json), Some(context_hash)) => (context_json, context_hash),
        _ => return Err("AI助手请求冻结上下文不完整，无法安全继续".to_string()),
    };
    let expected_hash = format!("sha256:{:x}", Sha256::digest(context_json.as_bytes()));
    if context_hash != expected_hash {
        return Err("AI助手请求冻结的上下文校验失败，无法安全继续".to_string());
    }
    let messages = serde_json::from_str::<Vec<Value>>(&context_json)
        .map_err(|_| "AI助手请求冻结的上下文已损坏，无法安全继续".to_string())?;
    if messages.is_empty() {
        return Err("AI助手请求冻结的上下文为空，无法安全继续".to_string());
    }
    Ok(Some((context_hash, messages)))
}

pub(crate) fn enqueue_request(
    database: &RuntimeDatabase,
    scope: &str,
    input: &AssistantRequestInput,
) -> Result<AssistantRequestRecord, String> {
    let request_id = input.request_id.trim();
    let conversation_id = input.conversation_id.trim();
    validate_ids(request_id, conversation_id)?;
    if input.conversation_revision < 0 {
        return Err("AI助手对话修订号不能为负数".to_string());
    }
    let trace_id = request_trace_id(&input.payload, request_id)?;
    let frozen = frozen_request_payload(
        &input.payload,
        &input.model_config,
        &trace_id,
        conversation_id,
    )?;
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手入队事务：{error}"))?;

    if let Some((conversation, revision, payload)) = tx
        .query_row(
            "SELECT conversation_id, conversation_revision, payload_json
             FROM assistant_requests WHERE workspace_scope=?1 AND id=?2",
            params![scope, request_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法检查重复 AI助手请求：{error}"))?
    {
        let stored_messages = load_request_messages(&tx, scope, request_id)?;
        if conversation != conversation_id
            || revision != input.conversation_revision
            || compact_request_payload_json(&payload)? != frozen.payload_json
            || stored_messages != frozen.messages
        {
            return Err("AI助手请求 ID 已被其他内容占用".to_string());
        }
        return read_request(&tx, scope, request_id);
    }

    let stored_revision = tx
        .query_row(
            "SELECT revision FROM assistant_conversations WHERE workspace_scope=?1 AND id=?2",
            params![scope, conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取 AI助手对话修订号：{error}"))?;

    // 严格的版本检查（乐观锁）
    if let Some(stored) = stored_revision {
        if input.conversation_revision != stored {
            return Err(format!(
                "对话版本冲突：期望 {}，实际 {}。请刷新对话后重试",
                input.conversation_revision, stored
            ));
        }
    }

    // 先取消旧版本的请求（如果存在）
    if stored_revision.is_some() {
        tx.execute(
            "UPDATE assistant_requests
             SET state='cancelled', last_error='对话上下文已进入新修订版本',
                 completed_at=?3, updated_at=?3
             WHERE workspace_scope=?1 AND conversation_id=?2
               AND conversation_revision < ?4
               AND state IN ('queued', 'running')",
            params![scope, conversation_id, now, input.conversation_revision],
        )
        .map_err(|error| format!("无法取消旧修订版本的 AI助手请求：{error}"))?;
    }

    // 计算新版本号
    let next_revision = input.conversation_revision + 1;

    // 使用 UPDATE 来实现乐观锁（确保原子性）
    let updated = tx
        .execute(
            "UPDATE assistant_conversations
         SET revision=?3, updated_at=?4
         WHERE workspace_scope=?1 AND id=?2 AND revision=?5",
            params![
                scope,
                conversation_id,
                next_revision,
                now,
                input.conversation_revision
            ],
        )
        .map_err(|error| format!("无法更新 AI助手对话修订号：{error}"))?;

    // 如果没有更新任何行，说明发生了并发冲突或首次创建
    if updated == 0 {
        // 尝试插入（首次创建对话）
        match tx.execute(
            "INSERT INTO assistant_conversations
             (workspace_scope, id, revision, context_json, updated_at)
             VALUES (?1, ?2, ?3, '[]', ?4)",
            params![scope, conversation_id, next_revision, now],
        ) {
            Ok(_) => {
                log::debug!("首次创建对话 {} 版本 {}", conversation_id, next_revision);
            }
            Err(_) => {
                // 插入失败说明其他事务已经创建，版本冲突
                return Err("对话版本已被其他请求更新，请重试".to_string());
            }
        }
    }
    let sequence = tx
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM assistant_requests
             WHERE workspace_scope=?1 AND conversation_id=?2",
            params![scope, conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法分配 AI助手请求顺序号：{error}"))?;
    tx.execute(
        "INSERT INTO assistant_requests
         (workspace_scope, id, conversation_id, conversation_revision, sequence, state,
          payload_json, context_json, context_hash, has_volatile_attachments,
          recovery_count, last_error, created_at, started_at, completed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, NULL, NULL, ?7, 0, NULL, ?8, NULL, NULL, ?8)",
        params![
            scope,
            request_id,
            conversation_id,
            input.conversation_revision,
            sequence,
            frozen.payload_json,
            i64::from(input.has_volatile_attachments),
            now
        ],
    )
    .map_err(|error| format!("无法持久化 AI助手请求：{error}"))?;
    store_request_messages(&tx, scope, request_id, &frozen.messages)?;
    crate::trace::record_trace_event_in_connection(
        &tx,
        scope,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "conversation_turn",
            entity_id: request_id,
            event_type: "conversation.turn.queued",
            state: "queued",
            payload: &json!({
                "conversationId": conversation_id,
                "conversationRevision": input.conversation_revision,
                "sequence": sequence,
            }),
            created_at: &now,
        },
    )?;
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手入队事务：{error}"))?;
    read_request(&connection, scope, request_id)
}

pub(crate) fn claim_request(
    database: &RuntimeDatabase,
    scope: &str,
    request_id: &str,
) -> Result<AssistantRequestClaim, String> {
    if !valid_id(request_id, 160) {
        return Err("AI助手请求 ID 格式无效".to_string());
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手领取事务：{error}"))?;
    let request = read_request(&tx, scope, request_id)?;
    if request.state == "running" {
        return Ok(AssistantRequestClaim {
            // Only the transition from queued to running owns execution. Returning a
            // successful claim here would let duplicate IPC deliveries run the same
            // request concurrently.
            claim_granted: false,
            request,
        });
    }
    if request.state != "queued" {
        return Ok(AssistantRequestClaim {
            claim_granted: false,
            request,
        });
    }
    let revision = tx
        .query_row(
            "SELECT revision FROM assistant_conversations WHERE workspace_scope=?1 AND id=?2",
            params![scope, request.conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法核对 AI助手对话修订号：{error}"))?;
    if revision != request.conversation_revision {
        tx.execute(
            "UPDATE assistant_requests SET state='cancelled', last_error='请求修订号已经失效',
             completed_at=?3, updated_at=?3 WHERE workspace_scope=?1 AND id=?2",
            params![scope, request_id, now],
        )
        .map_err(|error| format!("无法取消失效的 AI助手请求：{error}"))?;
        tx.commit()
            .map_err(|error| format!("无法提交 AI助手领取事务：{error}"))?;
        return Ok(AssistantRequestClaim {
            claim_granted: false,
            request: read_request(&connection, scope, request_id)?,
        });
    }
    let blockers: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM assistant_requests
             WHERE workspace_scope=?1 AND conversation_id=?2 AND id<>?3
               AND (state='running' OR (state='queued' AND sequence<?4))",
            params![scope, request.conversation_id, request_id, request.sequence],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法核对 AI助手对话队列：{error}"))?;
    let granted = blockers == 0;
    if granted {
        tx.execute(
            "UPDATE assistant_requests SET state='running', started_at=?3,
             last_error=NULL, updated_at=?3
             WHERE workspace_scope=?1 AND id=?2 AND state='queued'",
            params![scope, request_id, now],
        )
        .map_err(|error| format!("无法领取 AI助手请求：{error}"))?;
        let trace_id = request_trace_id(&request.payload, &request.request_id)?;
        crate::trace::record_trace_event_in_connection(
            &tx,
            scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "conversation_turn",
                entity_id: &request.request_id,
                event_type: "conversation.turn.running",
                state: "running",
                payload: &json!({
                    "conversationId": &request.conversation_id,
                    "conversationRevision": request.conversation_revision,
                    "sequence": request.sequence,
                }),
                created_at: &now,
            },
        )?;
    }
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手领取事务：{error}"))?;
    Ok(AssistantRequestClaim {
        claim_granted: granted,
        request: read_request(&connection, scope, request_id)?,
    })
}

fn string_at<'a>(value: &'a Value, camel: &str, snake: &str) -> Option<&'a str> {
    value
        .get(camel)
        .or_else(|| value.get(snake))
        .and_then(Value::as_str)
}

fn limited(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

fn attachment_description(attachment: &Value) -> String {
    let name = limited(
        string_at(attachment, "name", "name")
            .unwrap_or_else(|| prompt_text(ATTACHMENT_UNNAMED_PROMPT)),
        160,
    );
    let kind = limited(
        string_at(attachment, "type", "type")
            .or_else(|| string_at(attachment, "kind", "kind"))
            .unwrap_or_else(|| prompt_text(ATTACHMENT_DEFAULT_KIND_PROMPT)),
        120,
    );
    let analysis = attachment
        .get("imageAnalysis")
        .or_else(|| attachment.get("image_analysis"));
    let summary = analysis
        .and_then(|value| string_at(value, "summary", "summary"))
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_default();
    let visible = analysis
        .and_then(|value| string_at(value, "visibleText", "visible_text"))
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_default();
    if summary.is_empty() && visible.is_empty() {
        return render_prompt_template(
            ATTACHMENT_UNREAD_PROMPT_TEMPLATE,
            &[("name", &name), ("kind", &kind)],
        )
        .expect("bundled assistant attachment description Prompt must be valid");
    }
    let image_header =
        render_prompt_template(ATTACHMENT_IMAGE_HEADER_PROMPT_TEMPLATE, &[("name", &name)])
            .expect("bundled assistant image description Prompt must be valid");
    let mut parts = vec![image_header];
    if !summary.is_empty() {
        parts.push(summary);
    }
    if !visible.is_empty() {
        parts.push(
            render_prompt_template(
                ATTACHMENT_VISIBLE_TEXT_PROMPT_TEMPLATE,
                &[("visible_text", &visible)],
            )
            .expect("bundled assistant visible text Prompt must be valid"),
        );
    }
    parts.join("\n")
}

fn current_model_attachments(context: &Value) -> Vec<Value> {
    context
        .get("modelAttachments")
        .or_else(|| context.get("model_attachments"))
        .and_then(Value::as_array)
        .map(|attachments| {
            attachments
                .iter()
                .filter_map(|attachment| {
                    let name = limited(string_at(attachment, "name", "name")?, 160);
                    let mime = limited(
                        string_at(attachment, "mimeType", "mime_type")
                            .unwrap_or("application/octet-stream"),
                        120,
                    )
                    .to_lowercase();
                    let mut result = serde_json::Map::from_iter([
                        ("name".to_string(), Value::String(name)),
                        ("mimeType".to_string(), Value::String(mime)),
                    ]);
                    if let Some(text) = string_at(attachment, "textContent", "text_content") {
                        result.insert("textContent".to_string(), Value::String(text.to_string()));
                    }
                    Some(Value::Object(result))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn persisted_conversation_messages(
    connection: &Connection,
    scope: &str,
    request_id: &str,
    payload: &Value,
) -> Result<Option<Vec<Value>>, String> {
    let stored = load_request_messages(connection, scope, request_id)?;
    if !stored.is_empty() {
        return Ok(Some(stored));
    }
    let Some(messages) = payload
        .get("conversationMessages")
        .or_else(|| payload.get("conversation_messages"))
    else {
        return Ok(None);
    };
    let messages = messages
        .as_array()
        .ok_or_else(|| "AI助手持久化对话快照已经损坏".to_string())?;
    Ok(Some(messages.clone()))
}

fn estimate_context_tokens(value: &Value) -> usize {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    for character in serialized.chars() {
        if character.is_ascii() {
            ascii = ascii.saturating_add(1);
        } else {
            non_ascii = non_ascii.saturating_add(1);
        }
    }
    non_ascii.saturating_add(ascii.div_ceil(4))
}

fn page_context_messages(
    messages: Vec<Value>,
    context_budget: Option<AssistantContextBudget>,
) -> Result<(Vec<Value>, usize), String> {
    let total = messages.len();
    let token_budget = context_budget.map(|budget| {
        budget
            .input_tokens
            .saturating_sub(CONTEXT_PAGE_TOKEN_RESERVE)
            .max(512)
    });
    let mut selected = Vec::new();
    let mut selected_tokens = 0usize;
    let mut selected_bytes = 2usize;
    for message in messages.into_iter().rev() {
        let message_bytes = serde_json::to_vec(&message)
            .map_err(|error| format!("无法序列化 AI助手上下文消息：{error}"))?
            .len()
            .saturating_add(1);
        let message_tokens = estimate_context_tokens(&message);
        let exceeds_tokens = token_budget
            .is_some_and(|budget| selected_tokens.saturating_add(message_tokens) > budget);
        let exceeds_bytes =
            selected_bytes.saturating_add(message_bytes) > DEFAULT_CONTEXT_PAGE_BYTES;
        if exceeds_tokens || exceeds_bytes {
            if selected.is_empty() {
                return Err(
                    "最新一条 AI助手消息超过当前模型的单请求上下文页；请先把正文或附件作为耐久资产分块处理"
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
    let omitted = total.saturating_sub(selected.len());
    Ok((selected, omitted))
}

fn build_context(
    input: &AssistantContextInput,
    memory_context: Option<&str>,
    context_budget: Option<AssistantContextBudget>,
) -> Result<(Vec<Value>, usize), String> {
    let context_text = string_at(&input.attachment_context, "contextText", "context_text")
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_default();
    let latest_user = input
        .messages
        .iter()
        .rposition(|message| string_at(message, "role", "role") == Some("user"));
    let model_attachments = current_model_attachments(&input.attachment_context);
    let mut output = Vec::new();
    for (index, message) in input.messages.iter().enumerate() {
        if message
            .get("excludeFromModelContext")
            .or_else(|| message.get("exclude_from_model_context"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let role = string_at(message, "role", "role")
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if !matches!(role.as_str(), "user" | "assistant") {
            continue;
        }
        let mut content = string_at(message, "content", "content")
            .unwrap_or_default()
            .trim()
            .to_string();
        let attachment_notes = message
            .get("attachments")
            .and_then(Value::as_array)
            .map(|attachments| {
                attachments
                    .iter()
                    .map(attachment_description)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !attachment_notes.is_empty() {
            content.push_str("\n\n");
            content.push_str(
                &render_prompt_template(
                    ATTACHMENT_RECORDS_PROMPT_TEMPLATE,
                    &[("attachment_records", &attachment_notes.join("\n\n"))],
                )
                .expect("bundled assistant attachment records Prompt must be valid"),
            );
        }
        if latest_user == Some(index) && !context_text.is_empty() {
            content.push_str("\n\n");
            content.push_str(&context_text);
        }
        if latest_user == Some(index) {
            if let Some(memory_context) = memory_context.filter(|value| !value.trim().is_empty()) {
                content.push_str("\n\n");
                content.push_str(memory_context);
            }
        }
        if content.trim().is_empty() {
            continue;
        }
        let mut normalized = serde_json::Map::from_iter([
            ("role".to_string(), Value::String(role)),
            ("content".to_string(), Value::String(content)),
        ]);
        if latest_user == Some(index) && !model_attachments.is_empty() {
            normalized.insert(
                "attachments".to_string(),
                Value::Array(model_attachments.clone()),
            );
        }
        output.push(Value::Object(normalized));
    }
    if input.latest_user_only {
        let omitted = output.len().saturating_sub(1);
        let last_user = output
            .into_iter()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .ok_or_else(|| "AI助手请求缺少可用的用户消息".to_string())?;
        return Ok((vec![last_user], omitted));
    }
    if output.is_empty() {
        return Err("AI助手请求没有可用的对话上下文".to_string());
    }
    page_context_messages(output, context_budget)
}

pub(crate) fn assemble_request_context(
    database: &RuntimeDatabase,
    scope: &str,
    input: &AssistantContextInput,
) -> Result<AssistantContextReceipt, String> {
    if !valid_id(&input.request_id, 160) || input.conversation_revision < 0 {
        return Err("AI助手上下文请求参数无效".to_string());
    }
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手上下文事务：{error}"))?;
    let request = read_request(&tx, scope, &input.request_id)?;
    if request.state != "running" {
        return Err("只有已领取的 AI助手请求可以组装模型上下文".to_string());
    }
    if request.conversation_revision != input.conversation_revision {
        return Err("AI助手请求上下文修订号不一致".to_string());
    }
    let revision = tx
        .query_row(
            "SELECT revision FROM assistant_conversations WHERE workspace_scope=?1 AND id=?2",
            params![scope, request.conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法核对 AI助手对话修订号：{error}"))?;
    if revision != input.conversation_revision {
        return Err("AI助手对话已经进入新的上下文修订版本".to_string());
    }
    if let Some((context_hash, messages)) = load_frozen_context(&tx, scope, &input.request_id)? {
        let persisted_count = load_request_messages(&tx, scope, &input.request_id)?.len();
        let omitted_message_count = persisted_count.saturating_sub(messages.len());
        tx.commit()
            .map_err(|error| format!("无法提交 AI助手冻结上下文读取事务：{error}"))?;
        return Ok(AssistantContextReceipt {
            request_id: input.request_id.clone(),
            conversation_id: request.conversation_id,
            conversation_revision: input.conversation_revision,
            context_hash,
            messages,
            omitted_message_count,
        });
    }
    let persisted_messages =
        persisted_conversation_messages(&tx, scope, &input.request_id, &request.payload)?;
    let effective_input = AssistantContextInput {
        request_id: input.request_id.clone(),
        conversation_revision: input.conversation_revision,
        messages: persisted_messages.unwrap_or_else(|| input.messages.clone()),
        attachment_context: input.attachment_context.clone(),
        latest_user_only: input.latest_user_only,
    };
    let memory_scope = memory_scope_from_request(&request.payload, &request.conversation_id)?;
    let latest_user_query = effective_input
        .messages
        .iter()
        .rev()
        .find(|message| string_at(message, "role", "role") == Some("user"))
        .and_then(|message| string_at(message, "content", "content"))
        .unwrap_or_default();
    let memory_context = if effective_input.latest_user_only {
        None
    } else {
        memory::assistant_memory_context_in_connection(
            &tx,
            scope,
            latest_user_query,
            &memory_scope,
        )?
    };
    let model_config = request
        .payload
        .get("modelConfig")
        .or_else(|| request.payload.get("model_config"))
        .unwrap_or(&Value::Null);
    let context_budget = assistant_context_budget_from_snapshot(model_config)?;
    let (messages, omitted_message_count) =
        build_context(&effective_input, memory_context.as_deref(), context_budget)?;
    let context_json = serde_json::to_string(&messages)
        .map_err(|error| format!("无法序列化 AI助手模型上下文：{error}"))?;
    let context_hash = format!("sha256:{:x}", Sha256::digest(context_json.as_bytes()));
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "UPDATE assistant_requests SET context_json=?3, context_hash=?4, updated_at=?5
         WHERE workspace_scope=?1 AND id=?2 AND state='running'",
        params![scope, input.request_id, context_json, context_hash, now],
    )
    .map_err(|error| format!("无法冻结 AI助手请求上下文：{error}"))?;
    tx.execute(
        "UPDATE assistant_conversations SET context_json=?3, updated_at=?4
         WHERE workspace_scope=?1 AND id=?2 AND revision=?5",
        params![
            scope,
            request.conversation_id,
            context_json,
            now,
            input.conversation_revision
        ],
    )
    .map_err(|error| format!("无法保存 AI助手对话上下文：{error}"))?;
    let trace_id = request_trace_id(&request.payload, &request.request_id)?;
    crate::trace::record_trace_event_in_connection(
        &tx,
        scope,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "conversation_turn",
            entity_id: &request.request_id,
            event_type: "conversation.context.frozen",
            state: "running",
            payload: &json!({
                "conversationId": &request.conversation_id,
                "conversationRevision": input.conversation_revision,
                "contextHash": &context_hash,
                "messageCount": messages.len(),
                "omittedMessageCount": omitted_message_count,
                "contextWindowTokens": context_budget.map(|budget| budget.context_window_tokens),
                "memoryIncluded": memory_context.is_some(),
            }),
            created_at: &now,
        },
    )?;
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手上下文事务：{error}"))?;
    Ok(AssistantContextReceipt {
        request_id: input.request_id.clone(),
        conversation_id: request.conversation_id,
        conversation_revision: input.conversation_revision,
        context_hash,
        messages,
        omitted_message_count,
    })
}

fn nested_string_at<'a>(
    value: &'a Value,
    object_camel: &str,
    object_snake: &str,
    field_camel: &str,
    field_snake: &str,
) -> Option<&'a str> {
    value
        .get(object_camel)
        .or_else(|| value.get(object_snake))
        .and_then(|nested| {
            nested
                .as_str()
                .or_else(|| string_at(nested, field_camel, field_snake))
        })
}

fn memory_scope_from_request(
    payload: &Value,
    conversation_id: &str,
) -> Result<memory::MemoryScope, String> {
    let expected = assistant_memory_scope(conversation_id);
    payload
        .get("memoryScope")
        .or_else(|| payload.get("memory_scope"))
        .cloned()
        .map(serde_json::from_value::<memory::MemoryScope>)
        .transpose()
        .map_err(|error| format!("AI助手记忆作用域无效：{error}"))
        .and_then(|declared| {
            if declared.is_some_and(|scope| scope != expected) {
                return Err("AI助手请求记忆作用域与当前对话不一致".to_string());
            }
            Ok(expected)
        })
}

fn explicit_style_from_user_message(user_message: &str) -> Option<String> {
    let command = user_message.trim();
    ["/style", "/reflect-style"].iter().find_map(|prefix| {
        command.strip_prefix(prefix).and_then(|remainder| {
            if remainder.is_empty() || !remainder.chars().next().is_some_and(char::is_whitespace) {
                return None;
            }
            let style = limited(remainder, 240);
            (!style.is_empty()).then_some(style)
        })
    })
}

fn completion_memory_capture(
    request: &AssistantRequestRecord,
    input: &AssistantRequestCompletionInput,
    state: &str,
    error: Option<&str>,
) -> Result<memory::AssistantTurnMemoryCapture, String> {
    let user_message = string_at(&input.result, "userMessage", "user_message")
        .or_else(|| {
            nested_string_at(
                &input.result,
                "userMessage",
                "user_message",
                "content",
                "content",
            )
        })
        .or_else(|| {
            nested_string_at(
                &request.payload,
                "userMessage",
                "user_message",
                "content",
                "content",
            )
        })
        .or_else(|| string_at(&request.payload, "message", "message"))
        .or_else(|| string_at(&request.payload, "content", "content"))
        .unwrap_or_default()
        .to_string();
    let assistant_reply = string_at(&input.result, "assistantReply", "assistant_reply")
        .or_else(|| string_at(&input.result, "reply", "reply"))
        .or_else(|| {
            nested_string_at(
                &input.result,
                "assistantMessage",
                "assistant_message",
                "content",
                "content",
            )
        })
        .unwrap_or_default()
        .to_string();
    let user_message_id = string_at(&input.result, "userMessageId", "user_message_id")
        .or_else(|| nested_string_at(&input.result, "userMessage", "user_message", "id", "id"))
        .or_else(|| nested_string_at(&request.payload, "userMessage", "user_message", "id", "id"))
        .map(str::to_string);
    let assistant_message_id =
        string_at(&input.result, "assistantMessageId", "assistant_message_id")
            .or_else(|| {
                nested_string_at(
                    &input.result,
                    "assistantMessage",
                    "assistant_message",
                    "id",
                    "id",
                )
            })
            .map(str::to_string);
    Ok(memory::AssistantTurnMemoryCapture {
        request_id: request.request_id.clone(),
        conversation_id: request.conversation_id.clone(),
        user_message_id,
        explicit_user_style: explicit_style_from_user_message(&user_message),
        user_message,
        assistant_message_id,
        assistant_reply,
        intent: string_at(&input.result, "intent", "intent").map(str::to_string),
        action: string_at(&input.result, "action", "action").map(str::to_string),
        state: state.to_string(),
        error: error.map(str::to_string),
        scope: memory_scope_from_request(&request.payload, &request.conversation_id)?,
    })
}

pub(crate) fn finish_request(
    database: &RuntimeDatabase,
    scope: &str,
    input: &AssistantRequestCompletionInput,
) -> Result<AssistantRequestRecord, String> {
    if !valid_id(&input.request_id, 160) {
        return Err("AI助手请求 ID 格式无效".to_string());
    }
    let state = input.state.trim();
    if !matches!(state, "succeeded" | "failed" | "cancelled") {
        return Err("AI助手请求终态无效".to_string());
    }
    let result_json = serde_json::to_string(&input.result)
        .map_err(|error| format!("无法序列化 AI助手请求结果：{error}"))?;
    let error = input
        .error
        .as_deref()
        .map(|value| limited(value, 4000))
        .filter(|value| !value.is_empty());
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手终态事务：{error}"))?;
    let current = read_request(&tx, scope, &input.request_id)?;
    if matches!(
        current.state.as_str(),
        "succeeded" | "failed" | "cancelled" | "needs_input"
    ) {
        return Ok(current);
    }
    if state == "succeeded" && current.state != "running" {
        return Err("尚未领取的 AI助手请求不能标记为完成".to_string());
    }
    tx.execute(
        "UPDATE assistant_requests SET state=?3, result_json=?4, last_error=?5,
             completed_at=?6, updated_at=?6
             WHERE workspace_scope=?1 AND id=?2 AND state IN ('queued', 'running')",
        params![scope, input.request_id, state, result_json, error, now],
    )
    .map_err(|database_error| format!("无法保存 AI助手请求终态：{database_error}"))?;
    if state != "cancelled" {
        let capture = completion_memory_capture(&current, input, state, error.as_deref())?;
        memory::persist_assistant_turn_memories(&tx, scope, &capture)?;
    }
    let trace_id = request_trace_id(&current.payload, &current.request_id)?;
    let event_type = format!("conversation.turn.{state}");
    crate::trace::record_trace_event_in_connection(
        &tx,
        scope,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "conversation_turn",
            entity_id: &current.request_id,
            event_type: &event_type,
            state,
            payload: &json!({
                "conversationId": current.conversation_id,
                "conversationRevision": current.conversation_revision,
                "sequence": current.sequence,
                "contextHash": current.context_hash,
                "taskId": string_at(&input.result, "taskId", "task_id"),
                "intent": string_at(&input.result, "intent", "intent"),
                "action": string_at(&input.result, "action", "action"),
                "hasError": error.is_some(),
            }),
            created_at: &now,
        },
    )?;
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手终态事务：{error}"))?;
    read_request(&connection, scope, &input.request_id)
}

pub(crate) fn cancel_request(
    database: &RuntimeDatabase,
    scope: &str,
    request_id: &str,
    reason: &str,
) -> Result<AssistantRequestRecord, String> {
    finish_request(
        database,
        scope,
        &AssistantRequestCompletionInput {
            request_id: request_id.to_string(),
            state: "cancelled".to_string(),
            result: json!({ "reason": limited(reason, 4000) }),
            error: Some(reason.to_string()),
        },
    )
}

pub(crate) fn advance_conversation_revision(
    database: &RuntimeDatabase,
    scope: &str,
    input: &AssistantConversationRevisionInput,
) -> Result<AssistantConversationRevisionReceipt, String> {
    let conversation_id = input.conversation_id.trim();
    if !valid_id(conversation_id, 200)
        || input.expected_revision < 0
        || input.next_revision != input.expected_revision + 1
    {
        return Err("AI助手对话修订参数无效".to_string());
    }
    if input
        .keep_request_id
        .as_deref()
        .is_some_and(|request_id| !valid_id(request_id, 160))
    {
        return Err("保留的 AI助手请求 ID 格式无效".to_string());
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手修订事务：{error}"))?;
    let revision = tx
        .query_row(
            "SELECT revision FROM assistant_conversations WHERE workspace_scope=?1 AND id=?2",
            params![scope, conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取 AI助手对话修订号：{error}"))?
        .unwrap_or(0);
    if revision != input.expected_revision {
        return Err(format!(
            "AI助手对话修订冲突：当前为 {revision}，请求基于 {}",
            input.expected_revision
        ));
    }
    let cancelled_requests = tx
        .execute(
            "UPDATE assistant_requests SET state='cancelled', last_error='对话上下文已清空',
             completed_at=?4, updated_at=?4
             WHERE workspace_scope=?1 AND conversation_id=?2 AND state IN ('queued', 'running')
               AND (?3 IS NULL OR id<>?3)",
            params![scope, conversation_id, input.keep_request_id, now],
        )
        .map_err(|error| format!("无法取消旧修订版本的 AI助手请求：{error}"))?;
    if let Some(request_id) = input.keep_request_id.as_deref() {
        let changed = tx
            .execute(
                "UPDATE assistant_requests SET conversation_revision=?4, context_json=NULL,
                 context_hash=NULL, updated_at=?5
                 WHERE workspace_scope=?1 AND conversation_id=?2 AND id=?3
                   AND state='running' AND conversation_revision=?6",
                params![
                    scope,
                    conversation_id,
                    request_id,
                    input.next_revision,
                    now,
                    input.expected_revision
                ],
            )
            .map_err(|error| format!("无法迁移当前 AI助手请求修订号：{error}"))?;
        if changed != 1 {
            return Err("需要保留的 AI助手请求不在运行状态".to_string());
        }
    }
    tx.execute(
        "INSERT INTO assistant_conversations
         (workspace_scope, id, revision, context_json, updated_at)
         VALUES (?1, ?2, ?3, '[]', ?4)
         ON CONFLICT(workspace_scope, id) DO UPDATE SET
           revision=excluded.revision, context_json='[]', updated_at=excluded.updated_at",
        params![scope, conversation_id, input.next_revision, now],
    )
    .map_err(|error| format!("无法更新 AI助手对话修订号：{error}"))?;
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手修订事务：{error}"))?;
    Ok(AssistantConversationRevisionReceipt {
        conversation_id: conversation_id.to_string(),
        revision: input.next_revision,
        cancelled_requests,
    })
}

pub(crate) fn recover_requests_for_startup(
    database: &RuntimeDatabase,
) -> Result<Vec<AssistantRequestRecord>, String> {
    let scope = database.local_workspace_scope()?;
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始 AI助手启动恢复事务：{error}"))?;
    tx.execute(
        "UPDATE assistant_requests SET state='needs_input', recovery_count=recovery_count+1,
         last_error='应用重启后附件二进制内容不可恢复，请重新附加原文件',
         completed_at=?2, updated_at=?2
         WHERE workspace_scope=?1 AND state IN ('queued', 'running')
           AND has_volatile_attachments=1",
        params![scope, now],
    )
    .map_err(|error| format!("无法隔离需重新附加文件的 AI助手请求：{error}"))?;
    tx.execute(
        "UPDATE assistant_requests SET state='queued', recovery_count=recovery_count+1,
         last_error='应用重启后已恢复到对话队列', started_at=NULL, updated_at=?2
         WHERE workspace_scope=?1 AND state='running' AND has_volatile_attachments=0",
        params![scope, now],
    )
    .map_err(|error| format!("无法恢复中断的 AI助手请求：{error}"))?;
    tx.commit()
        .map_err(|error| format!("无法提交 AI助手启动恢复事务：{error}"))?;

    let mut statement = connection
        .prepare(
            "SELECT id, conversation_id, conversation_revision, sequence, state,
                    payload_json, context_hash, has_volatile_attachments, recovery_count,
                    last_error, created_at, updated_at
             FROM assistant_requests
             WHERE workspace_scope=?1 AND state IN ('queued', 'needs_input')
             ORDER BY conversation_id, sequence",
        )
        .map_err(|error| format!("无法准备 AI助手恢复查询：{error}"))?;
    let rows = statement
        .query_map([scope], request_from_row)
        .map_err(|error| format!("无法读取可恢复的 AI助手请求：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析可恢复的 AI助手请求：{error}"))
}

#[tauri::command]
pub fn enqueue_assistant_request(
    database: State<'_, RuntimeDatabase>,
    request: AssistantRequestInput,
) -> Result<AssistantRequestRecord, String> {
    let scope = database.local_workspace_scope()?;
    enqueue_request(database.inner(), &scope, &request)
}

#[tauri::command]
pub fn claim_assistant_request(
    database: State<'_, RuntimeDatabase>,
    request_id: String,
) -> Result<AssistantRequestClaim, String> {
    let scope = database.local_workspace_scope()?;
    claim_request(database.inner(), &scope, request_id.trim())
}

#[tauri::command]
pub fn assemble_assistant_request_context(
    database: State<'_, RuntimeDatabase>,
    input: AssistantContextInput,
) -> Result<AssistantContextReceipt, String> {
    let scope = database.local_workspace_scope()?;
    assemble_request_context(database.inner(), &scope, &input)
}

#[tauri::command]
pub fn finish_assistant_request(
    database: State<'_, RuntimeDatabase>,
    input: AssistantRequestCompletionInput,
) -> Result<AssistantRequestRecord, String> {
    let scope = database.local_workspace_scope()?;
    finish_request(database.inner(), &scope, &input)
}

#[tauri::command]
pub fn cancel_assistant_runtime_request(
    database: State<'_, RuntimeDatabase>,
    request_id: String,
    reason: Option<String>,
) -> Result<AssistantRequestRecord, String> {
    let scope = database.local_workspace_scope()?;
    cancel_request(
        database.inner(),
        &scope,
        request_id.trim(),
        reason.as_deref().unwrap_or("用户取消 AI助手请求"),
    )
}

#[tauri::command]
pub fn advance_assistant_conversation_revision(
    database: State<'_, RuntimeDatabase>,
    input: AssistantConversationRevisionInput,
) -> Result<AssistantConversationRevisionReceipt, String> {
    let scope = database.local_workspace_scope()?;
    advance_conversation_revision(database.inner(), &scope, &input)
}

#[tauri::command]
pub fn recover_assistant_requests(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<AssistantRequestRecord>, String> {
    recover_requests_for_startup(database.inner())
}
