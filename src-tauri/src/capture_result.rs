use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 采集状态
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// 完全成功：所有内容和增强功能都成功
    FullSuccess,
    /// 部分成功：核心内容已保存，部分增强功能失败
    PartialSuccess,
    /// 核心失败：核心内容无法保存
    CoreFailed,
}

/// 采集结果（包含部分成功信息）
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    /// 采集状态
    pub status: CaptureStatus,
    /// 核心内容是否已保存
    pub core_saved: bool,
    /// 增强功能结果
    pub enhancements: EnhancementResults,
    /// 警告信息
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<CaptureWarning>,
    /// 错误信息（仅在 CoreFailed 时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 增强功能结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementResults {
    /// 外链图片处理结果
    pub linked_images: ImageEnhancementResult,
    /// 模型分析结果
    pub model_analysis: ModelEnhancementResult,
    /// Agent 库保存结果
    pub agent_vault: AgentVaultResult,
}

/// 图片增强结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageEnhancementResult {
    /// 总图片数
    pub total: usize,
    /// 成功数
    pub succeeded: usize,
    /// 失败的图片
    pub failed: Vec<FailedImage>,
    /// 是否可重试
    pub retryable: bool,
}

impl ImageEnhancementResult {
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn new_empty() -> Self {
        Self {
            total: 0,
            succeeded: 0,
            failed: Vec::new(),
            retryable: false,
        }
    }
}

/// 失败的图片
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedImage {
    /// 图片 URL
    pub url: String,
    /// 失败原因
    pub reason: String,
    /// 是否对理解内容至关重要
    pub is_critical: bool,
}

/// 模型分析结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEnhancementResult {
    /// 是否成功
    pub succeeded: bool,
    /// 分析数据（成功时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// 失败原因（失败时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ModelEnhancementResult {
    pub fn success(data: Value) -> Self {
        Self {
            succeeded: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            succeeded: false,
            data: None,
            error: Some(error),
        }
    }
}

/// Agent 库保存结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentVaultResult {
    /// 是否成功
    pub succeeded: bool,
    /// 失败原因（失败时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AgentVaultResult {
    pub fn success() -> Self {
        Self {
            succeeded: true,
            error: None,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            succeeded: false,
            error: Some(error),
        }
    }
}

/// 采集警告
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureWarning {
    /// 警告类型
    pub warning_type: String,
    /// 警告消息
    pub message: String,
    /// 受影响的资源
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_resource: Option<String>,
}

impl CaptureWarning {
    pub fn new(warning_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            warning_type: warning_type.into(),
            message: message.into(),
            affected_resource: None,
        }
    }

    pub fn with_resource(
        warning_type: impl Into<String>,
        message: impl Into<String>,
        resource: impl Into<String>,
    ) -> Self {
        Self {
            warning_type: warning_type.into(),
            message: message.into(),
            affected_resource: Some(resource.into()),
        }
    }
}

/// 采集策略配置
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePolicy {
    /// 外链图片失败时是否阻断保存（默认 false）
    #[serde(default)]
    pub block_on_image_failure: bool,

    /// 模型分析失败时是否阻断保存（默认 false）
    #[serde(default)]
    pub block_on_analysis_failure: bool,

    /// 是否自动重试失败的增强功能（默认 true）
    #[serde(default = "default_true")]
    pub auto_retry_enhancements: bool,

    /// 重试次数（默认 2）
    #[serde(default = "default_retry_attempts")]
    pub max_retry_attempts: u32,
}

fn default_true() -> bool {
    true
}

fn default_retry_attempts() -> u32 {
    2
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            block_on_image_failure: false,
            block_on_analysis_failure: false,
            auto_retry_enhancements: true,
            max_retry_attempts: 2,
        }
    }
}

/// 重试选项
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryOptions {
    /// 是否重试图片下载
    #[serde(default)]
    pub retry_images: bool,

    /// 是否重试模型分析
    #[serde(default)]
    pub retry_analysis: bool,

    /// 是否重试 Agent 库保存
    #[serde(default)]
    pub retry_agent_vault: bool,
}
