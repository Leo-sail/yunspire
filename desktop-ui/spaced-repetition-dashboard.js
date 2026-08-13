/**
 * 间隔重复学习系统组件
 */

import { SpacedRepetitionApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class SpacedRepetitionDashboard {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.reviewPlan = null;
    this.dueNotes = null;

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
      const now = new Date().toISOString();

      // 加载复习计划摘要
      const plan = await dataLoader.loadData(
        'reviewPlan',
        () => SpacedRepetitionApi.getReviewPlanSummary(vaultId, now),
        { forceRefresh: true }
      );

      this.reviewPlan = plan;

      // 加载待复习笔记
      const due = await SpacedRepetitionApi.getDueForReview(vaultId, now, 20);
      this.dueNotes = due;

      this.render();
      notifications.success('复习数据加载成功');
    } catch (error) {
      console.error('加载复习数据失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  render() {
    this.container.innerHTML = `
      <div class="spaced-repetition-dashboard">
        <div class="dashboard-header">
          <h2>📅 间隔重复学习系统</h2>
          <button class="refresh-btn" id="refresh-review-btn">刷新数据</button>
        </div>

        ${this.reviewPlan ? this.renderContent() : this.renderEmpty()}
      </div>
    `;

    this.bindEvents();
  }

  renderContent() {
    return `
      <div class="review-content">
        <!-- 复习计划摘要 -->
        ${this.renderPlanSummary()}

        <!-- 今日待复习列表 -->
        ${this.renderDueNotesList()}

        <!-- 记忆强度分布 -->
        ${this.renderMemoryStrengthDistribution()}

        <!-- 复习质量说明 -->
        ${this.renderQualityGuide()}
      </div>
    `;
  }

  renderPlanSummary() {
    const {
      due_today,
      due_this_week,
      total_tracked_notes,
      average_memory_strength,
    } = this.reviewPlan;

    return `
      <div class="plan-summary">
        <div class="summary-cards">
          <div class="summary-card urgent">
            <div class="card-icon">🔥</div>
            <div class="card-info">
              <div class="card-value">${due_today || dueToday || 0}</div>
              <div class="card-label">今日待复习</div>
            </div>
          </div>
          <div class="summary-card">
            <div class="card-icon">📆</div>
            <div class="card-info">
              <div class="card-value">${due_this_week || dueThisWeek || 0}</div>
              <div class="card-label">本周待复习</div>
            </div>
          </div>
          <div class="summary-card">
            <div class="card-icon">📚</div>
            <div class="card-info">
              <div class="card-value">${total_tracked_notes || totalTrackedNotes || 0}</div>
              <div class="card-label">跟踪笔记数</div>
            </div>
          </div>
          <div class="summary-card">
            <div class="card-icon">💪</div>
            <div class="card-info">
              <div class="card-value">${((average_memory_strength || averageMemoryStrength || 0) * 100).toFixed(0)}%</div>
              <div class="card-label">平均记忆强度</div>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  renderDueNotesList() {
    if (!this.dueNotes || this.dueNotes.length === 0) {
      return `
        <div class="due-notes-section">
          <h3>✅ 今日待复习笔记</h3>
          <p class="empty-message">太棒了！今天没有需要复习的笔记。</p>
        </div>
      `;
    }

    return `
      <div class="due-notes-section">
        <h3>📝 今日待复习笔记 (${this.dueNotes.length})</h3>
        <div class="due-notes-list">
          ${this.dueNotes.map(note => `
            <div class="due-note-item" data-path="${note.note_path || note.notePath}">
              <div class="note-main">
                <div class="note-title">${note.title}</div>
                <div class="note-meta">
                  <span class="note-review-count">
                    已复习 ${note.review_count || note.reviewCount || 0} 次
                  </span>
                  <span class="note-interval">
                    间隔 ${this.formatInterval(note.interval_days || note.intervalDays || 1)} 天
                  </span>
                  <span class="note-strength">
                    记忆强度: ${((note.memory_strength || note.memoryStrength || 0) * 100).toFixed(0)}%
                  </span>
                </div>
              </div>
              <div class="note-actions">
                <button class="quality-btn" data-quality="5" title="完美回忆">😄</button>
                <button class="quality-btn" data-quality="4" title="正确但犹豫">🙂</button>
                <button class="quality-btn" data-quality="3" title="困难但回忆">😐</button>
                <button class="quality-btn" data-quality="2" title="错误但似曾相识">😕</button>
                <button class="quality-btn" data-quality="1" title="完全不记得">😞</button>
              </div>
            </div>
          `).join('')}
        </div>
      </div>
    `;
  }

  renderMemoryStrengthDistribution() {
    if (!this.reviewPlan || !this.reviewPlan.strength_distribution) {
      return '';
    }

    const dist = this.reviewPlan.strength_distribution || this.reviewPlan.strengthDistribution;
    const categories = [
      { name: '非常弱 (0-20%)', key: 'very_weak', color: '#e74c3c', min: 0, max: 20 },
      { name: '弱 (20-40%)', key: 'weak', color: '#f39c12', min: 20, max: 40 },
      { name: '中等 (40-60%)', key: 'medium', color: '#3498db', min: 40, max: 60 },
      { name: '强 (60-80%)', key: 'strong', color: '#2ecc71', min: 60, max: 80 },
      { name: '非常强 (80-100%)', key: 'very_strong', color: '#27ae60', min: 80, max: 100 },
    ];

    const total = Object.values(dist).reduce((sum, val) => sum + val, 0);

    return `
      <div class="strength-distribution-section">
        <h3>💪 记忆强度分布</h3>
        <div class="strength-chart">
          ${categories.map(cat => {
            const count = dist[cat.key] || 0;
            const percentage = total > 0 ? (count / total * 100).toFixed(1) : 0;
            return `
              <div class="strength-bar">
                <div class="strength-label">${cat.name}</div>
                <div class="strength-bar-bg">
                  <div class="strength-bar-fill" style="width: ${percentage}%; background: ${cat.color};">
                    <span class="strength-bar-text">${count} (${percentage}%)</span>
                  </div>
                </div>
              </div>
            `;
          }).join('')}
        </div>
      </div>
    `;
  }

  renderQualityGuide() {
    return `
      <div class="quality-guide-section">
        <h3>📖 复习质量评分指南</h3>
        <div class="quality-guide">
          <div class="quality-item">
            <span class="quality-emoji">😄</span>
            <div class="quality-info">
              <div class="quality-name">5 - 完美回忆</div>
              <div class="quality-desc">轻松回忆，答案立即浮现</div>
            </div>
          </div>
          <div class="quality-item">
            <span class="quality-emoji">🙂</span>
            <div class="quality-info">
              <div class="quality-name">4 - 正确但犹豫</div>
              <div class="quality-desc">回忆正确但需要一些思考时间</div>
            </div>
          </div>
          <div class="quality-item">
            <span class="quality-emoji">😐</span>
            <div class="quality-info">
              <div class="quality-name">3 - 困难但回忆</div>
              <div class="quality-desc">经过努力终于想起来</div>
            </div>
          </div>
          <div class="quality-item">
            <span class="quality-emoji">😕</span>
            <div class="quality-info">
              <div class="quality-name">2 - 错误但似曾相识</div>
              <div class="quality-desc">答错了但感觉见过</div>
            </div>
          </div>
          <div class="quality-item">
            <span class="quality-emoji">😞</span>
            <div class="quality-info">
              <div class="quality-name">1 - 完全不记得</div>
              <div class="quality-desc">完全没有印象</div>
            </div>
          </div>
        </div>
        <div class="ebbinghaus-note">
          <strong>💡 基于 Ebbinghaus 遗忘曲线：</strong>
          间隔会根据你的复习质量自动调整。质量越高，下次复习间隔越长。
        </div>
      </div>
    `;
  }

  renderEmpty() {
    return `
      <div class="review-empty">
        <p>暂无复习数据</p>
        <p>请选择一个 Vault 并加载数据</p>
      </div>
    `;
  }

  formatInterval(days) {
    if (days < 1) return '< 1';
    if (days === 1) return '1';
    if (days < 7) return days.toFixed(0);
    if (days < 30) return `${(days / 7).toFixed(1)} 周`;
    return `${(days / 30).toFixed(1)} 月`;
  }

  bindEvents() {
    // 刷新按钮
    const refreshBtn = document.getElementById('refresh-review-btn');
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

    // 复习质量按钮
    const qualityBtns = this.container.querySelectorAll('.quality-btn');
    qualityBtns.forEach(btn => {
      btn.addEventListener('click', async (e) => {
        const quality = parseInt(btn.dataset.quality);
        const noteItem = btn.closest('.due-note-item');
        const notePath = noteItem.dataset.path;
        const vaultId = this.getCurrentVaultId();

        if (vaultId) {
          await this.recordReview(vaultId, notePath, quality);
        }
      });
    });

    // 笔记点击
    const noteItems = this.container.querySelectorAll('.due-note-item .note-main');
    noteItems.forEach(item => {
      item.addEventListener('click', () => {
        const noteItem = item.closest('.due-note-item');
        const notePath = noteItem.dataset.path;
        this.onNoteClick(notePath);
      });
    });
  }

  async recordReview(vaultId, notePath, quality) {
    try {
      await SpacedRepetitionApi.recordNoteReview(vaultId, notePath, quality);
      notifications.success('复习记录已保存');

      // 重新加载数据
      setTimeout(() => {
        this.loadData(vaultId);
      }, 500);
    } catch (error) {
      notifications.error(`记录失败: ${error.message}`);
    }
  }

  onNoteClick(notePath) {
    console.log('打开笔记:', notePath);
    notifications.show(`打开笔记: ${notePath}`, 'info', 2000);
    // TODO: 触发打开笔记事件
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
  .spaced-repetition-dashboard {
    padding: 20px;
  }

  .plan-summary {
    margin-bottom: 24px;
  }

  .summary-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 16px;
  }

  .summary-card {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .summary-card.urgent {
    border-left: 4px solid #e74c3c;
  }

  .card-icon {
    font-size: 36px;
  }

  .card-value {
    font-size: 32px;
    font-weight: bold;
    color: #2c3e50;
  }

  .card-label {
    font-size: 14px;
    color: #7f8c8d;
  }

  .due-notes-section, .strength-distribution-section, .quality-guide-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 24px;
  }

  .due-notes-section h3, .strength-distribution-section h3, .quality-guide-section h3 {
    margin: 0 0 16px 0;
    font-size: 18px;
  }

  .due-notes-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .due-note-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 16px;
    background: #f8f9fa;
    border-radius: 6px;
    border-left: 4px solid #3498db;
  }

  .note-main {
    flex: 1;
    cursor: pointer;
  }

  .note-title {
    font-size: 16px;
    font-weight: 500;
    margin-bottom: 8px;
  }

  .note-meta {
    display: flex;
    gap: 16px;
    font-size: 13px;
    color: #7f8c8d;
  }

  .note-actions {
    display: flex;
    gap: 8px;
  }

  .quality-btn {
    padding: 8px 12px;
    font-size: 20px;
    border: 1px solid #ddd;
    background: white;
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.2s;
  }

  .quality-btn:hover {
    transform: scale(1.1);
    box-shadow: 0 2px 8px rgba(0,0,0,0.15);
  }

  .strength-chart {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .strength-bar {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .strength-label {
    width: 140px;
    font-size: 14px;
    font-weight: 500;
  }

  .strength-bar-bg {
    flex: 1;
    height: 32px;
    background: #ecf0f1;
    border-radius: 4px;
    overflow: hidden;
  }

  .strength-bar-fill {
    height: 100%;
    display: flex;
    align-items: center;
    padding: 0 8px;
    transition: width 0.3s ease;
  }

  .strength-bar-text {
    color: white;
    font-size: 12px;
    font-weight: 600;
    text-shadow: 0 1px 2px rgba(0,0,0,0.2);
  }

  .quality-guide {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 16px;
  }

  .quality-item {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px;
    background: #f8f9fa;
    border-radius: 6px;
  }

  .quality-emoji {
    font-size: 32px;
  }

  .quality-name {
    font-weight: 600;
    margin-bottom: 4px;
  }

  .quality-desc {
    font-size: 14px;
    color: #7f8c8d;
  }

  .ebbinghaus-note {
    padding: 16px;
    background: #e8f5e9;
    border-left: 4px solid #2ecc71;
    border-radius: 4px;
    font-size: 14px;
  }

  .empty-message {
    text-align: center;
    padding: 40px;
    color: #7f8c8d;
  }
`;
document.head.appendChild(style);
