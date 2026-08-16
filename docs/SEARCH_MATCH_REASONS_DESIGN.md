# 搜索结果匹配原因解释功能设计

## 问题分析

### 当前问题
- 用户搜索后看到结果，但不知道"为什么匹配"
- 无法理解搜索排序的依据
- 难以优化搜索关键词

### 用户需求
- 知道是标题匹配还是内容匹配
- 看到具体匹配的片段
- 了解是否有标签、链接等其他信号
- 理解时间衰减对排序的影响

## 设计方案

### 1. 数据结构

```rust
// src-tauri/src/obsidian.rs

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub note: NoteMetadata,
    pub score: f64,
    pub match_reasons: MatchReasons,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReasons {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_match: Option<TitleMatchInfo>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_match: Option<ContentMatchInfo>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_match: Option<TagMatchInfo>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki_link_match: Option<WikiLinkMatchInfo>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_match: Option<SemanticMatchInfo>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recency_boost: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleMatchInfo {
    pub query: String,
    pub highlight: String,  // 高亮的标题片段
    pub exact: bool,        // 是否精确匹配
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatchInfo {
    pub occurrences: usize,
    pub snippets: Vec<String>,  // 匹配的上下文片段
    pub positions: Vec<usize>,  // 匹配位置（用于高亮）
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagMatchInfo {
    pub matched_tags: Vec<String>,
    pub total_tags: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiLinkMatchInfo {
    pub backlinks: usize,       // 被多少笔记引用
    pub outlinks: usize,        // 引用了多少笔记
    pub related_notes: Vec<String>,  // 相关笔记标题
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticMatchInfo {
    pub similarity: f64,
    pub concept: String,  // 匹配的语义概念
}
```

### 2. 搜索算法增强

```rust
pub fn search_notes_with_reasons(
    database: &RuntimeDatabase,
    vault_id: &str,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    // 1. FTS 词法搜索
    let fts_results = fts_search(database, vault_id, query)?;
    
    // 2. 本地特征向量搜索
    let vector_results = local_vector_search(database, vault_id, query)?;
    
    // 3. 可选的神经 Embedding 搜索
    let neural_results = if neural_embedding_enabled() {
        neural_vector_search(database, vault_id, query).ok()
    } else {
        None
    };
    
    // 4. RRF 融合
    let fused_results = reciprocal_rank_fusion(
        fts_results,
        vector_results,
        neural_results,
    );
    
    // 5. 为每个结果生成匹配原因
    let results_with_reasons = fused_results
        .into_iter()
        .map(|result| {
            let match_reasons = analyze_match_reasons(
                database,
                &result.note,
                query,
                &result.ranks,
            );
            
            SearchResult {
                note: result.note,
                score: result.final_score,
                match_reasons,
            }
        })
        .collect();
    
    Ok(results_with_reasons)
}

fn analyze_match_reasons(
    database: &RuntimeDatabase,
    note: &NoteMetadata,
    query: &str,
    ranks: &RankSources,
) -> MatchReasons {
    let mut reasons = MatchReasons::default();
    
    // 标题匹配分析
    if note.title.to_lowercase().contains(&query.to_lowercase()) {
        reasons.title_match = Some(TitleMatchInfo {
            query: query.to_string(),
            highlight: highlight_in_title(&note.title, query),
            exact: note.title.to_lowercase() == query.to_lowercase(),
        });
    }
    
    // 内容匹配分析
    if let Some(fts_rank) = ranks.fts_rank {
        let snippets = extract_content_snippets(database, note.id, query, 3);
        reasons.content_match = Some(ContentMatchInfo {
            occurrences: count_occurrences(&note.content, query),
            snippets,
            positions: find_match_positions(&note.content, query),
        });
    }
    
    // 标签匹配分析
    let query_lower = query.to_lowercase();
    let matched_tags: Vec<String> = note.tags
        .iter()
        .filter(|tag| tag.to_lowercase().contains(&query_lower))
        .cloned()
        .collect();
    
    if !matched_tags.is_empty() {
        reasons.tag_match = Some(TagMatchInfo {
            matched_tags,
            total_tags: note.tags.len(),
        });
    }
    
    // Wiki Links 分析
    let (backlinks, outlinks) = count_wiki_links(database, note.id);
    if backlinks > 0 || outlinks > 0 {
        reasons.wiki_link_match = Some(WikiLinkMatchInfo {
            backlinks,
            outlinks,
            related_notes: get_top_related_notes(database, note.id, 5),
        });
    }
    
    // 语义匹配分析
    if let Some(similarity) = ranks.semantic_similarity {
        reasons.semantic_match = Some(SemanticMatchInfo {
            similarity,
            concept: extract_semantic_concept(query),
        });
    }
    
    // 时间衰减分析
    let age_days = (SystemTime::now().duration_since(note.modified_at).unwrap().as_secs() / 86400) as f64;
    let recency_boost = calculate_recency_boost(age_days);
    if recency_boost != 0.0 {
        reasons.recency_boost = Some(recency_boost);
    }
    
    reasons
}
```

