use serde::{Deserialize, Serialize};

/// 创作模式
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CreationMode {
    /// 快速模式:日常笔记，跳过候选审核和品牌评测
    #[default]
    Quick,
    /// 专业模式：对外发布，完整流程
    Professional,
}

/// 创作工作流配置
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationWorkflow {
    /// 当前模式
    pub mode: CreationMode,
    /// 工作流描述
    pub workflow_description: String,
    /// 跳过的步骤
    pub skip_steps: Vec<String>,
    /// 使用场景说明
    pub use_case: String,
}

impl CreationWorkflow {
    pub fn quick() -> Self {
        Self {
            mode: CreationMode::Quick,
            workflow_description: "输入 → AI 生成 → 直接保存".to_string(),
            skip_steps: vec!["候选审核".to_string(), "品牌评测".to_string()],
            use_case: "日常笔记、临时想法".to_string(),
        }
    }

    pub fn professional() -> Self {
        Self {
            mode: CreationMode::Professional,
            workflow_description:
                "输入 → AI 生成 → 候选审核 → 品牌评测 → 最终确认 → 保存".to_string(),
            skip_steps: vec![],
            use_case: "博客文章、正式文档".to_string(),
        }
    }

    pub fn from_mode(mode: CreationMode) -> Self {
        match mode {
            CreationMode::Quick => Self::quick(),
            CreationMode::Professional => Self::professional(),
        }
    }
}

/// 获取当前创作模式
#[tauri::command]
pub fn get_creation_mode() -> Result<CreationWorkflow, String> {
    // TODO: 从用户配置读取，目前返回默认模式
    Ok(CreationWorkflow::quick())
}

/// 设置创作模式
#[tauri::command]
pub fn set_creation_mode(mode: CreationMode) -> Result<CreationWorkflow, String> {
    // TODO: 保存到用户配置
    Ok(CreationWorkflow::from_mode(mode))
}

/// 判断是否应跳过某步骤
#[allow(dead_code)]
pub fn should_skip_step(mode: &CreationMode, step: &str) -> bool {
    match mode {
        CreationMode::Quick => matches!(step, "candidate_review" | "brand_compliance"),
        CreationMode::Professional => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quick_mode_skips_steps() {
        let mode = CreationMode::Quick;
        assert!(should_skip_step(&mode, "candidate_review"));
        assert!(should_skip_step(&mode, "brand_compliance"));
        assert!(!should_skip_step(&mode, "ai_generation"));
    }

    #[test]
    fn test_professional_mode_no_skip() {
        let mode = CreationMode::Professional;
        assert!(!should_skip_step(&mode, "candidate_review"));
        assert!(!should_skip_step(&mode, "brand_compliance"));
    }

    #[test]
    fn test_workflow_descriptions() {
        let quick = CreationWorkflow::quick();
        assert_eq!(quick.skip_steps.len(), 2);

        let pro = CreationWorkflow::professional();
        assert_eq!(pro.skip_steps.len(), 0);
    }
}
