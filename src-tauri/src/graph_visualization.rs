use serde::Serialize;

/// 图谱可视化节点
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphVizNode {
    pub id: String,
    pub label: String,
    pub size: f64,
    pub color: String,
    pub group: String,
    pub metadata: NodeMetadata,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeMetadata {
    pub note_path: String,
    pub vault_id: String,
    pub centrality_score: f64,
    pub pagerank_score: f64,
    pub incoming_links: usize,
    pub outgoing_links: usize,
    pub tags: Vec<String>,
}

/// 图谱可视化边
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphVizEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub weight: f64,
}

/// 图谱可视化布局配置
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphLayout {
    pub nodes: Vec<GraphVizNode>,
    pub edges: Vec<GraphVizEdge>,
    pub clusters: Vec<ClusterLayout>,
    pub statistics: LayoutStatistics,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterLayout {
    pub cluster_id: usize,
    pub label: String,
    pub color: String,
    pub node_ids: Vec<String>,
    pub center_x: f64,
    pub center_y: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutStatistics {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub density: f64,
    pub avg_degree: f64,
}

use crate::knowledge_graph::{EdgeType, KnowledgeGraph};
use crate::runtime_db::RuntimeDatabase;
use tauri::State;

/// 获取图谱可视化数据
#[tauri::command]
pub fn get_graph_visualization(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    max_nodes: Option<usize>,
    include_tag_edges: Option<bool>,
) -> Result<GraphLayout, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // 构建知识图谱
    let graph = KnowledgeGraph::build_from_database(&connection, &vault_id)?;

    if graph.nodes.is_empty() {
        return Ok(GraphLayout {
            nodes: vec![],
            edges: vec![],
            clusters: vec![],
            statistics: LayoutStatistics {
                total_nodes: 0,
                total_edges: 0,
                density: 0.0,
                avg_degree: 0.0,
            },
        });
    }

    let limit = max_nodes.unwrap_or(100).min(500);
    let show_tag_edges = include_tag_edges.unwrap_or(false);

    // 按 PageRank 排序，选择最重要的节点
    let mut sorted_nodes = graph.nodes.clone();
    sorted_nodes.sort_by(|a, b| {
        b.pagerank_score
            .partial_cmp(&a.pagerank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted_nodes.truncate(limit);

    let node_ids: std::collections::HashSet<String> = sorted_nodes
        .iter()
        .map(|n| n.note_path.clone())
        .collect();

    // 转换节点为可视化格式
    let viz_nodes: Vec<GraphVizNode> = sorted_nodes
        .iter()
        .map(|node| {
            // 节点大小基于 PageRank（5-50 范围）
            let size = 5.0 + (node.pagerank_score * 1000.0).min(45.0);

            // 节点颜色基于中心度
            let color = if node.centrality_score > 70.0 {
                "#ff4757".to_string() // 高中心度 - 红色
            } else if node.centrality_score > 40.0 {
                "#ffa502".to_string() // 中等中心度 - 橙色
            } else if node.centrality_score > 20.0 {
                "#1e90ff".to_string() // 低中心度 - 蓝色
            } else {
                "#95a5a6".to_string() // 孤立节点 - 灰色
            };

            // 节点分组基于主要标签
            let group = node
                .tags
                .first()
                .cloned()
                .unwrap_or_else(|| "未分类".to_string());

            GraphVizNode {
                id: node.note_path.clone(),
                label: node.title.clone(),
                size,
                color,
                group,
                metadata: NodeMetadata {
                    note_path: node.note_path.clone(),
                    vault_id: node.vault_id.clone(),
                    centrality_score: node.centrality_score,
                    pagerank_score: node.pagerank_score,
                    incoming_links: node.incoming_links,
                    outgoing_links: node.outgoing_links,
                    tags: node.tags.clone(),
                },
            }
        })
        .collect();

    // 转换边为可视化格式（仅保留在选中节点集合内的边）
    let viz_edges: Vec<GraphVizEdge> = graph
        .edges
        .iter()
        .filter(|edge| {
            node_ids.contains(&edge.from_note) && node_ids.contains(&edge.to_note)
        })
        .filter(|edge| {
            // 根据配置决定是否包含标签关联边
            show_tag_edges || edge.edge_type == EdgeType::WikiLink
        })
        .map(|edge| {
            let weight = match edge.edge_type {
                EdgeType::WikiLink => 1.0,
                EdgeType::TagRelated => 0.3,
            };

            GraphVizEdge {
                source: edge.from_note.clone(),
                target: edge.to_note.clone(),
                edge_type: format!("{:?}", edge.edge_type),
                weight,
            }
        })
        .collect();

    // 转换簇为可视化格式
    let cluster_colors = ["#e74c3c", "#3498db", "#2ecc71", "#f39c12", "#9b59b6", "#1abc9c", "#e67e22", "#34495e"];

    let viz_clusters: Vec<ClusterLayout> = graph
        .clusters
        .iter()
        .enumerate()
        .take(8) // 最多显示 8 个簇
        .map(|(idx, cluster)| {
            let color = cluster_colors[idx % cluster_colors.len()].to_string();

            // 簇中心坐标（简化版，实际布局由前端算法决定）
            let angle = (idx as f64) * std::f64::consts::PI * 2.0 / graph.clusters.len() as f64;
            let radius = 200.0;
            let center_x = radius * angle.cos();
            let center_y = radius * angle.sin();

            ClusterLayout {
                cluster_id: cluster.cluster_id,
                label: cluster.cluster_name.clone(),
                color,
                node_ids: cluster
                    .notes
                    .iter()
                    .filter(|n| node_ids.contains(*n))
                    .cloned()
                    .collect(),
                center_x,
                center_y,
            }
        })
        .collect();

    // 计算统计数据
    let total_nodes = viz_nodes.len();
    let total_edges = viz_edges.len();
    let density = if total_nodes > 1 {
        total_edges as f64 / (total_nodes * (total_nodes - 1)) as f64
    } else {
        0.0
    };
    let avg_degree = if total_nodes > 0 {
        (total_edges * 2) as f64 / total_nodes as f64
    } else {
        0.0
    };

    Ok(GraphLayout {
        nodes: viz_nodes,
        edges: viz_edges,
        clusters: viz_clusters,
        statistics: LayoutStatistics {
            total_nodes,
            total_edges,
            density,
            avg_degree,
        },
    })
}

/// 获取节点的邻居子图
#[tauri::command]
pub fn get_node_subgraph(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
    depth: Option<usize>,
) -> Result<GraphLayout, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let graph = KnowledgeGraph::build_from_database(&connection, &vault_id)?;

    let max_depth = depth.unwrap_or(2).min(3);

    // BFS 收集邻居
    let mut visited = std::collections::HashSet::new();
    let mut current_level = vec![note_path.clone()];
    visited.insert(note_path.clone());

    for _d in 0..max_depth {
        let mut next_level = Vec::new();

        for node in &current_level {
            for edge in &graph.edges {
                if &edge.from_note == node && !visited.contains(&edge.to_note) {
                    visited.insert(edge.to_note.clone());
                    next_level.push(edge.to_note.clone());
                }
                if &edge.to_note == node && !visited.contains(&edge.from_note) {
                    visited.insert(edge.from_note.clone());
                    next_level.push(edge.from_note.clone());
                }
            }
        }

        current_level = next_level;

        if current_level.is_empty() {
            break;
        }
    }

    // 过滤节点和边
    let subgraph_nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| visited.contains(&n.note_path))
        .cloned()
        .collect();

    let viz_nodes: Vec<GraphVizNode> = subgraph_nodes
        .iter()
        .map(|node| {
            let size = if node.note_path == note_path {
                50.0 // 中心节点更大
            } else {
                10.0 + (node.pagerank_score * 500.0).min(30.0)
            };

            let color = if node.note_path == note_path {
                "#ff4757".to_string() // 中心节点 - 红色
            } else if node.centrality_score > 50.0 {
                "#ffa502".to_string() // 高中心度 - 橙色
            } else {
                "#1e90ff".to_string() // 普通节点 - 蓝色
            };

            let group = node
                .tags
                .first()
                .cloned()
                .unwrap_or_else(|| "未分类".to_string());

            GraphVizNode {
                id: node.note_path.clone(),
                label: node.title.clone(),
                size,
                color,
                group,
                metadata: NodeMetadata {
                    note_path: node.note_path.clone(),
                    vault_id: node.vault_id.clone(),
                    centrality_score: node.centrality_score,
                    pagerank_score: node.pagerank_score,
                    incoming_links: node.incoming_links,
                    outgoing_links: node.outgoing_links,
                    tags: node.tags.clone(),
                },
            }
        })
        .collect();

    let viz_edges: Vec<GraphVizEdge> = graph
        .edges
        .iter()
        .filter(|e| visited.contains(&e.from_note) && visited.contains(&e.to_note))
        .map(|edge| GraphVizEdge {
            source: edge.from_note.clone(),
            target: edge.to_note.clone(),
            edge_type: format!("{:?}", edge.edge_type),
            weight: if edge.edge_type == EdgeType::WikiLink {
                1.0
            } else {
                0.3
            },
        })
        .collect();

    let total_nodes = viz_nodes.len();
    let total_edges = viz_edges.len();
    let density = if total_nodes > 1 {
        total_edges as f64 / (total_nodes * (total_nodes - 1)) as f64
    } else {
        0.0
    };
    let avg_degree = if total_nodes > 0 {
        (total_edges * 2) as f64 / total_nodes as f64
    } else {
        0.0
    };

    Ok(GraphLayout {
        nodes: viz_nodes,
        edges: viz_edges,
        clusters: vec![],
        statistics: LayoutStatistics {
            total_nodes,
            total_edges,
            density,
            avg_degree,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_size_calculation() {
        let pagerank: f64 = 0.05;
        let size = 5.0 + (pagerank * 1000.0).min(45.0);
        assert!(size >= 5.0 && size <= 50.0);
    }

    #[test]
    fn test_density_calculation() {
        let nodes = 10;
        let edges = 20;
        let density = edges as f64 / (nodes * (nodes - 1)) as f64;
        assert!((density - 0.222).abs() < 0.01);
    }
}
