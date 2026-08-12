use crate::api_response::{ApiError, ApiResponse};
use crate::content_value::ContentValueScore;
use crate::knowledge_graph::KnowledgeGraph;
use crate::runtime_db::RuntimeDatabase;
use tauri::State;

/// 知识库 API 服务层
pub struct KnowledgeApiService;

impl KnowledgeApiService {
    /// 获取知识图谱（带统一响应格式）
    pub async fn get_knowledge_graph_wrapped(
        vault_id: String,
        database: State<'_, RuntimeDatabase>,
    ) -> ApiResponse<KnowledgeGraph> {
        let connection = match database.connection.lock() {
            Ok(conn) => conn,
            Err(_) => {
                return ApiResponse::error(ApiError::database(
                    "数据库连接不可用".to_string(),
                ))
            }
        };

        match KnowledgeGraph::build_from_database(&connection, &vault_id) {
            Ok(graph) => ApiResponse::success(graph),
            Err(e) => ApiResponse::error(ApiError::database(e)),
        }
    }

    /// 计算笔记价值（带统一响应格式）
    pub async fn calculate_note_value_wrapped(
        vault_id: String,
        note_path: String,
        database: State<'_, RuntimeDatabase>,
    ) -> ApiResponse<ContentValueScore> {
        let scope = match database.local_workspace_scope() {
            Ok(s) => s,
            Err(e) => {
                return ApiResponse::error(ApiError::database(format!(
                    "获取工作区失败: {}",
                    e
                )))
            }
        };

        let connection = match database.connection.lock() {
            Ok(conn) => conn,
            Err(_) => {
                return ApiResponse::error(ApiError::database(
                    "数据库连接不可用".to_string(),
                ))
            }
        };

        match ContentValueScore::calculate(&connection, &scope, &vault_id, &note_path) {
            Ok(score) => ApiResponse::success(score),
            Err(e) => ApiResponse::error(ApiError::database(e)),
        }
    }
}

/// 批量操作 API 服务
pub struct BatchOperationApiService;

impl BatchOperationApiService {
    /// 批量计算笔记价值
    pub async fn batch_calculate_value_wrapped(
        vault_id: String,
        note_paths: Vec<String>,
        database: State<'_, RuntimeDatabase>,
    ) -> ApiResponse<Vec<ContentValueScore>> {
        if note_paths.is_empty() {
            return ApiResponse::error(ApiError::validation(
                "笔记路径列表不能为空".to_string(),
            ));
        }

        if note_paths.len() > 100 {
            return ApiResponse::error(ApiError::validation(
                "单次最多处理 100 个笔记".to_string(),
            ));
        }

        let scope = match database.local_workspace_scope() {
            Ok(s) => s,
            Err(e) => {
                return ApiResponse::error(ApiError::database(format!(
                    "获取工作区失败: {}",
                    e
                )))
            }
        };

        let connection = match database.connection.lock() {
            Ok(conn) => conn,
            Err(_) => {
                return ApiResponse::error(ApiError::database(
                    "数据库连接不可用".to_string(),
                ))
            }
        };

        let mut results = Vec::new();

        for note_path in note_paths {
            match ContentValueScore::calculate(&connection, &scope, &vault_id, &note_path) {
                Ok(score) => results.push(score),
                Err(_) => {
                    // 跳过失败的笔记，继续处理其他笔记
                    continue;
                }
            }
        }

        if results.is_empty() {
            return ApiResponse::error(ApiError::database(
                "所有笔记计算失败".to_string(),
            ));
        }

        ApiResponse::success(results)
    }
}

/// 健康检查 API
pub struct HealthCheckApiService;

impl HealthCheckApiService {
    /// 数据库健康检查
    pub async fn database_health_check_wrapped(
        database: State<'_, RuntimeDatabase>,
    ) -> ApiResponse<DatabaseHealthStatus> {
        let connection = match database.connection.lock() {
            Ok(conn) => conn,
            Err(_) => {
                return ApiResponse::error(ApiError::database(
                    "数据库连接不可用".to_string(),
                ))
            }
        };

        // 检查连接
        let connection_ok = connection.execute_batch("SELECT 1").is_ok();

        // 检查完整性
        let integrity_ok = connection
            .query_row("PRAGMA integrity_check", [], |row| {
                row.get::<_, String>(0)
            })
            .map(|r| r == "ok")
            .unwrap_or(false);

        // 获取版本
        let schema_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        let status = DatabaseHealthStatus {
            connection_ok,
            integrity_ok,
            schema_version,
            status: if connection_ok && integrity_ok {
                "healthy".to_string()
            } else {
                "unhealthy".to_string()
            },
        };

        ApiResponse::success(status)
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseHealthStatus {
    pub connection_ok: bool,
    pub integrity_ok: bool,
    pub schema_version: i64,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_health_status_serialization() {
        let status = DatabaseHealthStatus {
            connection_ok: true,
            integrity_ok: true,
            schema_version: 46,
            status: "healthy".to_string(),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("connectionOk"));
        assert!(json.contains("integrityOk"));
        assert!(json.contains("schemaVersion"));
    }
}
