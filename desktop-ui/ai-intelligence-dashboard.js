/**
 * AI 内容智能组件
 */

import { ContentIntelligenceApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class AIIntelligenceDashboard {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.selectedNotePath = null;

    this.init();
  }

  init() {
    if (!this.container) {
      console.error('容器元素不存在');
      return;
    }

    this.render();
  }

  render() {
    this.container.innerHTML = `
      <div class="ai-intelligence-dashboard">
        <div class="dashboard-header">
          <h2>🤖 AI 内容智能</h2>
        </div>

        <div class="ai-content">
          <!-- 笔记选择器 -->
          <div class="note-selector-section">
            <h3>📝 选择笔记</h3>
            <div class="note-selector">
              <input type="text" id="note-path-input" placeholder="输入笔记路径 (例如: notes/example.md)" />
              <button class="analyze-btn" id="analyze-note-btn">分析</button>
            </div>
          </div>

          <!-- 分析结果 -->
          <div id="analysis-results" style="display: none;">
            <!-- 自动摘要 -->
            <div class="analysis-section" id="summary-section">
              <div class="section-header">
                <h3>📄 自动摘要</h3>
                <button class="generate-btn" id="generate-summary-btn">生成摘要</button>
              </div>
              <div class="section-content" id="summary-content">
                <p class="loading-text">点击生成按钮开始...</p>
              </div>
            </div>

            <!-- 关键词提取 -->
            <div class="analysis-section" id="keywords-section">
              <div class="section-header">
                <h3>🏷️ 关键词提取</h3>
                <button class="generate-btn" id="generate-keywords-btn">提取关键词</button>
              </div>
              <div class="section-content" id="keywords-content">
                <p class="loading-text">点击生成按钮开始...</p>
              </div>
            </div>

            <!-- 主题识别 -->
            <div class="analysis-section" id="topic-section">
              <div class="section-header">
                <h3>🎯 主题识别</h3>
                <button class="generate-btn" id="generate-topic-btn">识别主题</button>
              </div>
              <div class="section-content" id="topic-content">
                <p class="loading-text">点击生成按钮开始...</p>
              </div>
            </div>

            <!-- 相似内容推荐 -->
            <div class="analysis-section" id="similar-section">
              <div class="section-header">
                <h3>🔗 相似内容推荐</h3>
                <button class="generate-btn" id="generate-similar-btn">查找相似内容</button>
              </div>
              <div class="section-content" id="similar-content">
                <p class="loading-text">点击生成按钮开始...</p>
              </div>
            </div>
          </div>

          <!-- 空状态 -->
          <div id="empty-state" class="empty-state">
            <div class="empty-icon">🤖</div>
            <p class="empty-title">AI 内容智能助手</p>
            <p class="empty-desc">输入笔记路径并点击"分析"按钮开始</p>
            <div class="features-list">
              <div class="feature-item">
                <span class="feature-icon">📄</span>
                <span>自动生成摘要</span>
              </div>
              <div class="feature-item">
                <span class="feature-icon">🏷️</span>
                <span>智能提取关键词</span>
              </div>
              <div class="feature-item">
                <span class="feature-icon">🎯</span>
                <span>识别内容主题</span>
              </div>
              <div class="feature-item">
                <span class="feature-icon">🔗</span>
                <span>推荐相似内容</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    `;

    this.bindEvents();
  }

  bindEvents() {
    // 分析按钮
    const analyzeBtn = document.getElementById('analyze-note-btn');
    if (analyzeBtn) {
      analyzeBtn.addEventListener('click', () => {
        const input = document.getElementById('note-path-input');
        const notePath = input.value.trim();

        if (!notePath) {
          notifications.warning('请输入笔记路径');
          return;
        }

        this.selectedNotePath = notePath;
        this.showAnalysisResults();
      });
    }

    // 生成摘要
    const summaryBtn = document.getElementById('generate-summary-btn');
    if (summaryBtn) {
      summaryBtn.addEventListener('click', () => {
        this.generateSummary();
      });
    }

    // 提取关键词
    const keywordsBtn = document.getElementById('generate-keywords-btn');
    if (keywordsBtn) {
      keywordsBtn.addEventListener('click', () => {
        this.extractKeywords();
      });
    }

    // 识别主题
    const topicBtn = document.getElementById('generate-topic-btn');
    if (topicBtn) {
      topicBtn.addEventListener('click', () => {
        this.identifyTopic();
      });
    }

    // 查找相似内容
    const similarBtn = document.getElementById('generate-similar-btn');
    if (similarBtn) {
      similarBtn.addEventListener('click', () => {
        this.findSimilarContent();
      });
    }
  }

  showAnalysisResults() {
    document.getElementById('empty-state').style.display = 'none';
    document.getElementById('analysis-results').style.display = 'block';
    notifications.success(`已选择笔记: ${this.selectedNotePath}`);
  }

  async generateSummary() {
    const vaultId = this.getCurrentVaultId();
    if (!vaultId) {
      notifications.warning('请先选择一个 Vault');
      return;
    }

    const content = document.getElementById('summary-content');
    content.innerHTML = '<div class="loading-spinner"></div><p class="loading-text">正在生成摘要...</p>';

    try {
      const summary = await ContentIntelligenceApi.generateNoteSummary(
        vaultId,
        this.selectedNotePath,
        5
      );

      content.innerHTML = `
        <div class="summary-result">
          <p>${summary.summary || summary}</p>
          <div class="summary-meta">
            <span>📊 原文长度: ${summary.original_length || 'N/A'} 字</span>
            <span>📏 摘要长度: ${summary.summary_length || 'N/A'} 字</span>
          </div>
        </div>
      `;

      notifications.success('摘要生成成功');
    } catch (error) {
      content.innerHTML = `<p class="error-text">❌ 生成失败: ${error.message}</p>`;
      notifications.error(`生成失败: ${error.message}`);
    }
  }

  async extractKeywords() {
    const vaultId = this.getCurrentVaultId();
    if (!vaultId) {
      notifications.warning('请先选择一个 Vault');
      return;
    }

    const content = document.getElementById('keywords-content');
    content.innerHTML = '<div class="loading-spinner"></div><p class="loading-text">正在提取关键词...</p>';

    try {
      const result = await ContentIntelligenceApi.extractKeywords(
        vaultId,
        this.selectedNotePath,
        10
      );

      const keywords = result.keywords || result;

      content.innerHTML = `
        <div class="keywords-result">
          <div class="keywords-tags">
            ${keywords.map((kw, index) => `
              <span class="keyword-tag" style="opacity: ${1 - index * 0.08}">
                ${typeof kw === 'string' ? kw : kw.word}
                ${kw.score ? `<span class="keyword-score">${(kw.score * 100).toFixed(0)}%</span>` : ''}
              </span>
            `).join('')}
          </div>
        </div>
      `;

      notifications.success('关键词提取成功');
    } catch (error) {
      content.innerHTML = `<p class="error-text">❌ 提取失败: ${error.message}</p>`;
      notifications.error(`提取失败: ${error.message}`);
    }
  }

  async identifyTopic() {
    const vaultId = this.getCurrentVaultId();
    if (!vaultId) {
      notifications.warning('请先选择一个 Vault');
      return;
    }

    const content = document.getElementById('topic-content');
    content.innerHTML = '<div class="loading-spinner"></div><p class="loading-text">正在识别主题...</p>';

    try {
      const result = await ContentIntelligenceApi.identifyNoteTopic(
        vaultId,
        this.selectedNotePath
      );

      const topic = result.topic || result.primary_topic || result;
      const confidence = result.confidence || 0.85;
      const relatedTopics = result.related_topics || [];

      content.innerHTML = `
        <div class="topic-result">
          <div class="primary-topic">
            <span class="topic-label">主要主题:</span>
            <span class="topic-value">${topic}</span>
            <span class="topic-confidence">${(confidence * 100).toFixed(0)}% 置信度</span>
          </div>
          ${relatedTopics.length > 0 ? `
            <div class="related-topics">
              <div class="related-label">相关主题:</div>
              <div class="related-tags">
                ${relatedTopics.map(t => `<span class="related-tag">${t}</span>`).join('')}
              </div>
            </div>
          ` : ''}
        </div>
      `;

      notifications.success('主题识别成功');
    } catch (error) {
      content.innerHTML = `<p class="error-text">❌ 识别失败: ${error.message}</p>`;
      notifications.error(`识别失败: ${error.message}`);
    }
  }

  async findSimilarContent() {
    const vaultId = this.getCurrentVaultId();
    if (!vaultId) {
      notifications.warning('请先选择一个 Vault');
      return;
    }

    const content = document.getElementById('similar-content');
    content.innerHTML = '<div class="loading-spinner"></div><p class="loading-text">正在查找相似内容...</p>';

    try {
      const recommendations = await ContentIntelligenceApi.recommendSimilarContent(
        vaultId,
        this.selectedNotePath,
        10
      );

      if (!recommendations || recommendations.length === 0) {
        content.innerHTML = '<p class="empty-text">未找到相似内容</p>';
        return;
      }

      content.innerHTML = `
        <div class="similar-result">
          <div class="similar-list">
            ${recommendations.map((rec, index) => `
              <div class="similar-item">
                <div class="similar-rank">#${index + 1}</div>
                <div class="similar-info">
                  <div class="similar-title">${rec.title}</div>
                  <div class="similar-meta">
                    <span class="similar-similarity">
                      相似度: ${((rec.similarity || rec.score || 0.8) * 100).toFixed(0)}%
                    </span>
                    <span class="similar-path">${rec.note_path || rec.notePath || rec.path}</span>
                  </div>
                </div>
              </div>
            `).join('')}
          </div>
        </div>
      `;

      notifications.success('相似内容查找成功');
    } catch (error) {
      content.innerHTML = `<p class="error-text">❌ 查找失败: ${error.message}</p>`;
      notifications.error(`查找失败: ${error.message}`);
    }
  }

  getCurrentVaultId() {
    return 'demo'; // TODO: 从全局状态获取
  }

  destroy() {
    if (this.container) {
      this.container.innerHTML = '';
    }
  }
}

