use crate::{
    execution_ticket::ExecutionTicketState,
    model_config::{decrypt_api_key_with_key, encrypt_api_key_with_key},
    obsidian::OperationEvent,
    runtime_db::RuntimeDatabase,
};
use chrono::{DateTime, Utc};
use futures_util::{
    future::{AbortHandle, AbortRegistration, Abortable},
    StreamExt,
};
use reqwest::{redirect::Policy, Client, Url};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tauri::State;
use uuid::Uuid;

const MAX_CONNECTOR_RESPONSE_BYTES: u64 = 1024 * 1024;
const PREPARED_EXTERNAL_DELIVERY_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PREPARED_EXTERNAL_DELIVERIES: usize = 1_024;
const MAX_ACTIVE_EXTERNAL_DELIVERIES: usize = 256;

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
    execution_ticket: String,
    preparation_id: String,
    connector_id: String,
    content: String,
    #[serde(default)]
    subject: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDeliveryPrepareInput {
    task_id: String,
    execution_ticket: String,
    connector_id: String,
    content: String,
    #[serde(default)]
    subject: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDeliveryPreparation {
    id: String,
    connector_id: String,
    connector_name: String,
    connector_type: String,
    endpoint_host: String,
    prepared_at: String,
    expires_at: String,
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

#[derive(Clone)]
struct ConnectorSnapshot {
    name: String,
    connector_type: String,
    endpoint_ciphertext: Vec<u8>,
    secret_ciphertext: Vec<u8>,
    updated_at: String,
}

struct PreparedExternalDelivery {
    workspace_scope: String,
    task_id: String,
    execution_ticket_digest: String,
    connector_id: String,
    connector_config_digest: String,
    approval_id: String,
    effect_digest: String,
    expires_at: SystemTime,
    generation: u64,
    connector_generation: u64,
}

struct ActiveExternalDelivery {
    connector_id: String,
    abort_handle: AbortHandle,
}

#[derive(Default)]
struct ExternalConnectorRuntime {
    generation: u64,
    connector_generations: HashMap<String, u64>,
    prepared: HashMap<String, PreparedExternalDelivery>,
    active: HashMap<String, ActiveExternalDelivery>,
}

#[derive(Default)]
pub(crate) struct ExternalConnectorRuntimeState {
    runtime: Mutex<ExternalConnectorRuntime>,
}

impl ExternalConnectorRuntimeState {
    fn generation_snapshot(&self, connector_id: &str) -> Result<(u64, u64), String> {
        self.runtime
            .lock()
            .map(|runtime| {
                (
                    runtime.generation,
                    runtime
                        .connector_generations
                        .get(connector_id)
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .map_err(|_| "外部连接器运行状态不可用".to_string())
    }

    fn store_preparation(
        &self,
        expected_generation: u64,
        expected_connector_generation: u64,
        mut preparation: PreparedExternalDelivery,
    ) -> Result<String, String> {
        let now = SystemTime::now();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "外部连接器运行状态不可用".to_string())?;
        runtime
            .prepared
            .retain(|_, prepared| prepared.expires_at > now);
        if runtime.generation != expected_generation {
            return Err("统一授权已撤销，本次外部投递确认已经失效".to_string());
        }
        if runtime
            .connector_generations
            .get(&preparation.connector_id)
            .copied()
            .unwrap_or_default()
            != expected_connector_generation
        {
            return Err("连接器配置在确认期间发生变化，请重新确认目标".to_string());
        }
        if runtime.prepared.len() >= MAX_PREPARED_EXTERNAL_DELIVERIES {
            return Err("待执行的外部投递确认过多，请稍后重试".to_string());
        }
        let preparation_id = format!("external-preparation-{}", Uuid::new_v4());
        preparation.generation = expected_generation;
        preparation.connector_generation = expected_connector_generation;
        runtime.prepared.insert(preparation_id.clone(), preparation);
        Ok(preparation_id)
    }

    fn take_preparation(
        &self,
        preparation_id: &str,
        workspace_scope: &str,
        task_id: &str,
        execution_ticket: &str,
        connector_id: &str,
    ) -> Result<PreparedExternalDelivery, String> {
        let now = SystemTime::now();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "外部连接器运行状态不可用".to_string())?;
        runtime
            .prepared
            .retain(|_, prepared| prepared.expires_at > now);
        let prepared = runtime
            .prepared
            .remove(preparation_id.trim())
            .ok_or_else(|| "外部投递确认不存在、已过期或已经使用".to_string())?;
        let ticket_digest = sha256_hex(execution_ticket.as_bytes());
        if prepared.generation != runtime.generation
            || prepared.connector_generation
                != runtime
                    .connector_generations
                    .get(connector_id)
                    .copied()
                    .unwrap_or_default()
            || prepared.workspace_scope != workspace_scope
            || prepared.task_id != task_id
            || prepared.execution_ticket_digest != ticket_digest
            || prepared.connector_id != connector_id
        {
            return Err("外部投递确认与当前任务、票据或连接器不一致".to_string());
        }
        Ok(prepared)
    }

    fn begin_request(
        &self,
        expected_generation: u64,
        expected_connector_generation: u64,
        connector_id: &str,
    ) -> Result<(String, AbortRegistration), String> {
        let (abort_handle, registration) = AbortHandle::new_pair();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "外部连接器运行状态不可用".to_string())?;
        if runtime.generation != expected_generation {
            return Err("统一授权已撤销，外部投递没有开始".to_string());
        }
        if runtime
            .connector_generations
            .get(connector_id)
            .copied()
            .unwrap_or_default()
            != expected_connector_generation
        {
            return Err("连接器配置在确认后发生变化，外部投递没有开始".to_string());
        }
        if runtime.active.len() >= MAX_ACTIVE_EXTERNAL_DELIVERIES {
            return Err("正在进行的外部投递过多，请稍后重试".to_string());
        }
        let request_id = format!("external-request-{}", Uuid::new_v4());
        runtime.active.insert(
            request_id.clone(),
            ActiveExternalDelivery {
                connector_id: connector_id.to_string(),
                abort_handle,
            },
        );
        Ok((request_id, registration))
    }

    fn finish_request(&self, request_id: &str) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.active.remove(request_id);
        }
    }

    fn scope_is_active(
        &self,
        generation: u64,
        connector_generation: u64,
        connector_id: &str,
    ) -> bool {
        self.runtime
            .lock()
            .map(|runtime| {
                runtime.generation == generation
                    && runtime
                        .connector_generations
                        .get(connector_id)
                        .copied()
                        .unwrap_or_default()
                        == connector_generation
            })
            .unwrap_or(false)
    }

    fn invalidate_connector(&self, connector_id: &str) -> Result<usize, String> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "外部连接器运行状态不可用".to_string())?;
        let connector_generation = runtime
            .connector_generations
            .entry(connector_id.to_string())
            .or_default();
        *connector_generation = connector_generation.wrapping_add(1);
        runtime
            .prepared
            .retain(|_, prepared| prepared.connector_id != connector_id);
        let request_ids = runtime
            .active
            .iter()
            .filter_map(|(request_id, request)| {
                (request.connector_id == connector_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in &request_ids {
            if let Some(request) = runtime.active.remove(request_id) {
                request.abort_handle.abort();
            }
        }
        Ok(request_ids.len())
    }

    pub(crate) fn cancel_all(&self) -> Result<usize, String> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "外部连接器运行状态不可用".to_string())?;
        runtime.generation = runtime.generation.wrapping_add(1);
        runtime.prepared.clear();
        let active = std::mem::take(&mut runtime.active);
        let cancelled = active.len();
        for request in active.into_values() {
            request.abort_handle.abort();
        }
        Ok(cancelled)
    }
}

