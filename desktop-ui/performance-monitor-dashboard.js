/**
 * 性能监控仪表盘组件
 */

import { PerformanceApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class PerformanceMonitorDashboard {
  constructor(containerId) {
    this.container = document.getElementById(containerId);
    this.data = null;
    this.autoRefreshInterval = null;

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
        'performance',
        () => PerformanceApi.getPerformanceReport(),
        { forceRefresh: true }
      );

      this.data = report;
      this.render();
    } catch (error) {
      console.error('加载性能报告失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  render() {
    this.container.innerHTML = `
      <div class="performance-dashboard">
        <div class="dashboard-header">
          <h2>📊 性能监控仪表盘</h2>
          <div class="header-actions">
            <label class="auto-refresh-toggle">
              <input type="checkbox" id="auto-refresh-checkbox" />
              自动刷新 (5s)
            </label>
            <button class="refresh-btn" id="refresh-perf-btn">刷新数据</button>
            <button class="clear-btn" id="clear-perf-btn">清空数据</button>
          </div>
        </div>

        ${this.data ? this.renderContent() : this.renderEmpty()}
      </div>
    `;

    this.bindEvents();
  }

  renderContent() {
    const {
      total_queries,
      slow_queries_count,
      avg_query_time_ms,
      p95_query_time_ms,
      p99_query_time_ms,
      top_slow_queries,
      query_performance,
    } = this.data;

    return `
      <div class="performance-content">
        <!-- 关键指标卡片 -->
        <div class="metrics-cards">
          <div class="metric-card">
            <div class="metric-icon">📈</div>
            <div class="metric-info">
              <div class="metric-value">${total_queries || totalQueries || 0}</div>
              <div class="metric-label">总查询数</div>
            </div>
          </div>
          <div class="metric-card ${slow_queries_count > 0 ? 'warning' : ''}">
            <div class="metric-icon">⚠️</div>
            <div class="metric-info">
              <div class="metric-value">${slow_queries_count || slowQueriesCount || 0}</div>
              <div class="metric-label">慢查询数 (&gt;100ms)</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">⏱️</div>
            <div class="metric-info">
              <div class="metric-value">${(avg_query_time_ms || avgQueryTimeMs || 0).toFixed(1)}ms</div>
              <div class="metric-label">平均查询时间</div>
            </div>
          </div>
          <div class="metric-card">
            <div class="metric-icon">📊</div>
            <div class="metric-info">
              <div class="metric-value">${p95_query_time_ms || p95QueryTimeMs || 0}ms</div>
              <div class="metric-label">P95 响应时间</div>
            </div>
          </div>
          <div class="metric-card ${(p99_query_time_ms || p99QueryTimeMs || 0) > 500 ? 'warning' : ''}">
            <div class="metric-icon">🔥</div>
            <div class="metric-info">
              <div class="metric-value">${p99_query_time_ms || p99QueryTimeMs || 0}ms</div>
              <div class="metric-label">P99 响应时间</div>
            </div>
          </div>
        </div>

        <!-- 慢查询列表 -->
        <div class="slow-queries-section">
          <h3>🐌 Top 10 慢查询</h3>
          ${this.renderSlowQueries(top_slow_queries || topSlowQueries)}
        </div>

        <!-- 查询性能统计 -->
        <div class="query-stats-section">
          <h3>⚡ 查询性能统计 (按总耗时排序)</h3>
          ${this.renderQueryStats(query_performance || queryPerformance)}
        </div>
      </div>
    `;
  }

  renderSlowQueries(queries) {
    if (!queries || queries.length === 0) {
      return '<p class="empty-message">✅ 暂无慢查询记录</p>';
    }

    return `
      <div class="slow-queries-list">
        ${queries.map((q, index) => `
          <div class="slow-query-item">
            <div class="query-rank">#${index + 1}</div>
            <div class="query-info">
              <div class="query-text">${this.truncateQuery(q.query)}</div>
              <div class="query-meta">
                <span class="query-duration ${q.duration_ms > 500 ? 'critical' : 'warning'}">
                  ${q.duration_ms || q.durationMs}ms
                </span>
                <span class="query-timestamp">${this.formatTimestamp(q.timestamp)}</span>
              </div>
            </div>
          </div>
        `).join('')}
      </div>
    `;
  }

  renderQueryStats(stats) {
    if (!stats || stats.length === 0) {
      return '<p class="empty-message">暂无统计数据</p>';
    }

    // 只显示前 20 个
    const topStats = stats.slice(0, 20);

    return `
      <div class="query-stats-table">
        <table>
          <thead>
            <tr>
              <th>查询模式</th>
              <th>执行次数</th>
              <th>总耗时</th>
              <th>平均耗时</th>
              <th>最大耗时</th>
              <th>最小耗时</th>
            </tr>
          </thead>
          <tbody>
            ${topStats.map(stat => `
              <tr>
                <td class="query-pattern">${this.truncateQuery(stat.query_pattern || stat.queryPattern)}</td>
                <td>${stat.execution_count || stat.executionCount}</td>
                <td>${stat.total_duration_ms || stat.totalDurationMs}ms</td>
                <td class="${(stat.avg_duration_ms || stat.avgDurationMs) > 100 ? 'warning' : ''}">
                  ${(stat.avg_duration_ms || stat.avgDurationMs).toFixed(1)}ms
                </td>
                <td class="${(stat.max_duration_ms || stat.maxDurationMs) > 500 ? 'critical' : ''}">
                  ${stat.max_duration_ms || stat.maxDurationMs}ms
                </td>
                <td>${stat.min_duration_ms || stat.minDurationMs}ms</td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    `;
  }

  renderEmpty() {
    return `
      <div class="performance-empty">
        <p>暂无性能数据</p>
        <p>请稍后再试</p>
      </div>
    `;
  }

  truncateQuery(query, maxLength = 60) {
    if (!query) return '';
    return query.length > maxLength ? query.substring(0, maxLength) + '...' : query;
  }

  formatTimestamp(timestamp) {
    if (!timestamp) return '';
    try {
      const date = new Date(timestamp);
      return date.toLocaleTimeString('zh-CN');
    } catch {
      return timestamp;
    }
  }

  bindEvents() {
    // 刷新按钮
    const refreshBtn = document.getElementById('refresh-perf-btn');
    if (refreshBtn) {
      refreshBtn.addEventListener('click', () => {
        this.loadData();
      });
    }

    // 清空按钮
    const clearBtn = document.getElementById('clear-perf-btn');
    if (clearBtn) {
      clearBtn.addEventListener('click', async () => {
        try {
          await PerformanceApi.clearPerformanceMetrics();
          notifications.success('性能数据已清空');
          this.data = null;
          this.render();
        } catch (error) {
          notifications.error(`清空失败: ${error.message}`);
        }
      });
    }

    // 自动刷新
    const autoRefreshCheckbox = document.getElementById('auto-refresh-checkbox');
    if (autoRefreshCheckbox) {
      autoRefreshCheckbox.addEventListener('change', (e) => {
        if (e.target.checked) {
          this.startAutoRefresh();
          notifications.success('已启用自动刷新');
        } else {
          this.stopAutoRefresh();
          notifications.success('已停止自动刷新');
        }
      });
    }
  }

  startAutoRefresh() {
    this.stopAutoRefresh();
    this.autoRefreshInterval = setInterval(() => {
      this.loadData();
    }, 5000);
  }

  stopAutoRefresh() {
    if (this.autoRefreshInterval) {
      clearInterval(this.autoRefreshInterval);
      this.autoRefreshInterval = null;
    }
  }

  destroy() {
    this.stopAutoRefresh();
    if (this.container) {
      this.container.innerHTML = '';
    }
  }
}

// 添加样式
const style = document.createElement('style');
style.textContent = `
  .performance-dashboard {
    padding: 20px;
  }

  .dashboard-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 24px;
  }

  .header-actions {
    display: flex;
    gap: 12px;
    align-items: center;
  }

  .auto-refresh-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
  }

  .clear-btn {
    padding: 8px 16px;
    background: #e74c3c;
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 14px;
  }

  .clear-btn:hover {
    background: #c0392b;
  }

  .metrics-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
    margin-bottom: 24px;
  }

  .metric-card {
    background: white;
    padding: 20px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .metric-card.warning {
    border-left: 4px solid #f39c12;
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
    font-size: 14px;
    color: #7f8c8d;
  }

  .slow-queries-section, .query-stats-section {
    background: white;
    padding: 24px;
    border-radius: 8px;
    box-shadow: 0 2px 4px rgba(0,0,0,0.1);
    margin-bottom: 24px;
  }

  .slow-queries-section h3, .query-stats-section h3 {
    margin: 0 0 16px 0;
    font-size: 18px;
  }

  .slow-queries-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .slow-query-item {
    display: flex;
    align-items: center;
    padding: 12px;
    background: #f8f9fa;
    border-radius: 6px;
  }

  .query-rank {
    width: 40px;
    font-size: 18px;
    font-weight: bold;
    color: #7f8c8d;
  }

  .query-info {
    flex: 1;
  }

  .query-text {
    font-family: 'Monaco', 'Courier New', monospace;
    font-size: 14px;
    margin-bottom: 4px;
    color: #2c3e50;
  }

  .query-meta {
    display: flex;
    gap: 12px;
    font-size: 13px;
  }

  .query-duration {
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 600;
  }

  .query-duration.warning {
    background: #fef5e7;
    color: #f39c12;
  }

  .query-duration.critical {
    background: #fadbd8;
    color: #e74c3c;
  }

  .query-timestamp {
    color: #7f8c8d;
  }

  .query-stats-table {
    overflow-x: auto;
  }

  .query-stats-table table {
    width: 100%;
    border-collapse: collapse;
  }

  .query-stats-table th {
    background: #f8f9fa;
    padding: 12px;
    text-align: left;
    font-weight: 600;
    border-bottom: 2px solid #dee2e6;
  }

  .query-stats-table td {
    padding: 12px;
    border-bottom: 1px solid #dee2e6;
  }

  .query-pattern {
    font-family: 'Monaco', 'Courier New', monospace;
    font-size: 13px;
  }

  .query-stats-table td.warning {
    color: #f39c12;
    font-weight: 600;
  }

  .query-stats-table td.critical {
    color: #e74c3c;
    font-weight: 600;
  }

  .empty-message {
    text-align: center;
    padding: 40px;
    color: #7f8c8d;
  }
`;
document.head.appendChild(style);
