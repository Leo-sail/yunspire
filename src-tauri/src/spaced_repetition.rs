use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;

/// 间隔重复记录
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacedRepetitionRecord {
    pub note_path: String,
    pub vault_id: String,
    pub title: String,
    /// 当前复习次数
    pub review_count: usize,
    /// 上次复习时间
    pub last_reviewed_at: String,
    /// 下次复习时间
    pub next_review_at: String,
    /// 间隔天数
    pub interval_days: usize,
    /// 记忆强度 (0-1)
    pub memory_strength: f64,
}

/// 待复习笔记
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DueForReview {
    pub note_path: String,
    pub vault_id: String,
    pub title: String,
    pub days_overdue: i64,
    pub priority: ReviewPriority,
    pub last_reviewed_at: String,
    pub review_count: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPriority {
    High,    // 逾期 > 3 天
    Medium,  // 逾期 1-3 天
    Low,     // 今天到期
}

/// 复习计划摘要
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPlanSummary {
    pub total_notes_in_system: usize,
    pub due_today: usize,
    pub due_this_week: usize,
    pub overdue: usize,
    pub average_interval: f64,
    pub completion_rate: f64,
}

impl SpacedRepetitionRecord {
    /// 艾宾浩斯遗忘曲线间隔（修正版）
    /// 1, 2, 4, 7, 15, 30, 60, 120... 天
    fn ebbinghaus_intervals() -> Vec<usize> {
        vec![1, 2, 4, 7, 15, 30, 60, 120, 240]
    }

    /// 计算下次复习时间
    pub fn calculate_next_review(review_count: usize, last_reviewed: &DateTime<Utc>) -> DateTime<Utc> {
        let intervals = Self::ebbinghaus_intervals();
        let interval_days = intervals
            .get(review_count)
            .copied()
            .unwrap_or_else(|| intervals.last().copied().unwrap_or(120));

        *last_reviewed + Duration::days(interval_days as i64)
    }

    /// 计算记忆强度（基于复习间隔和时间衰减）
    pub fn calculate_memory_strength(
        review_count: usize,
        last_reviewed: &DateTime<Utc>,
        now: &DateTime<Utc>,
    ) -> f64 {
        if review_count == 0 {
            return 0.0;
        }

        let intervals = Self::ebbinghaus_intervals();
        let expected_interval = intervals
            .get(review_count - 1)
            .copied()
            .unwrap_or(120) as f64;

        let days_since = (*now - *last_reviewed).num_days() as f64;

        // 记忆强度 = 1 - (已过天数 / 预期间隔) * 衰减系数
        let decay_rate = 0.8;
        let strength = 1.0 - (days_since / expected_interval).min(1.0) * decay_rate;

        strength.clamp(0.0, 1.0)
    }
}

use crate::runtime_db::RuntimeDatabase;
use tauri::State;

/// 记录笔记复习
#[tauri::command]
pub fn record_note_review(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    note_path: String,
) -> Result<SpacedRepetitionRecord, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let workspace_scope = database.local_workspace_scope()?;
    let now = Utc::now();

    // 获取笔记标题
    let title = connection
        .query_row(
            "SELECT title FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note_path],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("查询笔记失败：{e}"))?;

    // 查询现有记录
    let existing = connection
        .query_row(
            "SELECT review_count, last_reviewed_at FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2 AND note_path=?3",
            params![workspace_scope, vault_id, note_path],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as usize, row.get::<_, String>(1)?)),
        )
        .ok();

    let (review_count, last_reviewed_str) = if let Some((count, last)) = existing {
        (count + 1, last)
    } else {
        (1, now.to_rfc3339())
    };

    let last_reviewed = DateTime::parse_from_rfc3339(&last_reviewed_str)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);

    let next_review = SpacedRepetitionRecord::calculate_next_review(review_count, &now);
    let intervals = SpacedRepetitionRecord::ebbinghaus_intervals();
    let interval_days = intervals
        .get(review_count)
        .copied()
        .unwrap_or_else(|| intervals.last().copied().unwrap_or(120));

    let memory_strength =
        SpacedRepetitionRecord::calculate_memory_strength(review_count, &last_reviewed, &now);

    // 插入或更新记录
    connection
        .execute(
            "INSERT INTO spaced_repetition_records
             (workspace_scope, vault_id, note_path, review_count, last_reviewed_at, next_review_at, interval_days, memory_strength, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(workspace_scope, vault_id, note_path) DO UPDATE SET
               review_count=excluded.review_count,
               last_reviewed_at=excluded.last_reviewed_at,
               next_review_at=excluded.next_review_at,
               interval_days=excluded.interval_days,
               memory_strength=excluded.memory_strength,
               updated_at=excluded.updated_at",
            params![
                workspace_scope,
                vault_id,
                note_path,
                review_count as i64,
                now.to_rfc3339(),
                next_review.to_rfc3339(),
                interval_days as i64,
                memory_strength,
                now.to_rfc3339(),
            ],
        )
        .map_err(|e| format!("保存复习记录失败：{e}"))?;

    Ok(SpacedRepetitionRecord {
        note_path,
        vault_id,
        title,
        review_count,
        last_reviewed_at: now.to_rfc3339(),
        next_review_at: next_review.to_rfc3339(),
        interval_days,
        memory_strength,
    })
}