fn default_enabled() -> bool {
    true
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn connector_configuration_digest(connector_id: &str, snapshot: &ConnectorSnapshot) -> String {
    let payload = serde_json::json!({
        "connectorId": connector_id,
        "name": snapshot.name,
        "connectorType": snapshot.connector_type,
        "endpointCiphertextSha256": sha256_hex(&snapshot.endpoint_ciphertext),
        "secretCiphertextSha256": sha256_hex(&snapshot.secret_ciphertext),
        "updatedAt": snapshot.updated_at,
    });
    sha256_hex(
        &serde_json::to_vec(&payload)
            .expect("connector configuration digest payload is always serializable"),
    )
}

fn external_delivery_effect_digest(
    task_id: &str,
    connector_id: &str,
    subject: &str,
    content: &str,
    connector_config_digest: &str,
) -> String {
    let payload = serde_json::json!({
        "taskId": task_id,
        "connectorId": connector_id,
        "subjectSha256": sha256_hex(subject.as_bytes()),
        "subjectByteLength": subject.len(),
        "contentSha256": sha256_hex(content.as_bytes()),
        "contentByteLength": content.len(),
        "connectorConfigDigest": connector_config_digest,
    });
    sha256_hex(
        &serde_json::to_vec(&payload)
            .expect("external delivery digest payload is always serializable"),
    )
}

fn connector_scope(workspace_scope: &str, connector_id: &str, field: &str) -> String {
    format!("{workspace_scope}:connector:{connector_id}:{field}")
}

fn valid_connector_type(value: &str) -> bool {
    matches!(value, "feishu" | "wechat" | "email_webhook" | "webhook")
}

fn public_connector_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => {
            !(value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_broadcast()
                || value.is_documentation()
                || value.is_unspecified())
        }
        IpAddr::V6(value) => {
            let first = value.segments()[0];
            !(value.is_loopback()
                || value.is_unspecified()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80)
        }
    }
}

