import React, { useState } from 'react';
import {
  ArrowUp,
  ArrowUpRight,
  BookOpenCheck,
  GitBranch,
  History,
  Orbit,
  ShieldCheck,
  Sparkles,
  SquarePen,
  X,
} from 'lucide-react';

const defaultSuggestions = [
  { icon: History, label: '找回最近积累', prompt: '帮我找出还没有继续整理的知识。' },
  { icon: GitBranch, label: '梳理一条连接', prompt: '结合当前知识库，帮我梳理一条值得继续阅读的连接。' },
  { icon: SquarePen, label: '继续一段创作', prompt: '根据我当前的上下文，给出一个可以继续写下去的起点。' },
];

const noop = () => {};

export function AssistantDock({
  open = true,
  contextLabel = '工作台 · 最近积累',
  status = '贴近当前积累，按需出现',
  intro = '我可以帮你找回一篇笔记、梳理已有连接，或把当前思路继续写下去。',
  suggestions = defaultSuggestions,
  onClose = noop,
  onOpenFull = noop,
  onSubmit = noop,
}) {
  const [value, setValue] = useState('');

  function submit(event) {
    event.preventDefault();
    const message = value.trim();
    if (!message) return;
    onSubmit(message);
    setValue('');
  }

  return (
    <aside className={`r10-assistant-dock r10-story-assistant-dock${open ? ' open' : ''}`} role="dialog" aria-label="AI 助手" aria-hidden={!open}>
      <header className="r10-assistant-dock-header">
        <div className="r10-assistant-dock-identity">
          <span className="r10-assistant-dock-avatar"><Sparkles aria-hidden="true" /></span>
          <div><strong>AI 助手</strong><small>{status}</small></div>
        </div>
        <div className="r10-assistant-dock-header-actions">
          <span className="r10-assistant-local-state"><ShieldCheck aria-hidden="true" />本地上下文</span>
          <button className="icon-button quiet" type="button" aria-label="关闭 AI 助手" onClick={onClose}><X aria-hidden="true" /></button>
        </div>
      </header>
      <div className="r10-assistant-context">
        <span className="r10-assistant-context-icon"><BookOpenCheck aria-hidden="true" /></span>
        <span><small>当前上下文</small><strong>{contextLabel}</strong></span>
      </div>
      <div className="r10-assistant-dock-stream">
        <div className="r10-assistant-dock-intro">
          <span className="r10-assistant-dock-intro-icon"><Orbit aria-hidden="true" /></span>
          <strong>从你正在做的事开始</strong>
          <p>{intro}</p>
        </div>
        <div className="r10-assistant-suggestions" aria-label="AI 助手建议动作">
          {suggestions.map(({ icon: Icon, label, prompt }) => (
            <button type="button" key={label} onClick={() => onSubmit(prompt)}>
              <Icon aria-hidden="true" />
              <span>{label}</span>
              <ArrowUpRight aria-hidden="true" />
            </button>
          ))}
        </div>
      </div>
      <form className="r10-assistant-dock-composer" onSubmit={submit}>
        <label className="sr-only" htmlFor="storybook-assistant-dock-input">向 AI 助手提问</label>
        <textarea id="storybook-assistant-dock-input" rows="2" value={value} onChange={(event) => setValue(event.target.value)} placeholder="问问当前知识，或告诉我下一步……" />
        <div className="r10-assistant-dock-composer-footer">
          <span><Sparkles aria-hidden="true" />建议会保留依据，执行前需要确认</span>
          <button className="r10-assistant-dock-send" type="submit" aria-label="发送给 AI 助手"><ArrowUp aria-hidden="true" /></button>
        </div>
      </form>
      <footer className="r10-assistant-dock-footer">
        <button type="button" onClick={onOpenFull}><span>展开完整会话</span><ArrowUpRight aria-hidden="true" /></button>
        <span>当前工作面不会被修改</span>
      </footer>
    </aside>
  );
}
