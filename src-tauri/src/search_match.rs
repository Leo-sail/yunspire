use serde::Serialize;

/// 搜索结果（包含匹配原因）
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// 笔记元数据
    pub note: NoteMetadata,
    /// 相关度评分 (0.0 - 1.0)
    pub score: f64,
    /// 匹配原因详情
    pub match_reasons: MatchReasons,
}

/// 笔记元数据（简化版，实际应从 obsidian.rs 导入）
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMetadata {
    pub id: String,
    pub title: String,
    pub relative_path: String,
    pub tags: Vec<String>,
}

/// 匹配原因
#[allow(dead_code)]
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReasons {
    /// 标题匹配信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_match: Option<TitleMatchInfo>,

    /// 内容匹配信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_match: Option<ContentMatchInfo>,

    /// 标签匹配信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_match: Option<TagMatchInfo>,

    /// Wiki Links 匹配信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki_link_match: Option<WikiLinkMatchInfo>,

    /// 语义匹配信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_match: Option<SemanticMatchInfo>,

    /// 时间衰减加权
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recency_boost: Option<f64>,
}

/// 标题匹配信息
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleMatchInfo {
    /// 查询词
    pub query: String,
    /// 高亮后的标题（带 <mark> 标签）
    pub highlight: String,
    /// 是否精确匹配
    pub exact: bool,
}

/// 内容匹配信息
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatchInfo {
    /// 匹配次数
    pub occurrences: usize,
    /// 内容片段（带上下文）
    pub snippets: Vec<String>,
    /// 匹配位置（字符偏移量）
    pub positions: Vec<usize>,
}

/// 标签匹配信息
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagMatchInfo {
    /// 匹配的标签
    pub matched_tags: Vec<String>,
    /// 总标签数
    pub total_tags: usize,
}

/// Wiki Links 匹配信息
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiLinkMatchInfo {
    /// 被多少笔记引用（backlinks）
    pub backlinks: usize,
    /// 引用了多少笔记（outlinks）
    pub outlinks: usize,
    /// 相关笔记标题
    pub related_notes: Vec<String>,
}

/// 语义匹配信息
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticMatchInfo {
    /// 语义相似度 (0.0 - 1.0)
    pub similarity: f64,
    /// 匹配的语义概念
    pub concept: String,
}

/// 搜索匹配分析器
#[allow(dead_code)]
pub struct MatchReasonAnalyzer;

impl MatchReasonAnalyzer {
    /// 分析标题匹配
    #[allow(dead_code)]
    pub fn analyze_title_match(title: &str, query: &str) -> Option<TitleMatchInfo> {
        let title_lower = title.to_lowercase();
        let query_lower = query.to_lowercase();

        if !title_lower.contains(&query_lower) {
            return None;
        }

        let exact = title_lower == query_lower;
        let highlight = Self::highlight_text(title, query);

        Some(TitleMatchInfo {
            query: query.to_string(),
            highlight,
            exact,
        })
    }

    /// 分析内容匹配
    #[allow(dead_code)]
    pub fn analyze_content_match(
        content: &str,
        query: &str,
        max_snippets: usize,
    ) -> Option<ContentMatchInfo> {
        let content_lower = content.to_lowercase();
        let query_lower = query.to_lowercase();

        // 查找所有匹配位置
        let positions: Vec<usize> = content_lower
            .match_indices(&query_lower)
            .map(|(pos, _)| pos)
            .collect();

        if positions.is_empty() {
            return None;
        }

        // 提取片段（带上下文）
        let snippets = positions
            .iter()
            .take(max_snippets)
            .map(|&pos| Self::extract_snippet(content, pos, query.len(), 50))
            .collect();

        Some(ContentMatchInfo {
            occurrences: positions.len(),
            snippets,
            positions,
        })
    }

    /// 分析标签匹配
    #[allow(dead_code)]
    pub fn analyze_tag_match(tags: &[String], query: &str) -> Option<TagMatchInfo> {
        let query_lower = query.to_lowercase();
        let matched_tags: Vec<String> = tags
            .iter()
            .filter(|tag| tag.to_lowercase().contains(&query_lower))
            .cloned()
            .collect();

        if matched_tags.is_empty() {
            return None;
        }

        Some(TagMatchInfo {
            matched_tags,
            total_tags: tags.len(),
        })
    }