fn public_connector_host(host: &str) -> bool {
    let normalized = host
        .trim_end_matches('.')
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or_else(|| host.trim_end_matches('.'))
        .to_ascii_lowercase();
    if normalized.is_empty()
        || normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return false;
    }
    normalized
        .parse::<IpAddr>()
        .map(public_connector_ip)
        .unwrap_or(true)
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
    if !public_connector_host(url.host_str().expect("host checked")) {
        return Err("连接器地址不能指向本机、私网、回环、链路本地或保留地址".to_string());
    }
    Ok(url)
}

async fn resolve_public_connector_endpoint(url: &Url) -> Result<Vec<SocketAddr>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "连接器地址缺少主机名".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "连接器地址缺少有效端口".to_string())?;
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("无法解析连接器地址：{error}"))?;
    let mut unique = HashSet::new();
    for address in resolved {
        if !public_connector_ip(address.ip()) {
            return Err("连接器 DNS 解析到了私网、回环、链路本地或保留地址".to_string());
        }
        unique.insert(address);
    }
    if unique.is_empty() {
        return Err("连接器地址没有可用的公网解析结果".to_string());
    }
    let mut addresses = unique.into_iter().collect::<Vec<_>>();
    addresses.sort_by_key(|address| address.to_string());
    Ok(addresses)
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

