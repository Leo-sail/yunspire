use crate::execution_plan::{ExecutionPlan, OperationType, PlannedStep};
use regex::Regex;

/// 从 AI 响应中提取执行计划
pub fn extract_execution_plan(assistant_reply: &str, intent: &str) -> Option<ExecutionPlan> {
    // 1. 尝试查找结构化计划标记
    if let Some(plan) = extract_structured_plan(assistant_reply) {
        return Some(plan);
    }

    // 2. 尝试识别自然语言计划
    extract_natural_language_plan(assistant_reply, intent)
}

/// 提取结构化计划（AI 主动输出 ```plan-json 标记）
fn extract_structured_plan(content: &str) -> Option<ExecutionPlan> {
    // 查找 ```plan-json ... ``` 代码块
    let pattern = r"```plan-json\s*([\s\S]*?)\s*```";
    let re = Regex::new(pattern).ok()?;
    let captures = re.captures(content)?;
    let json_str = captures.get(1)?.as_str();

    serde_json::from_str::<ExecutionPlan>(json_str).ok()
}

/// 提取自然语言计划
fn extract_natural_language_plan(content: &str, intent: &str) -> Option<ExecutionPlan> {
    let steps = extract_steps_from_text(content)?;

    if steps.is_empty() {
        return None;
    }

    Some(ExecutionPlan {
        task_id: uuid::Uuid::new_v4().to_string(),
        intent: intent.to_string(),
        steps,
        explanation: extract_explanation(content),
        risks: extract_risks(content),
        user_choice_required: has_confirmation_keywords(content),
    })
}

