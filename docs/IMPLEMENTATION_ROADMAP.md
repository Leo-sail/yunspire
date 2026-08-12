# 云枢 v0.4.2+ 完善开发实施计划

本文档整合了所有待完成任务的设计方案和实施步骤。

---

## 已完成 ✅

### 1. 修复 Worktree 污染导致的发布审计失败 ✅
- 改进 .gitignore 排除 .claude/worktrees/
- 修改 audit-release.mjs 忽略 .claude 目录
- 验证通过：777 files version=0.4.2

### 2. 补充 IPv6 私网地址安全检测 ✅
- 实现 is_private_or_local_address() 函数
- 检测 IPv6: ::1, fc00::/7, fe80::/10
- 3 个单元测试全部通过

---

## 待完成任务概览

### P0 - 已设计，待实施

#### 任务 #2: 优雅降级的采集错误处理
**设计文档**: `docs/GRACEFUL_DEGRADATION_DESIGN.md`

**核心改进**:
- 引入 `CaptureStatus::PartialSuccess` 状态
- 外链图片失败不阻断核心内容保存
- Agent 库失败不影响用户库
- 支持重试失败的增强功能

**实施要点**:
```rust
// 核心数据结构
pub enum CaptureStatus {
    FullSuccess,
    PartialSuccess,  // 新增
    CoreFailed,
}

pub struct EnhancementResults {
    pub linked_images: ImageEnhancementResult,
    pub model_analysis: ModelEnhancementResult,
    pub agent_vault: AgentVaultResult,
}
```

**修改文件**:
- `src-tauri/src/capture_pipeline.rs` (主要修改)
- `desktop-ui/app.js` (UI 反馈)

---

#### 任务 #3: 搜索结果匹配原因解释
**设计文档**: `docs/SEARCH_MATCH_REASONS_DESIGN.md`

**核心改进**:
- 显示标题/内容/标签/链接/语义匹配
- 展示内容片段和匹配位置
- 时间衰减权重可视化

**实施要点**:
```rust
pub struct SearchResult {
    pub note: NoteMetadata,
    pub score: f64,
    pub match_reasons: MatchReasons,  // 新增
}

pub struct MatchReasons {
    pub title_match: Option<TitleMatchInfo>,
    pub content_match: Option<ContentMatchInfo>,
    pub tag_match: Option<TagMatchInfo>,
    // ...
}
```

**修改文件**:
- `src-tauri/src/obsidian.rs` (搜索逻辑)
- `desktop-ui/app.js` (结果展示)
- `desktop-ui/styles.css` (样式)

---

### P1 - 需要详细设计

#### 任务 #4: 知识库健康度可视化仪表盘

**设计概要**:

```typescript
interface KnowledgeHealthDashboard {
  // 数量指标
  stats: {
    totalNotes: number;
    orphanNotes: number;      // 没有任何链接
    stubNotes: number;         // 字数 < 50
    richNotes: number;         // 有图片、链接、标签
  };
  
  // 健康度评分 (0-100)
  healthScore: number;
  
  // 具体问题
  issues: Array<{
    type: 'orphan' | 'duplicate' | 'outdated' | 'broken_link';
    severity: 'low' | 'medium' | 'high';
    affectedNotes: string[];
    autoFixAvailable: boolean;
  }>;
  
  // 改进建议
  suggestions: Array<{
    action: 'merge_duplicates' | 'add_links' | 'enrich_tags';
    impact: string;  // "提升 8 分健康度"
    effort: 'low' | 'medium' | 'high';
  }>;
}
```

**实施步骤**:
1. 后端分析函数 (`src-tauri/src/obsidian.rs`)
   - 统计各类笔记数量
   - 检测孤立笔记（无 backlinks/outlinks）
   - 检测 stub 笔记（字数少）
   - 检测失效链接

2. 健康度评分算法
   ```rust
   fn calculate_health_score(stats: &VaultStats) -> f64 {
       let mut score = 100.0;
       
       // 孤立笔记扣分
       let orphan_ratio = stats.orphan_notes as f64 / stats.total_notes as f64;
       score -= orphan_ratio * 30.0;
       
       // stub 笔记扣分
       let stub_ratio = stats.stub_notes as f64 / stats.total_notes as f64;
       score -= stub_ratio * 20.0;
       
       // 富笔记加分
       let rich_ratio = stats.rich_notes as f64 / stats.total_notes as f64;
       score += rich_ratio * 10.0;
       
       score.max(0.0).min(100.0)
   }
   ```

