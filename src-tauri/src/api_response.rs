use serde::{Deserialize, Serialize};
use std::fmt;

/// 统一的 API 响应结构
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse<T> {
    /// 是否成功
    pub success: bool,
    /// 响应数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    /// 错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// 请求追踪 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// 响应时间戳
    pub timestamp: String,
}

impl<T> ApiResponse<T> {
    /// 创建成功响应
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            trace_id: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建成功响应（带追踪 ID）
    pub fn success_with_trace(data: T, trace_id: String) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            trace_id: Some(trace_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建错误响应
    pub fn error(error: ApiError) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            trace_id: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建错误响应（带追踪 ID）
    pub fn error_with_trace(error: ApiError, trace_id: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error),
            trace_id: Some(trace_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// API 错误结构
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// 错误码
    pub code: ErrorCode,
    /// 错误消息
    pub message: String,
    /// 详细信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// 是否可重试
    pub retryable: bool,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: String) -> Self {
        let retryable = code.is_retryable();
        Self {
            code,
            message,
            details: None,
            retryable,
        }
    }

    pub fn with_details(mut self, details: String) -> Self {
        self.details = Some(details);
        self
    }

    /// 从字符串错误创建
    pub fn from_string(error: String) -> Self {
        Self::new(ErrorCode::InternalError, error)
    }

    /// 数据库错误
    pub fn database(message: String) -> Self {
        Self::new(ErrorCode::DatabaseError, message)
    }

    /// 验证错误
    pub fn validation(message: String) -> Self {
        Self::new(ErrorCode::ValidationError, message)
    }

    /// 资源未找到
    pub fn not_found(resource: String) -> Self {
        Self::new(ErrorCode::NotFound, format!("资源未找到: {}", resource))
    }

    /// 权限错误
    pub fn permission_denied(message: String) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }
}

/// 错误码枚举
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // 通用错误 (1000-1999)
    InternalError,
    InvalidRequest,
    ValidationError,
    NotFound,
    Conflict,

    // 数据库错误 (2000-2999)
    DatabaseError,
    DatabaseConnectionError,
    DatabaseQueryError,
    DatabaseTransactionError,

    // 权限错误 (3000-3999)
    PermissionDenied,
    AuthenticationRequired,
    AuthorizationFailed,

    // 资源错误 (4000-4999)
    ResourceNotFound,
    ResourceAlreadyExists,
    ResourceLocked,
    ResourceCorrupted,

    // 业务逻辑错误 (5000-5999)
    BusinessRuleViolation,
    InvalidState,
    OperationNotAllowed,

    // 外部服务错误 (6000-6999)
    ExternalServiceError,
    NetworkError,
    TimeoutError,

    // 限流和配额 (7000-7999)
    RateLimitExceeded,
    QuotaExceeded,
    TooManyRequests,
}

impl ErrorCode {
    /// 判断错误是否可重试
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ErrorCode::DatabaseConnectionError
                | ErrorCode::NetworkError
                | ErrorCode::TimeoutError
                | ErrorCode::ExternalServiceError
        )
    }

    /// 获取 HTTP 状态码（用于 Web API）
    pub fn http_status_code(&self) -> u16 {
        match self {
            ErrorCode::InvalidRequest | ErrorCode::ValidationError => 400,
            ErrorCode::AuthenticationRequired => 401,
            ErrorCode::PermissionDenied | ErrorCode::AuthorizationFailed => 403,
            ErrorCode::NotFound | ErrorCode::ResourceNotFound => 404,
            ErrorCode::Conflict | ErrorCode::ResourceAlreadyExists => 409,
            ErrorCode::ResourceLocked => 423,
            ErrorCode::RateLimitExceeded | ErrorCode::TooManyRequests => 429,
            ErrorCode::InternalError
            | ErrorCode::DatabaseError
            | ErrorCode::DatabaseConnectionError
            | ErrorCode::DatabaseQueryError
            | ErrorCode::DatabaseTransactionError
            | ErrorCode::ResourceCorrupted
            | ErrorCode::BusinessRuleViolation
            | ErrorCode::InvalidState
            | ErrorCode::OperationNotAllowed => 500,
            ErrorCode::ExternalServiceError => 502,
            ErrorCode::NetworkError | ErrorCode::TimeoutError => 504,
            ErrorCode::QuotaExceeded => 507,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Result 类型别名
pub type ApiResult<T> = Result<ApiResponse<T>, ApiError>;

/// 将 Result<T, String> 转换为 ApiResponse<T>
impl<T> From<Result<T, String>> for ApiResponse<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(data) => ApiResponse::success(data),
            Err(error) => ApiResponse::error(ApiError::from_string(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_success_response() {
        let response = ApiResponse::success("test data".to_string());
        assert!(response.success);
        assert_eq!(response.data, Some("test data".to_string()));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_error_response() {
        let error = ApiError::database("Connection failed".to_string());
        let response: ApiResponse<String> = ApiResponse::error(error.clone());
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error.unwrap().code, ErrorCode::DatabaseError);
    }

    #[test]
    fn test_error_code_retryable() {
        assert!(ErrorCode::NetworkError.is_retryable());
        assert!(ErrorCode::TimeoutError.is_retryable());
        assert!(!ErrorCode::ValidationError.is_retryable());
        assert!(!ErrorCode::PermissionDenied.is_retryable());
    }

    #[test]
    fn test_http_status_codes() {
        assert_eq!(ErrorCode::NotFound.http_status_code(), 404);
        assert_eq!(ErrorCode::PermissionDenied.http_status_code(), 403);
        assert_eq!(ErrorCode::InternalError.http_status_code(), 500);
        assert_eq!(ErrorCode::ValidationError.http_status_code(), 400);
    }

    #[test]
    fn test_from_result() {
        let ok_result: Result<i32, String> = Ok(42);
        let response: ApiResponse<i32> = ok_result.into();
        assert!(response.success);
        assert_eq!(response.data, Some(42));

        let err_result: Result<i32, String> = Err("error".to_string());
        let response: ApiResponse<i32> = err_result.into();
        assert!(!response.success);
        assert!(response.data.is_none());
    }
}
