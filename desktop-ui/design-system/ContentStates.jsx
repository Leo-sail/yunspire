import React from 'react';
import { BookOpenText, CircleAlert, History, LoaderCircle, Plus, RefreshCcw } from 'lucide-react';

const stateContent = {
  knowledge: {
    Icon: BookOpenText,
    title: '知识库还没有内容',
    description: '采集一篇文章、一个网页或一段想法，之后可以在这里搜索和继续阅读。',
    action: '采集第一条知识',
    ActionIcon: Plus,
  },
  reflection: {
    Icon: History,
    title: '积累一段时间后再来回望',
    description: '回望会根据真实的阅读、创作与知识变化生成，不用先配置报表。',
    action: '回到知识',
    ActionIcon: BookOpenText,
  },
  error: {
    Icon: CircleAlert,
    title: '暂时无法读取知识库',
    description: '请确认本地知识库仍可访问，然后重新读取。现有内容不会被修改。',
    action: '重新读取',
    ActionIcon: RefreshCcw,
  },
};

export function ContentState({ kind = 'knowledge', onAction = () => {} }) {
  if (kind === 'loading') {
    return (
      <section className="r10-content-state is-loading" aria-live="polite" aria-busy="true">
        <LoaderCircle className="r10-content-state-spinner" aria-hidden="true" />
        <h2>正在读取本地知识</h2>
        <p>正在整理目录与最近内容。</p>
      </section>
    );
  }

  const { Icon, title, description, action, ActionIcon } = stateContent[kind] || stateContent.knowledge;
  return (
    <section className={`r10-content-state${kind === 'error' ? ' is-error' : ''}`}>
      <Icon aria-hidden="true" />
      <h2>{title}</h2>
      <p>{description}</p>
      <button className="r10-button" type="button" onClick={onAction}>
        <ActionIcon aria-hidden="true" />
        <span>{action}</span>
      </button>
    </section>
  );
}
