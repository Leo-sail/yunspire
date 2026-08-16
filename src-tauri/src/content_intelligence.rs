use serde::Serialize;

/// 笔记摘要
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub note_path: String,
    pub summary: String,
    pub key_points: Vec<String>,
    pub word_count: usize,
    pub summary_ratio: f64,
}

/// 关键词提取结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordExtractionResult {
    pub note_path: String,
    pub keywords: Vec<Keyword>,
    pub topics: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyword {
    pub term: String,
    pub score: f64,
    pub frequency: usize,
}

/// 主题识别结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicIdentificationResult {
    pub note_path: String,
    pub primary_topic: String,
    pub secondary_topics: Vec<String>,
    pub confidence: f64,
}

/// 内容推荐
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentRecommendation {
    pub note_path: String,
    pub title: String,
    pub relevance_score: f64,
    pub reason: String,
}

use crate::runtime_db::RuntimeDatabase;
use rusqlite::params;
use std::collections::HashMap;
use tauri::State;

/// 生成笔记摘要（基于启发式）
#[tauri::command]
pub fn generate_note_summary(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
    max_sentences: Option<usize>,
) -> Result<NoteSummary, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 获取笔记内容
    let content = connection
        .query_row(
            "SELECT content FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let word_count = content.split_whitespace().count();
    let max_sent = max_sentences.unwrap_or(3).min(10);

    // 简单的摘要生成：选择最重要的句子
    let sentences: Vec<&str> = content
        .split(&['.', '。', '!', '！', '?', '？'][..])
        .filter(|s| !s.trim().is_empty())
        .collect();

    if sentences.is_empty() {
        return Err("笔记内容为空".to_string());
    }

    // 计算句子重要性（基于关键词密度）
    let keywords = extract_keywords_internal(&content, 10);
    let keyword_set: std::collections::HashSet<String> =
        keywords.iter().map(|k| k.term.to_lowercase()).collect();

    let mut sentence_scores: Vec<(usize, f64)> = sentences
        .iter()
        .enumerate()
        .map(|(idx, sentence)| {
            let words: Vec<String> = sentence
                .split_whitespace()
                .map(|w| w.to_lowercase())
                .collect();

            let keyword_count = words
                .iter()
                .filter(|w| keyword_set.contains(*w))
                .count();

            let score = keyword_count as f64 / words.len().max(1) as f64;
            (idx, score)
        })
        .collect();

    // 按分数排序
    sentence_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 选择 top N 句子，按原文顺序排列
    let mut selected_indices: Vec<usize> = sentence_scores
        .iter()
        .take(max_sent)
        .map(|(idx, _)| *idx)
        .collect();
    selected_indices.sort();

    let summary = selected_indices
        .iter()
        .map(|&idx| sentences[idx].trim())
        .collect::<Vec<_>>()
        .join("。");

    // 提取要点（高分句子的简化）
    let key_points: Vec<String> = sentence_scores
        .iter()
        .take(max_sent.min(5))
        .map(|(idx, _)| {
            let sentence = sentences[*idx].trim();
            // 简化：取前 50 字符
            if sentence.len() > 50 {
                format!("{}...", &sentence[..50])
            } else {
                sentence.to_string()
            }
        })
        .collect();

    let summary_ratio = summary.len() as f64 / content.len().max(1) as f64;

    Ok(NoteSummary {
        note_path,
        summary,
        key_points,
        word_count,
        summary_ratio,
    })
}

/// 提取关键词（TF-IDF 简化版）
#[tauri::command]
pub fn extract_keywords(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
    max_keywords: Option<usize>,
) -> Result<KeywordExtractionResult, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let content = connection
        .query_row(
            "SELECT content FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let max_kw = max_keywords.unwrap_or(10).min(50);
    let keywords = extract_keywords_internal(&content, max_kw);

    // 提取主题（基于关键词聚类）
    let topics = extract_topics_from_keywords(&keywords);

    Ok(KeywordExtractionResult {
        note_path,
        keywords,
        topics,
    })
}

fn extract_keywords_internal(content: &str, max_keywords: usize) -> Vec<Keyword> {
    // 停用词列表
    let stop_words: std::collections::HashSet<&str> = [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "up", "about", "into", "through", "is", "was", "are", "were", "be", "been",
        "being", "have", "has", "had", "do", "does", "did", "will", "would", "should", "could",
        "may", "might", "must", "can", "this", "that", "these", "those", "i", "you", "he", "she",
        "it", "we", "they", "what", "which", "who", "when", "where", "why", "how",
    ]
    .iter()
    .copied()
    .collect();

    // 统计词频
    let mut word_freq: HashMap<String, usize> = HashMap::new();
    for word in content.split_whitespace() {
        let clean_word = word
            .to_lowercase()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string();

        if clean_word.len() > 2 && !stop_words.contains(clean_word.as_str()) {
            *word_freq.entry(clean_word).or_insert(0) += 1;
        }
    }

    // 计算 TF 分数（简化版）
    let total_words = word_freq.values().sum::<usize>() as f64;
    let mut keywords: Vec<Keyword> = word_freq
        .into_iter()
        .map(|(term, freq)| {
            let tf = freq as f64 / total_words;
            let score = tf * (1.0 + (freq as f64).ln()); // TF * log(freq)
            Keyword {
                term,
                score,
                frequency: freq,
            }
        })
        .collect();

    // 按分数排序
    keywords.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    keywords.truncate(max_keywords);

    keywords
}

