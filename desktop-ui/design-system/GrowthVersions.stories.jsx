import React from 'react';
import { ChevronRight, GitBranch, History, RotateCcw } from 'lucide-react';

const versions = [
  { version: 12, guidance: '把阅读后的待判断事项集中到工作台，保留原始来源和恢复路径。', time: '今天 10:24', state: '当前' },
  { version: 11, guidance: '将长期记忆的纠错入口放回回望工作区，避免打断阅读。', time: '昨天 18:42', state: '已应用' },
  { version: 10, guidance: '恢复上一版的创作候选筛选规则。', time: '2026 年 8 月 1 日', state: '恢复生成' },
];

function GrowthVersions() {
  const current = versions[0];
  return (
    <section className="view active" data-view="reports">
      <div className="growth-workspace">
        <header className="growth-workspace-header">
          <div>
            <h2>成长版本</h2>
            <p>每次经用户确认的优化都会保留版本来源、形成时间和恢复路径。</p>
          </div>
          <button className="button secondary" type="button"><RotateCcw aria-hidden="true" />刷新</button>
        </header>
        <div className="growth-current-summary">
          <span><strong>v{current.version}</strong><small>当前版本</small></span>
          <p>{current.guidance}</p>
          <b className="badge success">已应用</b>
        </div>
        <div className="growth-layout split-tool">
          <section className="growth-list-pane list-pane">
            <div className="tool-toolbar"><strong>版本记录</strong><span className="toolbar-meta">3 个版本</span></div>
            <div className="growth-version-list">
              {versions.map((version, index) => (
                <button className={`growth-version-row${index === 0 ? ' selected' : ''}`} type="button" key={version.version}>
                  <span className="growth-version-mark">v{version.version}</span>
                  <span><strong>{version.guidance}</strong><small>{version.time}</small></span>
                  <b className={`badge ${index === 2 ? 'neutral' : 'success'}`}>{version.state}</b>
                  <ChevronRight aria-hidden="true" />
                </button>
              ))}
            </div>
          </section>
          <aside className="inspector-pane growth-detail">
            <div className="inspector-header"><div><span>版本详情</span><strong>版本 v12</strong></div><span className="badge success">当前</span></div>
            <div className="inspector-section"><h3>形成时间</h3><p>今天 10:24</p></div>
            <div className="inspector-section"><h3>优化说明</h3><p className="body-copy">{current.guidance}</p></div>
            <div className="inspector-section"><h3>版本来源</h3><dl><div><dt>候选 ID</dt><dd className="mono">opt_01HZX7</dd></div><div><dt>回滚目标</dt><dd>不适用</dd></div></dl></div>
            <div className="growth-detail-actions"><button className="button primary block-button" type="button"><History aria-hidden="true" />恢复到此版本</button><small>恢复会生成一个新版本，不覆盖历史。</small></div>
          </aside>
        </div>
      </div>
    </section>
  );
}

const meta = {
  title: 'Feedback/GrowthVersions',
  component: GrowthVersions,
  parameters: { layout: 'fullscreen' },
};

export default meta;

export const CurrentVersion = {};

export const Empty = {
  render: () => (
    <section className="view active" data-view="reports">
      <div className="growth-workspace">
        <header className="growth-workspace-header"><div><h2>成长版本</h2><p>经用户确认并应用的优化会按时间保存在这里。</p></div></header>
        <div className="growth-layout split-tool">
          <section className="growth-list-pane list-pane"><div className="growth-empty"><GitBranch aria-hidden="true" /><strong>还没有成长版本</strong><span>当前工作保持不变，直到你确认一项优化。</span></div></section>
        </div>
      </div>
    </section>
  ),
};
