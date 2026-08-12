use serde::{Deserialize, Serialize};
use crate::runtime_db::RuntimeDatabase;
use rusqlite::params;
use tauri::State;
use chrono::{DateTime, Utc, Duration};

/// 使用指标
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetrics {
    /// 采集次数
    pub captures_count: usize,
    /// 创作次数
    pub creations_count: usize,
    /// 搜索次数
    pub searches_count: usize,
    /// 笔记查看次数
    pub note_views_count: usize,
    /// 平均笔记质量分
    pub avg_note_quality: f64,
    /// 笔记重访率
    pub revisit_rate: f64,
    /// 平均创作时间（毫秒）
    pub avg_creation_time_ms: f64,
    /// 用户主动优化次数
    pub user_initiated_optimizations: usize,
}

/// 趋势数据
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsTrends {
    /// 与上周对比
    pub vs_last_week: MetricsComparison,
    /// 与上月对比
    pub vs_last_month: MetricsComparison,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsComparison {
    /// 采集变化百分比
    pub captures_change_pct: f64,
    /// 质量变化百分比
    pub quality_change_pct: f64,
    /// 重访率变化百分比
    pub revisit_rate_change_pct: f64,
}

/// 日期范围
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateRange {
    /// 开始日期
    pub start_date: String,
    /// 结束日期
    pub end_date: String,
}

/// 使用效果报告
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsReport {
    /// 统计周期
    pub period: DateRange,
    /// 指标数据
    pub metrics: UsageMetrics,
    /// 趋势对比
    pub trends: MetricsTrends,
    /// 洞察建议
    pub insights: Vec<String>,
}

impl UsageMetrics {
    /// 计算笔记质量分
    #[allow(dead_code)]
    pub fn calculate_note_quality(
        char_count: usize,
        wiki_links_count: usize,
        tags_count: usize,
        has_images: bool,
    ) -> f64 {
        let length_score = (char_count as f64 / 500.0).min(1.0) * 0.3;
        let links_score = (wiki_links_count as f64 / 5.0).min(1.0) * 0.3;
        let tags_score = (tags_count as f64 / 3.0).min(1.0) * 0.2;
        let images_score = if has_images { 0.2 } else { 0.0 };

        (length_score + links_score + tags_score + images_score) * 100.0
    }
}

/// 记录用户活动事件
pub fn record_activity_event(
    database: &RuntimeDatabase,
    event_type: &str,
    vault_id: Option<&str>,
    note_path: Option<&str>,
    entity_id: Option<&str>,
) -> Result<(), String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let now = Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    connection
        .execute(
            "INSERT INTO user_activity_events
             (id, workspace_scope, event_type, vault_id, note_path, entity_id, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, scope, event_type, vault_id, note_path, entity_id, now],
        )
        .map_err(|error| format!("无法记录活动事件：{error}"))?;

    Ok(())
}

/// 记录笔记查看
#[tauri::command]
pub fn record_note_view(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
) -> Result<(), String> {
    record_activity_event(
        database.inner(),
        "note_view",
        Some(&vault_id),
        Some(&note_path),
        None,
    )
}

