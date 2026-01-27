// 设置面板处理模块
// 此文件包含设置面板的数据加载、渲染和交互逻辑

import { saveWebviewState } from '../core/state.js';
import { postMessage, getProfileConfig, refreshAgentConnections, resetExecutionStats } from '../core/vscode-api.js';
import { getWorkerConfig } from './event-handlers.js';

// ============================================
// 模型连接状态更新
// ============================================

export function updateModelConnectionStatus(modelStatuses) {
  // 停止刷新按钮的 loading 状态
  const refreshBtn = document.getElementById('model-refresh-btn');
  if (refreshBtn) {
    refreshBtn.classList.remove('loading');
    refreshBtn.disabled = false;
  }

  if (!modelStatuses) return;

  // 扩展状态文本映射
  const statusTexts = {
    'available': '已连接',
    'disabled': '已禁用',
    'not_configured': '未配置',
    'auth_failed': '认证失败',
    'network_error': '网络错误',
    'timeout': '连接超时',
    'invalid_model': '模型无效',
    'not_installed': '未安装',
    'unknown': '未知错误'
  };

  // 更新所有模型状态（Worker + 编排者 + 压缩模型）
  ['claude', 'codex', 'gemini', 'orchestrator', 'compressor'].forEach(worker => {
    const item = document.querySelector(`.model-connection-item[data-worker="${worker}"]`);
    if (!item) return;
    const status = modelStatuses[worker] || { status: 'unknown' };
    const isAvailable = status.status === 'available';

    // 更新样式
    item.classList.remove('available', 'unavailable', 'disabled', 'error');
    if (isAvailable) {
      item.classList.add('available');
    } else if (status.status === 'disabled') {
      item.classList.add('disabled');
    } else {
      item.classList.add('unavailable');
    }

    updateModelConnectionModel(worker, item);

    // 更新状态文本（显示版本信息或错误信息）
    const statusEl = item.querySelector('.model-connection-status');
    if (statusEl) {
      if (status.version) {
        statusEl.textContent = status.version;
      } else if (status.error) {
        statusEl.textContent = status.error;
        statusEl.title = status.error;
      } else {
        statusEl.textContent = statusTexts[status.status] || status.status;
      }
    }

    // 更新徽章
    const badge = item.querySelector('.model-connection-badge');
    if (badge) {
      badge.classList.remove('available', 'unavailable', 'checking', 'disabled', 'error');

      if (isAvailable) {
        badge.classList.add('available');
        badge.textContent = '已连接';
      } else if (status.status === 'disabled') {
        badge.classList.add('disabled');
        badge.textContent = '已禁用';
      } else {
        badge.classList.add('error');
        badge.textContent = statusTexts[status.status] || '不可用';
        if (status.error) {
          badge.title = status.error;
        }
      }
    }
  });
}

export function updateModelConnectionModels() {
  ['claude', 'codex', 'gemini', 'orchestrator', 'compressor'].forEach(worker => {
    const item = document.querySelector(`.model-connection-item[data-worker="${worker}"]`);
    if (!item) return;
    updateModelConnectionModel(worker, item);
  });
}

function updateModelConnectionModel(worker, item) {
  const modelEl = item.querySelector('.model-connection-model');
  if (!modelEl) return;

  let modelName = '';
  if (worker === 'orchestrator') {
    const input = document.getElementById('orch-model');
    modelName = input ? input.value.trim() : '';
  } else if (worker === 'compressor') {
    const input = document.getElementById('comp-model');
    modelName = input ? input.value.trim() : '';
  } else {
    const config = getWorkerConfig(worker);
    modelName = config && config.model ? String(config.model).trim() : '';
  }

  modelEl.textContent = modelName || '未配置';
  if (modelName) {
    modelEl.title = modelName;
  } else {
    modelEl.removeAttribute('title');
  }
}

// ============================================
// 执行统计更新
// ============================================

function formatTokenCount(count) {
  if (count >= 1000000000) return (count / 1000000000).toFixed(2) + 'G';
  if (count >= 1000000) return (count / 1000000).toFixed(2) + 'M';
  if (count >= 1000) return (count / 1000).toFixed(2) + 'K';
  return count.toString();
}

export function updateExecutionStats(stats, orchestratorStats, modelCatalog) {
  if (!stats || !Array.isArray(stats)) return;
  renderModelConnectionStats(stats);
  updateTotalTokensSummary(stats);
}

function renderModelConnectionStats(stats) {
  const statsMap = new Map();
  stats.forEach(stat => {
    statsMap.set(stat.worker, stat);
  });

  ['claude', 'codex', 'gemini', 'orchestrator', 'compressor'].forEach(worker => {
    const item = document.querySelector(`.model-connection-item[data-worker="${worker}"]`);
    if (!item) return;

    const stat = statsMap.get(worker);
    const totalExecutions = stat?.totalExecutions || 0;
    const successRate = stat?.successRate || 0;
    const totalInputTokens = stat?.totalInputTokens || 0;
    const totalOutputTokens = stat?.totalOutputTokens || 0;

    const executionsEl = item.querySelector('[data-stat="executions"]');
    if (executionsEl) {
      executionsEl.textContent = `${totalExecutions}次`;
    }

    const successRateEl = item.querySelector('[data-stat="success-rate"]');
    if (successRateEl) {
      successRateEl.textContent = `${Math.round(successRate * 100)}%`;
    }

    const tokensEl = item.querySelector('[data-stat="tokens"]');
    if (tokensEl) {
      tokensEl.textContent = `In ${formatTokenCount(totalInputTokens)} · Out ${formatTokenCount(totalOutputTokens)}`;
    }
  });
}