3. 前端仪表盘 UI
   - 圆形进度条显示健康度
   - 问题列表（分严重程度）
   - 建议操作按钮

**新增文件**:
- `src-tauri/src/knowledge_health.rs`
- `desktop-ui/knowledge-health-dashboard.js`

---

#### 任务 #5: 多层内容指纹去重机制

**设计概要**:

```rust
struct ContentFingerprint {
    // L1: 精确哈希（现有）
    exact_hash: String,
    
    // L2: 结构哈希（标题 + 段落数 + 字数范围）
    structure_hash: String,
    
    // L3: SimHash（语义相似度）
    simhash: u64,
    
    // L4: 来源指纹（URL 主域名 + 发布时间）
    source_fingerprint: Option<String>,
}

enum DuplicateLevel {
    Exact,           // 完全相同
    StructuralSimilar,  // 结构相似
    SemanticSimilar,    // 语义相似
    UpdatedVersion,     // 更新版本
}
```

**去重策略**:
- exact_hash 相同 → 完全重复，跳过
- structure_hash 相同 → 疑似重复，询问用户
- simhash 汉明距离 < 3 → 提示"发现相似内容"
- source_fingerprint 相同但内容不同 → 标记为"更新版本"

**实施要点**:
1. 实现 SimHash 算法
2. 结构哈希计算
3. 来源指纹提取
4. 数据库存储多层哈希
5. UI 展示相似内容

**修改文件**:
- `src-tauri/src/capture_pipeline.rs`
- `src-tauri/src/runtime_db.rs` (Schema 扩展)

---

#### 任务 #6: AI 执行计划透明解释模式

**设计概要**:

```rust
pub struct ExecutionPlan {
    pub task_id: String,
    pub intent: String,
    pub steps: Vec<PlannedStep>,
    pub explanation: String,  // "我打算这样做，因为..."
    pub risks: Vec<String>,
    pub user_choice_required: bool,
}

pub struct PlannedStep {
    pub description: String,
    pub capability: String,
    pub parameters: Value,
    pub rationale: String,  // 为什么这样做
}

pub enum UserChoice {
    DirectExecute,    // 直接执行
    ReviewPlan,       // 先看计划
    ModifyPlan,       // 修改计划
    Cancel,           // 取消
}
```

**三阶段模型**:
```rust
pub enum AutonomyLevel {
    Observe,    // AI 只建议，用户手动执行
    Imitate,    // AI 执行前询问确认
    Autonomous, // AI 直接执行，只报告结果
}

// 每个能力可以独立设置
struct CapabilityAutonomy {
    capability_id: String,
    level: AutonomyLevel,
}
```

**实施步骤**:
1. 模型生成执行计划（在实际执行前）
2. 根据用户设置的自主程度决定是否询问
3. 显示计划和原因
4. 记录用户批准/拒绝模式
5. 自动调整未来行为

**修改文件**:
- `src-tauri/src/assistant_runtime.rs`
- `src-tauri/src/command_bus.rs`
- `desktop-ui/app.js` (计划审阅 UI)

---

#### 任务 #7: 创作流程简化（快速/专业模式）

**设计概要**:

```rust
pub enum CreationMode {
    Quick,        // 快速模式（默认）
    Professional, // 专业模式
}

pub struct CreationConfig {
    pub mode: CreationMode,
    pub skip_candidate_review: bool,    // 快速模式跳过
    pub skip_brand_evaluation: bool,    // 快速模式跳过
    pub publish_checklist: Option<Vec<CheckItem>>,
}

pub struct CheckItem {
    pub name: String,
    pub rule: CheckRule,
    pub required: bool,
}

pub enum CheckRule {
    MinWords(usize),
    RequiresKeyword(String),
    BrandCompliance,
    HasImages,
}
```

**用户体验**:
```typescript
// 快速模式：日常笔记
{
  mode: 'quick',
  workflow: '输入 → AI 生成 → 直接保存',
  skipSteps: ['候选审核', '品牌评测'],
  useCase: '日常笔记、临时想法'
}

// 专业模式：对外发布
{
  mode: 'professional',
  workflow: '输入 → AI 生成 → 候选审核 → 品牌评测 → 最终确认 → 保存',
  skipSteps: [],
  useCase: '博客文章、正式文档'
}
```

**修改文件**:
- `src-tauri/src/creation/runtime.rs`
- `desktop-ui/creation/` (模式切换 UI)

---

#### 任务 #8: Lease 心跳续期机制

