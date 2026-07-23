use crate::runtime_db::RuntimeDatabase;
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tauri::State;
use uuid::Uuid;

const MAX_API_KEY_BYTES: usize = 64 * 1024;

pub(crate) fn encrypt_api_key_with_key(
    key: &[u8; 32],
    workspace_scope: &str,
    api_key: &str,
) -> Result<Vec<u8>, String> {
    if api_key.len() > MAX_API_KEY_BYTES {
        return Err("API 密钥超过安全长度上限".to_string());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "无法初始化本机加密器".to_string())?;
    let nonce_bytes = Uuid::new_v4().as_bytes()[..12].to_vec();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: api_key.as_bytes(),
                aad: workspace_scope.as_bytes(),
            },
        )
        .map_err(|_| "无法加密 API 密钥".to_string())?;
    let mut stored = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    stored.extend_from_slice(&nonce_bytes);
    stored.extend_from_slice(&ciphertext);
    Ok(stored)
}

fn model_credential_scope(workspace_scope: &str, role: &str) -> String {
    format!("{workspace_scope}:model:{role}")
}

fn model_provider_credential_scope(workspace_scope: &str, provider_id: &str) -> String {
    format!("{workspace_scope}:model-provider:{provider_id}")
}

pub(crate) fn decrypt_api_key_with_key(
    key: &[u8; 32],
    workspace_scope: &str,
    stored: &[u8],
) -> Result<String, String> {
    if stored.len() <= 12 {
        return Err("本地 API 密钥密文格式无效".to_string());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "无法初始化本机加密器".to_string())?;
    let nonce = Nonce::from_slice(&stored[..12]);
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &stored[12..],
                aad: workspace_scope.as_bytes(),
            },
        )
        .map_err(|_| "无法解密本地 API 密钥，请重新保存 API 配置".to_string())?;
    if plaintext.len() > MAX_API_KEY_BYTES {
        return Err("本地 API 密钥超过安全长度上限".to_string());
    }
    String::from_utf8(plaintext).map_err(|_| "本地 API 密钥格式无效".to_string())
}

fn load_model_api_key(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    role: &str,
    ciphertext: &[u8],
) -> Result<Option<String>, String> {
    if ciphertext.is_empty() {
        return Ok(None);
    }
    let key = database.device_encryption_key()?;
    let scoped = model_credential_scope(workspace_scope, role);
    match decrypt_api_key_with_key(&key, &scoped, ciphertext) {
        Ok(api_key) => Ok(Some(api_key)),
        Err(_) => decrypt_api_key_with_key(&key, workspace_scope, ciphertext).map(Some),
    }
}

fn encrypt_model_provider_api_key(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    provider_id: &str,
    api_key: &str,
) -> Result<Vec<u8>, String> {
    let key = database.device_encryption_key()?;
    encrypt_api_key_with_key(
        &key,
        &model_provider_credential_scope(workspace_scope, provider_id),
        api_key,
    )
}

