/**
 * 内容价值仪表盘组件
 */

import { ContentValueApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class ContentValueDashboard {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.data = null;

    this.init();
  }

  /**
   * 初始化仪表盘
   */
  init() {
    if (!this.container) {
      console.error('容器元素不存在');
      return;
    }

    this.render();
  }

  /**
   * 加载数据
   */
  async loadData(vaultId) {
    try {
      const report = await dataLoader.loadData(
        'valueReport',
        () => ContentValueApi.getValueReport(vaultId),
        { forceRefresh: true }
      );

      this.data = report;
      this.render();
      notifications.success('价值报告加载成功');
    } catch (error) {
      console.error('加载价值报告失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  /**
   * 渲染仪表盘
   */
  render() {
    this.container.innerHTML = `
      <div class="value-dashboard">
        <div class="dashboard-header">
          <h2>📊 内容价值仪表盘</h2>
          <button class="refresh-btn" id="refresh-value-btn">刷新数据</button>
        </div>

        ${this.data ? this.renderContent() : this.renderEmpty()}
      </div>
    `;

    // 绑定事件
    this.bindEvents();
  }

  /**
   * 渲染内容
   */
  renderContent() {
    const { distribution, top_notes, average_score, total_notes } = this.data;

    return `
      <div class="dashboard-content">
        <!-- 统计卡片 -->
        <div class="stats-cards">
          <div class="stat-card">
            <div class="stat-label">总笔记数</div>
            <div class="stat-value">${total_notes || 0}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">平均分数</div>
            <div class="stat-value">${(average_score || 0).toFixed(1)}</div>
          </div>
          <div class="stat-card s-tier">
            <div class="stat-label">S 级笔记</div>
            <div class="stat-value">${distribution?.s || 0}</div>
          </div>
          <div class="stat-card a-tier">
            <div class="stat-label">A 级笔记</div>
            <div class="stat-value">${distribution?.a || 0}</div>
          </div>
        </div>

        <!-- 分布图 -->
        <div class="distribution-chart">
          <h3>价值分布</h3>
          ${this.renderDistributionChart(distribution)}
        </div>

        <!-- Top 笔记列表 -->
        <div class="top-notes">
          <h3>🏆 Top 10 高价值笔记</h3>
          ${this.renderTopNotes(top_notes)}
        </div>
      </div>
    `;
  }

  /**
   * 渲染分布图
   */
  renderDistributionChart(distribution) {
    if (!distribution) return '<p>暂无数据</p>';

    const total = Object.values(distribution).reduce((sum, count) => sum + count, 0);
    const tiers = [
      { name: 'S', count: distribution.s || 0, color: '#e74c3c' },
      { name: 'A', count: distribution.a || 0, color: '#f39c12' },
      { name: 'B', count: distribution.b || 0, color: '#3498db' },
      { name: 'C', count: distribution.c || 0, color: '#95a5a6' },
      { name: 'D', count: distribution.d || 0, color: '#7f8c8d' },
    ];

    return `
      <div class="chart-container">
        ${tiers.map(tier => {
          const percentage = total > 0 ? (tier.count / total * 100).toFixed(1) : 0;
          return `
            <div class="chart-bar">
              <div class="chart-label">${tier.name} 级</div>
              <div class="chart-bar-bg">
                <div class="chart-bar-fill" style="width: ${percentage}%; background: ${tier.color};">
                  <span class="chart-bar-text">${tier.count} (${percentage}%)</span>
                </div>
              </div>
            </div>
          `;
        }).join('')}
      </div>
    `;
  }

  /**
   * 渲染 Top 笔记
   */
  renderTopNotes(topNotes) {
    if (!topNotes || topNotes.length === 0) {
      return '<p>暂无数据</p>';
    }

    return `
      <div class="notes-list">
        ${topNotes.map((note, index) => `
          <div class="note-item" data-path="${note.note_path || note.notePath}">
            <div class="note-rank">#${index + 1}</div>
            <div class="note-info">
              <div class="note-title">${note.title}</div>
              <div class="note-meta">
                <span class="note-tier tier-${(note.value_tier || note.valueTier || 'd').toLowerCase()}">
                  ${(note.value_tier || note.valueTier || 'D').toUpperCase()}
                </span>
                <span class="note-score">分数: ${(note.total_score || note.totalScore || 0).toFixed(1)}</span>
              </div>
            </div>
          </div>
        `).join('')}
      </div>
    `;
  }

  /**
   * 渲染空状态
   */
  renderEmpty() {
    return `
      <div class="dashboard-empty">
        <p>暂无数据</p>
        <p>请选择一个 Vault 并加载数据</p>
      </div>
    `;
  }

  /**
   * 绑定事件
   */
  bindEvents() {
    // 刷新按钮
    const refreshBtn = document.getElementById('refresh-value-btn');
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

    // 笔记点击
    const noteItems = this.container.querySelectorAll('.note-item');
    noteItems.forEach(item => {
      item.addEventListener('click', () => {
        const path = item.dataset.path;
        this.onNoteClick(path);
      });
    });
  }

  /**
   * 笔记点击事件
   */
  onNoteClick(notePath) {
    console.log('打开笔记:', notePath);
    notifications.show(`打开笔记: ${notePath}`, 'info', 2000);
    // TODO: 触发打开笔记事件
  }

  /**
   * 获取当前 Vault ID
   */
  getCurrentVaultId() {
    // TODO: 从全局状态获取
    return null;
  }

  /**
   * 销毁组件
   */
  destroy() {
    if (this.container) {
      this.container.innerHTML = '';
    }
  }
}

/**
 * 添加样式
 */
const style = document.createElement('style');
style.textContent = `
  .value-dashboard {
    padding: 20px;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
  }

  .dashboard-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 24px;
  }

  .dashboard-header h2 {
    margin: 0;
    font-size: 24px;
    font-weight: 600;
  }

  .refresh-btn {
    padding: 8px 16px;
    background: #3498db;
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 14px;
  }

  .refresh-btn:hover {
    background: #2980b9;
  }

  .stats-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
    margin-bottom: 24px;
  }

  .stat-card {
    padding: 20px;
    background: white;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
  }

  .stat-card.s-tier { border-left: 4px solid #e74c3c; }
  .stat-card.a-tier { border-left: 4px solid #f39c12; }

  .stat-label {
    font-size: 14px;
    color: #7f8c8d;
    margin-bottom: 8px;
  }

  .stat-value {
    font-size: 32px;
    font-weight: bold;
    color: #2c3e50;
  }

  .distribution-chart, .top-notes {
    background: white;
    padding: 20px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 24px;
  }

  .distribution-chart h3, .top-notes h3 {
    margin: 0 0 16px 0;
    font-size: 18px;
  }

  .chart-bar {
    display: flex;
    align-items: center;
    margin-bottom: 12px;
  }

  .chart-label {
    width: 60px;
    font-weight: 600;
  }

  .chart-bar-bg {
    flex: 1;
    height: 32px;
    background: #ecf0f1;
    border-radius: 4px;
    overflow: hidden;
  }

  .chart-bar-fill {
    height: 100%;
    display: flex;
    align-items: center;
    padding: 0 8px;
    transition: width 0.3s ease;
  }

  .chart-bar-text {
    color: white;
    font-size: 12px;
    font-weight: 600;
  }

  .notes-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .note-item {
    display: flex;
    align-items: center;
    padding: 12px;
    background: #f8f9fa;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.2s;
  }

  .note-item:hover {
    background: #e9ecef;
  }

  .note-rank {
    width: 40px;
    font-size: 18px;
    font-weight: bold;
    color: #7f8c8d;
  }

  .note-info {
    flex: 1;
  }

  .note-title {
    font-size: 16px;
    font-weight: 500;
    margin-bottom: 4px;
  }

  .note-meta {
    display: flex;
    gap: 12px;
    font-size: 14px;
  }

  .note-tier {
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 600;
    font-size: 12px;
  }

  .tier-s { background: #e74c3c; color: white; }
  .tier-a { background: #f39c12; color: white; }
  .tier-b { background: #3498db; color: white; }
  .tier-c { background: #95a5a6; color: white; }
  .tier-d { background: #7f8c8d; color: white; }

  .note-score {
    color: #7f8c8d;
  }

  .dashboard-empty {
    padding: 60px;
    text-align: center;
    color: #7f8c8d;
  }
`;
document.head.appendChild(style);
