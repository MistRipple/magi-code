// 设置面板处理模块
// 此文件包含设置面板的数据加载、渲染和交互逻辑

import { saveWebviewState } from '../core/state.js';
import { postMessage, getProfileConfig, refreshCliConnections, resetExecutionStats } from '../core/vscode-api.js';

// ============================================
// CLI 连接状态更新
// ============================================

export function updateCliConnectionStatus(cliStatuses) {
  // 停止刷新按钮的 loading 状态
  const refreshBtn = document.getElementById('cli-refresh-btn');
  if (refreshBtn) {
    refreshBtn.classList.remove('loading');
    refreshBtn.disabled = false;
  }

  if (!cliStatuses) return;

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
  ['claude', 'codex', 'gemini', 'orchestrator', 'compressor'].forEach(cli => {
    const item = document.querySelector(`.cli-connection-item[data-cli="${cli}"]`);
    if (!item) return;
    const status = cliStatuses[cli] || { status: 'unknown' };
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

    // 更新状态文本（显示版本信息或错误信息）
    const statusEl = item.querySelector('.cli-connection-status');
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
    const badge = item.querySelector('.cli-connection-badge');
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

// ============================================
// 执行统计更新
// ============================================

function formatTokenCount(count) {
  if (count >= 1000000000) return (count / 1000000000).toFixed(2) + 'G';
  if (count >= 1000000) return (count / 1000000).toFixed(2) + 'M';
  if (count >= 1000) return (count / 1000).toFixed(2) + 'K';
  return count.toString();
}

export function updateExecutionStats(stats, orchestratorStats) {
  if (!stats || !Array.isArray(stats)) return;

  // 更新编排者统计
  if (orchestratorStats) {
    const totalTasksEl = document.getElementById('orch-total-tasks');
    const successEl = document.getElementById('orch-success');
    const failedEl = document.getElementById('orch-failed');
    const inputTokensEl = document.getElementById('orch-input-tokens');
    const outputTokensEl = document.getElementById('orch-output-tokens');
    if (totalTasksEl) totalTasksEl.textContent = orchestratorStats.totalTasks || 0;
    if (successEl) successEl.textContent = orchestratorStats.totalSuccess || 0;
    if (failedEl) failedEl.textContent = orchestratorStats.totalFailed || 0;
    if (inputTokensEl) inputTokensEl.textContent = formatTokenCount(orchestratorStats.totalInputTokens || 0);
    if (outputTokensEl) outputTokensEl.textContent = formatTokenCount(orchestratorStats.totalOutputTokens || 0);
  }

  // 更新各 CLI 统计
  stats.forEach(stat => {
    const card = document.querySelector(`.cli-stat-card[data-cli="${stat.cli}"]`);
    if (!card) return;

    // 更新健康状态
    card.classList.toggle('healthy', stat.isHealthy);
    card.classList.toggle('unhealthy', !stat.isHealthy);

    const healthDot = card.querySelector('.health-dot');
    if (healthDot) {
      healthDot.classList.toggle('healthy', stat.isHealthy);
      healthDot.classList.toggle('unhealthy', !stat.isHealthy);
    }

    // 更新成功率
    const rateEl = card.querySelector('.cli-stat-rate');
    if (rateEl) {
      const rate = stat.totalExecutions > 0 ? Math.round(stat.successRate * 100) : '--';
      rateEl.textContent = rate === '--' ? '--' : rate + '%';
      rateEl.className = 'cli-stat-rate';
      if (rate !== '--') {
        if (rate >= 80) rateEl.classList.add('good');
        else if (rate >= 60) rateEl.classList.add('warning');
        else rateEl.classList.add('bad');
      }
    }

    // 更新详情
    const detailEl = card.querySelector('.cli-stat-detail');
    if (detailEl) {
      const avgTime = stat.avgDuration > 0 ? Math.round(stat.avgDuration / 1000) + 's' : '-';
      detailEl.textContent = `${stat.totalExecutions} 次执行 · 平均 ${avgTime}`;
    }

    // 更新 Token 统计
    const tokensEl = card.querySelector('.cli-stat-tokens');
    if (tokensEl) {
      const inputTokens = formatTokenCount(stat.totalInputTokens || 0);
      const outputTokens = formatTokenCount(stat.totalOutputTokens || 0);
      tokensEl.textContent = `${inputTokens} / ${outputTokens}`;
    }

    // 更新进度条
    const barFill = card.querySelector('.cli-stat-bar-fill');
    if (barFill) {
      const rate = stat.totalExecutions > 0 ? stat.successRate * 100 : 0;
      barFill.style.width = rate + '%';
      barFill.className = 'cli-stat-bar-fill';
      if (rate >= 80) barFill.classList.add('good');
      else if (rate >= 60) barFill.classList.add('warning');
      else barFill.classList.add('bad');
    }
  });
}

// ============================================
// Profile 配置更新
// ============================================

export function updateProfileConfig(config) {
  if (!config) return;

  // 更新各 Worker 的配置
  ['claude', 'codex', 'gemini'].forEach(worker => {
    const workerConfig = config[worker];
    if (!workerConfig) return;

    // 更新系统提示词
    const systemPromptEl = document.getElementById(`${worker}-system-prompt`);
    if (systemPromptEl) {
      systemPromptEl.value = workerConfig.systemPrompt || '';
    }

    // 更新标签
    if (workerConfig.tags) {
      updateProfileTags(worker, workerConfig.tags);
    }
  });
}

function updateProfileTags(worker, tags) {
  const container = document.getElementById(`${worker}-tags-container`);
  if (!container) return;

  container.innerHTML = '';

  if (tags.strengths && tags.strengths.length > 0) {
    tags.strengths.forEach(tag => {
      const tagEl = document.createElement('span');
      tagEl.className = 'profile-tag strength';
      tagEl.textContent = tag;
      container.appendChild(tagEl);
    });
  }

  if (tags.weaknesses && tags.weaknesses.length > 0) {
    tags.weaknesses.forEach(tag => {
      const tagEl = document.createElement('span');
      tagEl.className = 'profile-tag weakness';
      tagEl.textContent = tag;
      container.appendChild(tagEl);
    });
  }
}

// ============================================
// 初始化设置面板
// ============================================

export function initializeSettingsPanel() {
  // 请求 CLI 连接状态
  refreshCliConnections();

  // 请求执行统计
  postMessage({ type: 'requestExecutionStats' });

  // 请求 Profile 配置
  getProfileConfig();

  // 绑定刷新按钮
  const refreshBtn = document.getElementById('cli-refresh-btn');
  if (refreshBtn) {
    refreshBtn.addEventListener('click', () => {
      refreshBtn.classList.add('loading');
      refreshBtn.disabled = true;
      refreshCliConnections();
    });
  }

  // 绑定重置统计按钮
  const resetStatsBtn = document.getElementById('reset-stats-btn');
  if (resetStatsBtn) {
    resetStatsBtn.addEventListener('click', () => {
      if (confirm('确定要重置所有执行统计吗？此操作不可撤销。')) {
        resetExecutionStats();
      }
    });
  }

  console.log('[SettingsHandler] 设置面板初始化完成');
}
