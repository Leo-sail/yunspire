use serde::{Deserialize, Serialize};

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

/// 获取使用效果报告
#[tauri::command]
pub async fn get_metrics_report(period: DateRange) -> Result<MetricsReport, String> {
    // TODO: 从数据库查询统计数据
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
}