**设计概要**:

```rust
pub struct LeaseHeartbeat {
    task_id: String,
    step_id: String,
    claim_token: String,
    last_heartbeat: SystemTime,
    renewal_interval: Duration,  // 默认 30 秒
}

impl TaskRuntime {
    // 周期性调用
    pub fn renew_active_leases(&self) -> Result<usize, String> {
        let now = SystemTime::now();
        let active_steps = self.get_active_steps()?;
        
        let mut renewed = 0;
        for step in active_steps {
            if should_renew_lease(&step, now) {
                self.extend_lease(&step.task_id, &step.step_id)?;
                renewed += 1;
            }
        }
        
        Ok(renewed)
    }
}

fn should_renew_lease(step: &ActiveStep, now: SystemTime) -> bool {
    let elapsed = now.duration_since(step.last_heartbeat).unwrap();
    elapsed >= step.renewal_interval
}
```

**实施步骤**:
1. 添加 lease 续期表到 SQLite
2. 实现心跳周期检查（后台线程）
3. 为长时任务提供显式续期 API
4. 记录续期历史（审计）

**修改文件**:
- `src-tauri/src/task_runtime.rs`
- `src-tauri/src/runtime_db.rs` (Schema)

---

#### 任务 #10: 使用效果量化指标系统

**设计概要**:

```rust
pub struct UsageMetrics {
    // 知识积累
    notes_created_per_week: f64,
    average_note_quality: f64,
    knowledge_graph_density: f64,
    
    // 知识利用
    searches_per_day: f64,
    note_revisit_rate: f64,
    cross_note_navigation_rate: f64,
    
    // AI 效果
    ai_suggestions_accept_rate: f64,
    capture_success_rate: f64,
    creation_output_per_week: f64,
    
    // 用户满意度
    task_completion_time_ms: f64,
    error_recovery_time_ms: f64,
    user_initiated_optimizations: usize,
}

pub struct MetricsReport {
    pub period: DateRange,
    pub metrics: UsageMetrics,
    pub trends: MetricsTrends,
    pub insights: Vec<String>,
}
```

**指标计算**:
```sql
-- 笔记质量评分
SELECT 
    note_id,
    (char_count / 500.0) * 0.3 +          -- 长度分
    (wiki_links_count / 5.0) * 0.3 +      -- 链接分
    (tags_count / 3.0) * 0.2 +            -- 标签分
    (has_images * 1.0) * 0.2              -- 图片分
AS quality_score
FROM notes;

-- 笔记重访率
SELECT 
    COUNT(DISTINCT note_id) / (SELECT COUNT(*) FROM notes) 
AS revisit_rate
FROM note_views
WHERE viewed_at >= date('now', '-7 days');
```

**前端展示**:
- 仪表盘卡片（关键指标）
- 趋势图表（Chart.js）
- 对比上周/上月
- 个性化建议

**新增文件**:
- `src-tauri/src/metrics.rs`
- `desktop-ui/metrics-dashboard.js`

---

## 实施优先级建议

### Sprint 1 (立即开始)
1. **任务 #8**: Lease 续期 (防止超长任务被回收)
2. **任务 #2**: 优雅降级 (提升核心用户体验)

### Sprint 2 (近期)
3. **任务 #3**: 搜索解释 (增强可理解性)
4. **任务 #7**: 创作模式 (简化日常使用)

### Sprint 3 (中期)
5. **任务 #6**: AI 透明化 (建立信任)
6. **任务 #4**: 健康度仪表盘 (可视化价值)

### Sprint 4 (长期)
7. **任务 #5**: 多层去重 (智能识别)
8. **任务 #10**: 量化指标 (衡量效果)

---

## 开发规范

### 代码质量要求
- Rust: Clippy 零警告
- 单元测试覆盖关键逻辑
- 文档注释完整
- 错误处理友好

### 提交规范
```
feat: 功能描述
fix: 修复描述
refactor: 重构描述
test: 测试描述
docs: 文档描述
```

### 测试策略
1. 单元测试：每个新函数
2. 集成测试：跨模块功能
3. 用户测试：UI 交互流程
4. 性能测试：大数据量场景

---

## 总结

当前已完成 2 个关键安全修复：
- ✅ Worktree 发布审计
- ✅ IPv6 安全检测

剩余 8 个功能增强任务已完成设计，按优先级分 4 个 Sprint 实施。

每个 Sprint 聚焦 2 个任务，确保质量和进度平衡。