/// 获取使用效果报告
#[tauri::command]
pub async fn get_metrics_report(
    database: State<'_, RuntimeDatabase>,
    period: DateRange,
) -> Result<MetricsReport, String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let start = &period.start_date;
    let end = &period.end_date;

    // 1. 统计采集次数（从 model_usage_events 推断 capture 操作）
    let captures_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM model_usage_events
             WHERE workspace_scope=?1 AND operation='capture'
               AND created_at BETWEEN ?2 AND ?3 AND state='succeeded'",
            params![scope, start, end],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;

    // 2. 统计创作次数
    let creations_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM creation_writing_runs
             WHERE workspace_scope=?1 AND created_at BETWEEN ?2 AND ?3
               AND state='succeeded'",
            params![scope, start, end],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;

    // 3. 统计搜索次数
    let searches_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM user_activity_events
             WHERE workspace_scope=?1 AND event_type='search'
               AND occurred_at BETWEEN ?2 AND ?3",
            params![scope, start, end],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;

    // 4. 统计笔记查看次数
    let note_views_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM user_activity_events
             WHERE workspace_scope=?1 AND event_type='note_view'
               AND occurred_at BETWEEN ?2 AND ?3",
            params![scope, start, end],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;

    // 5. 计算平均笔记质量（简化版本，仅基于字数和链接）
    let avg_note_quality: f64 = connection
        .query_row(
            "SELECT AVG(
               (byte_length * 1.0 / 500.0 * 0.3 +
                json_array_length(wiki_links_json) * 1.0 / 5.0 * 0.3 +
                json_array_length(tags_json) * 1.0 / 3.0 * 0.2) * 100.0
             ) as avg_quality
             FROM note_index
             WHERE vault_id IN (
               SELECT id FROM vault_registry WHERE connection_state='connected'
             )",
            [],
            |row| row.get::<_, Option<f64>>(0),
        )
        .unwrap_or(Some(0.0))
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);

    // 6. 计算笔记重访率
    let (revisit_notes, total_notes) = connection
        .query_row(
            "SELECT
               SUM(CASE WHEN view_count >= 2 THEN 1 ELSE 0 END) as revisit_notes,
               COUNT(*) as total_notes
             FROM (
               SELECT vault_id, note_path, COUNT(*) as view_count
               FROM user_activity_events
               WHERE workspace_scope=?1 AND event_type='note_view'
                 AND occurred_at BETWEEN ?2 AND ?3
               GROUP BY vault_id, note_path
             )",
            params![scope, start, end],
            |row| {
                Ok((
                    row.get::<_, i64>(0).unwrap_or(0).max(0),
                    row.get::<_, i64>(1).unwrap_or(0).max(0),
                ))
            },
        )
        .unwrap_or((0, 0));

    let revisit_rate = if total_notes > 0 {
        (revisit_notes as f64 / total_notes as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // 7. 计算平均创作时间
    let avg_creation_time_ms: f64 = connection
        .query_row(
            "SELECT AVG(
               (julianday(completed_at) - julianday(created_at)) * 86400000
             ) as avg_time_ms
             FROM creation_writing_runs
             WHERE workspace_scope=?1 AND created_at BETWEEN ?2 AND ?3
               AND state='succeeded' AND completed_at IS NOT NULL",
            params![scope, start, end],
            |row| row.get::<_, Option<f64>>(0),
        )
        .unwrap_or(Some(0.0))
        .unwrap_or(0.0)
        .max(0.0);

    // 8. 统计用户主动优化次数（从 model_usage_events 推断优化操作）
    let user_initiated_optimizations: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM model_usage_events
             WHERE workspace_scope=?1 AND operation='optimization'
               AND created_at BETWEEN ?2 AND ?3 AND state='succeeded'",
            params![scope, start, end],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize;

    let metrics = UsageMetrics {
        captures_count,
        creations_count,
        searches_count,
        note_views_count,
        avg_note_quality,
        revisit_rate,
        avg_creation_time_ms,
        user_initiated_optimizations,
    };

    // 9. 计算趋势对比
    let trends = calculate_trends(&connection, &scope, &metrics, start)?;

    // 10. 生成智能洞察
    let insights = generate_insights(&metrics, &trends);

    Ok(MetricsReport {
        period,
        metrics,
        trends,
        insights,
    })
}

/// 计算趋势对比
fn calculate_trends(
    connection: &rusqlite::Connection,
    scope: &str,
    current: &UsageMetrics,
    current_start: &str,
) -> Result<MetricsTrends, String> {
    // 解析当前周期开始日期
    let current_start_dt = DateTime::parse_from_rfc3339(current_start)
        .map_err(|e| format!("日期解析失败：{e}"))?
        .with_timezone(&Utc);

    // 上周同期
    let last_week_start = (current_start_dt - Duration::days(7)).to_rfc3339();
    let last_week_end = current_start_dt.to_rfc3339();

    let last_week_captures: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM model_usage_events
             WHERE workspace_scope=?1 AND operation='capture'
               AND created_at BETWEEN ?2 AND ?3 AND state='succeeded'",
            params![scope, last_week_start, last_week_end],
            |row| row.get(0),
        )
        .unwrap_or(0)
        .max(0);

    let last_week_quality: f64 = connection
        .query_row(
            "SELECT AVG(
               (byte_length * 1.0 / 500.0 * 0.3 +
                json_array_length(wiki_links_json) * 1.0 / 5.0 * 0.3 +
                json_array_length(tags_json) * 1.0 / 3.0 * 0.2) * 100.0
             ) FROM note_index
             WHERE modified_at BETWEEN ?1 AND ?2",
            params![last_week_start, last_week_end],
            |row| row.get::<_, Option<f64>>(0),
        )
        .unwrap_or(Some(0.0))
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);

    let vs_last_week = MetricsComparison {
        captures_change_pct: calculate_change_pct(
            current.captures_count as f64,
            last_week_captures as f64,
        ),
        quality_change_pct: calculate_change_pct(current.avg_note_quality, last_week_quality),
        revisit_rate_change_pct: 0.0, // 简化：暂不计算
    };

    // 上月同期（简化：暂时使用默认值）
    let vs_last_month = MetricsComparison {
        captures_change_pct: 0.0,
        quality_change_pct: 0.0,
        revisit_rate_change_pct: 0.0,
    };

    Ok(MetricsTrends {
        vs_last_week,
        vs_last_month,
    })
}

