import React from 'react';
import * as DropdownMenu from '@radix-ui/react-dropdown-menu';
import {
  Bell,
  ChevronDown,
  Clock3,
  History,
  LayoutDashboard,
  LibraryBig,
  Plus,
  Search,
  Settings2,
  SquarePen,
} from 'lucide-react';
import { AssistantEntry } from './AssistantEntry.jsx';

const navigationItems = [
  { route: 'dashboard', label: '工作台', Icon: LayoutDashboard },
  { route: 'search', label: '知识', Icon: LibraryBig },
  { route: 'create', label: '创作', Icon: SquarePen },
  { route: 'reports', label: '回望', Icon: History },
];

const noop = () => {};

export function R10TopNavigation({
  activeRoute = 'dashboard',
  vaultName = '我的知识库',
  logoSrc = '/assets/brand/yunspire-symbol.png',
  disabledRoutes = [],
  onNavigate = noop,
  onSearch = noop,
  onCapture = noop,
  onAssistant = noop,
  onOpenTasks = noop,
  onOpenNotifications = noop,
  onOpenSettings = noop,
}) {
  return (
    <header className="r10-topbar r10-storybook-topbar" aria-label="云枢主导航">
      <div className="r10-topbar-inner">
        <button className="r10-brand" type="button" aria-label="云枢工作台" onClick={() => onNavigate('dashboard')}>
          <img src={logoSrc} alt="" />
          <span>云枢</span>
        </button>
        <span className="r10-brand-divider" aria-hidden="true" />
        <nav className="r10-primary-navigation" aria-label="知识工作区">
          {navigationItems.map(({ route, label, Icon }) => (
            <button
              className={`r10-nav-item${activeRoute === route ? ' active' : ''}`}
              type="button"
              aria-current={activeRoute === route ? 'page' : undefined}
              disabled={disabledRoutes.includes(route)}
              key={route}
              onClick={() => onNavigate(route)}
            >
              <Icon aria-hidden="true" strokeWidth={1.9} />
              <span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="r10-topbar-actions">
          <button className="r10-command-trigger" type="button" aria-label="搜索知识或运行命令" onClick={onSearch}>
            <Search aria-hidden="true" />
            <span>搜索</span>
            <kbd>⌘ K</kbd>
          </button>
          <button className="r10-capture-trigger" type="button" onClick={onCapture}>
            <Plus aria-hidden="true" />
            <span>采集</span>
          </button>
          <AssistantEntry onActivate={onAssistant} />
          <DropdownMenu.Root>
            <DropdownMenu.Trigger asChild>
              <button className="r10-vault-switcher" type="button" aria-label={`当前知识库：${vaultName}`}>
                <span className="r10-vault-icon"><LibraryBig aria-hidden="true" /></span>
                <span>{vaultName}</span>
                <ChevronDown aria-hidden="true" />
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content className="r10-radix-menu" sideOffset={8} align="end">
                <DropdownMenu.Label className="r10-radix-menu-label">本地知识库</DropdownMenu.Label>
                <DropdownMenu.Separator className="r10-radix-menu-separator" />
                <DropdownMenu.Item className="r10-radix-menu-item" onSelect={onOpenTasks}>
                  <Clock3 aria-hidden="true" /><span>后台任务</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item className="r10-radix-menu-item" onSelect={onOpenNotifications}>
                  <Bell aria-hidden="true" /><span>通知</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item className="r10-radix-menu-item" onSelect={onOpenSettings}>
                  <Settings2 aria-hidden="true" /><span>知识库与偏好</span>
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>
      </div>
    </header>
  );
}