function updateTotalTokensSummary(stats) {
  const totalEl = document.getElementById('stats-total-tokens');
  if (!totalEl) return;

  let totalInput = 0;
  let totalOutput = 0;
  stats.forEach(stat => {
    totalInput += stat.totalInputTokens || 0;
    totalOutput += stat.totalOutputTokens || 0;
  });

  totalEl.textContent = `总 Token In ${formatTokenCount(totalInput)} · Out ${formatTokenCount(totalOutput)}`;
}

// ============================================
// 模型配置 Tab
// ============================================

let modelConfigTabsInitialized = false;

function initModelConfigTabs() {
  if (modelConfigTabsInitialized) return;
  modelConfigTabsInitialized = true;

  const tabs = document.querySelectorAll('.model-config-tab');
  const panels = document.querySelectorAll('.model-config-panel');
  if (!tabs.length || !panels.length) return;

  tabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      const target = tab.dataset.modelTab;
      if (!target) return;
      tabs.forEach((t) => t.classList.toggle('active', t === tab));
      panels.forEach((panel) => {
        panel.classList.toggle('active', panel.dataset.modelPanel === target);
      });
    });
  });
}

// ============================================
// Profile 配置更新
// ============================================

let profileInitialized = false;
let currentProfileWorker = 'claude';

let profileData = {
  claude: { role: '', focus: [], constraints: [] },
  codex: { role: '', focus: [], constraints: [] },
  gemini: { role: '', focus: [], constraints: [] }
};

let categoryConfig = {
  architecture: 'claude',
  implement: 'codex',
  refactor: 'claude',
  bugfix: 'codex',
  debug: 'claude',
  frontend: 'gemini',
  backend: 'claude',
  test: 'codex',
  document: 'gemini',
  review: 'claude',
  general: 'claude'
};

function renderProfileTags(containerId, tags, type) {
  const container = document.getElementById(containerId);
  if (!container) return;
  container.innerHTML = (tags || []).map((tag, idx) => `
    <span class="profile-tag">
      ${tag}
      <button class="profile-tag-remove" data-type="${type}" data-index="${idx}">×</button>
    </span>
  `).join('');
}

function loadWorkerProfile(worker) {
  const data = profileData[worker] || { role: '', focus: [], constraints: [] };
  const roleEl = document.getElementById('profile-role');
  if (roleEl) roleEl.value = data.role || '';
  renderProfileTags('profile-focus-tags', data.focus || [], 'focus');
  renderProfileTags('profile-constraint-tags', data.constraints || [], 'constraint');
}

