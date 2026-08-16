use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// 搜索相关常量
pub(crate) const LOCAL_FEATURE_VECTOR_VERSION: i64 = 1;
pub(crate) const LOCAL_FEATURE_VECTOR_DIMENSIONS: usize = 384;
pub(crate) const MAX_LOCAL_VECTOR_CONTENT_CHARS: usize = 250_000;
pub(crate) const MIN_LOCAL_VECTOR_SIMILARITY: f64 = 0.025;
pub(crate) const MIN_NEURAL_EMBEDDING_SIMILARITY: f64 = 0.1;
pub(crate) const MAX_NEURAL_EMBEDDING_INPUT_CHARS: usize = 24_000;
pub(crate) const NEURAL_EMBEDDING_BATCH_SIZE: usize = 32;
pub(crate) const MAX_NEURAL_EMBEDDING_REFRESH_NOTES: usize = 64;
pub(crate) const NEURAL_RRF_WEIGHT: f64 = 2.0;
pub(crate) const LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL: f64 = 0.5;
pub(crate) const RRF_K: f64 = 60.0;

/// 归一化神经嵌入向量（L2 范数）
///
/// # 参数
/// - `vector`: 原始向量
///
/// # 返回
/// - `Ok(vector)`: 归一化后的向量
/// - `Err(String)`: 向量无效（空、过大、非有限值、零向量）
pub(crate) fn normalize_neural_embedding(mut vector: Vec<f32>) -> Result<Vec<f32>, String> {
    if vector.is_empty() || vector.len() > 65_536 || vector.iter().any(|value| !value.is_finite()) {
        return Err("神经 Embedding 向量为空、过大或包含非有限数值".to_string());
    }

    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();

    if !norm.is_finite() || norm <= f64::EPSILON {
        return Err("神经 Embedding 向量不能是零向量".to_string());
    }

    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }

    Ok(vector)
}

/// 编码神经嵌入向量为数据库存储格式
///
/// # 参数
/// - `vector`: 原始向量
///
/// # 返回
/// - `Ok((dimensions, blob))`: 维度和 blob 数据
/// - `Err(String)`: 编码失败
pub(crate) fn encode_neural_embedding(vector: Vec<f32>) -> Result<(i64, Vec<u8>), String> {
    let vector = normalize_neural_embedding(vector)?;
    let dimensions = vector.len() as i64;
    let blob = vector
        .into_iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    Ok((dimensions, blob))
}

/// 解码神经嵌入向量
///
/// # 参数
/// - `dimensions`: 向量维度
/// - `blob`: blob 数据
///
/// # 返回
/// - `Some(vector)`: 解码成功
/// - `None`: 解码失败（维度不匹配）
pub(crate) fn decode_neural_embedding(dimensions: i64, blob: &[u8]) -> Option<Vec<f32>> {
    let expected_bytes = dimensions as usize * 4;
    if blob.len() != expected_bytes {
        return None;
    }
    let vector = blob
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Some(vector)
}

/// 计算神经嵌入输入的哈希值
///
/// # 参数
/// - `input`: 输入字符串
///
/// # 返回
/// SHA-256 哈希值（格式: "sha256:hex"）
pub(crate) fn neural_embedding_input_hash(input: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(input.as_bytes()))
}

/// 生成神经笔记嵌入的输入
///
/// # 参数
/// - `relative_path`: 笔记相对路径
/// - `title`: 笔记标题
/// - `tags_json`: 标签 JSON
/// - `wiki_links_json`: Wiki 链接 JSON
/// - `content`: 笔记内容
///
/// # 返回
/// 格式化的嵌入输入字符串
pub(crate) fn neural_note_embedding_input(
    relative_path: &str,
    title: &str,
    tags_json: &str,
    wiki_links_json: &str,
    content: &str,
) -> String {
    // 规范化并截断内容
    let content = content
        .nfc()
        .take(MAX_NEURAL_EMBEDDING_INPUT_CHARS)
        .collect::<String>();

    // 渲染提示模板
    const NEURAL_NOTE_EMBEDDING_PROMPT_TEMPLATE: &str =
        include_str!("../../../../prompts/runtime/search/neural-note-embedding.template.txt");

    crate::prompt::render_prompt_template(
        NEURAL_NOTE_EMBEDDING_PROMPT_TEMPLATE,
        &[
            ("title", title),
            ("relative_path", relative_path),
            ("tags_json", tags_json),
            ("wiki_links_json", wiki_links_json),
            ("content", &content),
        ],
    )
    .expect("bundled neural note embedding Prompt must be valid")
    .nfc()
    .take(crate::model_provider::MAX_EMBEDDING_INPUT_CHARS)
    .collect()
}

