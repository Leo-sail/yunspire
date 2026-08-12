use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// 笔记节点
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteNode {
    pub note_path: String,
    pub title: String,
    pub vault_id: String,
    /// 出链数量
    pub outgoing_links: usize,
    /// 入链数量
    pub incoming_links: usize,
    /// 标签列表
    pub tags: Vec<String>,
    /// 中心度分数（0-100）
    pub centrality_score: f64,
    /// PageRank 分数（0-1）
    pub pagerank_score: f64,
}

/// 笔记边（连接）
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEdge {
    pub from_note: String,
    pub to_note: String,
    pub edge_type: EdgeType,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    WikiLink,    // 双链
    TagRelated,  // 标签关联
}

/// 知识簇
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeCluster {
    pub cluster_id: usize,
    pub cluster_name: String,
    pub note_count: usize,
    pub notes: Vec<String>,
    pub dominant_tags: Vec<String>,
    pub cohesion_score: f64,
}

/// 知识图谱统计
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStatistics {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub avg_degree: f64,
    pub max_degree: usize,
    pub isolated_nodes: usize,
    pub hub_nodes: Vec<String>,
    pub bridge_nodes: Vec<String>,
}

/// 完整的知识图谱
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGraph {
    pub nodes: Vec<NoteNode>,
    pub edges: Vec<NoteEdge>,
    pub clusters: Vec<KnowledgeCluster>,
    pub statistics: GraphStatistics,
}