/// 从文本中提取步骤
fn extract_steps_from_text(content: &str) -> Option<Vec<PlannedStep>> {
    let mut steps = Vec::new();

    // 匹配模式：
    // 1. "1. ..." / "一、..." / "①..."
    // 2. "- ..." / "• ..."
    // 3. "步骤 1:" / "Step 1:"

    let patterns = [
        r"(?m)^[\d一二三四五六七八九十]+[.、．)\s]\s*(.+)$",
        r"(?m)^[-•]\s+(.+)$",
        r"(?m)^步骤\s*[\d一二三四五六七八九十]+[:：]\s*(.+)$",
        r"(?mi)^step\s+\d+[:：]\s*(.+)$",
    ];

    for pattern in &patterns {
        if let Ok(re) = Regex::new(pattern) {
            for (index, captures) in re.captures_iter(content).enumerate() {
                if let Some(desc) = captures.get(1) {
                    let description = desc.as_str().trim().to_string();
                    // 过滤太短或太长的描述
                    if description.len() > 5 && description.len() < 200 {
                        steps.push(PlannedStep {
                            step_number: index + 1,
                            description,
                            operation_type: infer_operation_type(desc.as_str()),
                            expected_outcome: String::new(),
                            reversible: !is_destructive(desc.as_str()),
                        });
                    }
                }
            }

            if !steps.is_empty() {
                break;
            }
        }
    }

    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

/// 推断操作类型
fn infer_operation_type(description: &str) -> OperationType {
    let desc_lower = description.to_lowercase();

    if desc_lower.contains("读取")
        || desc_lower.contains("查看")
        || desc_lower.contains("read")
        || desc_lower.contains("查询")
        || desc_lower.contains("检查")
    {
        OperationType::Read
    } else if desc_lower.contains("删除")
        || desc_lower.contains("delete")
        || desc_lower.contains("remove")
        || desc_lower.contains("清空")
    {
        OperationType::Delete
    } else if desc_lower.contains("写入")
        || desc_lower.contains("创建")
        || desc_lower.contains("write")
        || desc_lower.contains("create")
        || desc_lower.contains("修改")
        || desc_lower.contains("更新")
        || desc_lower.contains("update")
    {
        OperationType::Write
    } else if desc_lower.contains("请求")
        || desc_lower.contains("调用")
        || desc_lower.contains("fetch")
        || desc_lower.contains("request")
        || desc_lower.contains("网络")
    {
        OperationType::Network
    } else {
        OperationType::Analysis
    }
}

/// 判断是否是破坏性操作
fn is_destructive(description: &str) -> bool {
    let desc_lower = description.to_lowercase();
    desc_lower.contains("删除")
        || desc_lower.contains("delete")
        || desc_lower.contains("清空")
        || desc_lower.contains("remove")
}

/// 提取解释
fn extract_explanation(content: &str) -> String {
    // 取前 200 个字符作为解释
    content.chars().take(200).collect()
}

/// 提取风险
fn extract_risks(content: &str) -> Vec<String> {
    let mut risks = Vec::new();

    let risk_keywords = ["风险", "注意", "警告", "danger", "warning", "caution"];

    for line in content.lines() {
        let line_lower = line.to_lowercase();
        if risk_keywords.iter().any(|kw| line_lower.contains(kw)) {
            let risk = line.trim().to_string();
            if !risk.is_empty() && risk.len() < 200 {
                risks.push(risk);
            }
        }
    }

    risks
}

/// 检查是否需要用户确认
fn has_confirmation_keywords(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("确认")
        || lower.contains("批准")
        || lower.contains("是否")
        || lower.contains("confirm")
        || lower.contains("approve")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_structured_plan() {
        let content = r#"
我将执行以下操作：

```plan-json
{
  "taskId": "test-123",
  "intent": "创建笔记",
  "steps": [
    {
      "stepNumber": 1,
      "description": "读取模板文件",
      "operationType": "read",
      "expectedOutcome": "获取模板内容",
      "reversible": true
    }
  ],
  "explanation": "创建新笔记",
  "risks": [],
  "userChoiceRequired": false
}
```
"#;
        let plan = extract_execution_plan(content, "创建笔记");
        assert!(plan.is_some());
        assert_eq!(plan.unwrap().steps.len(), 1);
    }

    #[test]
    fn test_extract_natural_language_plan() {
        let content = "我将执行以下步骤：\n1. 读取文件\n2. 分析内容\n3. 生成报告";
        let plan = extract_execution_plan(content, "生成报告");
        assert!(plan.is_some());
        let plan = plan.unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].operation_type, OperationType::Read);
    }

    #[test]
    fn test_infer_operation_type() {
        assert_eq!(infer_operation_type("读取文件"), OperationType::Read);
        assert_eq!(infer_operation_type("创建笔记"), OperationType::Write);
        assert_eq!(infer_operation_type("删除文件"), OperationType::Delete);
        assert_eq!(infer_operation_type("请求 API"), OperationType::Network);
        assert_eq!(infer_operation_type("分析数据"), OperationType::Analysis);
    }

    #[test]
    fn test_destructive_detection() {
        assert!(is_destructive("删除所有文件"));
        assert!(is_destructive("清空数据库"));
        assert!(!is_destructive("读取文件"));
        assert!(!is_destructive("创建笔记"));
    }

    #[test]
    fn test_extract_risks() {
        let content = "我将执行操作。\n警告：此操作不可逆。\n注意：请备份数据。";
        let risks = extract_risks(content);
        assert_eq!(risks.len(), 2);
        assert!(risks[0].contains("警告"));
        assert!(risks[1].contains("注意"));
    }

    #[test]
    fn test_has_confirmation_keywords() {
        assert!(has_confirmation_keywords("是否确认执行？"));
        assert!(has_confirmation_keywords("需要批准"));
        assert!(has_confirmation_keywords("Please confirm"));
        assert!(!has_confirmation_keywords("直接执行"));
    }

    #[test]
    fn test_extract_numbered_steps() {
        let content = "计划：\n1. 第一步\n2. 第二步\n3. 第三步";
        let steps = extract_steps_from_text(content);
        assert!(steps.is_some());
        assert_eq!(steps.unwrap().len(), 3);
    }

    #[test]
    fn test_extract_bullet_steps() {
        let content = "任务：\n- 读取配置\n- 处理数据\n- 保存结果";
        let steps = extract_steps_from_text(content);
        assert!(steps.is_some());
        assert_eq!(steps.unwrap().len(), 3);
    }
}