function initProfileUI() {
  if (profileInitialized) return;
  profileInitialized = true;

  // Worker Tab 切换
  document.querySelectorAll('.profile-worker-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      const worker = tab.dataset.worker;
      if (!worker || worker === currentProfileWorker) return;
      currentProfileWorker = worker;
      document.querySelectorAll('.profile-worker-tab').forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      loadWorkerProfile(worker);
    });
  });

  // 标签删除
  document.addEventListener('click', (e) => {
    const target = e.target;
    if (!target || !target.classList || !target.classList.contains('profile-tag-remove')) return;
    const type = target.dataset.type;
    const index = Number(target.dataset.index);
    const data = profileData[currentProfileWorker];
    if (!data) return;

    if (type === 'focus') {
      data.focus.splice(index, 1);
      renderProfileTags('profile-focus-tags', data.focus, 'focus');
    } else if (type === 'constraint') {
      data.constraints.splice(index, 1);
      renderProfileTags('profile-constraint-tags', data.constraints, 'constraint');
    }
  });

  // 自定义输入对话框
  let inputDialogCallback = null;
  const inputDialog = document.getElementById('profile-input-dialog');
  const inputDialogTitle = document.getElementById('profile-input-dialog-title');
  const inputDialogInput = document.getElementById('profile-input-dialog-input');
  const inputDialogCancel = document.getElementById('profile-input-dialog-cancel');
  const inputDialogConfirm = document.getElementById('profile-input-dialog-confirm');

  const showInputDialog = (title, placeholder, callback) => {
    if (!inputDialog || !inputDialogTitle || !inputDialogInput) return;
    inputDialogTitle.textContent = title;
    inputDialogInput.placeholder = placeholder;
    inputDialogInput.value = '';
    inputDialogCallback = callback;
    inputDialog.classList.add('visible');
    setTimeout(() => inputDialogInput.focus(), 50);
  };

  const hideInputDialog = () => {
    if (inputDialog) inputDialog.classList.remove('visible');
    inputDialogCallback = null;
  };

  if (inputDialogCancel) inputDialogCancel.addEventListener('click', hideInputDialog);
  if (inputDialogConfirm) {
    inputDialogConfirm.addEventListener('click', () => {
      if (!inputDialogInput) return;
      const value = inputDialogInput.value.trim();
      if (value && inputDialogCallback) inputDialogCallback(value);
      hideInputDialog();
    });
  }
  if (inputDialogInput) {
    inputDialogInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        if (inputDialogConfirm) inputDialogConfirm.click();
      } else if (e.key === 'Escape') {
        hideInputDialog();
      }
    });
  }
  if (inputDialog) {
    inputDialog.addEventListener('click', (e) => {
      if (e.target === inputDialog) hideInputDialog();
    });
  }

  // 添加专注领域
  const addFocusBtn = document.getElementById('profile-add-focus');
  if (addFocusBtn) {
    addFocusBtn.addEventListener('click', () => {
      showInputDialog('添加专注领域', '例如：优先考虑代码的可维护性', (value) => {
        profileData[currentProfileWorker].focus.push(value);
        renderProfileTags('profile-focus-tags', profileData[currentProfileWorker].focus, 'focus');
      });
    });
  }

  // 添加行为约束
  const addConstraintBtn = document.getElementById('profile-add-constraint');
  if (addConstraintBtn) {
    addConstraintBtn.addEventListener('click', () => {
      showInputDialog('添加行为约束', '例如：不要进行不必要的重构', (value) => {
        profileData[currentProfileWorker].constraints.push(value);
        renderProfileTags('profile-constraint-tags', profileData[currentProfileWorker].constraints, 'constraint');
      });
    });
  }

  // 角色定位输入
  const roleInput = document.getElementById('profile-role');
  if (roleInput) {
    roleInput.addEventListener('input', (e) => {
      profileData[currentProfileWorker].role = e.target.value;
    });
  }

  // 分类配置选择
  document.querySelectorAll('.profile-category-select').forEach(select => {
    select.addEventListener('change', (e) => {
      const category = e.target.dataset.category;
      if (category) categoryConfig[category] = e.target.value;
    });
  });

  // 保存配置
  const saveBtn = document.getElementById('profile-save-btn');
  if (saveBtn) {
    saveBtn.addEventListener('click', () => {
      if (window.__setSaveButtonState) {
        window.__setSaveButtonState('profile-save-btn', 'loading');
      }
      postMessage({
        type: 'saveProfileConfig',
        data: {
          workers: profileData,
          categories: categoryConfig
        }
      });
    });
  }

  // 重置配置
  const resetBtn = document.getElementById('profile-reset-btn');
  if (resetBtn) {
    resetBtn.addEventListener('click', () => {
      if (confirm('确定要重置为默认配置吗？')) {
        postMessage({ type: 'resetProfileConfig' });
      }
    });
  }

  // 初次加载
  loadWorkerProfile(currentProfileWorker);
  Object.entries(categoryConfig).forEach(([category, worker]) => {
    const select = document.querySelector(`.profile-category-select[data-category="${category}"]`);
    if (select) select.value = worker;
  });
}

export function updateProfileConfig(config) {
  if (!config) return;

  if (config.workers) {
    profileData = { ...profileData, ...config.workers };
  }
  if (config.categories) {
    categoryConfig = { ...categoryConfig, ...config.categories };
    Object.entries(categoryConfig).forEach(([category, worker]) => {
      const select = document.querySelector(`.profile-category-select[data-category="${category}"]`);
      if (select) select.value = worker;
    });
  }

  if (config.configPath) {
    const pathEl = document.querySelector('.profile-config-hint code');
    if (pathEl) pathEl.textContent = config.configPath;
  }

  loadWorkerProfile(currentProfileWorker);
}

// ============================================
// 初始化设置面板
// ============================================

export function initializeSettingsPanel() {
  initModelConfigTabs();
  initProfileUI();

  // 请求模型连接状态
  refreshAgentConnections();

  // 请求执行统计
  postMessage({ type: 'requestExecutionStats' });

  // 请求 Profile 配置
  getProfileConfig();

  // 请求工具配置
  postMessage({ type: 'loadMCPServers' });
  postMessage({ type: 'loadRepositories' });
  postMessage({ type: 'loadSkillsConfig' });

  // 绑定刷新按钮
  const refreshBtn = document.getElementById('model-refresh-btn');
  if (refreshBtn) {
    refreshBtn.addEventListener('click', () => {
      refreshBtn.classList.add('loading');
      refreshBtn.disabled = true;
      refreshAgentConnections();
    });
  }

  // 绑定重置统计按钮
  const resetStatsBtn = document.getElementById('stats-reset-btn');
  if (resetStatsBtn) {
    resetStatsBtn.addEventListener('click', () => {
      if (confirm('确定要重置所有执行统计吗？此操作不可撤销。')) {
        resetExecutionStats();
      }
    });
  }

  console.log('[SettingsHandler] 设置面板初始化完成');
}