impl KnowledgeGraph {
    /// 从数据库构建知识图谱
    pub fn build_from_database(
        connection: &Connection,
        vault_id: &str,
    ) -> Result<Self, String> {
        // 1. 获取所有笔记
        let mut stmt = connection
            .prepare(
                "SELECT relative_path, title, wiki_links_json, tags_json
                 FROM note_index
                 WHERE vault_id=?1",
            )
            .map_err(|e| format!("准备查询失败：{e}"))?;

        let note_data: Vec<(String, String, String, String)> = stmt
            .query_map(params![vault_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("查询笔记失败：{e}"))?
            .filter_map(|r| r.ok())
            .collect();

        if note_data.is_empty() {
            return Ok(Self::empty());
        }

        // 2. 构建链接映射
        let mut link_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut reverse_link_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut tag_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut edges = Vec::new();

        for (path, _title, links_json, tags_json) in &note_data {
            let links: Vec<String> = serde_json::from_str(links_json).unwrap_or_default();
            let tags: Vec<String> = serde_json::from_str(tags_json).unwrap_or_default();

            // 双链边
            for link in &links {
                edges.push(NoteEdge {
                    from_note: path.clone(),
                    to_note: link.clone(),
                    edge_type: EdgeType::WikiLink,
                });

                link_map
                    .entry(path.clone())
                    .or_default()
                    .push(link.clone());

                reverse_link_map
                    .entry(link.clone())
                    .or_default()
                    .push(path.clone());
            }

            // 标签映射
            for tag in &tags {
                tag_map.entry(tag.clone()).or_default().push(path.clone());
            }
        }

        // 3. 添加标签关联边（同标签的笔记间建立关联）
        for (_tag, notes) in &tag_map {
            for i in 0..notes.len() {
                for j in (i + 1)..notes.len() {
                    edges.push(NoteEdge {
                        from_note: notes[i].clone(),
                        to_note: notes[j].clone(),
                        edge_type: EdgeType::TagRelated,
                    });
                }
            }
        }

        // 4. 计算 PageRank
        let pagerank_scores = Self::calculate_pagerank(&link_map, &note_data, 20, 0.85);

        // 5. 构建节点列表
        let mut nodes = Vec::new();
        for (path, title, links_json, tags_json) in &note_data {
            let links: Vec<String> = serde_json::from_str(links_json).unwrap_or_default();
            let tags: Vec<String> = serde_json::from_str(tags_json).unwrap_or_default();

            let outgoing = links.len();
            let incoming = reverse_link_map.get(path).map(|v| v.len()).unwrap_or(0);
            let total_degree = outgoing + incoming;

            let centrality_score = Self::calculate_centrality(total_degree, note_data.len());
            let pagerank_score = *pagerank_scores.get(path).unwrap_or(&0.0);

            nodes.push(NoteNode {
                note_path: path.clone(),
                title: title.clone(),
                vault_id: vault_id.to_string(),
                outgoing_links: outgoing,
                incoming_links: incoming,
                tags,
                centrality_score,
                pagerank_score,
            });
        }

        // 6. 检测知识簇
        let clusters = Self::detect_clusters(&nodes, &edges, &tag_map);

        // 7. 计算统计数据
        let statistics = Self::calculate_statistics(&nodes, &edges);

        Ok(KnowledgeGraph {
            nodes,
            edges,
            clusters,
            statistics,
        })
    }

    fn empty() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            clusters: vec![],
            statistics: GraphStatistics {
                total_nodes: 0,
                total_edges: 0,
                avg_degree: 0.0,
                max_degree: 0,
                isolated_nodes: 0,
                hub_nodes: vec![],
                bridge_nodes: vec![],
            },
        }
    }

    /// 计算中心度分数（基于度数）
    fn calculate_centrality(degree: usize, total_nodes: usize) -> f64 {
        if total_nodes <= 1 {
            return 0.0;
        }
        // 归一化到 0-100
        let max_possible_degree = (total_nodes - 1) as f64;
        (degree as f64 / max_possible_degree * 100.0).min(100.0)
    }

    /// 计算 PageRank（简化版）
    fn calculate_pagerank(
        link_map: &HashMap<String, Vec<String>>,
        note_data: &[(String, String, String, String)],
        iterations: usize,
        damping: f64,
    ) -> HashMap<String, f64> {
        let n = note_data.len();
        if n == 0 {
            return HashMap::new();
        }

        let initial_score = 1.0 / n as f64;
        let mut scores: HashMap<String, f64> = note_data
            .iter()
            .map(|(path, _, _, _)| (path.clone(), initial_score))
            .collect();

        for _ in 0..iterations {
            let mut new_scores = HashMap::new();

            for (path, _, _, _) in note_data {
                let mut rank = (1.0 - damping) / n as f64;

                // 累加所有指向此笔记的链接贡献
                for (from_path, to_paths) in link_map {
                    if to_paths.contains(path) {
                        let from_score = scores.get(from_path).unwrap_or(&initial_score);
                        let out_degree = to_paths.len() as f64;
                        rank += damping * from_score / out_degree;
                    }
                }

                new_scores.insert(path.clone(), rank);
            }

            scores = new_scores;
        }

        scores
    }

    /// 检测知识簇（基于标签和链接密度）
    fn detect_clusters(
        nodes: &[NoteNode],
        edges: &[NoteEdge],
        tag_map: &HashMap<String, Vec<String>>,
    ) -> Vec<KnowledgeCluster> {
        let mut clusters = Vec::new();
        let mut cluster_id = 0;

        // 基于标签的簇
        for (tag, note_paths) in tag_map {
            if note_paths.len() < 3 {
                continue; // 忽略小簇
            }

            // 计算簇的凝聚度（内部连接密度）
            let internal_edges = edges
                .iter()
                .filter(|e| note_paths.contains(&e.from_note) && note_paths.contains(&e.to_note))
                .count();

            let max_possible_edges = note_paths.len() * (note_paths.len() - 1);
            let cohesion_score = if max_possible_edges > 0 {
                internal_edges as f64 / max_possible_edges as f64
            } else {
                0.0
            };

            clusters.push(KnowledgeCluster {
                cluster_id,
                cluster_name: format!("#{}", tag),
                note_count: note_paths.len(),
                notes: note_paths.clone(),
                dominant_tags: vec![tag.clone()],
                cohesion_score,
            });

            cluster_id += 1;
        }

        // 按笔记数量排序
        clusters.sort_by(|a, b| b.note_count.cmp(&a.note_count));
        clusters.truncate(20); // 只保留前 20 个簇

        clusters
    }

    /// 计算图统计数据
    fn calculate_statistics(nodes: &[NoteNode], edges: &[NoteEdge]) -> GraphStatistics {
        let total_nodes = nodes.len();
        let total_edges = edges.len();

        if total_nodes == 0 {
            return GraphStatistics {
                total_nodes: 0,
                total_edges: 0,
                avg_degree: 0.0,
                max_degree: 0,
                isolated_nodes: 0,
                hub_nodes: vec![],
                bridge_nodes: vec![],
            };
        }

        // 计算度数统计
        let total_degree: usize = nodes
            .iter()
            .map(|n| n.outgoing_links + n.incoming_links)
            .sum();
        let avg_degree = total_degree as f64 / total_nodes as f64;
        let max_degree = nodes
            .iter()
            .map(|n| n.outgoing_links + n.incoming_links)
            .max()
            .unwrap_or(0);

        // 孤立节点（度数为 0）
        let isolated_nodes = nodes
            .iter()
            .filter(|n| n.outgoing_links == 0 && n.incoming_links == 0)
            .count();

        // Hub 节点（高中心度）
        let mut sorted_by_centrality = nodes.to_vec();
        sorted_by_centrality.sort_by(|a, b| {
            b.centrality_score
                .partial_cmp(&a.centrality_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let hub_nodes: Vec<String> = sorted_by_centrality
            .iter()
            .take(5)
            .filter(|n| n.centrality_score > 20.0)
            .map(|n| n.note_path.clone())
            .collect();

        // Bridge 节点（高 PageRank 但中等中心度）
        let mut sorted_by_pagerank = nodes.to_vec();
        sorted_by_pagerank.sort_by(|a, b| {
            b.pagerank_score
                .partial_cmp(&a.pagerank_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let bridge_nodes: Vec<String> = sorted_by_pagerank
            .iter()
            .take(10)
            .filter(|n| n.pagerank_score > avg_degree / total_nodes as f64)
            .filter(|n| n.centrality_score < 50.0 && n.centrality_score > 10.0)
            .map(|n| n.note_path.clone())
            .collect();

        GraphStatistics {
            total_nodes,
            total_edges,
            avg_degree,
            max_degree,
            isolated_nodes,
            hub_nodes,
            bridge_nodes,
        }
    }
}

use crate::runtime_db::RuntimeDatabase;
use tauri::State;

/// 构建知识图谱
#[tauri::command]
pub fn build_knowledge_graph(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<KnowledgeGraph, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    KnowledgeGraph::build_from_database(&connection, &vault_id)
}

/// 获取核心节点（Hub）
#[tauri::command]
pub fn get_hub_nodes(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    limit: Option<usize>,
) -> Result<Vec<NoteNode>, String> {
    let graph = build_knowledge_graph(database, vault_id)?;

    let mut hubs: Vec<NoteNode> = graph
        .nodes
        .into_iter()
        .filter(|n| n.centrality_score > 20.0)
        .collect();

    hubs.sort_by(|a, b| {
        b.centrality_score
            .partial_cmp(&a.centrality_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_limit = limit.unwrap_or(10).min(50);
    hubs.truncate(max_limit);

    Ok(hubs)
}

/// 获取孤立节点
#[tauri::command]
pub fn get_isolated_nodes(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<Vec<NoteNode>, String> {
    let graph = build_knowledge_graph(database, vault_id)?;

    let isolated: Vec<NoteNode> = graph
        .nodes
        .into_iter()
        .filter(|n| n.outgoing_links == 0 && n.incoming_links == 0)
        .collect();

    Ok(isolated)
}

/// 查找关联笔记（基于链接和标签）
#[tauri::command]
pub fn find_related_notes(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
    max_results: Option<usize>,
) -> Result<Vec<RelatedNote>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 获取目标笔记的链接和标签
    let (links_json, tags_json) = connection
        .query_row(
            "SELECT wiki_links_json, tags_json FROM note_index
             WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    let links: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
    let tags: HashSet<String> = serde_json::from_str(&tags_json).unwrap_or_default();

    // 查找所有笔记
    let mut stmt = connection
        .prepare(
            "SELECT relative_path, title, wiki_links_json, tags_json
             FROM note_index
             WHERE vault_id=?1 AND relative_path != ?2",
        )
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let candidates: Vec<(String, String, String, String)> = stmt
        .query_map(params![vault_id, note_path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("查询失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut related = Vec::new();

    for (path, title, cand_links_json, cand_tags_json) in candidates {
        let cand_links: Vec<String> = serde_json::from_str(&cand_links_json).unwrap_or_default();
        let cand_tags: HashSet<String> = serde_json::from_str(&cand_tags_json).unwrap_or_default();

        let mut relevance = 0.0;
        let mut relation_types = Vec::new();

        // 直接链接
        if links.contains(&path) {
            relevance += 50.0;
            relation_types.push("outgoing_link".to_string());
        }
        if cand_links.contains(&note_path) {
            relevance += 50.0;
            relation_types.push("incoming_link".to_string());
        }

        // 标签相似度
        let common_tags: Vec<String> = tags.intersection(&cand_tags).cloned().collect();
        if !common_tags.is_empty() {
            relevance += common_tags.len() as f64 * 10.0;
            relation_types.push("shared_tags".to_string());
        }

        if relevance > 0.0 {
            related.push(RelatedNote {
                note_path: path,
                title,
                relevance_score: relevance.min(100.0),
                relation_types,
                common_tags,
            });
        }
    }

    // 按相关性排序
    related.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let limit = max_results.unwrap_or(20).min(100);
    related.truncate(limit);

    Ok(related)
}

/// 关联笔记
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedNote {
    pub note_path: String,
    pub title: String,
    pub relevance_score: f64,
    pub relation_types: Vec<String>,
    pub common_tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_centrality() {
        assert_eq!(KnowledgeGraph::calculate_centrality(0, 10), 0.0);
        assert_eq!(KnowledgeGraph::calculate_centrality(9, 10), 100.0);
        assert!((KnowledgeGraph::calculate_centrality(5, 10) - 55.55).abs() < 0.1);
    }

    #[test]
    fn test_empty_graph() {
        let graph = KnowledgeGraph::empty();
        assert_eq!(graph.nodes.len(), 0);
        assert_eq!(graph.edges.len(), 0);
        assert_eq!(graph.statistics.total_nodes, 0);
    }
}