fn extract_topics_from_keywords(keywords: &[Keyword]) -> Vec<String> {
    // 简单的主题识别：基于关键词的常见模式
    let topic_keywords = [
        ("programming", vec!["code", "function", "class", "method", "api", "algorithm"]),
        ("data", vec!["data", "database", "query", "table", "record", "schema"]),
        ("design", vec!["design", "ui", "ux", "interface", "layout", "component"]),
        ("testing", vec!["test", "testing", "unit", "integration", "coverage"]),
        ("performance", vec!["performance", "optimization", "speed", "latency", "throughput"]),
        ("security", vec!["security", "authentication", "authorization", "encryption"]),
    ];

    let mut topics = Vec::new();
    for (topic, patterns) in &topic_keywords {
        for keyword in keywords.iter().take(10) {
            if patterns.contains(&keyword.term.as_str()) {
                topics.push(topic.to_string());
                break;
            }
        }
    }

    topics
}

/// 识别笔记主题
#[tauri::command]
pub fn identify_note_topic(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
) -> Result<TopicIdentificationResult, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let (content, tags_json) = connection
        .query_row(
            "SELECT content, tags_json FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

    // 如果有标签，使用标签作为主题
    if !tags.is_empty() {
        let primary_topic = tags[0].clone();
        let secondary_topics = tags.iter().skip(1).take(3).cloned().collect();
        return Ok(TopicIdentificationResult {
            note_path,
            primary_topic,
            secondary_topics,
            confidence: 0.9,
        });
    }

    // 否则基于关键词推断主题
    let keywords = extract_keywords_internal(&content, 20);
    let topics = extract_topics_from_keywords(&keywords);

    let primary_topic = topics.first().cloned().unwrap_or_else(|| "general".to_string());
    let secondary_topics = topics.iter().skip(1).take(3).cloned().collect();
    let confidence = if topics.is_empty() { 0.3 } else { 0.6 };

    Ok(TopicIdentificationResult {
        note_path,
        primary_topic,
        secondary_topics,
        confidence,
    })
}

/// 基于内容的推荐引擎
#[tauri::command]
pub fn recommend_similar_content(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
    max_recommendations: Option<usize>,
) -> Result<Vec<ContentRecommendation>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 获取目标笔记的关键词
    let content = connection
        .query_row(
            "SELECT content FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let target_keywords = extract_keywords_internal(&content, 10);
    let target_keyword_set: std::collections::HashSet<String> = target_keywords
        .iter()
        .map(|k| k.term.clone())
        .collect();

    // 获取所有其他笔记
    let mut stmt = connection
        .prepare(
            "SELECT relative_path, title, content FROM note_index
             WHERE vault_id=?1 AND relative_path != ?2",
        )
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let candidates: Vec<(String, String, String)> = stmt
        .query_map(params![vault_id, note_path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("查询失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    // 计算相似度
    let mut recommendations: Vec<ContentRecommendation> = candidates
        .iter()
        .map(|(path, title, cand_content)| {
            let cand_keywords = extract_keywords_internal(cand_content, 10);
            let cand_keyword_set: std::collections::HashSet<String> =
                cand_keywords.iter().map(|k| k.term.clone()).collect();

            // Jaccard 相似度
            let intersection: usize = target_keyword_set
                .intersection(&cand_keyword_set)
                .count();
            let union: usize = target_keyword_set.union(&cand_keyword_set).count();
            let relevance_score = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            let common_keywords: Vec<String> = target_keyword_set
                .intersection(&cand_keyword_set)
                .cloned()
                .collect();

            let reason = if !common_keywords.is_empty() {
                format!("共同关键词：{}", common_keywords.join(", "))
            } else {
                "相关内容".to_string()
            };

            ContentRecommendation {
                note_path: path.clone(),
                title: title.clone(),
                relevance_score,
                reason,
            }
        })
        .filter(|rec| rec.relevance_score > 0.1) // 过滤低相似度
        .collect();

    // 按相似度排序
    recommendations.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let limit = max_recommendations.unwrap_or(10).min(50);
    recommendations.truncate(limit);

    Ok(recommendations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords_stopwords() {
        let content = "the quick brown fox jumps over the lazy dog";
        let keywords = extract_keywords_internal(content, 5);
        // 停用词应该被过滤
        assert!(keywords.iter().all(|k| k.term != "the"));
    }

    #[test]
    fn test_keyword_scoring() {
        let content = "rust rust programming language rust code";
        let keywords = extract_keywords_internal(content, 3);
        // "rust" 应该是最高分
        assert_eq!(keywords[0].term, "rust");
        assert_eq!(keywords[0].frequency, 3);
    }
}
