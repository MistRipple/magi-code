// UI 配置
// 集中管理所有 UI 相关的配置项

/**
 * UI 配置对象
 */
export const UI_CONFIG = {
  // 代码块配置
  codeblock: {
    // 折叠阈值（行数）
    collapseThreshold: 15,
    // 是否显示行号
    showLineNumbers: true,
    // 是否启用复制按钮
    enableCopy: true,
    // 是否启用语法高亮
    enableHighlight: true
  },

  // Thinking 面板配置
  thinking: {
    // 流式输出时是否自动展开
    autoExpandOnStreaming: true,
    // 摘要长度（字符数）
    summaryLength: 60,
    // 是否默认展开
    defaultExpanded: false
  },

  // 工具调用配置
  toolCall: {
    // 是否展开最新的工具调用
    expandLatest: true,
    // 是否默认展开所有工具调用
    defaultExpanded: false
  },

  // 消息配置
  message: {
    // 消息间距（像素）
    spacing: 12,
    // 分组消息间距（像素）
    groupedSpacing: 4,
    // 是否启用消息动画
    enableAnimation: true
  },

  // 滚动配置
  scroll: {
    // 自动滚动到底部的阈值（像素）
    autoScrollThreshold: 50,
    // 是否启用平滑滚动
    smoothScroll: true
  },

  // 流式输出配置
  streaming: {
    // 最小更新间隔（毫秒）
    minUpdateInterval: 50,
    // 流式超时时间（毫秒）
    timeout: 5 * 60 * 1000,
    // 是否显示流式动画
    showAnimation: true
  }
};

/**
 * 获取配置项
 * @param {string} path - 配置路径，如 'codeblock.collapseThreshold'
 * @param {*} defaultValue - 默认值
 */
export function getConfig(path, defaultValue = null) {
  const keys = path.split('.');
  let value = UI_CONFIG;

  for (const key of keys) {
    if (value && typeof value === 'object' && key in value) {
      value = value[key];
    } else {
      return defaultValue;
    }
  }

  return value;
}

/**
 * 设置配置项
 * @param {string} path - 配置路径
 * @param {*} value - 新值
 */
export function setConfig(path, value) {
  const keys = path.split('.');
  const lastKey = keys.pop();
  let target = UI_CONFIG;

  for (const key of keys) {
    if (!(key in target)) {
      target[key] = {};
    }
    target = target[key];
  }

  target[lastKey] = value;

  // 保存到 localStorage
  saveConfig();
}

/**
 * 从 localStorage 加载配置
 */
export function loadConfig() {
  try {
    const saved = localStorage.getItem('ui-config');
    if (saved) {
      const config = JSON.parse(saved);
      Object.assign(UI_CONFIG, config);
      console.log('[Config] 配置已加载');
    }
  } catch (error) {
    console.error('[Config] 加载配置失败:', error);
  }
}

/**
 * 保存配置到 localStorage
 */
export function saveConfig() {
  try {
    localStorage.setItem('ui-config', JSON.stringify(UI_CONFIG));
  } catch (error) {
    console.error('[Config] 保存配置失败:', error);
  }
}

/**
 * 重置配置为默认值
 */
export function resetConfig() {
  localStorage.removeItem('ui-config');
  console.log('[Config] 配置已重置');
}

// 初始化时加载配置
loadConfig();
