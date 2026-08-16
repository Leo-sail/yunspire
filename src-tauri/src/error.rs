use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YunspireError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub technical_details: Option<String>,
    pub recoverable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ErrorContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    PermissionDenied,
    AuthorizationRequired,
    VaultAccessDenied,
    FileNotFound,
    NoteNotFound,
    VaultNotFound,
    ResourceLocked,
    ResourceExhausted,
    DataCorrupted,
    VersionConflict,
    DuplicateEntry,
    InvalidFormat,
    NetworkError,
    ModelProviderError,
    ConnectionTimeout,
    DatabaseError,
    InternalError,
    ConfigurationError,
    InvalidInput,
    OperationNotAllowed,
    TaskFailed,
    BudgetExceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

impl YunspireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            technical_details: None,
            recoverable: false,
            suggested_action: None,
            context: None,
        }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.technical_details = Some(details.into());
        self
    }

    pub fn recoverable(mut self) -> Self {
        self.recoverable = true;
        self
    }

    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.suggested_action = Some(action.into());
        self
    }

    pub fn with_context(mut self, context: ErrorContext) -> Self {
        self.context = Some(context);
        self
    }

    #[allow(dead_code)]
    pub fn permission_denied(resource: &str) -> Self {
        Self::new(
            ErrorCode::PermissionDenied,
            format!("没有权限访问 {}", resource),
        )
        .recoverable()
        .with_action("请在设置 > 权限中授权")
    }

    #[allow(dead_code)]
    pub fn file_not_found(path: &str) -> Self {
        Self::new(ErrorCode::FileNotFound, "找不到指定的文件")
            .recoverable()
            .with_details(format!("路径：{}", path))
            .with_action("请刷新文件列表后重试")
            .with_context(ErrorContext {
                file_path: Some(path.to_string()),
                ..Default::default()
            })
    }

    #[allow(dead_code)]
    pub fn version_conflict(resource: &str, expected: i64, actual: i64) -> Self {
        Self::new(ErrorCode::VersionConflict, format!("{} 版本冲突", resource))
            .recoverable()
            .with_details(format!("期望版本 {}，实际版本 {}", expected, actual))
            .with_action("请刷新后重试")
    }

    #[allow(dead_code)]
    pub fn database_error(details: impl Into<String>) -> Self {
        Self::new(ErrorCode::DatabaseError, "数据库操作失败")
            .with_details(details)
            .with_action("如果问题持续，请检查数据库完整性")
    }

    #[allow(dead_code)]
    pub fn model_provider_error(provider: &str, details: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::ModelProviderError,
            format!("模型 {} 请求失败", provider),
        )
        .recoverable()
        .with_details(details)
        .with_action("请检查网络连接和 API 密钥")
    }
}

impl fmt::Display for YunspireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)?;
        if let Some(details) = &self.technical_details {
            write!(f, " ({})", details)?;
        }
        Ok(())
    }
}

impl std::error::Error for YunspireError {}

impl From<rusqlite::Error> for YunspireError {
    fn from(error: rusqlite::Error) -> Self {
        YunspireError::database_error(error.to_string())
    }
}

impl From<std::io::Error> for YunspireError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;

        match error.kind() {
            ErrorKind::NotFound => YunspireError::new(ErrorCode::FileNotFound, "文件不存在")
                .recoverable()
                .with_details(error.to_string()),
            ErrorKind::PermissionDenied => {
                YunspireError::permission_denied("文件系统").with_details(error.to_string())
            }
            _ => YunspireError::new(ErrorCode::InternalError, "文件操作失败")
                .with_details(error.to_string()),
        }
    }
}

#[allow(dead_code)]
pub type CommandResult<T> = Result<T, YunspireError>;
