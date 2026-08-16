use crate::knowledge_graph::KnowledgeGraph;
use serde::Serialize;
use std::collections::HashMap;

/// 智能学习路径
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningPath {
    pub path_id: String,
    pub title: String,
    pub description: String,
    pub notes: Vec<PathNode>,
    pub estimated_days: usize,
    pub difficulty: Difficulty,
    pub completion_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathNode {
    pub note_path: String,
    pub title: String,
    pub order: usize,
    pub prerequisites: Vec<String>,
    pub importance_score: f64,
    pub is_reviewed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Beginner,
    Intermediate,
    Advanced,
}

/// 智能标签建议
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagSuggestion {
    pub note_path: String,
    pub suggested_tags: Vec<SuggestedTag>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedTag {
    pub tag: String,
    pub confidence: f64,
    pub reason: String,
}

/// 知识缺口分析
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGap {
    pub gap_type: GapType,
    pub description: String,
    pub affected_areas: Vec<String>,
    pub suggested_topics: Vec<String>,
    pub priority: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapType {
    MissingFoundation,  // 缺少基础知识
    WeakConnection,     // 连接薄弱
    UnderdevelopedArea, // 欠发达领域
}

use crate::runtime_db::RuntimeDatabase;
use rusqlite::params;
use tauri::State;

/// 生成智能学习路径（基于图谱和重要性）
#[tauri::command]
pub fn generate_learning_path(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    start_note: Option<String>,
    max_notes: Option<usize>,
) -> Result<LearningPath, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 构建知识图谱
    let graph = KnowledgeGraph::build_from_database(&connection, &vault_id)?;

    if graph.nodes.is_empty() {
        return Err("知识库为空，无法生成学习路径".to_string());
    }

    // 获取复习记录
    let workspace_scope = database.local_workspace_scope()?;
    let mut review_map: HashMap<String, bool> = HashMap::new();

    let mut stmt = connection
        .prepare(
            "SELECT note_path FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2",
        )
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let reviewed: Vec<String> = stmt
        .query_map(params![workspace_scope, vault_id], |row| row.get(0))
        .map_err(|e| format!("查询失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    for path in reviewed {
        review_map.insert(path, true);
    }

    // 选择起点（高 PageRank 或用户指定）
    let start = if let Some(ref note) = start_note {
        graph
            .nodes
            .iter()
            .find(|n| n.note_path == *note)
            .ok_or_else(|| "起始笔记不存在".to_string())?
            .clone()
    } else {
        // 选择 PageRank 最高的未复习笔记
        graph
            .nodes
            .iter()
            .filter(|n| !review_map.contains_key(&n.note_path))
            .max_by(|a, b| {
                a.pagerank_score
                    .partial_cmp(&b.pagerank_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| "没有未复习的笔记".to_string())?
            .clone()
    };

    // 构建路径（广度优先 + PageRank 排序）
    let mut path_nodes = vec![PathNode {
        note_path: start.note_path.clone(),
        title: start.title.clone(),
        order: 0,
        prerequisites: vec![],
        importance_score: start.pagerank_score * 100.0,
        is_reviewed: review_map.contains_key(&start.note_path),
    }];

    let mut visited = std::collections::HashSet::new();
    visited.insert(start.note_path.clone());

    let limit = max_notes.unwrap_or(10).min(50);
    let mut current_order = 1;

    // 扩展路径：优先选择高 PageRank 的链接笔记
    for current in path_nodes.clone() {
        if path_nodes.len() >= limit {
            break;
        }

        let outgoing = graph
            .edges
            .iter()
            .filter(|e| e.from_note == current.note_path)
            .map(|e| &e.to_note);

        let mut candidates: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| outgoing.clone().any(|target| target == &n.note_path))
            .filter(|n| !visited.contains(&n.note_path))
            .collect();

        // 按 PageRank 排序
        candidates.sort_by(|a, b| {
            b.pagerank_score
                .partial_cmp(&a.pagerank_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for candidate in candidates {
            if path_nodes.len() >= limit {
                break;
            }

            path_nodes.push(PathNode {
                note_path: candidate.note_path.clone(),
                title: candidate.title.clone(),
                order: current_order,
                prerequisites: vec![current.note_path.clone()],
                importance_score: candidate.pagerank_score * 100.0,
                is_reviewed: review_map.contains_key(&candidate.note_path),
            });

            visited.insert(candidate.note_path.clone());
            current_order += 1;
        }
    }

    // 计算难度和完成率
    let avg_degree: f64 = path_nodes
        .iter()
        .map(|n| {
            graph
                .nodes
                .iter()
                .find(|gn| gn.note_path == n.note_path)
                .map(|gn| (gn.incoming_links + gn.outgoing_links) as f64)
                .unwrap_or(0.0)
        })
        .sum::<f64>()
        / path_nodes.len() as f64;

    let difficulty = if avg_degree < 2.0 {
        Difficulty::Beginner
    } else if avg_degree < 5.0 {
        Difficulty::Intermediate
    } else {
        Difficulty::Advanced
    };

    let reviewed_count = path_nodes.iter().filter(|n| n.is_reviewed).count();
    let completion_rate = reviewed_count as f64 / path_nodes.len() as f64;

    // 估算学习天数（每天 2-3 个笔记）
    let estimated_days = (path_nodes.len() as f64 / 2.5).ceil() as usize;

    Ok(LearningPath {
        path_id: uuid::Uuid::new_v4().to_string(),
        title: format!("从《{}》开始的学习路径", start.title),
        description: format!(
            "基于知识图谱生成的 {} 步学习路径，涵盖核心概念和相关主题",
            path_nodes.len()
        ),
        notes: path_nodes,
        estimated_days,
        difficulty,
        completion_rate,
    })
}

/// 智能标签建议（基于内容和图谱）
#[tauri::command]
pub fn suggest_tags_for_note(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
) -> Result<TagSuggestion, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 获取笔记内容和现有标签
    let (content, existing_tags_json) = connection
        .query_row(
            "SELECT content, tags_json FROM note_index
             WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let existing_tags: Vec<String> = serde_json::from_str(&existing_tags_json).unwrap_or_default();

    // 构建图谱获取所有标签
    let graph = KnowledgeGraph::build_from_database(&connection, &vault_id)?;

    let mut tag_scores: HashMap<String, f64> = HashMap::new();

    // 基于关联笔记的标签（协同过滤）
    for edge in &graph.edges {
        if edge.from_note == note_path || edge.to_note == note_path {
            let related_path = if edge.from_note == note_path {
                &edge.to_note
            } else {
                &edge.from_note
            };

            if let Some(related_node) = graph.nodes.iter().find(|n| &n.note_path == related_path) {
                for tag in &related_node.tags {
                    if !existing_tags.contains(tag) {
                        *tag_scores.entry(tag.clone()).or_insert(0.0) += 10.0;
                    }
                }
            }
        }
    }

    // 基于内容关键词（简单启发式）
    let content_lower = content.to_lowercase();
    let keyword_tags = vec![
        ("rust", "编程语言"),
        ("python", "编程语言"),
        ("javascript", "编程语言"),
        ("algorithm", "算法"),
        ("database", "数据库"),
        ("design", "设计"),
        ("architecture", "架构"),
        ("test", "测试"),
        ("performance", "性能"),
        ("security", "安全"),
    ];

    for (keyword, tag) in keyword_tags {
        if content_lower.contains(keyword) && !existing_tags.contains(&tag.to_string()) {
            *tag_scores.entry(tag.to_string()).or_insert(0.0) += 5.0;
        }
    }

    // 转换为建议列表
    let mut suggestions: Vec<SuggestedTag> = tag_scores
        .into_iter()
        .map(|(tag, score)| {
            let confidence = (score / 15.0).min(1.0);
            let reason = if score >= 10.0 {
                "相关笔记都使用了此标签".to_string()
            } else {
                "内容中出现了相关关键词".to_string()
            };

            SuggestedTag {
                tag,
                confidence,
                reason,
            }
        })
        .collect();

    // 按置信度排序
    suggestions.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    suggestions.truncate(5);

    Ok(TagSuggestion {
        note_path,
        suggested_tags: suggestions,
    })
}

/// 识别知识缺口
#[tauri::command]
pub fn identify_knowledge_gaps(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<Vec<KnowledgeGap>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let graph = KnowledgeGraph::build_from_database(&connection, &vault_id)?;

    if graph.nodes.is_empty() {
        return Ok(vec![]);
    }

    let mut gaps = Vec::new();

    // Gap 1: 高 PageRank 但低连接度（缺少基础）
    let high_pr_low_degree: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.pagerank_score > 0.02
                && (n.incoming_links + n.outgoing_links) < 3
        })
        .collect();

    if !high_pr_low_degree.is_empty() {
        gaps.push(KnowledgeGap {
            gap_type: GapType::MissingFoundation,
            description: "发现高重要性但连接稀疏的笔记，可能缺少基础概念支撑".to_string(),
            affected_areas: high_pr_low_degree
                .iter()
                .map(|n| n.title.clone())
                .collect(),
            suggested_topics: vec![
                "基础概念".to_string(),
                "前置知识".to_string(),
                "入门教程".to_string(),
            ],
            priority: 0.8,
        });
    }

    // Gap 2: 孤立的小簇（欠发达领域）
    let small_clusters: Vec<_> = graph
        .clusters
        .iter()
        .filter(|c| c.note_count < 5 && c.cohesion_score < 0.3)
        .collect();

    if !small_clusters.is_empty() {
        gaps.push(KnowledgeGap {
            gap_type: GapType::UnderdevelopedArea,
            description: "发现多个小型知识簇，内容关联性不足".to_string(),
            affected_areas: small_clusters
                .iter()
                .map(|c| c.cluster_name.clone())
                .collect(),
            suggested_topics: vec![
                "扩充相关内容".to_string(),
                "建立主题笔记".to_string(),
                "添加实例和应用".to_string(),
            ],
            priority: 0.6,
        });
    }

    // Gap 3: 高中心度但缺少出链（薄弱连接）
    let high_in_low_out: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.incoming_links > 5 && n.outgoing_links < 2)
        .collect();

    if !high_in_low_out.is_empty() {
        gaps.push(KnowledgeGap {
            gap_type: GapType::WeakConnection,
            description: "发现被频繁引用但缺少延伸的笔记，知识链断裂".to_string(),
            affected_areas: high_in_low_out.iter().map(|n| n.title.clone()).collect(),
            suggested_topics: vec![
                "深入探讨".to_string(),
                "实践案例".to_string(),
                "相关主题".to_string(),
            ],
            priority: 0.7,
        });
    }

    // 按优先级排序
    gaps.sort_by(|a, b| {
        b.priority
            .partial_cmp(&a.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_difficulty_classification() {
        let beginner_score = 1.5;
        let difficulty = if beginner_score < 2.0 {
            Difficulty::Beginner
        } else {
            Difficulty::Advanced
        };
        assert!(matches!(difficulty, Difficulty::Beginner));
    }
}