### 3. 前端展示

```typescript
// desktop-ui/app.js

function renderSearchResult(result: SearchResult) {
  const reasons = buildMatchReasonSummary(result.match_reasons);
  
  return `
    <div class="search-result">
      <div class="result-title">${result.note.title}</div>
      <div class="result-path">${result.note.relative_path}</div>
      
      <!-- 匹配原因 -->
      <div class="match-reasons">
        ${reasons.map(r => `
          <span class="reason-badge" title="${r.tooltip}">
            ${r.icon} ${r.label}
          </span>
        `).join('')}
      </div>
      
      <!-- 内容片段 -->
      ${result.match_reasons.content_match ? `
        <div class="content-snippets">
          ${result.match_reasons.content_match.snippets.map(s => `
            <div class="snippet">${highlightMatches(s, query)}</div>
          `).join('')}
        </div>
      ` : ''}
      
      <div class="result-score">相关度: ${(result.score * 100).toFixed(0)}%</div>
    </div>
  `;
}

function buildMatchReasonSummary(reasons: MatchReasons): ReasonBadge[] {
  const badges: ReasonBadge[] = [];
  
  if (reasons.title_match) {
    badges.push({
      icon: '📝',
      label: reasons.title_match.exact ? '标题精确匹配' : '标题匹配',
      tooltip: `标题中包含 "${reasons.title_match.query}"`,
    });
  }
  
  if (reasons.content_match) {
    badges.push({
      icon: '📄',
      label: `内容出现 ${reasons.content_match.occurrences} 次`,
      tooltip: `在正文中找到 ${reasons.content_match.occurrences} 处匹配`,
    });
  }
  
  if (reasons.tag_match) {
    badges.push({
      icon: '🏷️',
      label: `标签: ${reasons.tag_match.matched_tags.join(', ')}`,
      tooltip: `标签匹配: ${reasons.tag_match.matched_tags.length}/${reasons.tag_match.total_tags}`,
    });
  }
  
  if (reasons.wiki_link_match) {
    if (reasons.wiki_link_match.backlinks > 0) {
      badges.push({
        icon: '🔗',
        label: `被 ${reasons.wiki_link_match.backlinks} 篇笔记引用`,
        tooltip: `高引用表示这是重要笔记`,
      });
    }
  }
  
  if (reasons.semantic_match) {
    badges.push({
      icon: '🧠',
      label: `语义相似度 ${(reasons.semantic_match.similarity * 100).toFixed(0)}%`,
      tooltip: `通过语义理解匹配概念: ${reasons.semantic_match.concept}`,
    });
  }
  
  if (reasons.recency_boost) {
    const sign = reasons.recency_boost > 0 ? '+' : '';
    badges.push({
      icon: '⏰',
      label: `最近修改 (${sign}${(reasons.recency_boost * 100).toFixed(0)}%)`,
      tooltip: `最近修改的笔记获得权重提升`,
    });
  }
  
  return badges;
}
```

