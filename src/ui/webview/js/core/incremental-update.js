// 增量更新引擎
// 实现消息级别的 DOM 差异更新，避免全量重渲染

// ============================================
// 消息 DOM 映射
// ============================================

// 消息 ID -> DOM 元素映射
const messageDOMMap = new Map();

// 消息 ID -> 消息快照映射（用于比较变化）
const messageSnapshotMap = new Map();

// 渲染队列（批量更新）
let renderQueue = [];
let renderScheduled = false;

// ============================================
// 核心 API
// ============================================

/**
 * 获取消息的唯一标识
 */
export function getMessageKey(message) {
  // 优先使用 standardMessageId，其次使用 streamKey，最后使用索引
  return message.standardMessageId || message.streamKey || message.id || null;
}

/**
 * 创建消息快照（用于比较变化）
 */
export function createMessageSnapshot(message) {
  return {
    content: message.content || '',
    streaming: !!message.streaming,
    toolCallsCount: message.toolCalls?.length || 0,
    lastToolCallStatus: message.toolCalls?.[message.toolCalls.length - 1]?.status,
    thinkingCount: message.thinking?.length || 0,
    timestamp: message.timestamp || message.startedAt,
    role: message.role,
    source: message.source,
    agent: message.agent
  };
}

/**
 * 比较两个快照是否相同
 */
export function snapshotsEqual(a, b) {
  if (!a || !b) return false;
  return (
    a.content === b.content &&
    a.streaming === b.streaming &&
    a.toolCallsCount === b.toolCallsCount &&
    a.lastToolCallStatus === b.lastToolCallStatus &&
    a.thinkingCount === b.thinkingCount
  );
}

/**
 * 注册消息 DOM 元素
 */
export function registerMessageDOM(messageKey, element) {
  if (!messageKey || !element) return;
  messageDOMMap.set(messageKey, element);
}

/**
 * 获取消息 DOM 元素
 */
export function getMessageDOM(messageKey) {
  return messageDOMMap.get(messageKey);
}

/**
 * 清理已移除的消息
 */
export function cleanupRemovedMessages(currentMessageKeys) {
  const keysToRemove = [];
  messageDOMMap.forEach((_, key) => {
    if (!currentMessageKeys.has(key)) {
      keysToRemove.push(key);
    }
  });
  keysToRemove.forEach(key => {
    messageDOMMap.delete(key);
    messageSnapshotMap.delete(key);
  });
}

// ============================================
// 增量更新逻辑
// ============================================

/**
 * 计算需要更新的消息
 */
export function computeMessageUpdates(messages, renderMessageFn) {
  const updates = {
    add: [],      // 新增的消息
    update: [],   // 需要更新的消息
    remove: [],   // 需要移除的消息
    unchanged: [] // 未变化的消息
  };

  const currentKeys = new Set();

  messages.forEach((message, index) => {
    const key = getMessageKey(message);
    if (!key) {
      // 没有 key 的消息总是重新渲染
      updates.add.push({ message, index, key: 'temp-' + index });
      return;
    }

    currentKeys.add(key);
    const existingDOM = messageDOMMap.get(key);
    const existingSnapshot = messageSnapshotMap.get(key);
    const newSnapshot = createMessageSnapshot(message);

    if (!existingDOM) {
      // 新消息
      updates.add.push({ message, index, key });
      messageSnapshotMap.set(key, newSnapshot);
    } else if (!snapshotsEqual(existingSnapshot, newSnapshot)) {
      // 消息已变化
      updates.update.push({ message, index, key, dom: existingDOM });
      messageSnapshotMap.set(key, newSnapshot);
    } else {
      // 消息未变化
      updates.unchanged.push({ message, index, key, dom: existingDOM });
    }
  });

  // 找出需要移除的消息
  messageDOMMap.forEach((dom, key) => {
    if (!currentKeys.has(key)) {
      updates.remove.push({ key, dom });
    }
  });

  return updates;
}

/**
 * 应用增量更新到 DOM
 */
export function applyIncrementalUpdates(container, updates, renderMessageFn) {
  // 1. 移除已删除的消息
  updates.remove.forEach(({ key, dom }) => {
    if (dom && dom.parentNode) {
      dom.parentNode.removeChild(dom);
    }
    messageDOMMap.delete(key);
    messageSnapshotMap.delete(key);
  });

  // 2. 更新已变化的消息
  updates.update.forEach(({ message, index, key, dom }) => {
    if (dom) {
      const newHTML = renderMessageFn(message, index);
      // 使用 insertAdjacentHTML 创建新元素
      const temp = document.createElement('div');
      temp.innerHTML = newHTML;
      const newElement = temp.firstElementChild;
      if (newElement) {
        dom.parentNode.replaceChild(newElement, dom);
        messageDOMMap.set(key, newElement);
      }
    }
  });

  // 3. 添加新消息（需要找到正确的插入位置）
  // 这里简化处理：新消息追加到末尾
  updates.add.forEach(({ message, index, key }) => {
    const newHTML = renderMessageFn(message, index);
    const temp = document.createElement('div');
    temp.innerHTML = newHTML;
    const newElement = temp.firstElementChild;
    if (newElement) {
      container.appendChild(newElement);
      if (key && !key.startsWith('temp-')) {
        messageDOMMap.set(key, newElement);
      }
    }
  });

  return {
    added: updates.add.length,
    updated: updates.update.length,
    removed: updates.remove.length,
    unchanged: updates.unchanged.length
  };
}

// ============================================
// 批量更新调度
// ============================================

/**
 * 调度渲染更新（防抖）
 */
export function scheduleUpdate(updateFn) {
  renderQueue.push(updateFn);
  
  if (!renderScheduled) {
    renderScheduled = true;
    requestAnimationFrame(() => {
      renderScheduled = false;
      const queue = renderQueue;
      renderQueue = [];
      
      // 执行所有排队的更新
      queue.forEach(fn => {
        try {
          fn();
        } catch (e) {
          console.error('[IncrementalUpdate] 更新失败:', e);
        }
      });
    });
  }
}

/**
 * 重置增量更新状态（切换会话时调用）
 */
export function resetIncrementalState() {
  messageDOMMap.clear();
  messageSnapshotMap.clear();
  renderQueue = [];
  renderScheduled = false;
}

// ============================================
// 调试工具
// ============================================

export function getIncrementalStats() {
  return {
    trackedMessages: messageDOMMap.size,
    snapshots: messageSnapshotMap.size,
    pendingUpdates: renderQueue.length
  };
}