fn load_model_provider_api_key(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    provider_id: &str,
    ciphertext: &[u8],
) -> Result<Option<String>, String> {
    if ciphertext.is_empty() {
        return Ok(None);
    }
    let key = database.device_encryption_key()?;
    decrypt_api_key_with_key(
        &key,
        &model_provider_credential_scope(workspace_scope, provider_id),
        ciphertext,
    )
    .map(Some)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredModelProvider {
    id: String,
    name: String,
    provider: String,
    base_url: String,
    available_models: serde_json::Value,
    assignments: serde_json::Value,
    defaults: serde_json::Value,
    api_key: String,
}

fn normalize_model_provider_assignments(
    available_models: &serde_json::Value,
    assignments: &serde_json::Value,
    defaults: &serde_json::Value,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    let available = available_models
        .as_array()
        .ok_or_else(|| "供应商模型列表必须是数组".to_string())?;
    let available_ids = available
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let source_assignments = assignments
        .as_object()
        .ok_or_else(|| "模型用途必须是对象".to_string())?;
    let source_defaults = defaults
        .as_object()
        .ok_or_else(|| "默认模型必须是对象".to_string())?;
    let mut normalized_assignments = serde_json::Map::new();
    let mut normalized_defaults = serde_json::Map::new();
    for role in ["chat", "analysis", "image"] {
        let mut assigned = source_assignments
            .get(role)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|model_id| available_ids.contains(*model_id))
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let default_model = source_defaults
            .get(role)
            .and_then(serde_json::Value::as_str)
            .filter(|model_id| available_ids.contains(*model_id));
        if let Some(default_model) = default_model {
            assigned.insert(default_model.to_string());
            normalized_defaults.insert(
                role.to_string(),
                serde_json::Value::String(default_model.to_string()),
            );
        }
        normalized_assignments.insert(
            role.to_string(),
            serde_json::Value::Array(
                assigned
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    Ok((
        serde_json::Value::Object(normalized_assignments),
        serde_json::Value::Object(normalized_defaults),
    ))
}

fn provider_display_name(provider: &str) -> &str {
    match provider {
        "openai" => "OpenAI 兼容",
        "anthropic" => "Anthropic",
        "openrouter" => "OpenRouter",
        "ollama" => "本地 Ollama",
        _ => "自定义供应商",
    }
}

fn migrate_legacy_model_providers(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<(), String> {
    if !database.load_model_providers(workspace_scope)?.is_empty() {
        return Ok(());
    }
    let legacy = database.load_legacy_model_profiles(workspace_scope)?;
    if legacy.is_empty() {
        return Ok(());
    }
    struct LegacyGroup {
        provider: String,
        base_url: String,
        api_key: String,
        models: BTreeMap<String, serde_json::Value>,
        assignments: serde_json::Map<String, serde_json::Value>,
        defaults: serde_json::Map<String, serde_json::Value>,
    }
    let mut groups: Vec<LegacyGroup> = Vec::new();
    for profile in legacy {
        let api_key = load_model_api_key(
            database,
            workspace_scope,
            &profile.role,
            &profile.api_key_ciphertext,
        )?
        .unwrap_or_default();
        let index = groups
            .iter()
            .position(|group| {
                group.provider == profile.provider
                    && group.base_url == profile.base_url
                    && group.api_key == api_key
            })
            .unwrap_or_else(|| {
                groups.push(LegacyGroup {
                    provider: profile.provider.clone(),
                    base_url: profile.base_url.clone(),
                    api_key: api_key.clone(),
                    models: BTreeMap::new(),
                    assignments: serde_json::Map::new(),
                    defaults: serde_json::Map::new(),
                });
                groups.len() - 1
            });
        let group = &mut groups[index];
        for model in profile.available_models.as_array().into_iter().flatten() {
            if let Some(id) = model.get("id").and_then(serde_json::Value::as_str) {
                group
                    .models
                    .entry(id.to_string())
                    .or_insert_with(|| model.clone());
            }
        }
        group.models.entry(profile.selected_model.clone()).or_insert_with(|| {
            serde_json::json!({"id": profile.selected_model, "name": profile.selected_model})
        });
        group.assignments.insert(
            profile.role.clone(),
            serde_json::json!([profile.selected_model]),
        );
        group.defaults.insert(
            profile.role,
            serde_json::Value::String(profile.selected_model),
        );
    }
    for (index, group) in groups.into_iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        let name = if index == 0 {
            provider_display_name(&group.provider).to_string()
        } else {
            format!("{} {}", provider_display_name(&group.provider), index + 1)
        };
        let api_key_ciphertext = if group.provider == "ollama" || group.api_key.is_empty() {
            Vec::new()
        } else {
            encrypt_model_provider_api_key(database, workspace_scope, &id, &group.api_key)?
        };
        database.save_model_provider_record(
            workspace_scope,
            &id,
            &name,
            &group.provider,
            &group.base_url,
            &serde_json::Value::Array(group.models.into_values().collect()),
            &serde_json::Value::Object(group.assignments),
            &serde_json::Value::Object(group.defaults),
            &api_key_ciphertext,
        )?;
    }
    database.clear_legacy_model_profiles(workspace_scope)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn save_model_provider(
    database: State<'_, RuntimeDatabase>,
    id: String,
    name: String,
    provider: String,
    base_url: String,
    available_models: serde_json::Value,
    assignments: serde_json::Value,
    defaults: serde_json::Value,
    api_key: String,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    let id = id.trim();
    Uuid::parse_str(id).map_err(|_| "供应商 ID 无效".to_string())?;
    let name = name.trim();
    let provider = provider.trim();
    let base_url = base_url.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("供应商名称长度必须为 1 到 80 个字符".to_string());
    }
    if provider.is_empty() || base_url.is_empty() {
        return Err("供应商类型和 API URL 不能为空".to_string());
    }
    let (assignments, defaults) =
        normalize_model_provider_assignments(&available_models, &assignments, &defaults)?;
    let existing = database.load_model_provider(&workspace_scope, id)?;
    let existing_ciphertext = existing
        .as_ref()
        .map(|profile| profile.api_key_ciphertext.clone())
        .unwrap_or_default();
    let api_key = api_key.trim();
    let api_key_ciphertext = if !api_key.is_empty() {
        encrypt_model_provider_api_key(&database, &workspace_scope, id, api_key)?
    } else if provider == "ollama" {
        Vec::new()
    } else if existing_ciphertext.is_empty() {
        return Err("API 密钥不能为空".to_string());
    } else {
        existing_ciphertext
    };
    database.save_model_provider_record(
        &workspace_scope,
        id,
        name,
        provider,
        base_url,
        &available_models,
        &assignments,
        &defaults,
        &api_key_ciphertext,
    )
}

#[tauri::command]
pub fn load_model_providers(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<StoredModelProvider>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    migrate_legacy_model_providers(&database, &workspace_scope)?;
    database
        .load_model_providers(&workspace_scope)?
        .into_iter()
        .map(|profile| {
            let api_key = load_model_provider_api_key(
                &database,
                &workspace_scope,
                &profile.id,
                &profile.api_key_ciphertext,
            )?
            .unwrap_or_default();
            Ok(StoredModelProvider {
                id: profile.id,
                name: profile.name,
                provider: profile.provider,
                base_url: profile.base_url,
                available_models: profile.available_models,
                assignments: profile.assignments,
                defaults: profile.defaults,
                api_key,
            })
        })
        .collect()
}

#[tauri::command]
pub fn delete_model_provider(
    database: State<'_, RuntimeDatabase>,
    id: String,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    Uuid::parse_str(id.trim()).map_err(|_| "供应商 ID 无效".to_string())?;
    database.delete_model_provider_record(&workspace_scope, id.trim())
}