### 4. 样式

```css
/* desktop-ui/styles.css */

.search-result {
  padding: 16px;
  border-bottom: 1px solid var(--border-color);
}

.result-title {
  font-size: 18px;
  font-weight: 600;
  margin-bottom: 4px;
}

.result-path {
  font-size: 12px;
  color: var(--text-muted);
  margin-bottom: 8px;
}

.match-reasons {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 12px;
}

.reason-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  background: var(--bg-secondary);
  border-radius: 12px;
  font-size: 12px;
  cursor: help;
}

.reason-badge:hover {
  background: var(--bg-hover);
}

.content-snippets {
  margin: 12px 0;
  padding: 12px;
  background: var(--bg-code);
  border-radius: 8px;
}

.snippet {
  line-height: 1.6;
  margin-bottom: 8px;
}

.snippet:last-child {
  margin-bottom: 0;
}

.snippet mark {
  background: var(--highlight-color);
  padding: 2px 4px;
  border-radius: 2px;
}

.result-score {
  font-size: 11px;
  color: var(--text-muted);
  text-align: right;
}
```

## 实施步骤

### Phase 1: 后端数据结构
- [ ] 定义 `MatchReasons` 及相关结构体
- [ ] 修改 `search_notes` 函数返回类型
- [ ] 实现 `analyze_match_reasons` 函数

### Phase 2: 匹配原因分析
- [ ] 实现标题匹配检测和高亮
- [ ] 实现内容片段提取
- [ ] 实现标签匹配分析
- [ ] 实现 Wiki Links 统计
- [ ] 实现时间衰减计算

### Phase 3: 前端展示
- [ ] 实现匹配原因徽章组件
- [ ] 实现内容片段高亮
- [ ] 添加悬停提示
- [ ] 样式优化

### Phase 4: 测试和优化
- [ ] 单元测试
- [ ] 性能优化（避免重复查询）
- [ ] 用户体验测试

## 性能考虑

### 缓存策略
```rust
// 缓存笔记的 backlink/outlink 统计
struct LinkCache {
    cache: HashMap<String, (usize, usize)>,  // note_id -> (backlinks, outlinks)
    last_updated: SystemTime,
}
```

### 批量查询
```rust
// 一次查询获取所有需要的信息
fn batch_analyze_match_reasons(
    database: &RuntimeDatabase,
    notes: &[NoteMetadata],
    query: &str,
) -> Vec<MatchReasons> {
    // 批量查询 link 统计
    let link_stats = batch_query_link_stats(database, notes);
    
    // 批量提取片段
    let snippets = batch_extract_snippets(database, notes, query);
    
    // 组装结果
    notes.iter().zip(link_stats).zip(snippets)
        .map(|((note, links), snips)| {
            build_match_reasons(note, query, links, snips)
        })
        .collect()
}
```

## 测试用例

1. **标题精确匹配**
   - 输入：搜索 "第一性原理"
   - 预期：显示 "📝 标题精确匹配"

2. **内容多次出现**
   - 输入：搜索 "知识管理"
   - 预期：显示 "📄 内容出现 5 次"，展示 3 个片段

3. **标签匹配**
   - 输入：搜索 "思维模型"
   - 预期：显示 "🏷️ 标签: #思维模型"

4. **高引用笔记**
   - 输入：搜索 "Agent"
   - 预期：显示 "🔗 被 12 篇笔记引用"

5. **最近修改**
   - 输入：搜索 "云枢"
   - 预期：显示 "⏰ 最近修改 (+20%)"

## 总结

这个功能将显著提升搜索的可解释性，帮助用户：
1. 理解为什么某个结果排在前面
2. 快速定位相关内容
3. 优化搜索关键词
4. 发现笔记之间的关联
