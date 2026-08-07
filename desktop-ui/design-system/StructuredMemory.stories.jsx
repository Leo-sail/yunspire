import React, { useMemo, useState } from 'react';
import {
  BrainCircuit,
  BriefcaseBusiness,
  Download,
  History,
  Layers3,
  Milestone,
  RefreshCw,
  Search,
  SlidersHorizontal,
  Sparkles,
} from 'lucide-react';

const tracks = [
  { id: 'all', label: '全部记忆', icon: Layers3 },
  { id: 'user_episode', label: '用户经历', icon: Milestone },
  { id: 'user_profile', label: '用户偏好', icon: SlidersHorizontal },
  { id: 'agent_case', label: 'Agent 案例', icon: BriefcaseBusiness },
  { id: 'agent_skill', label: 'Skill 经验', icon: Sparkles },
];

const records = [
  { id: 'episode-1', track: 'user_episode', title: '完成云枢工作台的信息架构复盘', content: '用户逐项检查了工作台、知识库、创作和成长中心，并确认长期知识积累是产品的最高优先级。', confidence: 0.96, updated: '今天 10:42', source: '对话/界面重构复盘', icon: Milestone },
  { id: 'profile-1', track: 'user_profile', title: '偏好温暖但克制的知识空间', content: '界面需要降低工具感，保持专业知识工作台的效率，同时避免绿色、黑色侧栏和宣传式文案。', confidence: 0.99, updated: '今天 10:31', source: '用户明确偏好', icon: SlidersHorizontal },
  { id: 'case-1', track: 'agent_case', title: '浮层 AI 助手保持当前工作上下文', content: 'AI 助手浮层直接发送并显示真实回复，只有用户主动展开时才进入完整会话。', confidence: 0.94, updated: '昨天 18:08', source: '功能验证/AI 助手', icon: BriefcaseBusiness },
  { id: 'skill-1', track: 'agent_skill', title: '先核对产品功能再调整导航', content: '进行界面重构前应先扫描最新功能与交互入口，再决定导航层级和页面归属。', confidence: 0.92, updated: '昨天 16:24', source: '已批准反思建议', icon: Sparkles },
];

function StructuredMemory() {
  const [track, setTrack] = useState('all');
  const [query, setQuery] = useState('');
  const visible = useMemo(() => records.filter((record) => (
    (track === 'all' || record.track === track)
    && `${record.title} ${record.content} ${record.source}`.toLowerCase().includes(query.trim().toLowerCase())
  )), [query, track]);
  const [selectedId, setSelectedId] = useState(records[0].id);
  const selected = visible.find((record) => record.id === selectedId) || visible[0] || null;

  return (
    <section className="view active" data-view="reports">
      <div className="memory-workspace">
        <header className="memory-workspace-header">
          <div><h2>长期记忆</h2><p>直接查看云枢从证据中提炼的用户经历、用户偏好、Agent 案例和 Skill 经验。</p></div>
          <div><button className="button secondary" type="button"><RefreshCw aria-hidden="true" />刷新</button><button className="button primary" type="button"><Download aria-hidden="true" />导出 JSON</button></div>
        </header>
        <div className="memory-view-bar">
          <div className="memory-mode-tabs" role="tablist" aria-label="长期记忆视图">
            <button className="active" type="button" role="tab" aria-selected="true"><BrainCircuit aria-hidden="true" /><span>结构化记忆</span></button>
            <button type="button" role="tab" aria-selected="false"><History aria-hidden="true" /><span>行为记录</span></button>
          </div>
          <small>已读取 4 条结构化记忆</small>
        </div>
        <div className="memory-track-strip" role="tablist" aria-label="结构化记忆类型">
          {tracks.map((item) => {
            const Icon = item.icon;
            const count = item.id === 'all' ? records.length : records.filter((record) => record.track === item.id).length;
            return <button className={track === item.id ? 'active' : ''} type="button" role="tab" aria-selected={track === item.id} onClick={() => setTrack(item.id)} key={item.id}><span><Icon aria-hidden="true" />{item.label}</span><strong>{count}</strong></button>;
          })}
        </div>
        <div className="memory-layout split-tool">
          <section className="memory-list-pane list-pane">
            <div className="tool-toolbar"><div className="search-control"><Search aria-hidden="true" /><input type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索标题、内容或来源" aria-label="搜索长期记忆" /></div><label className="memory-history-toggle"><input type="checkbox" /><span>包含已替代与停用</span></label><span className="toolbar-meta">显示 {visible.length} 条</span></div>
            <div className="memory-list">
              {visible.map((record) => {
                const Icon = record.icon;
                return <button className={`memory-list-row structured-memory-row${selected?.id === record.id ? ' selected' : ''}`} type="button" onClick={() => setSelectedId(record.id)} key={record.id}><span className="memory-list-row-icon" data-memory-track-icon={record.track}><Icon aria-hidden="true" /></span><span className="memory-list-row-copy"><strong>{record.title}</strong><small>{tracks.find((item) => item.id === record.track)?.label} · {record.updated} · {Math.round(record.confidence * 100)}%</small></span><b className="badge success">正在使用</b></button>;
              })}
            </div>
          </section>
          <aside className={`inspector-pane memory-detail${selected ? '' : ' is-empty'}`}>
            <div className="inspector-header"><div><span>记忆详情</span><strong>{selected?.title || '尚未选择记录'}</strong></div><span className={`badge ${selected ? 'success' : 'neutral'}`}>{selected ? '正在使用' : '无记录'}</span></div>
            <div className="inspector-section"><h3>内容</h3><p className="memory-detail-content">{selected?.content || '从左侧选择一条记忆。'}</p></div>
            <div className="inspector-section"><h3>结构与来源</h3><dl><div><dt>记忆类型</dt><dd>{tracks.find((item) => item.id === selected?.track)?.label || '未读取'}</dd></div><div><dt>置信度</dt><dd>{selected ? `${Math.round(selected.confidence * 100)}%` : '未读取'}</dd></div><div><dt>版本</dt><dd>v1</dd></div><div><dt>更新时间</dt><dd>{selected?.updated || '未读取'}</dd></div><div><dt>来源</dt><dd>{selected?.source || '未读取'}</dd></div><div><dt>作用域</dt><dd>全局用户记忆</dd></div></dl></div>
            <div className="inspector-section memory-evidence-section"><h3>证据</h3><div className="memory-evidence-list"><div className="memory-evidence-item"><strong>{selected?.source || '未记录来源'}</strong><p>{selected?.content || '未保存证据摘录'}</p></div></div></div>
          </aside>
        </div>
      </div>
    </section>
  );
}

const meta = {
  title: 'Feedback/StructuredMemory',
  component: StructuredMemory,
  parameters: { layout: 'fullscreen' },
};

export default meta;

export const FourTracks = {};
