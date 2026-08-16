use crate::runtime_db::RuntimeDatabase;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::State;

/// 内容价值评估结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentValueScore {
    pub note_path: String,
    pub vault_id: String,
    pub title: String,
    pub total_score: f64,
    pub value_tier: ValueTier,
    pub dimensions: ValueDimensions,
    pub suggestions: Vec<String>,
}

/// 价值等级
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueTier {
    S, // 90-100: 核心知识
    A, // 75-89: 重要内容
    B, // 60-74: 一般笔记
    C, // 40-59: 待优化
    D, // 0-39: 低价值
}

/// 价值维度得分
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueDimensions {
    pub quality: f64,
    pub connectivity: f64,
    pub activity: f64,
    pub uniqueness: f64,
    pub completeness: f64,
}

/// 价值排序选项
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueSortOptions {
    pub sort_by: ValueSortBy,
    pub ascending: bool,
    pub limit: usize,
    pub min_tier: Option<ValueTier>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueSortBy {
    TotalScore,
    Quality,
    Connectivity,
    Activity,
    Uniqueness,
    Completeness,
}

/// 价值统计报告
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueReport {
    pub total_notes: usize,
    pub tier_distribution: TierDistribution,
    pub average_score: f64,
    pub top_notes: Vec<ContentValueScore>,
    pub low_value_notes: Vec<ContentValueScore>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TierDistribution {
    pub s_count: usize,
    pub a_count: usize,
    pub b_count: usize,
    pub c_count: usize,
    pub d_count: usize,
}

impl ContentValueScore {
    /// 计算笔记的价值分数
    pub fn calculate(
        connection: &Connection,
        workspace_scope: &str,
        vault_id: &str,
        note_path: &str,
    ) -> Result<Self, String> {
        let note_info = connection
            .query_row(
                "SELECT title, byte_length, wiki_links_json, tags_json, modified_at
                 FROM note_index
                 WHERE vault_id=?1 AND relative_path=?2",
                params![vault_id, note_path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(|e| format!("无法读取笔记信息：{e}"))?;

        let (title, byte_length, wiki_links_json, tags_json, modified_at) = note_info;

        let outgoing_links: Vec<String> =
            serde_json::from_str(&wiki_links_json).unwrap_or_default();
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

        let incoming_links: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM note_index
                 WHERE vault_id=?1 AND wiki_links_json LIKE ?2",
                params![vault_id, format!("%{}%", note_path)],
                |row| row.get(0),
            )
            .unwrap_or(0)
            .max(0);

        let thirty_days_ago = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let view_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM user_activity_events
                 WHERE workspace_scope=?1 AND event_type='note_view'
                   AND vault_id=?2 AND note_path=?3
                   AND occurred_at >= ?4",
                params![workspace_scope, vault_id, note_path, thirty_days_ago],
                |row| row.get(0),
            )
            .unwrap_or(0)
            .max(0);

        let quality = Self::calculate_quality(byte_length, &tags, &outgoing_links);
        let connectivity =
            Self::calculate_connectivity(outgoing_links.len(), incoming_links as usize, tags.len());
        let activity = Self::calculate_activity(view_count as usize, &modified_at);
        let uniqueness = 100.0; // 简化：暂时给满分
        let completeness = Self::calculate_completeness(byte_length, &outgoing_links);

        let total_score =
            quality * 0.30 + connectivity * 0.25 + activity * 0.20 + uniqueness * 0.15 + completeness * 0.10;

        let value_tier = Self::determine_tier(total_score);

        let dimensions = ValueDimensions {
            quality,
            connectivity,
            activity,
            uniqueness,
            completeness,
        };

        let suggestions =
            Self::generate_suggestions(&dimensions, byte_length, &outgoing_links, &tags);

        Ok(ContentValueScore {
            note_path: note_path.to_string(),
            vault_id: vault_id.to_string(),
            title,
            total_score,
            value_tier,
            dimensions,
            suggestions,
        })
    }

    fn calculate_quality(byte_length: i64, tags: &[String], links: &[String]) -> f64 {
        let length_score = (byte_length as f64 / 500.0).min(1.0) * 40.0;
        let tags_score = (tags.len() as f64 / 3.0).min(1.0) * 30.0;
        let links_score = (links.len() as f64 / 5.0).min(1.0) * 30.0;
        (length_score + tags_score + links_score).clamp(0.0, 100.0)
    }

    fn calculate_connectivity(outgoing: usize, incoming: usize, tags: usize) -> f64 {
        let outgoing_score = (outgoing as f64 / 5.0).min(1.0) * 40.0;
        let incoming_score = (incoming as f64 / 3.0).min(1.0) * 40.0;
        let tags_score = (tags as f64 / 3.0).min(1.0) * 20.0;
        (outgoing_score + incoming_score + tags_score).clamp(0.0, 100.0)
    }

    fn calculate_activity(view_count: usize, modified_at: &str) -> f64 {
        let view_score = (view_count as f64 / 10.0).min(1.0) * 60.0;
        let time_score = Self::calculate_time_score(modified_at) * 40.0;
        (view_score + time_score).clamp(0.0, 100.0)
    }

    fn calculate_time_score(modified_at: &str) -> f64 {
        let now = Utc::now();
        if let Ok(modified) = chrono::DateTime::parse_from_rfc3339(modified_at) {
            let days_since = (now - modified.with_timezone(&Utc)).num_days();
            if days_since < 7 {
                1.0
            } else if days_since < 30 {
                0.8
            } else if days_since < 90 {
                0.5
            } else {
                0.2
            }
        } else {
            0.5
        }
    }

    fn calculate_completeness(byte_length: i64, links: &[String]) -> f64 {
        let mut score: f64 = 100.0;
        if byte_length < 50 {
            score -= 50.0;
        } else if byte_length < 200 {
            score -= 20.0;
        }
        if links.is_empty() {
            score -= 30.0;
        }
        score.clamp(0.0, 100.0)
    }

    fn determine_tier(score: f64) -> ValueTier {
        if score >= 90.0 {
            ValueTier::S
        } else if score >= 75.0 {
            ValueTier::A
        } else if score >= 60.0 {
            ValueTier::B
        } else if score >= 40.0 {
            ValueTier::C
        } else {
            ValueTier::D
        }
    }

    fn generate_suggestions(
        dimensions: &ValueDimensions,
        byte_length: i64,
        links: &[String],
        tags: &[String],
    ) -> Vec<String> {
        let mut suggestions = Vec::new();

        if dimensions.quality < 60.0 {
            if byte_length < 200 {
                suggestions.push("扩充内容，增加细节和示例（目标 500+ 字）".to_string());
            }
            if tags.is_empty() {
                suggestions.push("添加标签，便于分类和检索".to_string());
            }
            if links.is_empty() {
                suggestions.push("添加双链，建立知识关联".to_string());
            }
        }

        if dimensions.connectivity < 60.0 {
            suggestions.push("增加与其他笔记的链接，构建知识网络".to_string());
        }

        if dimensions.activity < 40.0 {
            suggestions.push("此笔记较少使用，考虑归档或更新".to_string());
        }

        if dimensions.completeness < 60.0 {
            suggestions.push("补充内容，确保笔记完整性".to_string());
        }

        suggestions
    }
}

/// 计算单个笔记的价值分数
#[tauri::command]
pub fn calculate_note_value(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
) -> Result<ContentValueScore, String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    ContentValueScore::calculate(&connection, &scope, &vault_id, &note_path)
}

/// 获取价值排序的笔记列表
#[tauri::command]
pub fn get_value_ranked_notes(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    options: ValueSortOptions,
) -> Result<Vec<ContentValueScore>, String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut stmt = connection
        .prepare("SELECT relative_path FROM note_index WHERE vault_id=?1")
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let note_paths: Vec<String> = stmt
        .query_map(params![vault_id], |row| row.get(0))
        .map_err(|e| format!("查询笔记失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut scores: Vec<ContentValueScore> = note_paths
        .iter()
        .filter_map(|path| ContentValueScore::calculate(&connection, &scope, &vault_id, path).ok())
        .collect();

    if let Some(min_tier) = &options.min_tier {
        let min_tier_value = min_tier.clone();
        scores.retain(|s| (s.value_tier.clone() as u8) <= (min_tier_value.clone() as u8));
    }

    scores.sort_by(|a, b| {
        let val_a = match options.sort_by {
            ValueSortBy::TotalScore => a.total_score,
            ValueSortBy::Quality => a.dimensions.quality,
            ValueSortBy::Connectivity => a.dimensions.connectivity,
            ValueSortBy::Activity => a.dimensions.activity,
            ValueSortBy::Uniqueness => a.dimensions.uniqueness,
            ValueSortBy::Completeness => a.dimensions.completeness,
        };
        let val_b = match options.sort_by {
            ValueSortBy::TotalScore => b.total_score,
            ValueSortBy::Quality => b.dimensions.quality,
            ValueSortBy::Connectivity => b.dimensions.connectivity,
            ValueSortBy::Activity => b.dimensions.activity,
            ValueSortBy::Uniqueness => b.dimensions.uniqueness,
            ValueSortBy::Completeness => b.dimensions.completeness,
        };

        if options.ascending {
            val_a.partial_cmp(&val_b).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            val_b.partial_cmp(&val_a).unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    scores.truncate(options.limit);
    Ok(scores)
}

/// 获取价值统计报告
#[tauri::command]
pub fn get_value_report(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<ValueReport, String> {
    let all_notes = get_value_ranked_notes(
        database,
        vault_id,
        ValueSortOptions {
            sort_by: ValueSortBy::TotalScore,
            ascending: false,
            limit: 10000,
            min_tier: None,
        },
    )?;

    let total_notes = all_notes.len();
    let mut tier_distribution = TierDistribution {
        s_count: 0,
        a_count: 0,
        b_count: 0,
        c_count: 0,
        d_count: 0,
    };

    for note in &all_notes {
        match note.value_tier {
            ValueTier::S => tier_distribution.s_count += 1,
            ValueTier::A => tier_distribution.a_count += 1,
            ValueTier::B => tier_distribution.b_count += 1,
            ValueTier::C => tier_distribution.c_count += 1,
            ValueTier::D => tier_distribution.d_count += 1,
        }
    }

    let average_score = if total_notes > 0 {
        all_notes.iter().map(|n| n.total_score).sum::<f64>() / total_notes as f64
    } else {
        0.0
    };

    let top_notes = all_notes.iter().take(10).cloned().collect();
    let low_value_notes = all_notes
        .iter()
        .filter(|n| matches!(n.value_tier, ValueTier::C | ValueTier::D))
        .take(10)
        .cloned()
        .collect();

    Ok(ValueReport {
        total_notes,
        tier_distribution,
        average_score,
        top_notes,
        low_value_notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_quality_high() {
        let quality = ContentValueScore::calculate_quality(
            600,
            &vec!["rust".to_string(), "tauri".to_string(), "sql".to_string()],
            &vec!["note1".to_string(); 5],
        );
        assert!(quality >= 90.0);
    }

    #[test]
    fn test_calculate_quality_low() {
        let quality = ContentValueScore::calculate_quality(30, &vec![], &vec![]);
        assert!(quality < 30.0);
    }

    #[test]
    fn test_calculate_connectivity() {
        let connectivity = ContentValueScore::calculate_connectivity(5, 3, 3);
        assert!(connectivity >= 90.0);
    }

    #[test]
    fn test_calculate_activity_high() {
        let activity =
            ContentValueScore::calculate_activity(10, &chrono::Utc::now().to_rfc3339());
        assert!(activity >= 90.0);
    }

    #[test]
    fn test_determine_tier() {
        assert_eq!(ContentValueScore::determine_tier(95.0), ValueTier::S);
        assert_eq!(ContentValueScore::determine_tier(80.0), ValueTier::A);
        assert_eq!(ContentValueScore::determine_tier(65.0), ValueTier::B);
        assert_eq!(ContentValueScore::determine_tier(50.0), ValueTier::C);
        assert_eq!(ContentValueScore::determine_tier(30.0), ValueTier::D);
    }

    #[test]
    fn test_generate_suggestions_low_quality() {
        let dimensions = ValueDimensions {
            quality: 40.0,
            connectivity: 80.0,
            activity: 70.0,
            uniqueness: 100.0,
            completeness: 50.0,
        };
        let suggestions =
            ContentValueScore::generate_suggestions(&dimensions, 100, &vec![], &vec![]);
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.contains("扩充内容")));
    }
}
