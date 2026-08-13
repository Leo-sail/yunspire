/**
 * 使用指标仪表盘组件
 */

import { MetricsApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class MetricsDashboard {
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

  async loadData() {
    try {
      const report = await dataLoader.loadData(
        'metrics',
        () => MetricsApi.getMetricsReport(),
        { forceRefresh: true }
      );

      this.data = report;
      this.render();
      notifications.success('使用指标加载成功');
    } catch (error) {
      console.error('加载使用指标失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  render() {
    this.container.innerHTML = `
      <div class="metrics-dashboard">
        <div class="dashboard-header">
          <h2>📈 使用统计仪表盘</h2>
          <button class="refresh-btn" id="refresh-metrics-btn">刷新数据</button>
        </div>

        ${this.data ? this.renderContent() : this.renderEmpty()}
      </div>
    `;

    this.bindEvents();
  }

  renderContent() {
    return `
      <div class="metrics-content">
        <!-- 关键指标概览 -->
        ${this.renderKeyMetrics()}

        <!-- 活动趋势 -->
        ${this.renderActivityTrend()}

        <!-- 功能使用统计 -->
        ${this.renderFeatureUsage()}

        <!-- 使用习惯分析 -->
        ${this.renderUsagePatterns()}
      </div>
    `;
  }

  renderKeyMetrics() {
    const {
      total_sessions,
      total_captures,
      total_notes_created,
      total_ai_requests,
      avg_session_duration_minutes,
      most_used_feature,
      peak_usage_hour,
      active_days_count,
    } = this.data;

    return `
      <div class="key-metrics-section">
        <h3>📊 关键指标概览</h3>
        <div class="metrics-grid">
          <div class="metric-card">
            <div class="metric-icon">🚀</div>
            <div class="metric-info">
              <div class="metric-value">${total_sessions || totalSessions || 0}</div>
              <div class="metric-label">总会话数</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">📸</div>
            <div class="metric-info">
              <div class="metric-value">${total_captures || totalCaptures || 0}</div>
              <div class="metric-label">采集次数</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">📝</div>
            <div class="metric-info">
              <div class="metric-value">${total_notes_created || totalNotesCreated || 0}</div>
              <div class="metric-label">创建笔记数</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">🤖</div>
            <div class="metric-info">
              <div class="metric-value">${total_ai_requests || totalAiRequests || 0}</div>
              <div class="metric-label">AI 请求数</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">⏱️</div>
            <div class="metric-info">
              <div class="metric-value">${(avg_session_duration_minutes || avgSessionDurationMinutes || 0).toFixed(1)}min</div>
              <div class="metric-label">平均会话时长</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">⭐</div>
            <div class="metric-info">
              <div class="metric-value">${most_used_feature || mostUsedFeature || 'N/A'}</div>
              <div class="metric-label">最常用功能</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">🕐</div>
            <div class="metric-info">
              <div class="metric-value">${peak_usage_hour || peakUsageHour || 0}:00</div>
              <div class="metric-label">使用高峰时段</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">📅</div>
            <div class="metric-info">
              <div class="metric-value">${active_days_count || activeDaysCount || 0}</div>
              <div class="metric-label">活跃天数</div>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  renderActivityTrend() {
    const trend = this.data.activity_trend || this.data.activityTrend || [];

    if (trend.length === 0) {
      return '';
    }

    // 简化的活动趋势可视化
    const maxValue = Math.max(...trend.map(t => t.count || 0), 1);

    return `
      <div class="activity-trend-section">
        <h3>📈 活动趋势 (最近 ${trend.length} 天)</h3>
        <div class="trend-chart">
          ${trend.map(day => {
            const height = ((day.count || 0) / maxValue * 100).toFixed(1);
            return `
              <div class="trend-bar-container" title="${day.date}: ${day.count} 次活动">
                <div class="trend-bar" style="height: ${height}%"></div>
                <div class="trend-label">${this.formatDate(day.date)}</div>
              </div>
            `;
          }).join('')}
        </div>
      </div>
    `;
  }

  renderFeatureUsage() {
    const features = this.data.feature_usage || this.data.featureUsage || [];

    if (features.length === 0) {
      return '';
    }

    const total = features.reduce((sum, f) => sum + (f.count || 0), 0);

    return `
      <div class="feature-usage-section">
        <h3>🎯 功能使用统计</h3>
        <div class="feature-list">
          ${features.map(feature => {
            const percentage = total > 0 ? ((feature.count || 0) / total * 100).toFixed(1) : 0;
            return `
              <div class="feature-item">
                <div class="feature-name">${feature.feature || feature.name}</div>
                <div class="feature-bar-container">
                  <div class="feature-bar" style="width: ${percentage}%">
                    <span class="feature-bar-text">${feature.count} (${percentage}%)</span>
                  </div>
                </div>
              </div>
            `;
          }).join('')}
        </div>
      </div>
    `;
  }

  renderUsagePatterns() {
    const patterns = this.data.usage_patterns || this.data.usagePatterns || {};

    return `
      <div class="usage-patterns-section">
        <h3>🔍 使用习惯分析</h3>
        <div class="patterns-grid">
          <div class="pattern-card">
            <div class="pattern-title">时间偏好</div>
            <div class="pattern-content">
              <div class="pattern-item">
                <span class="pattern-label">最活跃时段:</span>
                <span class="pattern-value">${patterns.peak_hours || patterns.peakHours || '9:00-12:00'}</span>
              </div>
              <div class="pattern-item">
                <span class="pattern-label">平均日活跃度:</span>
                <span class="pattern-value">${(patterns.daily_activity_rate || patterns.dailyActivityRate || 0.7 * 100).toFixed(0)}%</span>
              </div>
            </div>
          </div>

          <div class="pattern-card">
            <div class="pattern-title">工作习惯</div>
            <div class="pattern-content">
              <div class="pattern-item">
                <span class="pattern-label">平均会话间隔:</span>
                <span class="pattern-value">${patterns.avg_session_gap_hours || patterns.avgSessionGapHours || 3.5} 小时</span>
              </div>
              <div class="pattern-item">
                <span class="pattern-label">连续工作天数:</span>
                <span class="pattern-value">${patterns.consecutive_days || patterns.consecutiveDays || 5} 天</span>
              </div>
            </div>
          </div>

          <div class="pattern-card">
            <div class="pattern-title">内容偏好</div>
            <div class="pattern-content">
              <div class="pattern-item">
                <span class="pattern-label">偏好功能:</span>
                <span class="pattern-value">${patterns.preferred_features || patterns.preferredFeatures || '采集, 创作'}</span>
              </div>
              <div class="pattern-item">
                <span class="pattern-label">AI 使用率:</span>
                <span class="pattern-value">${(patterns.ai_usage_rate || patterns.aiUsageRate || 0.45 * 100).toFixed(0)}%</span>
              </div>
            </div>
          </div>

          <div class="pattern-card">
            <div class="pattern-title">效率指标</div>
            <div class="pattern-content">
              <div class="pattern-item">
                <span class="pattern-label">平均每次创作:</span>
                <span class="pattern-value">${patterns.avg_notes_per_session || patterns.avgNotesPerSession || 2.3} 篇</span>
              </div>
              <div class="pattern-item">
                <span class="pattern-label">完成率:</span>
                <span class="pattern-value">${(patterns.completion_rate || patterns.completionRate || 0.82 * 100).toFixed(0)}%</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  renderEmpty() {
    return `
      <div class="metrics-empty">
        <p>暂无使用统计数据</p>
        <p>开始使用云枢后将自动记录数据</p>
      </div>
    `;
  }

  formatDate(dateStr) {
    if (!dateStr) return '';
    try {
      const date = new Date(dateStr);
      return `${date.getMonth() + 1}/${date.getDate()}`;
    } catch {
      return dateStr;
    }
  }

  bindEvents() {
    const refreshBtn = document.getElementById('refresh-metrics-btn');
    if (refreshBtn) {
      refreshBtn.addEventListener('click', () => {
        this.loadData();
      });
    }
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
  .metrics-dashboard {
    padding: 20px;
  }

  .key-metrics-section, .activity-trend-section, .feature-usage-section, .usage-patterns-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 24px;
  }

  .key-metrics-section h3, .activity-trend-section h3, .feature-usage-section h3, .usage-patterns-section h3 {
    margin: 0 0 20px 0;
    font-size: 18px;
  }

  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
  }

  .metric-card {
    padding: 20px;
    background: #f8f9fa;
    border-radius: 8px;
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .metric-icon {
    font-size: 32px;
  }

  .metric-value {
    font-size: 28px;
    font-weight: bold;
    color: #2c3e50;
  }

  .metric-label {
    font-size: 13px;
    color: #7f8c8d;
  }

  .trend-chart {
    display: flex;
    align-items: flex-end;
    gap: 4px;
    height: 200px;
    padding: 20px 0;
  }

  .trend-bar-container {
    flex: 1;
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    align-items: center;
  }

  .trend-bar {
    width: 100%;
    background: linear-gradient(to top, #3498db, #5dade2);
    border-radius: 4px 4px 0 0;
    transition: height 0.3s ease;
    min-height: 2px;
  }

  .trend-bar-container:hover .trend-bar {
    background: linear-gradient(to top, #2980b9, #3498db);
  }

  .trend-label {
    font-size: 10px;
    color: #7f8c8d;
    margin-top: 8px;
  }

  .feature-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .feature-item {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .feature-name {
    font-weight: 600;
    font-size: 14px;
  }

  .feature-bar-container {
    height: 32px;
    background: #ecf0f1;
    border-radius: 4px;
    overflow: hidden;
  }

  .feature-bar {
    height: 100%;
    background: #3498db;
    display: flex;
    align-items: center;
    padding: 0 12px;
    transition: width 0.3s ease;
  }

  .feature-bar-text {
    color: white;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
  }

  .patterns-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 16px;
  }

  .pattern-card {
    padding: 20px;
    background: #f8f9fa;
    border-radius: 8px;
  }

  .pattern-title {
    font-size: 16px;
    font-weight: 600;
    margin-bottom: 16px;
    color: #2c3e50;
  }

  .pattern-content {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .pattern-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 14px;
  }

  .pattern-label {
    color: #7f8c8d;
  }

  .pattern-value {
    font-weight: 600;
    color: #2c3e50;
  }

  .metrics-empty {
    background: white;
    padding: 60px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    text-align: center;
    color: #7f8c8d;
  }
`;
document.head.appendChild(style);