    /// 高亮文本中的匹配词
    #[allow(dead_code)]
    fn highlight_text(text: &str, query: &str) -> String {
        let text_lower = text.to_lowercase();
        let query_lower = query.to_lowercase();

        let mut result = String::new();
        let mut last_pos = 0;

        for (pos, _) in text_lower.match_indices(&query_lower) {
            result.push_str(&text[last_pos..pos]);
            result.push_str("<mark>");
            result.push_str(&text[pos..pos + query.len()]);
            result.push_str("</mark>");
            last_pos = pos + query.len();
        }

        result.push_str(&text[last_pos..]);
        result
    }

    /// 提取内容片段（带上下文）
    #[allow(dead_code)]
    fn extract_snippet(
        content: &str,
        match_pos: usize,
        match_len: usize,
        context_chars: usize,
    ) -> String {
        // 确保边界在字符边界上
        let char_indices: Vec<(usize, char)> = content.char_indices().collect();

        // 找到匹配位置对应的字符索引
        let match_char_idx = char_indices
            .iter()
            .position(|(idx, _)| *idx >= match_pos)
            .unwrap_or(0);

        // 计算上下文范围
        let start_char_idx = match_char_idx.saturating_sub(context_chars);
        let end_char_idx = (match_char_idx + context_chars).min(char_indices.len());

        let start = if start_char_idx > 0 {
            char_indices[start_char_idx].0
        } else {
            0
        };

        let end = if end_char_idx < char_indices.len() {
            char_indices[end_char_idx].0
        } else {
            content.len()
        };

        let mut snippet = String::new();

        // 添加前缀省略号
        if start > 0 {
            snippet.push_str("...");
        }

        // 提取文本
        let text = &content[start..end];

        // 高亮匹配部分
        let relative_pos = match_pos - start;
        let relative_end = relative_pos + match_len;

        // 确保 relative_pos 和 relative_end 在字符边界上
        let text_len = text.len();
        if relative_pos <= text_len && relative_end <= text_len {
            snippet.push_str(&text[..relative_pos]);
            snippet.push_str("<mark>");
            snippet.push_str(&text[relative_pos..relative_end]);
            snippet.push_str("</mark>");
            snippet.push_str(&text[relative_end..]);
        } else {
            // 如果边界不对，直接返回整段文本
            snippet.push_str(text);
        }

        // 添加后缀省略号
        if end < content.len() {
            snippet.push_str("...");
        }

        snippet
    }

    /// 计算时间衰减权重
    #[allow(dead_code)]
    pub fn calculate_recency_boost(days_since_modified: f64) -> f64 {
        // 最近 7 天内的笔记获得正向加权
        if days_since_modified <= 7.0 {
            0.2 * (1.0 - days_since_modified / 7.0)
        }
        // 7-30 天无加权
        else if days_since_modified <= 30.0 {
            0.0
        }
        // 超过 30 天开始衰减
        else {
            -0.1 * ((days_since_modified - 30.0) / 365.0).min(1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_match_exact() {
        let result = MatchReasonAnalyzer::analyze_title_match("第一性原理", "第一性原理");
        assert!(result.is_some());
        let info = result.unwrap();
        assert!(info.exact);
        assert!(info.highlight.contains("<mark>"));
    }

    #[test]
    fn test_title_match_partial() {
        let result = MatchReasonAnalyzer::analyze_title_match("第一性原理思考", "第一性原理");
        assert!(result.is_some());
        let info = result.unwrap();
        assert!(!info.exact);
    }

    #[test]
    fn test_content_match() {
        let content = "第一性原理是一种思维方式。第一性原理帮助我们思考。";
        let result = MatchReasonAnalyzer::analyze_content_match(content, "第一性原理", 3);
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.occurrences, 2);
        assert_eq!(info.snippets.len(), 2);
    }

    #[test]
    fn test_tag_match() {
        let tags = vec!["思维模型".to_string(), "第一性原理".to_string()];
        let result = MatchReasonAnalyzer::analyze_tag_match(&tags, "第一性");
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.matched_tags.len(), 1);
    }

    #[test]
    fn test_recency_boost() {
        assert!(MatchReasonAnalyzer::calculate_recency_boost(0.0) > 0.0); // 今天
        assert_eq!(MatchReasonAnalyzer::calculate_recency_boost(15.0), 0.0); // 15天
        assert!(MatchReasonAnalyzer::calculate_recency_boost(100.0) < 0.0); // 100天
    }
}
