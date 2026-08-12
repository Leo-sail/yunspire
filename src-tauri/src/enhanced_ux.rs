use crate::metrics::UsageMetrics;
use serde::Serialize;

/// 错误详情（增强版）
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancedError {
    pub error_type: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub recoverable: bool,
}

impl EnhancedError {
    pub fn from_string(error: String) -> Self {
        let (error_type, message, suggestion, recoverable) = Self::parse_error(&error);
        Self {
            error_type,
            message,
            suggestion,
            recoverable,
        }
    }

    fn parse_error(error: &str) -> (String, String, Option<String>, bool) {
        if error.contains("SQLite") || error.contains("数据库") {
            (
                "database_error".to_string(),
                error.to_string(),
                Some("请检查数据库连接状态，如果问题持续请重启应用".to_string()),
                true,
            )
        } else if error.contains("不可用") || error.contains("锁") {
            (
                "lock_error".to_string(),
                error.to_string(),
                Some("资源正在被占用，请稍后重试".to_string()),
                true,
            )
        } else if error.contains("无法读取") || error.contains("未找到") {
            (
                "not_found".to_string(),
                error.to_string(),
                Some("请确认资源路径正确，或尝试刷新列表".to_string()),
                true,
            )
        } else if error.contains("权限") || error.contains("不允许") {
            (
                "permission_denied".to_string(),
                error.to_string(),
                Some("请检查文件或数据库的访问权限".to_string()),
                false,
            )
        } else {
            (
                "unknown_error".to_string(),
                error.to_string(),
                Some("发生未知错误，请查看日志获取更多信息".to_string()),
                true,
            )
        }
    }
}

/// 带进度的操作结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ProgressResult<T> {
    pub total: usize,
    pub completed: usize,
    pub success: usize,
    pub failed: usize,
    pub data: T,
    pub errors: Vec<EnhancedError>,
}

#[allow(dead_code)]
impl<T> ProgressResult<T> {
    pub fn new(total: usize, data: T) -> Self {
        Self {
            total,
            completed: 0,
            success: 0,
            failed: 0,
            data,
            errors: vec![],
        }
    }

    pub fn add_success(&mut self) {
        self.completed += 1;
        self.success += 1;
    }

    pub fn add_failure(&mut self, error: EnhancedError) {
        self.completed += 1;
        self.failed += 1;
        self.errors.push(error);
    }
}

use crate::content_value::ContentValueScore;
use crate::knowledge_health::KnowledgeHealthDashboard;
use crate::metrics::MetricsReport;
use crate::runtime_db::RuntimeDatabase;
use tauri::State;

