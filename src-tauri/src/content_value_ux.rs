use serde::Serialize;

/// 批量操作进度
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct BatchProgress {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub current_item: Option<String>,
}

/// 批量价值计算结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchValueCalculationResult {
    pub success_count: usize,
    pub failure_count: usize,
    pub results: Vec<ContentValueScore>,
    pub errors: Vec<BatchError>,
}

/// 批量操作错误
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchError {
    pub note_path: String,
    pub error_message: String,
}

/// 改进建议应用结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SuggestionApplicationResult {
    pub applied_count: usize,
    pub skipped_count: usize,
    pub details: Vec<SuggestionApplicationDetail>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SuggestionApplicationDetail {
    pub note_path: String,
    pub suggestion: String,
    pub applied: bool,
    pub reason: Option<String>,
}

use crate::content_value::ContentValueScore;
use crate::runtime_db::RuntimeDatabase;
use rusqlite::params;
use tauri::State;

/// 批量计算笔记价值（带进度）
#[tauri::command]
pub async fn calculate_notes_value_batch(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_paths: Vec<String>,
) -> Result<BatchValueCalculationResult, String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut success_count = 0;
    let mut failure_count = 0;

    for note_path in note_paths {
        match ContentValueScore::calculate(&connection, &scope, &vault_id, &note_path) {
            Ok(score) => {
                results.push(score);
                success_count += 1;
            }
            Err(e) => {
                errors.push(BatchError {
                    note_path: note_path.clone(),
                    error_message: format!("计算失败: {}", e),
                });
                failure_count += 1;
            }
        }
    }

    Ok(BatchValueCalculationResult {
        success_count,
        failure_count,
        results,
        errors,
    })
}

/// 获取可操作的改进建议列表
#[tauri::command]
pub fn get_actionable_suggestions(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    limit: Option<usize>,
) -> Result<Vec<ActionableSuggestion>, String> {
    let scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut stmt = connection
        .prepare("SELECT relative_path FROM note_index WHERE vault_id=?1 LIMIT ?2")
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let max_limit = limit.unwrap_or(50).min(100);
    let note_paths: Vec<String> = stmt
        .query_map(params![vault_id, max_limit], |row| row.get(0))
        .map_err(|e| format!("查询笔记失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut actionable = Vec::new();

    for path in note_paths {
        if let Ok(score) = ContentValueScore::calculate(&connection, &scope, &vault_id, &path) {
            // 只返回有建议的笔记
            if !score.suggestions.is_empty() {
                for suggestion in &score.suggestions {
                    actionable.push(ActionableSuggestion {
                        note_path: path.clone(),
                        note_title: score.title.clone(),
                        current_tier: score.value_tier.clone(),
                        current_score: score.total_score,
                        suggestion: suggestion.clone(),
                        action_type: infer_action_type(suggestion),
                        estimated_improvement: estimate_score_improvement(suggestion),
                    });
                }
            }
        }
    }

    // 按预期改进排序
    actionable.sort_by(|a, b| {
        b.estimated_improvement
            .partial_cmp(&a.estimated_improvement)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(actionable)
}

/// 可操作的改进建议
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionableSuggestion {
    pub note_path: String,
    pub note_title: String,
    pub current_tier: crate::content_value::ValueTier,
    pub current_score: f64,
    pub suggestion: String,
    pub action_type: ActionType,
    pub estimated_improvement: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    AddTags,
    AddLinks,
    ExpandContent,
    Archive,
    Review,
}

fn infer_action_type(suggestion: &str) -> ActionType {
    let lower = suggestion.to_lowercase();
    if lower.contains("标签") {
        ActionType::AddTags
    } else if lower.contains("链接") || lower.contains("关联") {
        ActionType::AddLinks
    } else if lower.contains("扩充") || lower.contains("增加") {
        ActionType::ExpandContent
    } else if lower.contains("归档") {
        ActionType::Archive
    } else {
        ActionType::Review
    }
}

fn estimate_score_improvement(suggestion: &str) -> f64 {
    let lower = suggestion.to_lowercase();
    if lower.contains("扩充内容") {
        15.0 // 质量维度 30%，字数权重高
    } else if lower.contains("添加链接") {
        12.0 // 连接度维度 25%
    } else if lower.contains("添加标签") {
        8.0 // 质量和连接度都有影响
    } else if lower.contains("归档") {
        5.0 // 清理低价值内容
    } else {
        3.0 // 一般性建议
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_type_inference() {
        assert!(matches!(infer_action_type("添加标签"), ActionType::AddTags));
        assert!(matches!(infer_action_type("建立链接"), ActionType::AddLinks));
        assert!(matches!(infer_action_type("扩充内容"), ActionType::ExpandContent));
        assert!(matches!(infer_action_type("归档笔记"), ActionType::Archive));
        assert!(matches!(infer_action_type("未知操作"), ActionType::Review));
    }

    #[test]
    fn test_score_improvement_estimation() {
        assert_eq!(estimate_score_improvement("扩充内容"), 15.0);
        assert_eq!(estimate_score_improvement("添加链接"), 12.0);
        assert_eq!(estimate_score_improvement("添加标签"), 8.0);
        assert_eq!(estimate_score_improvement("未知操作"), 3.0);
    }
}