/// 获取待复习笔记列表
#[tauri::command]
pub fn get_due_for_review(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
    include_future: Option<bool>,
) -> Result<Vec<DueForReview>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let workspace_scope = database.local_workspace_scope()?;
    let now = Utc::now();

    let query = if include_future.unwrap_or(false) {
        // 包含未来 7 天内到期的
        "SELECT sr.note_path, sr.vault_id, n.title, sr.next_review_at, sr.last_reviewed_at, sr.review_count
         FROM spaced_repetition_records sr
         JOIN note_index n ON sr.vault_id = n.vault_id AND sr.note_path = n.relative_path
         WHERE sr.workspace_scope=?1 AND sr.vault_id=?2 AND sr.next_review_at <= ?3"
    } else {
        // 仅包含今天及之前到期的
        "SELECT sr.note_path, sr.vault_id, n.title, sr.next_review_at, sr.last_reviewed_at, sr.review_count
         FROM spaced_repetition_records sr
         JOIN note_index n ON sr.vault_id = n.vault_id AND sr.note_path = n.relative_path
         WHERE sr.workspace_scope=?1 AND sr.vault_id=?2 AND sr.next_review_at <= ?3"
    };

    let future_date = if include_future.unwrap_or(false) {
        now + Duration::days(7)
    } else {
        now
    };

    let mut stmt = connection
        .prepare(query)
        .map_err(|e| format!("准备查询失败：{e}"))?;

    let results: Vec<DueForReview> = stmt
        .query_map(params![workspace_scope, vault_id, future_date.to_rfc3339()], |row| {
            let note_path: String = row.get(0)?;
            let vault_id: String = row.get(1)?;
            let title: String = row.get(2)?;
            let next_review_str: String = row.get(3)?;
            let last_reviewed_str: String = row.get(4)?;
            let review_count: i64 = row.get(5)?;

            let next_review = DateTime::parse_from_rfc3339(&next_review_str)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(now);

            let days_overdue = (now - next_review).num_days();

            let priority = if days_overdue > 3 {
                ReviewPriority::High
            } else if days_overdue >= 1 {
                ReviewPriority::Medium
            } else {
                ReviewPriority::Low
            };

            Ok(DueForReview {
                note_path,
                vault_id,
                title,
                days_overdue,
                priority,
                last_reviewed_at: last_reviewed_str,
                review_count: review_count.max(0) as usize,
            })
        })
        .map_err(|e| format!("查询失败：{e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// 获取复习计划摘要
#[tauri::command]
pub fn get_review_plan_summary(
    database: State<'_, RuntimeDatabase>,
    vault_id: String,
) -> Result<ReviewPlanSummary, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let workspace_scope = database.local_workspace_scope()?;
    let now = Utc::now();
    let week_later = now + Duration::days(7);

    // 总笔记数
    let total_notes: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM spaced_repetition_records WHERE workspace_scope=?1 AND vault_id=?2",
            params![workspace_scope, vault_id],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // 今天到期
    let due_today: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2 AND next_review_at <= ?3",
            params![workspace_scope, vault_id, now.to_rfc3339()],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // 本周到期
    let due_this_week: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2 AND next_review_at BETWEEN ?3 AND ?4",
            params![workspace_scope, vault_id, now.to_rfc3339(), week_later.to_rfc3339()],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // 逾期
    let overdue: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2 AND next_review_at < ?3",
            params![workspace_scope, vault_id, (now - Duration::days(1)).to_rfc3339()],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // 平均间隔
    let avg_interval: f64 = connection
        .query_row(
            "SELECT AVG(interval_days) FROM spaced_repetition_records WHERE workspace_scope=?1 AND vault_id=?2",
            params![workspace_scope, vault_id],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    // 完成率（7 天内复习过的比例）
    let reviewed_last_week: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM spaced_repetition_records
             WHERE workspace_scope=?1 AND vault_id=?2 AND last_reviewed_at >= ?3",
            params![workspace_scope, vault_id, (now - Duration::days(7)).to_rfc3339()],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let completion_rate = if total_notes > 0 {
        reviewed_last_week as f64 / total_notes as f64
    } else {
        0.0
    };

    Ok(ReviewPlanSummary {
        total_notes_in_system: total_notes.max(0) as usize,
        due_today: due_today.max(0) as usize,
        due_this_week: due_this_week.max(0) as usize,
        overdue: overdue.max(0) as usize,
        average_interval: avg_interval,
        completion_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ebbinghaus_intervals() {
        let intervals = SpacedRepetitionRecord::ebbinghaus_intervals();
        assert_eq!(intervals[0], 1);
        assert_eq!(intervals[1], 2);
        assert_eq!(intervals[2], 4);
        assert_eq!(intervals[3], 7);
        assert_eq!(intervals[6], 60);
    }

    #[test]
    fn test_calculate_next_review() {
        let now = Utc::now();
        let next = SpacedRepetitionRecord::calculate_next_review(0, &now);
        assert_eq!((next - now).num_days(), 1);

        let next2 = SpacedRepetitionRecord::calculate_next_review(3, &now);
        assert_eq!((next2 - now).num_days(), 7);
    }

    #[test]
    fn test_memory_strength() {
        let now = Utc::now();
        let last_week = now - Duration::days(7);

        // 刚复习完，记忆强度应该很高
        let strength1 = SpacedRepetitionRecord::calculate_memory_strength(1, &now, &now);
        assert!(strength1 > 0.9);

        // 7 天前复习，间隔也是 7 天，记忆强度应该中等
        let strength2 = SpacedRepetitionRecord::calculate_memory_strength(4, &last_week, &now);
        assert!(strength2 > 0.1 && strength2 < 0.5);
    }
}