// 添加样式
const style = document.createElement('style');
style.textContent = `
  .ai-intelligence-dashboard {
    padding: 20px;
  }

  .note-selector-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 24px;
  }

  .note-selector-section h3 {
    margin: 0 0 16px 0;
  }

  .note-selector {
    display: flex;
    gap: 12px;
  }

  #note-path-input {
    flex: 1;
    padding: 12px;
    border: 1px solid #d1d5db;
    border-radius: 6px;
    font-size: 14px;
  }

  .analyze-btn {
    padding: 12px 24px;
    background: #3498db;
    color: white;
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font-size: 14px;
    font-weight: 600;
  }

  .analyze-btn:hover {
    background: #2980b9;
  }

  .analysis-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 16px;
  }

  .section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
  }

  .section-header h3 {
    margin: 0;
    font-size: 18px;
  }

  .generate-btn {
    padding: 8px 16px;
    background: #2ecc71;
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
  }

  .generate-btn:hover {
    background: #27ae60;
  }

  .loading-spinner {
    width: 40px;
    height: 40px;
    border: 4px solid #f3f3f3;
    border-top: 4px solid #3498db;
    border-radius: 50%;
    animation: spin 1s linear infinite;
    margin: 20px auto;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .loading-text {
    text-align: center;
    color: #7f8c8d;
  }

  .error-text {
    color: #e74c3c;
    text-align: center;
    padding: 20px;
  }

  .summary-result p {
    line-height: 1.8;
    color: #2c3e50;
    margin-bottom: 16px;
  }

  .summary-meta {
    display: flex;
    gap: 20px;
    padding-top: 12px;
    border-top: 1px solid #ecf0f1;
    font-size: 13px;
    color: #7f8c8d;
  }

  .keywords-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }

  .keyword-tag {
    padding: 8px 16px;
    background: #3498db;
    color: white;
    border-radius: 20px;
    font-size: 14px;
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }

  .keyword-score {
    font-size: 12px;
    opacity: 0.8;
  }

  .primary-topic {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 16px;
    background: #e8f5e9;
    border-radius: 6px;
    margin-bottom: 16px;
  }

  .topic-label {
    font-weight: 600;
  }

  .topic-value {
    font-size: 18px;
    color: #2ecc71;
    font-weight: bold;
  }

  .topic-confidence {
    font-size: 13px;
    color: #7f8c8d;
  }

  .related-label {
    font-weight: 600;
    margin-bottom: 8px;
  }

  .related-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .related-tag {
    padding: 6px 12px;
    background: #f8f9fa;
    border: 1px solid #dee2e6;
    border-radius: 4px;
    font-size: 13px;
  }

  .similar-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .similar-item {
    display: flex;
    align-items: center;
    padding: 12px;
    background: #f8f9fa;
    border-radius: 6px;
  }

  .similar-rank {
    width: 40px;
    font-size: 18px;
    font-weight: bold;
    color: #7f8c8d;
  }

  .similar-info {
    flex: 1;
  }

  .similar-title {
    font-size: 16px;
    font-weight: 500;
    margin-bottom: 4px;
  }

  .similar-meta {
    display: flex;
    gap: 16px;
    font-size: 13px;
    color: #7f8c8d;
  }

  .similar-similarity {
    color: #2ecc71;
    font-weight: 600;
  }

  .empty-state {
    background: white;
    padding: 60px 40px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    text-align: center;
  }

  .empty-icon {
    font-size: 64px;
    margin-bottom: 16px;
  }

  .empty-title {
    font-size: 24px;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .empty-desc {
    color: #7f8c8d;
    margin-bottom: 32px;
  }

  .features-list {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
    max-width: 800px;
    margin: 0 auto;
  }

  .feature-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 16px;
    background: #f8f9fa;
    border-radius: 6px;
  }

  .feature-icon {
    font-size: 24px;
  }
`;
document.head.appendChild(style);
