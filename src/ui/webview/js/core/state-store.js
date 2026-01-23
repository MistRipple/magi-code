// 状态管理模块
// 提供集中式状态管理，支持订阅和响应式更新

// ============================================
// 状态存储
// ============================================

const store = {
  // 会话状态
  session: {
    currentId: null,
    list: [],
    isLoading: false
  },
  
  // 消息状态
  messages: {
    thread: [],
    agentOutputs: {
      claude: [],
      codex: [],
      gemini: []
    }
  },
  
  // 处理状态
  processing: {
    isProcessing: false,
    actor: null,
    agent: null,
    startedAt: null
  },
  
  // UI 状态
  ui: {
    currentTopTab: 'tasks',
    currentBottomTab: 'thread',
    autoScrollEnabled: {
      thread: true,
      claude: true,
      codex: true,
      gemini: true
    },
    scrollPositions: {}
  },
  
  // 任务状态
  tasks: [],
  
  // 待处理变更
  pendingChanges: [],
  
  // 附件
  attachedImages: []
};

// ============================================
// 订阅系统
// ============================================

const subscribers = new Map();
let subscriberId = 0;

/**
 * 订阅状态变化
 */
export function subscribe(path, callback) {
  const id = ++subscriberId;
  if (!subscribers.has(path)) {
    subscribers.set(path, new Map());
  }
  subscribers.get(path).set(id, callback);
  
  return () => {
    const pathSubscribers = subscribers.get(path);
    if (pathSubscribers) {
      pathSubscribers.delete(id);
    }
  };
}

/**
 * 通知订阅者
 */
function notify(path, newValue, oldValue) {
  const pathSubscribers = subscribers.get(path);
  if (pathSubscribers) {
    pathSubscribers.forEach(callback => {
      try {
        callback(newValue, oldValue, path);
      } catch (e) {
        console.error('[StateStore] 订阅回调错误:', e);
      }
    });
  }
  
  // 通知父路径的订阅者
  const parts = path.split('.');
  for (let i = parts.length - 1; i > 0; i--) {
    const parentPath = parts.slice(0, i).join('.');
    const parentSubscribers = subscribers.get(parentPath);
    if (parentSubscribers) {
      parentSubscribers.forEach(callback => {
        try {
          callback(getState(parentPath), null, path);
        } catch (e) {
          console.error('[StateStore] 父路径订阅回调错误:', e);
        }
      });
    }
  }
}

// ============================================
// 状态访问
// ============================================

/**
 * 获取状态（支持路径访问）
 */
export function getState(path) {
  if (!path) return store;
  
  const parts = path.split('.');
  let current = store;
  
  for (const part of parts) {
    if (current === undefined || current === null) {
      return undefined;
    }
    current = current[part];
  }
  
  return current;
}

/**
 * 设置状态（支持路径访问）
 */
export function setState(path, value) {
  const parts = path.split('.');
  const lastPart = parts.pop();
  let current = store;
  
  for (const part of parts) {
    if (current[part] === undefined) {
      current[part] = {};
    }
    current = current[part];
  }
  
  const oldValue = current[lastPart];
  current[lastPart] = value;
  
  notify(path, value, oldValue);

  return value;
}

/**
 * 更新状态（合并对象）
 */
export function updateState(path, updates) {
  const current = getState(path);
  if (typeof current === 'object' && current !== null && !Array.isArray(current)) {
    setState(path, { ...current, ...updates });
  } else {
    setState(path, updates);
  }
}

/**
 * 批量更新状态
 */
export function batchUpdate(updates) {
  Object.entries(updates).forEach(([path, value]) => {
    setState(path, value);
  });
}

/**
 * 重置状态到初始值
 */
export function resetState(path) {
  const initialValues = {
    'session': { currentId: null, list: [], isLoading: false },
    'messages.thread': [],
    'messages.agentOutputs': { claude: [], codex: [], gemini: [] },
    'processing': { isProcessing: false, actor: null, agent: null, startedAt: null },
    'tasks': [],
    'pendingChanges': [],
    'attachedImages': []
  };

  if (initialValues[path] !== undefined) {
    setState(path, initialValues[path]);
  }
}

// ============================================
// 调试工具
// ============================================

export function getStoreSnapshot() {
  return JSON.parse(JSON.stringify(store));
}

export function getSubscriberCount() {
  let count = 0;
  subscribers.forEach(pathSubs => {
    count += pathSubs.size;
  });
  return count;
}

