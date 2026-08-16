# 优雅降级采集错误处理设计文档

## 问题分析

### 当前问题
当前采集系统采用"全有或全无"策略：
- 外链图片下载失败 → 阻断整个入库
- Agent 库分析失败 → 阻断用户库保存
- 任何必需组件失败 → 显示错误，不保存任何内容

### 用户影响
- 用户只想"快速记录一个想法"
- 系统却要求"完美的数据完整性"
- 一张失效图片导致整篇文章无法保存

## 设计原则

### 核心原则：优雅降级 > 完美失败

```
核心价值（100% 必须保存）:
- 用户输入的文字
- 本地文件（PDF、Office、图片等）
- 来源 URL 和时间戳

增强价值（尽力而为，失败不阻断）:
- 外链图片本地化
- 模型分析和标签生成
- 相关笔记推荐
```

## 实现方案

### 1. 引入"部分成功"语义

#### 数据结构

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    pub status: CaptureStatus,
    pub core_saved: bool,           // 核心内容是否已保存
    pub enhancements: EnhancementResults,
    pub warnings: Vec<CaptureWarning>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    FullSuccess,      // 所有内容成功
    PartialSuccess,   // 核心成功，部分增强失败
    CoreFailed,       // 核心内容失败（无法保存）
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementResults {
    pub linked_images: ImageEnhancementResult,
    pub model_analysis: ModelEnhancementResult,
    pub agent_vault: AgentVaultResult,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageEnhancementResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: Vec<FailedImage>,
    pub retryable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedImage {
    pub url: String,
    pub reason: String,
    pub is_critical: bool,  // 是否对理解内容至关重要
}
```

#### 处理流程

```rust
async fn capture_with_graceful_degradation(input: CaptureInput) -> CaptureResult {
    // 阶段 1: 核心内容提取（必须成功）
    let core_content = match extract_core_content(&input).await {
        Ok(content) => content,
        Err(e) => return CaptureResult {
            status: CaptureStatus::CoreFailed,
            core_saved: false,
            error: Some(e),
            ...
        }
    };

    // 阶段 2: 外链图片本地化（尽力而为）
    let image_results = localize_linked_images(&core_content).await;
    
    // 阶段 3: 保存核心内容到用户库（即使图片失败）
    let user_vault_saved = save_to_user_vault(&core_content, &image_results).await?;

    // 阶段 4: 模型分析（可选，失败不影响核心）
    let model_analysis = analyze_with_model(&core_content).await
        .unwrap_or_else(|e| {
            log::warn!("模型分析失败，将跳过: {}", e);
            None
        });

    // 阶段 5: Agent 库保存（可选）
    let agent_vault_result = if let Some(analysis) = model_analysis {
        save_to_agent_vault(&analysis).await.ok()
    } else {
        None
    };

    // 返回部分成功结果
    CaptureResult {
        status: if image_results.all_succeeded() && agent_vault_result.is_some() {
            CaptureStatus::FullSuccess
        } else {
            CaptureStatus::PartialSuccess
        },
        core_saved: user_vault_saved,
        enhancements: EnhancementResults {
            linked_images: image_results,
            model_analysis: model_analysis.into(),
            agent_vault: agent_vault_result.into(),
        },
        warnings: build_warnings(&image_results, &agent_vault_result),
    }
}
```

### 2. 外链图片处理策略

#### 当前行为（阻断式）
```rust
// 当前：任何图片失败都阻断
for image_url in linked_images {
    let local_path = download_image(image_url)?;  // ? 会导致整个函数失败
    attachments.push(local_path);
}
```

#### 改进后（容错式）
```rust
// 改进：收集所有结果，区分关键和非关键
let mut succeeded = Vec::new();
let mut failed = Vec::new();

for image_ref in linked_images {
    match download_image(&image_ref.url).await {
        Ok(local_path) => {
            succeeded.push(ImageAttachment {
                reference_id: image_ref.id,
                local_path,
                url: image_ref.url,
            });
        }
        Err(e) => {
            failed.push(FailedImage {
                url: image_ref.url.clone(),
                reason: e.to_string(),
                is_critical: image_ref.is_content_image,  // 正文图 vs 装饰图
            });
            
            // 在 Markdown 中标记失败
            if image_ref.is_content_image {
                // 保留原始 URL，添加失败标记
                // ![图片](https://example.com/image.jpg "⚠️ 图片下载失败")
            }
        }
    }
}

ImageEnhancementResult {
    total: linked_images.len(),
    succeeded: succeeded.len(),
    failed,
    retryable: !failed.is_empty(),
}
```

### 3. 用户界面改进

#### 部分成功的展示

```typescript
// 前端处理
if (result.status === 'partial_success') {
  showNotification({
    type: 'warning',
    title: '内容已保存，部分增强功能未完成',
    message: `
      ✅ 原文已保存到 ${result.vault_name}
      ⚠️ ${result.enhancements.linked_images.failed.length} 张外链图片下载失败
      ${result.enhancements.agent_vault ? '⚠️ AI 分析未完成' : ''}
    `,
    actions: [
      { label: '查看详情', onClick: () => showDetails(result) },
      { label: '重试失败项', onClick: () => retryEnhancements(result.task_id) },
    ]
  });
}
```

#### 重试机制

```rust
#[tauri::command]
pub async fn retry_capture_enhancements(
    task_id: String,
    retry_options: RetryOptions,
) -> Result<CaptureResult, String> {
    let original_result = load_capture_result(&task_id)?;
    
    let mut new_result = original_result.clone();
    
    // 只重试失败的部分
    if retry_options.retry_images {
        let failed_images = original_result.enhancements.linked_images.failed;
        let retry_results = retry_image_downloads(failed_images).await;
        new_result.enhancements.linked_images.merge(retry_results);
    }
    
    if retry_options.retry_analysis && !original_result.enhancements.model_analysis.succeeded {
        let analysis = analyze_with_model(&original_result.core_content).await?;
        new_result.enhancements.model_analysis = ModelEnhancementResult::success(analysis);
    }
    
    Ok(new_result)
}
```

### 4. 配置选项

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePolicy {
    /// 外链图片失败时是否阻断保存（默认 false）
    pub block_on_image_failure: bool,
    
    /// 模型分析失败时是否阻断保存（默认 false）
    pub block_on_analysis_failure: bool,
    
    /// 是否自动重试失败的增强功能（默认 true）
    pub auto_retry_enhancements: bool,
    
    /// 重试次数（默认 2）
    pub max_retry_attempts: u32,
}
```

## 向后兼容

### 保持严格模式选项

对于需要"完美或失败"的场景（如正式发布内容），提供严格模式：

```rust
if capture_policy.block_on_image_failure && !image_results.all_succeeded() {
    return Err(format!(
        "图片下载失败（严格模式）：{}",
        image_results.failed.iter().map(|f| &f.url).join(", ")
    ));
}
```

## 实施步骤

### Phase 1: 数据结构和基础设施
- [ ] 定义 `CaptureStatus`、`EnhancementResults` 等结构
- [ ] 修改 `capture_pipeline.rs` 返回类型
- [ ] 添加部分成功的存储和查询

### Phase 2: 外链图片容错
- [ ] 改造图片下载为非阻断式
- [ ] 区分关键图片和装饰图片
- [ ] Markdown 中标记失败图片

### Phase 3: 模型分析容错
- [ ] 模型分析失败不影响用户库保存
- [ ] Agent 库保存失败时记录警告

### Phase 4: 用户界面
- [ ] 部分成功的通知 UI
- [ ] 详情查看界面
- [ ] 重试失败项功能

### Phase 5: 配置和测试
- [ ] 添加采集策略配置
- [ ] 编写单元测试和集成测试
- [ ] 文档更新

## 测试用例

### 测试场景

1. **所有内容成功**
   - 输入：包含有效外链图片的网页
   - 预期：`FullSuccess`，所有内容保存

2. **外链图片部分失败**
   - 输入：包含 3 张图片（2 张有效，1 张失效）
   - 预期：`PartialSuccess`，原文保存，2 张图片本地化，1 张标记失败

3. **模型分析失败**
   - 输入：有效网页，但模型 API 不可用
   - 预期：`PartialSuccess`，用户库保存，Agent 库未生成

4. **核心内容失败**
   - 输入：损坏的 PDF 文件
   - 预期：`CoreFailed`，不保存任何内容

5. **重试机制**
   - 输入：部分失败的任务 ID
   - 预期：只重试失败部分，成功后更新状态

## 性能考虑

### 并发下载
```rust
// 并发下载图片，但不阻塞主流程
let image_handles: Vec<_> = linked_images
    .into_iter()
    .map(|img| tokio::spawn(download_image(img)))
    .collect();

let results = futures::future::join_all(image_handles).await;
```

### 超时控制
```rust
const IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

match tokio::time::timeout(IMAGE_DOWNLOAD_TIMEOUT, download_image(url)).await {
    Ok(Ok(path)) => /* 成功 */,
    Ok(Err(e)) => /* 下载失败 */,
    Err(_) => /* 超时 */,
}
```

## 总结

优雅降级设计的核心思想：
1. **分离关注点**：核心价值 vs 增强价值
2. **容错优先**：尽力保存用户内容，而非追求完美
3. **透明反馈**：清晰告知用户哪些成功、哪些失败
4. **可恢复性**：支持重试失败的增强功能

这将显著提升用户体验，避免因小问题导致大损失。