/// 计算变化百分比
fn calculate_change_pct(current: f64, previous: f64) -> f64 {
    if previous == 0.0 {
        if current > 0.0 {
            100.0
        } else {
            0.0
        }
    } else {
        ((current - previous) / previous) * 100.0
    }
}

/// 生成智能洞察
fn generate_insights(metrics: &UsageMetrics, trends: &MetricsTrends) -> Vec<String> {
    let mut insights = Vec::new();

    // 1. 质量趋势
    if trends.vs_last_week.quality_change_pct > 5.0 {
        insights.push(format!(
            "笔记质量提升 {:.1}%，知识库正在变得更完善 ✨",
            trends.vs_last_week.quality_change_pct
        ));
    } else if trends.vs_last_week.quality_change_pct < -5.0 {
        insights.push(format!(
            "笔记质量下降 {:.1}%，建议增加双链和标签",
            trends.vs_last_week.quality_change_pct.abs()
        ));
    }

    // 2. 重访率
    if metrics.revisit_rate < 0.2 {
        insights.push("重访率较低（< 20%），建议定期回顾旧笔记".to_string());
    } else if metrics.revisit_rate > 0.5 {
        insights.push("重访率优秀（> 50%），知识正在被有效复用 🎯".to_string());
    }

    // 3. 创作效率
    if metrics.avg_creation_time_ms > 0.0 && metrics.avg_creation_time_ms < 60_000.0 {
        insights.push("创作效率很高，平均 1 分钟内完成 ⚡".to_string());
    }

    // 4. 活跃度
    if metrics.captures_count + metrics.creations_count + metrics.searches_count < 5 {
        insights.push("使用频率较低，建议每天至少进行一次知识采集或创作".to_string());
    }

    // 5. 采集趋势
    if trends.vs_last_week.captures_change_pct > 20.0 {
        insights.push(format!(
            "采集频率大幅提升 {:.1}%，保持这个节奏！🚀",
            trends.vs_last_week.captures_change_pct
        ));
    }

    insights
}

#[allow(dead_code)]
#[tauri::command]
pub async fn get_metrics_report_legacy(period: DateRange) -> Result<MetricsReport, String> {
    // 旧版兼容接口（无 database 参数）
    let metrics = UsageMetrics {
        captures_count: 0,
        creations_count: 0,
        searches_count: 0,
        note_views_count: 0,
        avg_note_quality: 0.0,
        revisit_rate: 0.0,
        avg_creation_time_ms: 0.0,
        user_initiated_optimizations: 0,
    };

    let trends = MetricsTrends {
        vs_last_week: MetricsComparison {
            captures_change_pct: 0.0,
            quality_change_pct: 0.0,
            revisit_rate_change_pct: 0.0,
        },
        vs_last_month: MetricsComparison {
            captures_change_pct: 0.0,
            quality_change_pct: 0.0,
            revisit_rate_change_pct: 0.0,
        },
    };

    Ok(MetricsReport {
        period,
        metrics,
        trends,
        insights: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_note_quality_perfect() {
        let score = UsageMetrics::calculate_note_quality(1000, 10, 5, true);
        assert!(score >= 90.0);
    }

    #[test]
    fn test_note_quality_minimal() {
        let score = UsageMetrics::calculate_note_quality(50, 0, 0, false);
        assert!(score < 20.0);
    }

    #[test]
    fn test_note_quality_balanced() {
        let score = UsageMetrics::calculate_note_quality(500, 5, 3, true);
        assert!(score >= 80.0 && score <= 100.0);
    }

    #[test]
    fn test_calculate_change_pct_increase() {
        let pct = calculate_change_pct(120.0, 100.0);
        assert!((pct - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_calculate_change_pct_decrease() {
        let pct = calculate_change_pct(80.0, 100.0);
        assert!((pct - (-20.0)).abs() < 0.01);
    }

    #[test]
    fn test_calculate_change_pct_zero_base() {
        let pct = calculate_change_pct(100.0, 0.0);
        assert_eq!(pct, 100.0);
    }

    #[test]
    fn test_generate_insights_low_revisit() {
        let metrics = UsageMetrics {
            captures_count: 10,
            creations_count: 5,
            searches_count: 3,
            note_views_count: 20,
            avg_note_quality: 70.0,
            revisit_rate: 0.1,
            avg_creation_time_ms: 45000.0,
            user_initiated_optimizations: 2,
        };
        let trends = MetricsTrends {
            vs_last_week: MetricsComparison {
                captures_change_pct: 0.0,
                quality_change_pct: 0.0,
                revisit_rate_change_pct: 0.0,
            },
            vs_last_month: MetricsComparison {
                captures_change_pct: 0.0,
                quality_change_pct: 0.0,
                revisit_rate_change_pct: 0.0,
            },
        };
        let insights = generate_insights(&metrics, &trends);
        assert!(!insights.is_empty());
        assert!(insights.iter().any(|s| s.contains("重访率较低")));
    }
}
