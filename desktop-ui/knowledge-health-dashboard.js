/**
 * 知识健康度仪表盘组件
 */

import { KnowledgeHealthApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class KnowledgeHealthDashboard {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.data = null;

    this.init();
  }

  init() {
    if (!this.container) {
      console.error('容器元素不存在');
      return;
    }

    this.render();
  }

  async loadData(vaultId) {
    try {
      const dashboard = await dataLoader.loadData(
        'health',
        () => KnowledgeHealthApi.getHealthDashboard(vaultId),
        { forceRefresh: true }
      );

      this.data = dashboard;
      this.render();
      notifications.success('健康度数据加载成功');
    } catch (error) {
      console.error('加载健康度失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  render() {
    this.container.innerHTML = `
      <div class="health-dashboard">
        <div class="dashboard-header">
          <h2>🏥 知识健康度仪表盘</h2>
          <button class="refresh-btn" id="refresh-health-btn">刷新数据</button>
        </div>

        ${this.data ? this.renderContent() : this.renderEmpty()}
      </div>
    `;

    this.bindEvents();
  }

  renderContent() {
    const { stats, health_score, issues, suggestions } = this.data;

    return `
      <div class="health-content">
        <!-- 健康度评分 -->
        <div class="health-score-card">
          <div class="score-circle">
            <svg viewBox="0 0 200 200" width="200" height="200">
              <circle cx="100" cy="100" r="90" fill="none" stroke="#e0e0e0" stroke-width="20"/>
              <circle cx="100" cy="100" r="90" fill="none" stroke="${this.getScoreColor(health_score)}"
                      stroke-width="20" stroke-dasharray="${health_score * 5.65} 565"
                      transform="rotate(-90 100 100)" stroke-linecap="round"/>
              <text x="100" y="110" text-anchor="middle" font-size="48" font-weight="bold" fill="#2c3e50">
                ${health_score.toFixed(0)}
              </text>
              <text x="100" y="140" text-anchor="middle" font-size="16" fill="#7f8c8d">
                健康度
              </text>
            </svg>
          </div>
          <div class="score-label">${this.getScoreLabel(health_score)}</div>
        </div>

        <!-- 统计数据 -->
        <div class="stats-grid">
          <div class="stat-item">
            <div class="stat-icon">📝</div>
            <div class="stat-info">
              <div class="stat-value">${stats.total_notes || stats.totalNotes}</div>
              <div class="stat-label">总笔记数</div>
            </div>
          </div>
          <div class="stat-item">
            <div class="stat-icon">🔗</div>
            <div class="stat-info">
              <div class="stat-value">${stats.linked_notes || stats.linkedNotes}</div>
              <div class="stat-label">有链接笔记</div>
            </div>
          </div>
          <div class="stat-item warning">
            <div class="stat-icon">🏝️</div>
            <div class="stat-info">
              <div class="stat-value">${stats.orphan_notes || stats.orphanNotes}</div>
              <div class="stat-label">孤立笔记</div>
            </div>
          </div>
          <div class="stat-item warning">
            <div class="stat-icon">📄</div>
            <div class="stat-info">
              <div class="stat-value">${stats.stub_notes || stats.stubNotes}</div>
              <div class="stat-label">短笔记 (&lt;50字)</div>
            </div>
          </div>
          <div class="stat-item good">
            <div class="stat-icon">✨</div>
            <div class="stat-info">
              <div class="stat-value">${stats.rich_notes || stats.richNotes}</div>
              <div class="stat-label">富文本笔记</div>
            </div>
          </div>
          <div class="stat-item good">
            <div class="stat-icon">🏷️</div>
            <div class="stat-info">
              <div class="stat-value">${stats.tagged_notes || stats.taggedNotes}</div>
              <div class="stat-label">有标签笔记</div>
            </div>
          </div>
        </div>

        <!-- 问题列表 -->
        <div class="issues-section">
          <h3>⚠️ 发现的问题 (${issues.length})</h3>
          ${this.renderIssues(issues)}
        </div>

        <!-- 改进建议 -->
        <div class="suggestions-section">
          <h3>💡 改进建议 (${suggestions.length})</h3>
          ${this.renderSuggestions(suggestions)}
        </div>
      </div>
    `;
  }

  renderIssues(issues) {
    if (issues.length === 0) {
      return '<p class="empty-message">✅ 未发现问题，知识库状态良好！</p>';
    }

    return `
      <div class="issues-list">
        ${issues.map(issue => `
          <div class="issue-item severity-${issue.severity}">
            <div class="issue-header">
              <span class="issue-type">${this.getIssueTypeLabel(issue.issue_type || issue.issueType)}</span>
              <span class="issue-severity">${this.getSeverityLabel(issue.severity)}</span>
            </div>
            <div class="issue-description">${issue.description}</div>
            ${issue.auto_fix_available || issue.autoFixAvailable
              ? '<button class="fix-btn">自动修复</button>'
              : ''}
          </div>
        `).join('')}
      </div>
    `;
  }

  renderSuggestions(suggestions) {
    if (suggestions.length === 0) {
      return '<p class="empty-message">暂无建议</p>';
    }

    return `
      <div class="suggestions-list">
        ${suggestions.map(suggestion => `
          <div class="suggestion-item">
            <div class="suggestion-header">
              <span class="suggestion-title">${suggestion.title}</span>
              <span class="suggestion-effort effort-${suggestion.effort}">
                ${this.getEffortLabel(suggestion.effort)}
              </span>
            </div>
            <div class="suggestion-description">${suggestion.description}</div>
            <div class="suggestion-impact">
              <strong>预期影响:</strong> ${suggestion.impact}
            </div>
            <div class="suggestion-affected">
              影响笔记: ${suggestion.affected_count || suggestion.affectedCount} 个
            </div>
          </div>
        `).join('')}
      </div>
    `;
  }

  renderEmpty() {
    return `
      <div class="health-empty">
        <p>暂无健康度数据</p>
        <p>请选择一个 Vault 并加载数据</p>
      </div>
    `;
  }

  getScoreColor(score) {
    if (score >= 80) return '#2ecc71';
    if (score >= 60) return '#3498db';
    if (score >= 40) return '#f39c12';
    return '#e74c3c';
  }

  getScoreLabel(score) {
    if (score >= 80) return '优秀';
    if (score >= 60) return '良好';
    if (score >= 40) return '一般';
    return '需改进';
  }

  getIssueTypeLabel(type) {
    const labels = {
      orphan: '孤立笔记',
      duplicate: '重复内容',
      outdated: '过时标签',
      broken_link: '断链',
      short_content: '内容过短',
    };
    return labels[type] || type;
  }

  getSeverityLabel(severity) {
    const labels = {
      low: '低',
      medium: '中',
      high: '高',
    };
    return labels[severity] || severity;
  }

  getEffortLabel(effort) {
    const labels = {
      low: '低工作量',
      medium: '中工作量',
      high: '高工作量',
    };
    return labels[effort] || effort;
  }

  bindEvents() {
    const refreshBtn = document.getElementById('refresh-health-btn');
    if (refreshBtn) {
      refreshBtn.addEventListener('click', () => {
        const vaultId = this.getCurrentVaultId();
        if (vaultId) {
          this.loadData(vaultId);
        } else {
          notifications.warning('请先选择一个 Vault');
        }
      });
    }
  }

  getCurrentVaultId() {
    return null; // TODO: 从全局状态获取
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
  .health-dashboard {
    padding: 20px;
  }

  .dashboard-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 24px;
  }

  .health-content {
    display: grid;
    gap: 24px;
  }

  .health-score-card {
    background: white;
    padding: 32px;
    border-radius: 12px;
    box-shadow: 0 2px 8px rgba(0,0,0,0.1);
    text-align: center;
  }

  .score-circle {
    display: inline-block;
    margin-bottom: 16px;
  }

  .score-label {
    font-size: 24px;
    font-weight: 600;
    color: #2c3e50;
  }

  .stats-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
  }

  .stat-item {
    background: white;
    padding: 20px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .stat-item.warning { border-left: 4px solid #f39c12; }
  .stat-item.good { border-left: 4px solid #2ecc71; }

  .stat-icon {
    font-size: 32px;
  }

  .stat-value {
    font-size: 28px;
    font-weight: bold;
    color: #2c3e50;
  }

  .stat-label {
    font-size: 14px;
    color: #7f8c8d;
  }

  .issues-section, .suggestions-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
  }

  .issues-section h3, .suggestions-section h3 {
    margin: 0 0 16px 0;
    font-size: 18px;
  }

  .issues-list, .suggestions-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .issue-item {
    padding: 16px;
    border-radius: 6px;
    border-left: 4px solid;
  }

  .issue-item.severity-low { border-left-color: #3498db; background: #ebf5fb; }
  .issue-item.severity-medium { border-left-color: #f39c12; background: #fef5e7; }
  .issue-item.severity-high { border-left-color: #e74c3c; background: #fadbd8; }

  .issue-header {
    display: flex;
    justify-content: space-between;
    margin-bottom: 8px;
  }

  .issue-type {
    font-weight: 600;
  }

  .issue-severity {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
    font-weight: 600;
  }

  .suggestion-item {
    padding: 16px;
    background: #f8f9fa;
    border-radius: 6px;
  }

  .suggestion-header {
    display: flex;
    justify-content: space-between;
    margin-bottom: 8px;
  }

  .suggestion-title {
    font-weight: 600;
    font-size: 16px;
  }

  .suggestion-effort {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 12px;
    font-weight: 600;
  }

  .effort-low { background: #d5f4e6; color: #27ae60; }
  .effort-medium { background: #fef5e7; color: #f39c12; }
  .effort-high { background: #fadbd8; color: #e74c3c; }

  .suggestion-description {
    margin: 8px 0;
    color: #555;
  }

  .suggestion-impact {
    margin: 8px 0;
    font-size: 14px;
  }

  .suggestion-affected {
    font-size: 13px;
    color: #7f8c8d;
  }

  .empty-message {
    text-align: center;
    padding: 40px;
    color: #7f8c8d;
  }

  .fix-btn {
    margin-top: 8px;
    padding: 6px 12px;
    background: #3498db;
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
  }

  .fix-btn:hover {
    background: #2980b9;
  }
`;
document.head.appendChild(style);
