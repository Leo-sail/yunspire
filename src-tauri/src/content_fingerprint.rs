use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 内容指纹（多层）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentFingerprint {
    /// L1: 精确哈希（完全相同）
    pub exact_hash: String,
    /// L2: 结构哈希（标题 + 段落数 + 字数范围）
    pub structure_hash: String,
    /// L3: SimHash（语义相似度）
    pub simhash: u64,
    /// L4: 来源指纹（URL 主域名 + 发布时间）
    pub source_fingerprint: Option<String>,
}

/// 重复级别
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateLevel {
    /// 完全相同
    Exact,
    /// 结构相似
    StructuralSimilar,
    /// 语义相似
    SemanticSimilar,
    /// 更新版本
    UpdatedVersion,
}

/// 重复检测结果
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateDetectionResult {
    /// 重复级别
    pub level: DuplicateLevel,
    /// 原笔记路径
    pub original_note: String,
    /// 相似度分数 (0.0-1.0)
    pub similarity: f64,
    /// 建议操作
    pub recommendation: String,
}

impl ContentFingerprint {
    /// 生成精确哈希
    pub fn exact_hash(content: &str) -> String {
        format!("{:x}", Sha256::digest(content.as_bytes()))
    }

    /// 生成结构哈希
    pub fn structure_hash(title: &str, content: &str) -> String {
        let paragraph_count = content.split("\n\n").count();
        let word_count = content.split_whitespace().count();
        let word_count_range = (word_count / 100) * 100; // 向下取整到百位

        let structure = format!("{title}|{paragraph_count}|{word_count_range}");
        format!("{:x}", Sha256::digest(structure.as_bytes()))
    }

    /// 生成 SimHash（简化版）
    pub fn simhash(content: &str) -> u64 {
        let words: Vec<&str> = content.split_whitespace().collect();
        let mut hash: u64 = 0;

        for word in words {
            let word_hash = Self::hash_word(word);
            hash ^= word_hash;
        }

        hash
    }

    fn hash_word(word: &str) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(word.as_bytes());
        let result = hasher.finalize();
        u64::from_le_bytes([
            result[0], result[1], result[2], result[3], result[4], result[5], result[6], result[7],
        ])
    }

    /// 计算 SimHash 汉明距离
    pub fn hamming_distance(hash1: u64, hash2: u64) -> u32 {
        (hash1 ^ hash2).count_ones()
    }

    /// 生成来源指纹
    pub fn source_fingerprint(url: Option<&str>, published_at: Option<&str>) -> Option<String> {
        if let Some(url_str) = url {
            if let Ok(parsed_url) = reqwest::Url::parse(url_str) {
                let domain = parsed_url.host_str()?;
                let timestamp = published_at.unwrap_or("unknown");
                return Some(format!("{domain}|{timestamp}"));
            }
        }
        None
    }

    /// 创建完整指纹
    #[allow(dead_code)]
    pub fn new(title: &str, content: &str, url: Option<&str>, published_at: Option<&str>) -> Self {
        Self {
            exact_hash: Self::exact_hash(content),
            structure_hash: Self::structure_hash(title, content),
            simhash: Self::simhash(content),
            source_fingerprint: Self::source_fingerprint(url, published_at),
        }
    }

    /// 检测重复
    #[allow(dead_code)]
    pub fn detect_duplicate(&self, other: &ContentFingerprint) -> Option<DuplicateLevel> {
        // L1: 精确匹配
        if self.exact_hash == other.exact_hash {
            return Some(DuplicateLevel::Exact);
        }

        // L2: 结构相似
        if self.structure_hash == other.structure_hash {
            return Some(DuplicateLevel::StructuralSimilar);
        }

        // L3: 语义相似（汉明距离 < 3）
        let distance = Self::hamming_distance(self.simhash, other.simhash);
        if distance < 3 {
            return Some(DuplicateLevel::SemanticSimilar);
        }

        // L4: 更新版本（来源相同但内容不同）
        if let (Some(source1), Some(source2)) = (&self.source_fingerprint, &other.source_fingerprint)
        {
            if source1 == source2 && self.exact_hash != other.exact_hash {
                return Some(DuplicateLevel::UpdatedVersion);
            }
        }

        None
    }
}

/// 检测内容重复
#[tauri::command]
pub fn detect_content_duplicate(
    _title: String,
    _content: String,
    _url: Option<String>,
) -> Result<Vec<DuplicateDetectionResult>, String> {
    // TODO: 从数据库查询现有笔记的指纹进行比对
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_duplicate() {
        let fp1 = ContentFingerprint::new("Title", "Content", None, None);
        let fp2 = ContentFingerprint::new("Title", "Content", None, None);
        assert_eq!(fp1.detect_duplicate(&fp2), Some(DuplicateLevel::Exact));
    }

    #[test]
    fn test_structure_similar() {
        let fp1 = ContentFingerprint::new(
            "Title",
            "Paragraph 1\n\nParagraph 2\n\nParagraph 3",
            None,
            None,
        );
        let fp2 = ContentFingerprint::new(
            "Title",
            "Different content\n\nWith same structure\n\nThree paragraphs",
            None,
            None,
        );
        // 注意：结构哈希包含标题，所以需要相同标题和段落数
        assert_eq!(
            fp1.detect_duplicate(&fp2),
            Some(DuplicateLevel::StructuralSimilar)
        );
    }

    #[test]
    fn test_hamming_distance() {
        assert_eq!(ContentFingerprint::hamming_distance(0b1111, 0b0000), 4);
        assert_eq!(ContentFingerprint::hamming_distance(0b1010, 0b1000), 1);
        assert_eq!(ContentFingerprint::hamming_distance(0b1111, 0b1111), 0);
    }

    #[test]
    fn test_source_fingerprint() {
        let fp = ContentFingerprint::source_fingerprint(
            Some("https://example.com/article"),
            Some("2024-01-01"),
        );
        assert_eq!(fp, Some("example.com|2024-01-01".to_string()));
    }
}
