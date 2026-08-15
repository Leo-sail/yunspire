/// 搜索算法模块
///
/// 包含核心搜索逻辑、RRF 计算、候选信号评分等
///
/// 注意：这是一个占位模块，完整实现将在后续迭代中添加
/// 当前版本保持与 runtime_db.rs 的兼容性

use crate::plugins::search::types::{IndexedSearchCandidate, IndexedSearchResult, NeuralSearchContext};
use rusqlite::Connection;

/// 计算搜索候选的排名信号
///
/// # 参数
/// - `candidate`: 搜索候选
/// - `normalized_query`: 规范化的查询
/// - `now`: 当前时间
///
/// # 返回
/// (title_path_bonus, relation_bonus, recency_bonus)
pub(crate) fn indexed_search_candidate_signals(
    candidate: &IndexedSearchCandidate,
    normalized_query: &str,
    now: &chrono::DateTime<chrono::Utc>,
) -> (f64, f64, f64) {
    use unicode_normalization::UnicodeNormalization;

    let title = candidate.title.nfc().collect::<String>().to_lowercase();
    let path = candidate
        .relative_path
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let tags = candidate.tags.join(" ").to_lowercase();
    let links = candidate.wiki_links.join(" ").to_lowercase();

    // 标题和路径匹配加分
    let title_path_bonus = if title == normalized_query {
        12.0
    } else if title.contains(normalized_query) {
        8.0
    } else if path.contains(normalized_query) {
        6.0
    } else {
        0.0
    };

    // 关联加分（标签、链接）
    let relation_bonus = if tags.contains(normalized_query) {
        4.0
    } else {
        0.0
    } + if links.contains(normalized_query) {
        3.0
    } else {
        0.0
    };

    // 时效性加分
    let modified_at = chrono::DateTime::parse_from_rfc3339(&candidate.modified_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok();
    let recency_bonus = if let Some(modified) = modified_at {
        let age = now.signed_duration_since(modified);
        let days = age.num_days().max(0) as f64;
        if days <= 7.0 {
            3.0
        } else if days <= 30.0 {
            2.0
        } else if days <= 90.0 {
            1.0
        } else {
            0.0
        }
    } else {
        0.0
    };

    (title_path_bonus, relation_bonus, recency_bonus)
}

/// CJK 词法分词
///
/// # 参数
/// - `value`: 输入文本
///
/// # 返回
/// 分词结果（空格分隔）
pub(crate) fn cjk_lexical_terms(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let mut terms = Vec::new();
    let mut run = Vec::new();

    let flush = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        terms.extend(run.iter().map(|c| c.to_string()));
        terms.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
        run.clear();
    };

    for character in value.nfc() {
        if is_cjk(character) {
            run.push(character);
        } else {
            flush(&mut run, &mut terms);
        }
    }
    flush(&mut run, &mut terms);

    terms.sort();
    terms.dedup();
    terms.join(" ")
}

/// 判断字符是否为 CJK
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF |   // CJK Unified Ideographs
        0x3400..=0x4DBF |   // CJK Extension A
        0x20000..=0x2A6DF | // CJK Extension B
        0x2A700..=0x2B73F | // CJK Extension C
        0x2B740..=0x2B81F | // CJK Extension D
        0x2B820..=0x2CEAF | // CJK Extension E
        0x2CEB0..=0x2EBEF | // CJK Extension F
        0x30000..=0x3134F   // CJK Extension G
    )
}

/// 在连接中执行索引搜索（带神经嵌入支持）
///
/// 注意：这是一个简化的占位实现
/// 完整的实现将在后续迭代中从 runtime_db.rs 迁移
///
/// # 参数
/// - `connection`: 数据库连接
/// - `vault_id`: 可选的 vault ID
/// - `query`: 搜索查询
/// - `max_results`: 最大结果数
/// - `neural`: 可选的神经搜索上下文
///
/// # 返回
/// 搜索结果列表
pub(crate) fn indexed_search_in_connection_with_neural(
    _connection: &Connection,
    _vault_id: Option<&str>,
    _query: &str,
    _max_results: usize,
    _neural: Option<&NeuralSearchContext>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // TODO: 从 runtime_db.rs 迁移完整实现
    // 当前返回占位错误，实际调用仍使用 runtime_db.rs 中的实现
    Err("搜索算法尚未完全迁移，请使用 runtime_db.rs 中的实现".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cjk_lexical_terms() {
        let result = cjk_lexical_terms("你好世界");
        assert!(result.contains("你"));
        assert!(result.contains("好"));
        assert!(result.contains("世"));
        assert!(result.contains("界"));
        assert!(result.contains("你好"));
        assert!(result.contains("好世"));
        assert!(result.contains("世界"));
    }

    #[test]
    fn test_is_cjk() {
        assert!(is_cjk('你'));
        assert!(is_cjk('好'));
        assert!(is_cjk('世'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('1'));
        assert!(!is_cjk(' '));
    }

    #[test]
    fn test_indexed_search_candidate_signals() {
        use chrono::Utc;

        let candidate = IndexedSearchCandidate {
            vault_id: "test".to_string(),
            relative_path: "notes/test.md".to_string(),
            title: "Test Note".to_string(),
            excerpt: "".to_string(),
            modified_at: Utc::now().to_rfc3339(),
            lexical_score: Some(1.0),
            vector_similarity: None,
            tags: vec!["test".to_string()],
            wiki_links: vec![],
        };

        let now = Utc::now();
        let (title, relation, recency) = indexed_search_candidate_signals(&candidate, "test", &now);

        assert!(title > 0.0); // 路径包含 test
        assert!(relation > 0.0); // 标签包含 test
        assert!(recency > 0.0); // 最近修改
    }
}
