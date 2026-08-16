/// 核心搜索实现模块
///
/// 包含完整的 indexed_search_in_connection_with_neural 实现

use crate::database::QueryProfiler;
use crate::plugins::search::algorithm::indexed_search_candidate_signals;
use crate::plugins::search::encoding::{
    LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL, NEURAL_RRF_WEIGHT, RRF_K,
};
use crate::plugins::search::types::{
    IndexedSearchCandidate, IndexedSearchResult, IndexedSearchSignals, NeuralSearchContext,
};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// 搜索查询最大字符数
const MAX_SEARCH_QUERY_CHARS: usize = 512;

/// 在数据库连接中执行完整的索引搜索（带神经嵌入支持）
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
    connection: &Connection,
    vault_id: Option<&str>,
    query: &str,
    max_results: usize,
    neural: Option<&NeuralSearchContext>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("indexed_search_in_connection_with_neural")
        .with_threshold(100);

    // 验证输入
    let query = query.trim();
    if query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }

    let scoped = vault_id.filter(|value| *value != "all");
    let candidate_limit = (max_results * 5).min(1_000);
    let normalized_query = query.nfc().collect::<String>().to_lowercase();
    let now = Utc::now();

    // 1. 词法搜索
    let mut lexical_candidates =
        load_lexical_search_candidates(connection, scoped, query, candidate_limit as i64)?;

    lexical_candidates.sort_by(|left, right| {
        let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
        let right_signals = indexed_search_candidate_signals(right, &normalized_query, &now);
        let left_score =
            -left.lexical_score.unwrap_or_default() + left_signals.0 + left_signals.1 + left_signals.2;
        let right_score = -right.lexical_score.unwrap_or_default()
            + right_signals.0
            + right_signals.1
            + right_signals.2;
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    lexical_candidates.truncate(candidate_limit);

    // 2. 本地向量搜索
    let mut local_vector_candidates = match load_vector_search_candidates(connection, scoped, query)
    {
        Ok(candidates) => candidates,
        Err(error) => {
            log::warn!("本地特征向量不可用，继续使用 FTS：{error}");
            Vec::new()
        }
    };

    local_vector_candidates.sort_by(|left, right| {
        right
            .vector_similarity
            .unwrap_or_default()
            .partial_cmp(&left.vector_similarity.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
                let right_signals =
                    indexed_search_candidate_signals(right, &normalized_query, &now);
                (right_signals.0 + right_signals.1 + right_signals.2)
                    .partial_cmp(&(left_signals.0 + left_signals.1 + left_signals.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    local_vector_candidates.truncate(candidate_limit);

    // 3. 神经嵌入搜索
    let (mut neural_candidates, neural_cache_degraded) = match neural {
        Some(context) => match load_neural_search_candidates(connection, scoped, context) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("神经 Embedding 候选不可用，继续使用 FTS 与本地向量：{error}");
                (Vec::new(), true)
            }
        },
        None => (Vec::new(), false),
    };

    neural_candidates.sort_by(|left, right| {
        right
            .vector_similarity
            .unwrap_or_default()
            .partial_cmp(&left.vector_similarity.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    neural_candidates.truncate(candidate_limit);

    // 4. 构建排名映射
    let lexical_ranks = build_rank_map(&lexical_candidates);
    let local_vector_ranks = build_rank_map(&local_vector_candidates);
    let neural_ranks = build_rank_map(&neural_candidates);
    let local_vector_similarities = build_similarity_map(&local_vector_candidates);
    let neural_similarities = build_similarity_map(&neural_candidates);
    let neural_active = !neural_ranks.is_empty();

    // 5. 融合候选
    let mut fused = HashMap::new();
    for candidate in lexical_candidates {
        fused.insert(
            (candidate.vault_id.clone(), candidate.relative_path.clone()),
            candidate,
        );
    }
    for candidate in local_vector_candidates {
        let key = (candidate.vault_id.clone(), candidate.relative_path.clone());
        fused
            .entry(key)
            .and_modify(|existing: &mut IndexedSearchCandidate| {
                if existing.excerpt.is_empty() {
                    existing.excerpt.clone_from(&candidate.excerpt);
                }
            })
            .or_insert(candidate);
    }
    for candidate in neural_candidates {
        let key = (candidate.vault_id.clone(), candidate.relative_path.clone());
        fused
            .entry(key)
            .and_modify(|existing: &mut IndexedSearchCandidate| {
                if existing.excerpt.is_empty() {
                    existing.excerpt.clone_from(&candidate.excerpt);
                }
            })
            .or_insert(candidate);
    }

    // 6. RRF 融合并计算最终分数
    let mut results = fused
        .into_iter()
        .map(|(key, candidate)| {
            let lexical_rank = lexical_ranks.get(&key).copied();
            let neural_rank = neural_ranks.get(&key).copied();
            let local_vector_rank = local_vector_ranks.get(&key).copied();

            // 计算 RRF 分数
            let lexical_rrf = lexical_rank
                .map(|rank| 1.0 / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let neural_rrf = neural_rank
                .map(|rank| NEURAL_RRF_WEIGHT / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let local_vector_rrf = local_vector_rank
                .map(|rank| {
                    let weight = if neural_active {
                        LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL
                    } else {
                        1.0
                    };
                    weight / (RRF_K + rank as f64)
                })
                .unwrap_or(0.0);

            let vector_rank = neural_rank.or(local_vector_rank);
            let vector_rrf = if neural_rank.is_some() {
                neural_rrf
            } else {
                local_vector_rrf
            };

            let neural_similarity = neural_similarities.get(&key).copied();
            let local_vector_similarity = local_vector_similarities.get(&key).copied();
            let vector_similarity = neural_similarity.or(local_vector_similarity);

            // 计算额外信号
            let (title_path_bonus, relation_bonus, recency_bonus) =
                indexed_search_candidate_signals(&candidate, &normalized_query, &now);

            // 最终分数 = RRF 分数 + 额外信号
            let score =
                lexical_rrf + vector_rrf + title_path_bonus + relation_bonus + recency_bonus;

            IndexedSearchResult {
                vault_id: candidate.vault_id,
                relative_path: candidate.relative_path,
                title: candidate.title,
                excerpt: candidate.excerpt,
                modified_at: candidate.modified_at,
                score,
                tags: candidate.tags,
                wiki_links: candidate.wiki_links,
                source_kind: "note".to_string(),
                ranking_signals: IndexedSearchSignals {
                    lexical_rank,
                    vector_rank,
                    neural_rank,
                    local_vector_rank,
                    lexical_rrf,
                    vector_rrf,
                    neural_rrf,
                    local_vector_rrf,
                    vector_similarity,
                    neural_similarity,
                    local_vector_similarity,
                    title_path_bonus,
                    relation_bonus,
                    recency_bonus,
                    vector_kind: if neural_rank.is_some() {
                        "neural".to_string()
                    } else if local_vector_rank.is_some() {
                        "local".to_string()
                    } else {
                        "none".to_string()
                    },
                    embedding_provider: neural.map(|ctx| ctx.provider.clone()),
                    embedding_model: neural.map(|ctx| ctx.model.clone()),
                    embedding_index_state: neural.map(|context| {
                        if neural_cache_degraded {
                            "degraded".to_string()
                        } else {
                            context.index_state.clone()
                        }
                    }),
                },
            }
        })
        .collect::<Vec<_>>();

    // 7. 最终排序
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.ranking_signals
                    .neural_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&right.ranking_signals.neural_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                right
                    .ranking_signals
                    .neural_similarity
                    .unwrap_or(f64::NEG_INFINITY)
                    .partial_cmp(
                        &left
                            .ranking_signals
                            .neural_similarity
                            .unwrap_or(f64::NEG_INFINITY),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                let left_signals = &left.ranking_signals;
                let right_signals = &right.ranking_signals;
                (right_signals.title_path_bonus
                    + right_signals.relation_bonus
                    + right_signals.recency_bonus)
                    .partial_cmp(
                        &(left_signals.title_path_bonus
                            + left_signals.relation_bonus
                            + left_signals.recency_bonus),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    results.truncate(max_results);
    Ok(results)
}

/// 构建排名映射
fn build_rank_map(candidates: &[IndexedSearchCandidate]) -> HashMap<(String, String), usize> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect()
}

/// 构建相似度映射
fn build_similarity_map(
    candidates: &[IndexedSearchCandidate],
) -> HashMap<(String, String), f64> {
    candidates
        .iter()
        .filter_map(|candidate| {
            candidate.vector_similarity.map(|similarity| {
                (
                    (candidate.vault_id.clone(), candidate.relative_path.clone()),
                    similarity,
                )
            })
        })
        .collect()
}

/// 加载词法搜索候选
fn load_lexical_search_candidates(
    connection: &Connection,
    vault_id: Option<&str>,
    query: &str,
    limit: i64,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    // TODO: 实现完整的 FTS 查询
    // 当前返回空列表，实际实现需要查询 note_fts 表
    let _ = (connection, vault_id, query, limit);
    Ok(Vec::new())
}

/// 加载向量搜索候选
fn load_vector_search_candidates(
    connection: &Connection,
    vault_id: Option<&str>,
    query: &str,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    // TODO: 实现完整的本地向量搜索
    // 当前返回空列表，实际实现需要计算本地特征向量
    let _ = (connection, vault_id, query);
    Ok(Vec::new())
}

/// 加载神经搜索候选
fn load_neural_search_candidates(
    connection: &Connection,
    vault_id: Option<&str>,
    context: &NeuralSearchContext,
) -> Result<(Vec<IndexedSearchCandidate>, bool), String> {
    // TODO: 实现完整的神经嵌入搜索
    // 当前返回空列表，实际实现需要计算余弦相似度
    let _ = (connection, vault_id, context);
    Ok((Vec::new(), false))
}