fn load_connector_snapshot(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    connector_id: &str,
) -> Result<ConnectorSnapshot, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    connection
        .query_row(
            "SELECT name, connector_type, endpoint_ciphertext, secret_ciphertext, updated_at
             FROM external_connectors WHERE workspace_scope=?1 AND id=?2 AND enabled=1",
            params![workspace_scope, connector_id],
            |row| {
                Ok(ConnectorSnapshot {
                    name: row.get(0)?,
                    connector_type: row.get(1)?,
                    endpoint_ciphertext: row.get(2)?,
                    secret_ciphertext: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("无法读取外部连接器：{error}"))?
        .ok_or_else(|| "指定连接器不存在或已停用".to_string())
}

#[tauri::command]
pub fn save_external_connector(
    database: State<'_, RuntimeDatabase>,
    runtime_state: State<'_, ExternalConnectorRuntimeState>,
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
    runtime_state.invalidate_connector(&connector.id)?;
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
    runtime_state: State<'_, ExternalConnectorRuntimeState>,
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
        runtime_state.invalidate_connector(&connector_id)?;
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
pub fn prepare_external_delivery(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    runtime_state: State<'_, ExternalConnectorRuntimeState>,
    input: ExternalDeliveryPrepareInput,
) -> Result<ExternalDeliveryPreparation, String> {
    if input.content.trim().is_empty() {
        return Err("外部发送内容不能为空".to_string());
    }
    if input.execution_ticket.trim().is_empty() {
        return Err("外部发送缺少一次性执行票据".to_string());
    }
    Uuid::parse_str(input.connector_id.trim()).map_err(|_| "连接器 ID 无效".to_string())?;
    let (generation, connector_generation) =
        runtime_state.generation_snapshot(input.connector_id.trim())?;
    if !database.application_authorization()?.is_granted() {
        return Err("统一授权未生效，不能准备外部投递".to_string());
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
    let snapshot = load_connector_snapshot(&database, &workspace_scope, &input.connector_id)?;
    let endpoint = decrypt_connector_value(
        &database,
        &workspace_scope,
        &input.connector_id,
        "endpoint",
        &snapshot.endpoint_ciphertext,
    )?;
    let endpoint = validate_endpoint(&endpoint)?;
    let connector_config_digest = connector_configuration_digest(&input.connector_id, &snapshot);
    let effect_digest = external_delivery_effect_digest(
        input.task_id.trim(),
        input.connector_id.trim(),
        input.subject.trim(),
        input.content.trim(),
        &connector_config_digest,
    );
    let approval_id = format!(
        "external-delivery:{}:{}",
        input.task_id.trim(),
        &effect_digest[..16]
    );
    ticket_state.bind_operation_approval(
        &input.execution_ticket,
        &workspace_scope,
        input.task_id.trim(),
        task.trace_id.as_deref(),
        &["system:external"],
        &["send"],
        &approval_id,
        &effect_digest,
    )?;
    if !database.application_authorization()?.is_granted() {
        return Err("统一授权已撤销，本次外部投递确认已经失效".to_string());
    }
    let prepared_at = SystemTime::now();
    let expires_at = prepared_at
        .checked_add(PREPARED_EXTERNAL_DELIVERY_TTL)
        .ok_or_else(|| "无法计算外部投递确认过期时间".to_string())?;
    let preparation_id = runtime_state.store_preparation(
        generation,
        connector_generation,
        PreparedExternalDelivery {
            workspace_scope,
            task_id: input.task_id.trim().to_string(),
            execution_ticket_digest: sha256_hex(input.execution_ticket.as_bytes()),
            connector_id: input.connector_id.clone(),
            connector_config_digest,
            approval_id,
            effect_digest,
            expires_at,
            generation,
            connector_generation,
        },
    )?;
    Ok(ExternalDeliveryPreparation {
        id: preparation_id,
        connector_id: input.connector_id,
        connector_name: snapshot.name,
        connector_type: snapshot.connector_type,
        endpoint_host: endpoint.host_str().unwrap_or_default().to_string(),
        prepared_at: DateTime::<Utc>::from(prepared_at).to_rfc3339(),
        expires_at: DateTime::<Utc>::from(expires_at).to_rfc3339(),
    })
}

#[tauri::command]
pub async fn send_external_message(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    runtime_state: State<'_, ExternalConnectorRuntimeState>,
    input: ExternalDeliveryInput,
) -> Result<ExternalDeliveryReceipt, String> {
    if input.content.trim().is_empty() {
        return Err("外部发送内容不能为空".to_string());
    }
    if input.execution_ticket.trim().is_empty() || input.preparation_id.trim().is_empty() {
        return Err("外部发送缺少一次性执行票据或原生确认".to_string());
    }
    Uuid::parse_str(input.connector_id.trim()).map_err(|_| "连接器 ID 无效".to_string())?;
    if !database.application_authorization()?.is_granted() {
        return Err("统一授权未生效，不能执行外部投递".to_string());
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
    let prepared = runtime_state.take_preparation(
        &input.preparation_id,
        &workspace_scope,
        input.task_id.trim(),
        &input.execution_ticket,
        input.connector_id.trim(),
    )?;
    let expected_effect_digest = external_delivery_effect_digest(
        input.task_id.trim(),
        input.connector_id.trim(),
        input.subject.trim(),
        input.content.trim(),
        &prepared.connector_config_digest,
    );
    if expected_effect_digest != prepared.effect_digest {
        return Err("外部投递正文或主题与用户确认的内容不一致".to_string());
    }
    let snapshot = load_connector_snapshot(&database, &workspace_scope, &input.connector_id)?;
    if connector_configuration_digest(&input.connector_id, &snapshot)
        != prepared.connector_config_digest
    {
        return Err("连接器配置在确认后发生变化，外部内容没有发送；请重新确认目标".to_string());
    }
    let endpoint = decrypt_connector_value(
        &database,
        &workspace_scope,
        &input.connector_id,
        "endpoint",
        &snapshot.endpoint_ciphertext,
    )?;
    let endpoint = validate_endpoint(&endpoint)?;
    let secret = if snapshot.secret_ciphertext.is_empty() {
        String::new()
    } else {
        decrypt_connector_value(
            &database,
            &workspace_scope,
            &input.connector_id,
            "secret",
            &snapshot.secret_ciphertext,
        )?
    };
    let resolved_addresses = resolve_public_connector_endpoint(&endpoint).await?;
    let endpoint_host = endpoint
        .host_str()
        .ok_or_else(|| "连接器地址缺少主机名".to_string())?
        .to_string();
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .resolve_to_addrs(&endpoint_host, &resolved_addresses)
        .build()
        .map_err(|error| format!("无法初始化外部连接器：{error}"))?;
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .json(&connector_payload(
            &snapshot.connector_type,
            input.subject.trim(),
            input.content.trim(),
        ));
    if !secret.is_empty() {
        request = request.bearer_auth(&secret);
    }
    if !database.application_authorization()?.is_granted() {
        return Err("统一授权已撤销，外部内容没有发送".to_string());
    }
    let (request_id, abort_registration) = runtime_state.begin_request(
        prepared.generation,
        prepared.connector_generation,
        &input.connector_id,
    )?;
    if let Err(error) = ticket_state.begin_commit(
        &input.execution_ticket,
        &workspace_scope,
        input.task_id.trim(),
        &[(&prepared.approval_id, &prepared.effect_digest)],
    ) {
        runtime_state.finish_request(&request_id);
        return Err(error);
    }
    let network_result = Abortable::new(
        async move {
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
            if !status.is_success() {
                return Err(format!("外部连接器返回 HTTP {}", status.as_u16()));
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| format!("无法读取外部连接器响应：{error}"))?;
                if body.len().saturating_add(chunk.len()) > MAX_CONNECTOR_RESPONSE_BYTES as usize {
                    return Err("外部连接器响应超过 1 MB 安全上限".to_string());
                }
                body.extend_from_slice(&chunk);
            }
            Ok((status, body))
        },
        abort_registration,
    )
    .await;
    runtime_state.finish_request(&request_id);
    let (status, body) = match network_result {
        Ok(Ok(result))
            if runtime_state.scope_is_active(
                prepared.generation,
                prepared.connector_generation,
                &input.connector_id,
            ) =>
        {
            result
        }
        Ok(Ok(_)) | Err(_) => {
            ticket_state.fail_commit(&input.execution_ticket)?;
            return Err("统一授权已撤销或连接器配置已变化，外部请求已取消；若请求已进入网络，远端是否收到内容无法确认".to_string());
        }
        Ok(Err(error)) => {
            ticket_state.fail_commit(&input.execution_ticket)?;
            return Err(error);
        }
    };
    let delivered_at = Utc::now().to_rfc3339();
    let receipt = ExternalDeliveryReceipt {
        id: Uuid::new_v4().to_string(),
        connector_id: input.connector_id.clone(),
        connector_name: snapshot.name,
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
    let persist_result = transaction
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
        .map_err(|error| format!("无法保存外部发送回执：{error}"))
        .and_then(|_| transaction
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
        .map_err(|error| format!("无法保存外部发送审计事件：{error}")))
        .and_then(|_| transaction
            .commit()
            .map_err(|error| format!("无法提交外部发送回执：{error}")));
    if let Err(error) = persist_result {
        ticket_state.fail_commit(&input.execution_ticket)?;
        return Err(error);
    }
    ticket_state.complete_commit(&input.execution_ticket)?;
    Ok(receipt)
}