/// 获取综合智能洞察报告
#[tauri::command]
pub async fn get_comprehensive_insights(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<ComprehensiveInsights, String> {
    // 1. 获取健康度报告
    let health = crate::knowledge_health::get_knowledge_health_dashboard(
        vault_id.clone(),
        database.clone(),
    )
    .await?;

    // 2. 获取价值报告
    let value = crate::content_value::get_value_report(database.clone(), vault_id.clone())?;

    // 3. 获取使用指标
    let metrics = crate::metrics::get_metrics_report(
        database.clone(),
        crate::metrics::DateRange {
            start_date: (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339(),
            end_date: chrono::Utc::now().to_rfc3339(),
        },
    )
    .await?;

    // 4. 生成综合洞察
    let insights = generate_comprehensive_insights(&health, &value, &metrics);

    Ok(ComprehensiveInsights {
        health: health.clone(),
        value_summary: ValueSummary {
            average_score: value.average_score,
            tier_distribution: value.tier_distribution.clone(),
            top_notes_count: value.top_notes.len(),
            low_value_notes_count: value.low_value_notes.len(),
        },
        metrics_summary: MetricsSummary {
            total_activity: metrics.metrics.captures_count
                + metrics.metrics.creations_count
                + metrics.metrics.searches_count
                + metrics.metrics.note_views_count,
            avg_note_quality: metrics.metrics.avg_note_quality,
            revisit_rate: metrics.metrics.revisit_rate,
        },
        insights,
        recommendations: generate_priority_recommendations(&health, &value, &metrics),
    })
}

/// 综合智能洞察
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComprehensiveInsights {
    pub health: KnowledgeHealthDashboard,
    pub value_summary: ValueSummary,
    pub metrics_summary: MetricsSummary,
    pub insights: Vec<String>,
    pub recommendations: Vec<PriorityRecommendation>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueSummary {
    pub average_score: f64,
    pub tier_distribution: crate::content_value::TierDistribution,
    pub top_notes_count: usize,
    pub low_value_notes_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSummary {
    pub total_activity: usize,
    pub avg_note_quality: f64,
    pub revisit_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorityRecommendation {
    pub priority: Priority,
    pub title: String,
    pub description: String,
    pub impact: String,
    pub action_items: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    Medium,
    Low,
}

fn generate_comprehensive_insights(
    health: &KnowledgeHealthDashboard,
    value: &crate::content_value::ValueReport,
    metrics: &MetricsReport,
) -> Vec<String> {
    let mut insights = Vec::new();

    // 健康度与价值的关联分析
    if health.health_score < 60.0 && value.average_score < 60.0 {
        insights.push(
            "知识库健康度和内容价值都偏低，建议优先改善笔记质量和连接性 🔴".to_string(),
        );
    } else if health.health_score >= 80.0 && value.average_score >= 75.0 {
        insights.push("知识库健康度和内容价值都很优秀，继续保持！✨".to_string());
    }

    // 活跃度分析
    let total_activity = metrics.metrics.captures_count
        + metrics.metrics.creations_count
        + metrics.metrics.searches_count
        + metrics.metrics.note_views_count;

    if total_activity < 10 {
        insights.push("使用频率较低，建议增加每日知识采集和复习的习惯 📚".to_string());
    } else if total_activity > 50 {
        insights.push("使用非常活跃，知识积累效率很高 🚀".to_string());
    }

    // 质量趋势分析
    if metrics.metrics.avg_note_quality > 70.0 {
        insights.push("笔记整体质量良好，知识管理系统运作健康 ✅".to_string());
    } else if metrics.metrics.avg_note_quality < 50.0 {
        insights.push("笔记质量有待提升，建议增加双链、标签和内容扩充 ⚠️".to_string());
    }

    // 重访率分析
    if metrics.metrics.revisit_rate > 0.4 {
        insights.push("笔记重访率高，知识正在被有效复用 🎯".to_string());
    } else if metrics.metrics.revisit_rate < 0.15 {
        insights.push("笔记重访率低，建议建立定期复习机制 📅".to_string());
    }

    // 价值分布分析
    let total_notes = value.tier_distribution.s_count
        + value.tier_distribution.a_count
        + value.tier_distribution.b_count
        + value.tier_distribution.c_count
        + value.tier_distribution.d_count;

    if total_notes > 0 {
        let high_value_ratio = (value.tier_distribution.s_count + value.tier_distribution.a_count)
            as f64
            / total_notes as f64;

        if high_value_ratio > 0.3 {
            insights.push(format!(
                "高价值笔记占比 {:.1}%，核心知识资产充足 💎",
                high_value_ratio * 100.0
            ));
        } else if high_value_ratio < 0.1 {
            insights.push(
                "高价值笔记较少，建议聚焦提升核心笔记质量 ⭐".to_string(),
            );
        }
    }

    insights
}

fn generate_priority_recommendations(
    health: &KnowledgeHealthDashboard,
    value: &crate::content_value::ValueReport,
    metrics: &MetricsReport,
) -> Vec<PriorityRecommendation> {
    let mut recommendations = Vec::new();

    // 高优先级：健康度低于 60
    if health.health_score < 60.0 {
        recommendations.push(PriorityRecommendation {
            priority: Priority::High,
            title: "改善知识库健康度".to_string(),
            description: format!("当前健康度 {:.1}，存在结构性问题", health.health_score),
            impact: "提升知识网络连接性，提高信息检索效率".to_string(),
            action_items: health
                .suggestions
                .iter()
                .take(3)
                .map(|s| s.title.clone())
                .collect(),
        });
    }

    // 高优先级：平均价值低于 60
    if value.average_score < 60.0 {
        recommendations.push(PriorityRecommendation {
            priority: Priority::High,
            title: "提升内容价值".to_string(),
            description: format!("当前平均分 {:.1}，内容质量需要改进", value.average_score),
            impact: "提高笔记实用性和可复用性".to_string(),
            action_items: vec![
                "扩充短笔记内容".to_string(),
                "添加更多双链".to_string(),
                "补充标签分类".to_string(),
            ],
        });
    }

    // 中优先级：重访率低
    if metrics.metrics.revisit_rate < 0.2 {
        recommendations.push(PriorityRecommendation {
            priority: Priority::Medium,
            title: "建立复习机制".to_string(),
            description: format!(
                "重访率 {:.1}%，知识复用不足",
                metrics.metrics.revisit_rate * 100.0
            ),
            impact: "巩固知识记忆，提高学习效果".to_string(),
            action_items: vec![
                "设置每日复习任务".to_string(),
                "标记重点笔记定期回顾".to_string(),
                "利用搜索功能重新发现旧知识".to_string(),
            ],
        });
    }

    // 低优先级：清理低价值内容
    if value.low_value_notes.len() > 10 {
        recommendations.push(PriorityRecommendation {
            priority: Priority::Low,
            title: "清理低价值笔记".to_string(),
            description: format!("发现 {} 个低价值笔记", value.low_value_notes.len()),
            impact: "减少信息噪音，聚焦核心内容".to_string(),
            action_items: vec![
                "审查 D 级笔记，删除无用内容".to_string(),
                "合并重复或相似笔记".to_string(),
                "归档过时信息".to_string(),
            ],
        });
    }

    // 按优先级排序
    recommendations.sort_by(|a, b| a.priority.cmp(&b.priority));

    recommendations
}
