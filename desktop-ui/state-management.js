/**
 * 云枢前端状态管理
 * 基于 Proxy 的响应式状态管理，轻量级无依赖
 */

/**
 * 创建响应式状态
 */
export function createReactiveState(initialState) {
  const listeners = new Set();

  const notify = (path) => {
    listeners.forEach(listener => listener(path));
  };

  const handler = {
    get(target, property) {
      const value = target[property];
      if (typeof value === 'object' && value !== null) {
        return new Proxy(value, handler);
      }
      return value;
    },
    set(target, property, value) {
      const oldValue = target[property];
      if (oldValue !== value) {
        target[property] = value;
        notify(property);
      }
      return true;
    }
  };

  const state = new Proxy(initialState, handler);

  return {
    state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getSnapshot() {
      return JSON.parse(JSON.stringify(initialState));
    }
  };
}

/**
 * 全局应用状态
 */
export const appState = createReactiveState({
  // 当前选中的 Vault
  currentVaultId: null,

  // 加载状态
  loading: {
    knowledgeGraph: false,
    contentValue: false,
    health: false,
    performance: false,
  },

  // 错误状态
  errors: {
    knowledgeGraph: null,
    contentValue: null,
    health: null,
    performance: null,
  },

  // 数据缓存
  cache: {
    knowledgeGraph: null,
    contentValue: {},
    health: null,
    performance: null,
    lastUpdated: {},
  },

  // UI 状态
  ui: {
    sidebarOpen: true,
    activeView: 'overview',
    notifications: [],
  }
});

/**
 * 数据加载管理器
 */
export class DataLoader {
  constructor(stateManager) {
    this.state = stateManager;
    this.cacheTTL = 5 * 60 * 1000; // 5 分钟缓存
  }

  /**
   * 检查缓存是否有效
   */
  isCacheValid(key) {
    const lastUpdated = this.state.state.cache.lastUpdated[key];
    if (!lastUpdated) return false;
    return Date.now() - lastUpdated < this.cacheTTL;
  }

  /**
   * 加载数据（带缓存和错误处理）
   */
  async loadData(key, loader, options = {}) {
    const { forceRefresh = false, onProgress } = options;

    // 检查缓存
    if (!forceRefresh && this.isCacheValid(key)) {
      return this.state.state.cache[key];
    }

    // 设置加载状态
    this.state.state.loading[key] = true;
    this.state.state.errors[key] = null;

    try {
      // 执行加载
      const data = await loader(onProgress);

      // 更新缓存
      this.state.state.cache[key] = data;
      this.state.state.cache.lastUpdated[key] = Date.now();

      return data;
    } catch (error) {
      // 记录错误
      this.state.state.errors[key] = {
        message: error.message,
        code: error.code || 'UNKNOWN_ERROR',
        timestamp: Date.now(),
      };
      throw error;
    } finally {
      // 清除加载状态
      this.state.state.loading[key] = false;
    }
  }

  /**
   * 清除缓存
   */
  clearCache(key) {
    if (key) {
      this.state.state.cache[key] = null;
      delete this.state.state.cache.lastUpdated[key];
    } else {
      this.state.state.cache = {
        knowledgeGraph: null,
        contentValue: {},
        health: null,
        performance: null,
        lastUpdated: {},
      };
    }
  }
}

/**
 * 通知管理器
 */
export class NotificationManager {
  constructor(stateManager) {
    this.state = stateManager;
    this.nextId = 1;
  }

  /**
   * 显示通知
   */
  show(message, type = 'info', duration = 3000) {
    const notification = {
      id: this.nextId++,
      message,
      type, // 'info' | 'success' | 'warning' | 'error'
      timestamp: Date.now(),
    };

    this.state.state.ui.notifications.push(notification);

    // 自动移除
    if (duration > 0) {
      setTimeout(() => {
        this.dismiss(notification.id);
      }, duration);
    }

    return notification.id;
  }

  /**
   * 显示成功通知
   */
  success(message, duration) {
    return this.show(message, 'success', duration);
  }

  /**
   * 显示错误通知
   */
  error(message, duration = 5000) {
    return this.show(message, 'error', duration);
  }

  /**
   * 显示警告通知
   */
  warning(message, duration) {
    return this.show(message, 'warning', duration);
  }

  /**
   * 移除通知
   */
  dismiss(id) {
    const index = this.state.state.ui.notifications.findIndex(n => n.id === id);
    if (index !== -1) {
      this.state.state.ui.notifications.splice(index, 1);
    }
  }

  /**
   * 清除所有通知
   */
  clearAll() {
    this.state.state.ui.notifications = [];
  }
}

/**
 * 全局数据加载器
 */
export const dataLoader = new DataLoader(appState);

/**
 * 全局通知管理器
 */
export const notifications = new NotificationManager(appState);

/**
 * 便捷的状态钩子
 */
export function useState(selector) {
  const value = selector(appState.state);

  return {
    value,
    subscribe(callback) {
      return appState.subscribe(() => {
        const newValue = selector(appState.state);
        if (newValue !== value) {
          callback(newValue);
        }
      });
    }
  };
}

/**
 * 加载状态钩子
 */
export function useLoading(key) {
  return useState(state => state.loading[key]);
}

/**
 * 错误状态钩子
 */
export function useError(key) {
  return useState(state => state.errors[key]);
}

/**
 * 缓存数据钩子
 */
export function useCache(key) {
  return useState(state => state.cache[key]);
}
