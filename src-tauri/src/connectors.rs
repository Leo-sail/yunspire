use crate::{
    model_config::{decrypt_api_key_with_key, encrypt_api_key_with_key},
    obsidian::OperationEvent,
    runtime_db::RuntimeDatabase,
};
use chrono::Utc;
use reqwest::{redirect::Policy, Client, Url};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tauri::State;
use uuid::Uuid;

const MAX_CONNECTOR_MESSAGE_BYTES: usize = 512 * 1024;
const MAX_CONNECTOR_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInput {
    id: String,
    name: String,
    connector_type: String,
    endpoint: String,
    #[serde(default)]
    secret: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredConnector {
    id: String,
    name: String,
    connector_type: String,
    endpoint_host: String,
    endpoint_configured: bool,
    secret_configured: bool,
    enabled: bool,
    updated_at: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDeliveryInput {
    task_id: String,
    connector_id: String,
    content: String,
    #[serde(default)]
    subject: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDeliveryReceipt {
    id: String,
    connector_id: String,
    connector_name: String,
    status_code: u16,
    delivered_at: String,
}

fn default_enabled() -> bool {
    true
}

fn connector_scope(workspace_scope: &str, connector_id: &str, field: &str) -> String {
    format!("{workspace_scope}:connector:{connector_id}:{field}")
}

fn valid_connector_type(value: &str) -> bool {
    matches!(value, "feishu" | "wechat" | "email_webhook" | "webhook")
}

fn validate_endpoint(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|_| "连接器地址不是有效 URL".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("连接器地址必须是无内嵌账号、密码和片段的 HTTPS URL".to_string());
    }
    Ok(url)
}

fn encrypt_connector_value(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    connector_id: &str,
    field: &str,
    value: &str,
) -> Result<Vec<u8>, String> {
    let key = database.device_encryption_key()?;
    encrypt_api_key_with_key(
        &key,
        &connector_scope(workspace_scope, connector_id, field),
        value,
    )
}

fn decrypt_connector_value(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    connector_id: &str,
    field: &str,
    value: &[u8],
) -> Result<String, String> {
    let key = database.device_encryption_key()?;
    decrypt_api_key_with_key(
        &key,
        &connector_scope(workspace_scope, connector_id, field),
        value,
    )
}

#[tauri::command]
pub fn save_external_connector(
    database: State<'_, RuntimeDatabase>,
    connector: ConnectorInput,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    Uuid::parse_str(connector.id.trim()).map_err(|_| "连接器 ID 无效".to_string())?;
    let name = connector.name.trim();
    let connector_type = connector.connector_type.trim();
    if name.is_empty() || name.chars().count() > 80 || !valid_connector_type(connector_type) {
        return Err("连接器名称或类型无效".to_string());
    }
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let existing = connection
        .query_row(
            "SELECT endpoint_ciphertext, secret_ciphertext FROM external_connectors
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, connector.id],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取连接器配置：{error}"))?;
    let endpoint_ciphertext = if connector.endpoint.trim().is_empty() {
        existing
            .as_ref()
            .map(|value| value.0.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "连接器地址不能为空".to_string())?
    } else {
        let endpoint = validate_endpoint(&connector.endpoint)?;
        encrypt_connector_value(
            &database,
            &workspace_scope,
            &connector.id,
            "endpoint",
            endpoint.as_str(),
        )?
    };
    let secret_ciphertext = if connector.secret.trim().is_empty() {
        existing.map(|value| value.1).unwrap_or_default()
    } else {
        encrypt_connector_value(
            &database,
            &workspace_scope,
            &connector.id,
            "secret",
            connector.secret.trim(),
        )?
    };
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO external_connectors
             (workspace_scope, id, name, connector_type, endpoint_ciphertext, secret_ciphertext,
              enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(workspace_scope, id) DO UPDATE SET
               name=excluded.name, connector_type=excluded.connector_type,
               endpoint_ciphertext=excluded.endpoint_ciphertext,
               secret_ciphertext=excluded.secret_ciphertext, enabled=excluded.enabled,
               updated_at=excluded.updated_at",
            params![
                workspace_scope,
                connector.id,
                name,
                connector_type,
                endpoint_ciphertext,
                secret_ciphertext,
                i64::from(connector.enabled),
                now
            ],
        )
        .map_err(|error| format!("无法保存外部连接器：{error}"))?;
    Ok(())
}

#[tauri::command]
pub fn load_external_connectors(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<StoredConnector>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, connector_type, endpoint_ciphertext, secret_ciphertext, enabled, updated_at
             FROM external_connectors WHERE workspace_scope=?1 ORDER BY updated_at DESC",
        )
        .map_err(|error| format!("无法准备连接器查询：{error}"))?;
    let rows = statement
        .query_map([&workspace_scope], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("无法读取连接器：{error}"))?;
    let mut result = Vec::new();
    for row in rows.filter_map(Result::ok) {
        let endpoint =
            decrypt_connector_value(&database, &workspace_scope, &row.0, "endpoint", &row.3)?;
        let endpoint_host = validate_endpoint(&endpoint)?
            .host_str()
            .unwrap_or_default()
            .to_string();
        result.push(StoredConnector {
            id: row.0,
            name: row.1,
            connector_type: row.2,
            endpoint_host,
            endpoint_configured: !row.3.is_empty(),
            secret_configured: !row.4.is_empty(),
            enabled: row.5 == 1,
            updated_at: row.6,
        });
    }
    Ok(result)
}

#[tauri::command]
pub fn delete_external_connector(
    database: State<'_, RuntimeDatabase>,
    connector_id: String,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    Uuid::parse_str(connector_id.trim()).map_err(|_| "连接器 ID 无效".to_string())?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let changed = connection
        .execute(
            "DELETE FROM external_connectors WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, connector_id],
        )
        .map_err(|error| format!("无法删除连接器：{error}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("连接器不存在".to_string())
    }
}

fn connector_payload(connector_type: &str, subject: &str, content: &str) -> Value {
    match connector_type {
        "feishu" => serde_json::json!({"msg_type": "text", "content": {"text": content}}),
        "wechat" => serde_json::json!({"msgtype": "text", "text": {"content": content}}),
        "email_webhook" => serde_json::json!({"subject": subject, "text": content}),
        _ => serde_json::json!({"subject": subject, "content": content, "format": "text"}),
    }
}

#[tauri::command]
pub async fn send_external_message(
    database: State<'_, RuntimeDatabase>,
    input: ExternalDeliveryInput,
) -> Result<ExternalDeliveryReceipt, String> {
    if input.content.trim().is_empty() || input.content.len() > MAX_CONNECTOR_MESSAGE_BYTES {
        return Err("外部发送内容为空或超过 512 KB".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let task = database.ensure_runtime_task_authorized(
        &workspace_scope,
        input.task_id.trim(),
        &["system:external"],
        &["send"],
        None,
        &["running"],
    )?;
    if task
        .payload
        .pointer("/policyDecision/approvalType")
        .and_then(Value::as_str)
        != Some("external_delivery")
    {
        return Err("外部发送任务没有有效的高风险操作确认".to_string());
    }
    let (name, connector_type, endpoint_ciphertext, secret_ciphertext) = {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT name, connector_type, endpoint_ciphertext, secret_ciphertext
                 FROM external_connectors WHERE workspace_scope=?1 AND id=?2 AND enabled=1",
                params![workspace_scope, input.connector_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取外部连接器：{error}"))?
            .ok_or_else(|| "指定连接器不存在或已停用".to_string())?
    };
    let endpoint = decrypt_connector_value(
        &database,
        &workspace_scope,
        &input.connector_id,
        "endpoint",
        &endpoint_ciphertext,
    )?;
    let endpoint = validate_endpoint(&endpoint)?;
    let secret = if secret_ciphertext.is_empty() {
        String::new()
    } else {
        decrypt_connector_value(
            &database,
            &workspace_scope,
            &input.connector_id,
            "secret",
            &secret_ciphertext,
        )?
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("无法初始化外部连接器：{error}"))?;
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .json(&connector_payload(
            &connector_type,
            input.subject.trim(),
            input.content.trim(),
        ));
    if !secret.is_empty() {
        request = request.bearer_auth(&secret);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("外部连接器请求失败：{error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CONNECTOR_RESPONSE_BYTES)
    {
        return Err("外部连接器响应超过 1 MB 安全上限".to_string());
    }
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("无法读取外部连接器响应：{error}"))?;
    if body.len() as u64 > MAX_CONNECTOR_RESPONSE_BYTES {
        return Err("外部连接器响应超过 1 MB 安全上限".to_string());
    }
    if !status.is_success() {
        return Err(format!("外部连接器返回 HTTP {}", status.as_u16()));
    }
    let delivered_at = Utc::now().to_rfc3339();
    let receipt = ExternalDeliveryReceipt {
        id: Uuid::new_v4().to_string(),
        connector_id: input.connector_id.clone(),
        connector_name: name,
        status_code: status.as_u16(),
        delivered_at: delivered_at.clone(),
    };
    let event = OperationEvent {
        id: Uuid::new_v4().to_string(),
        task_id: Some(input.task_id.clone()),
        trace_id: task.trace_id,
        event_type: "external.delivered".to_string(),
        state: "succeeded".to_string(),
        created_at: delivered_at.clone(),
        vault_id: None,
        relative_path: None,
        detail: format!("已通过连接器 {} 完成外部发送", receipt.connector_name),
    };
    let event_payload = serde_json::to_string(&event)
        .map_err(|error| format!("无法序列化外部发送审计事件：{error}"))?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("无法开始外部发送回执事务：{error}"))?;
    transaction
        .execute(
            "INSERT INTO external_delivery_receipts
             (id, workspace_scope, connector_id, task_id, trace_id, status_code, response_hash, delivered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                receipt.id,
                workspace_scope,
                receipt.connector_id,
                input.task_id,
                event.trace_id,
                i64::from(receipt.status_code),
                format!("{:x}", Sha256::digest(&body)),
                delivered_at
            ],
        )
        .map_err(|error| format!("无法保存外部发送回执：{error}"))?;
    transaction
        .execute(
            "INSERT INTO operation_events (id, task_id, event_type, state, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.task_id,
                event.event_type,
                event.state,
                event_payload,
                event.created_at
            ],
        )
        .map_err(|error| format!("无法保存外部发送审计事件：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交外部发送回执：{error}"))?;
    Ok(receipt)
}