/// 规范化神经嵌入的 vault ID
///
/// # 参数
/// - `vault_id`: 可选的 vault ID
///
/// # 返回
/// - `Ok(Some(id))`: 有效的 vault ID
/// - `Ok(None)`: 空或 "all"
/// - `Err(String)`: 无效的 vault ID
pub(crate) fn normalize_neural_embedding_vault_id(
    vault_id: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(vault_id) = vault_id.map(str::trim) else {
        return Ok(None);
    };

    if vault_id.is_empty() || vault_id == "all" {
        return Ok(None);
    }

    if vault_id.chars().count() > 160 || vault_id.contains('\0') {
        return Err("Vault ID 无效或超过 160 个字符".to_string());
    }

    Ok(Some(vault_id.to_string()))
}

/// 神经嵌入状态优先级（用于排序）
///
/// # 参数
/// - `state`: 状态字符串
///
/// # 返回
/// 优先级（数字越大越优先）
pub(crate) fn neural_embedding_state_priority(state: &str) -> u8 {
    match state {
        "failed" => 5,
        "degraded" => 4,
        "building" => 3,
        "pending" => 2,
        "ready" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_neural_embedding() {
        // 正常向量
        let vector = vec![3.0, 4.0];
        let normalized = normalize_neural_embedding(vector).unwrap();
        assert!((normalized[0] - 0.6).abs() < 0.001);
        assert!((normalized[1] - 0.8).abs() < 0.001);

        // 空向量
        let result = normalize_neural_embedding(vec![]);
        assert!(result.is_err());

        // 零向量
        let result = normalize_neural_embedding(vec![0.0, 0.0]);
        assert!(result.is_err());

        // 非有限值
        let result = normalize_neural_embedding(vec![f32::NAN, 1.0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_decode_neural_embedding() {
        let original = vec![0.6, 0.8];
        let (dimensions, blob) = encode_neural_embedding(original.clone()).unwrap();

        assert_eq!(dimensions, 2);

        let decoded = decode_neural_embedding(dimensions, &blob).unwrap();
        assert_eq!(decoded.len(), 2);
        assert!((decoded[0] - 0.6).abs() < 0.001);
        assert!((decoded[1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_decode_invalid_dimensions() {
        let blob = vec![0u8; 8]; // 2 个 f32
        let result = decode_neural_embedding(3, &blob); // 期望 3 个维度
        assert!(result.is_none());
    }

    #[test]
    fn test_neural_embedding_input_hash() {
        let hash1 = neural_embedding_input_hash("test input");
        let hash2 = neural_embedding_input_hash("test input");
        let hash3 = neural_embedding_input_hash("different input");

        assert_eq!(hash1, hash2); // 相同输入产生相同哈希
        assert_ne!(hash1, hash3); // 不同输入产生不同哈希
        assert!(hash1.starts_with("sha256:"));
    }

    #[test]
    fn test_normalize_vault_id() {
        // 正常 ID
        assert_eq!(
            normalize_neural_embedding_vault_id(Some("vault1")).unwrap(),
            Some("vault1".to_string())
        );

        // 空字符串
        assert_eq!(
            normalize_neural_embedding_vault_id(Some("")).unwrap(),
            None
        );

        // "all"
        assert_eq!(
            normalize_neural_embedding_vault_id(Some("all")).unwrap(),
            None
        );

        // None
        assert_eq!(normalize_neural_embedding_vault_id(None).unwrap(), None);

        // 带空格
        assert_eq!(
            normalize_neural_embedding_vault_id(Some("  vault1  ")).unwrap(),
            Some("vault1".to_string())
        );
    }

    #[test]
    fn test_neural_embedding_state_priority() {
        assert_eq!(neural_embedding_state_priority("failed"), 5);
        assert_eq!(neural_embedding_state_priority("degraded"), 4);
        assert_eq!(neural_embedding_state_priority("building"), 3);
        assert_eq!(neural_embedding_state_priority("pending"), 2);
        assert_eq!(neural_embedding_state_priority("ready"), 1);
        assert_eq!(neural_embedding_state_priority("unknown"), 0);
    }
}
