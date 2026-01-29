<script lang="ts">
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';

  interface Props {
    onClose?: () => void;
  }

  let { onClose }: Props = $props();

  // 当前激活的 Tab
  let activeTab = $state<'stats' | 'model' | 'profile' | 'tools'>('stats');

  // 模型连接状态类型
  interface ModelStatus {
    status: 'available' | 'disabled' | 'not_configured' | 'checking' | 'error';
    version?: string;
    model?: string;
    executions?: number;
    successRate?: number;
    tokens?: number;
    error?: string;
  }

  // 状态
  let modelStatuses = $state<Record<string, ModelStatus>>({
    claude: { status: 'checking' },
    codex: { status: 'checking' },
    gemini: { status: 'checking' },
    orchestrator: { status: 'checking' },
    compressor: { status: 'checking' }
  });

  let isRefreshing = $state(false);
  let totalTokens = $state(0);
  let userInfo = $state('');
  let showResetConfirm = $state(false);

  // Profile Tab 状态
  let profileWorker = $state<'claude' | 'codex' | 'gemini'>('claude');
  // 存储所有 workers 的画像配置
  let allWorkerProfiles = $state<Record<string, { role: string; focus: string[]; constraints: string[] }>>({
    claude: { role: '', focus: [], constraints: [] },
    codex: { role: '', focus: [], constraints: [] },
    gemini: { role: '', focus: [], constraints: [] }
  });
  let taskCategories = $state<Record<string, string>>({
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
  });

  // 当前 worker 的画像数据（响应式派生）
  let profileRole = $derived(allWorkerProfiles[profileWorker]?.role || '');
  let profileFocusAreas = $derived(ensureArray(allWorkerProfiles[profileWorker]?.focus));
  let profileConstraints = $derived(ensureArray(allWorkerProfiles[profileWorker]?.constraints));

  // Model Tab 状态
  let modelConfigTab = $state<'orch' | 'comp' | 'ace'>('orch');
  let workerModelTab = $state<'claude' | 'codex' | 'gemini'>('claude');

  // 测试连接状态: 'idle' | 'testing' | 'success' | 'error'
  let testStatus = $state<Record<string, 'idle' | 'testing' | 'success' | 'error'>>({
    orch: 'idle',
    comp: 'idle',
    ace: 'idle',
    claude: 'idle',
    codex: 'idle',
    gemini: 'idle'
  });

  // 模型配置表单
  let orchConfig = $state({ baseUrl: '', apiKey: '', model: '', provider: 'anthropic' });
  let compConfig = $state({ baseUrl: '', apiKey: '', model: '', provider: 'anthropic' });
  let aceConfig = $state({ url: '', key: '' });
  let workerConfigs = $state<Record<string, { baseUrl: string; apiKey: string; model: string; provider: string; enabled: boolean }>>({
    claude: { baseUrl: '', apiKey: '', model: '', provider: 'anthropic', enabled: true },
    codex: { baseUrl: '', apiKey: '', model: '', provider: 'openai', enabled: true },
    gemini: { baseUrl: '', apiKey: '', model: '', provider: 'openai', enabled: true }
  });

  // Tools Tab 状态 - MCP 服务器完整结构
  interface MCPServer {
    id: string;
    name: string;
    command?: string;
    args?: string[];
    env?: Record<string, string>;
    enabled: boolean;
    connected?: boolean;
    error?: string;
  }
  let mcpServers = $state<MCPServer[]>([]);
  let mcpExpandedServer = $state<string | null>(null);
  let mcpServerTools = $state<Record<string, Array<{ name: string; description: string; inputSchema?: any }>>>({});
  let mcpExpandedTool = $state<string | null>(null); // 用于跟踪展开描述的工具
  let currentEditingMCPServer = $state<MCPServer | null>(null);
  let mcpRefreshingServers = $state<Set<string>>(new Set()); // 正在刷新工具的服务器 ID

  // Skills 完整结构
  interface SkillItem {
    name: string;
    description: string;
    source: 'builtin' | 'custom' | 'instruction';
    enabled?: boolean;
  }
  let skills = $state<SkillItem[]>([]);

  // 仓库管理
  interface Repository {
    id: string;
    url: string;
    name?: string;
    skillCount?: number;
    lastUpdated?: string;
  }
  let repositories = $state<Repository[]>([]);

  // Skill 库
  interface LibrarySkill {
    name: string;
    fullName: string;
    description?: string;
    author?: string;
    version?: string;
    category?: string;
    skillType?: string;
    repositoryId?: string;
    repositoryName?: string;
    installed?: boolean;
    icon?: string;
  }
  let librarySkills = $state<LibrarySkill[]>([]);
  let skillSearchQuery = $state('');

  // 对话框状态
  let showInputDialog = $state(false);
  let inputDialogTitle = $state('');
  let inputDialogValue = $state('');
  let inputDialogCallback = $state<((value: string) => void) | null>(null);

  // MCP 对话框
  let showMCPDialogState = $state(false);
  let mcpDialogIsEdit = $state(false);
  let mcpDialogJson = $state('');

  // 仓库管理对话框
  let showRepoDialogState = $state(false);
  let repoAddUrl = $state('');
  let repoAddLoading = $state(false);
  let repositoriesLoading = $state(false); // 仓库列表加载状态

  // Skill 库对话框
  let showSkillLibraryDialogState = $state(false);
  let skillLibraryLoading = $state(false); // Skill 库加载状态

  // 通用确认对话框状态
  let showConfirmDialog = $state(false);
  let confirmDialogTitle = $state('');
  let confirmDialogMessage = $state('');
  let confirmDialogAction: (() => void) | null = $state(null);

  // 显示确认对话框
  function showConfirm(title: string, message: string, action: () => void) {
    confirmDialogTitle = title;
    confirmDialogMessage = message;
    confirmDialogAction = action;
    showConfirmDialog = true;
  }

  // 确认操作
  function handleConfirmYes() {
    if (confirmDialogAction) {
      confirmDialogAction();
    }
    showConfirmDialog = false;
    confirmDialogAction = null;
  }

  // 取消操作
  function handleConfirmNo() {
    showConfirmDialog = false;
    confirmDialogAction = null;
  }

  // 状态文本映射
  const statusTexts: Record<string, string> = {
    available: '已连接',
    connected: '已连接',
    disabled: '已禁用',
    not_configured: '未配置',
    checking: '检测中...',
    error: '连接失败',
    unavailable: '不可用'
  };

  const categoryLabels: Record<string, string> = {
    architecture: '架构设计(architecture)',
    implement: '功能实现(implement)',
    refactor: '代码重构(refactor)',
    bugfix: '缺陷修复(bugfix)',
    debug: '问题排查(debug)',
    frontend: '前端开发(frontend)',
    backend: '后端开发(backend)',
    test: '测试编写(test)',
    document: '文档编写(document)',
    review: '代码审查(review)',
    general: '通用任务(general)'
  };

  function getStatusClass(status: string): string {
    if (status === 'available' || status === 'connected') return 'success';
    if (status === 'checking') return 'checking';
    if (status === 'disabled' || status === 'not_configured') return 'disabled';
    if (status === 'error' || status === 'unavailable') return 'error';
    return 'error';
  }

  // 获取 worker 的执行统计数据
  function getWorkerStats(worker: string) {
    const stats = executionStats.find(s => s.worker === worker);
    return stats || null;
  }

  // 格式化 Token 数量
  function formatTokens(tokens: number | undefined): string {
    if (tokens === undefined || tokens === null) return '--';
    if (tokens >= 1000000) return `${(tokens / 1000000).toFixed(1)}M`;
    if (tokens >= 1000) return `${(tokens / 1000).toFixed(1)}K`;
    return String(tokens);
  }

  function refreshConnections() {
    if (isRefreshing) return;
    isRefreshing = true;
    // 将所有模型状态设为 checking
    modelStatuses = {
      claude: { status: 'checking' },
      codex: { status: 'checking' },
      gemini: { status: 'checking' },
      orchestrator: { status: 'checking' },
      compressor: { status: 'checking' }
    };
    vscode.postMessage({ type: 'checkWorkerStatus', force: true });
    // 30秒超时保护，防止状态卡住
    setTimeout(() => { isRefreshing = false; }, 30000);
  }

  function showResetConfirmDialog() {
    showResetConfirm = true;
  }

  function confirmResetStats() {
    vscode.postMessage({ type: 'resetExecutionStats' });
    showResetConfirm = false;
    totalTokens = 0;
  }

  function cancelResetStats() {
    showResetConfirm = false;
  }

  function logout() {
    vscode.postMessage({ type: 'logout' });
  }


  function closeSettings() {
    onClose?.();
  }

  // 更新当前 worker 的 role
  function updateProfileRole(value: string) {
    allWorkerProfiles[profileWorker] = {
      ...allWorkerProfiles[profileWorker],
      role: value
    };
    allWorkerProfiles = { ...allWorkerProfiles };
  }

  function addFocusArea() {
    inputDialogTitle = '添加专注领域';
    inputDialogValue = '';
    inputDialogCallback = (value: string) => {
      if (value.trim()) {
        const currentFocus = allWorkerProfiles[profileWorker]?.focus || [];
        allWorkerProfiles[profileWorker] = {
          ...allWorkerProfiles[profileWorker],
          focus: [...currentFocus, value.trim()]
        };
        allWorkerProfiles = { ...allWorkerProfiles };
      }
    };
    showInputDialog = true;
  }

  function removeFocusArea(index: number) {
    const currentFocus = allWorkerProfiles[profileWorker]?.focus || [];
    allWorkerProfiles[profileWorker] = {
      ...allWorkerProfiles[profileWorker],
      focus: currentFocus.filter((_, i) => i !== index)
    };
    allWorkerProfiles = { ...allWorkerProfiles };
  }

  function addConstraint() {
    inputDialogTitle = '添加行为约束';
    inputDialogValue = '';
    inputDialogCallback = (value: string) => {
      if (value.trim()) {
        const currentConstraints = allWorkerProfiles[profileWorker]?.constraints || [];
        allWorkerProfiles[profileWorker] = {
          ...allWorkerProfiles[profileWorker],
          constraints: [...currentConstraints, value.trim()]
        };
        allWorkerProfiles = { ...allWorkerProfiles };
      }
    };
    showInputDialog = true;
  }

  function removeConstraint(index: number) {
    const currentConstraints = allWorkerProfiles[profileWorker]?.constraints || [];
    allWorkerProfiles[profileWorker] = {
      ...allWorkerProfiles[profileWorker],
      constraints: currentConstraints.filter((_, i) => i !== index)
    };
    allWorkerProfiles = { ...allWorkerProfiles };
  }

  function saveProfile() {
    // 保存所有 workers 的配置
    vscode.postMessage({
      type: 'saveProfileConfig',
      data: {
        workers: allWorkerProfiles,
        categories: taskCategories
      }
    });
  }

  function resetProfile() {
    vscode.postMessage({ type: 'resetProfileConfig' });
  }

  function testModelConnection(target: 'orch' | 'comp' | 'ace' | 'worker') {
    // 设置测试中状态
    const statusKey = target === 'worker' ? workerModelTab : target;
    testStatus[statusKey] = 'testing';
    testStatus = { ...testStatus };

    // 后端使用 testWorkerConnection / testOrchestratorConnection / testCompressorConnection
    if (target === 'worker') {
      vscode.postMessage({ type: 'testWorkerConnection', worker: workerModelTab, config: workerConfigs[workerModelTab] });
    } else if (target === 'orch') {
      vscode.postMessage({ type: 'testOrchestratorConnection', config: orchConfig });
    } else if (target === 'comp') {
      vscode.postMessage({ type: 'testCompressorConnection', config: compConfig });
    }
  }

  // 重置测试状态（3秒后自动重置为 idle）
  function resetTestStatus(key: string) {
    setTimeout(() => {
      testStatus[key] = 'idle';
      testStatus = { ...testStatus };
    }, 3000);
  }

  function saveModelConfig(target: 'orch' | 'comp' | 'ace' | 'worker') {
    if (target === 'worker') {
      vscode.postMessage({ type: 'saveWorkerConfig', worker: workerModelTab, config: workerConfigs[workerModelTab] });
    } else if (target === 'orch') {
      vscode.postMessage({ type: 'saveOrchestratorConfig', config: orchConfig });
    } else if (target === 'comp') {
      vscode.postMessage({ type: 'saveCompressorConfig', config: compConfig });
    }
  }

  function confirmInputDialog() {
    if (inputDialogCallback && inputDialogValue.trim()) {
      inputDialogCallback(inputDialogValue.trim());
    }
    showInputDialog = false;
    inputDialogValue = '';
    inputDialogCallback = null;
  }

  function cancelInputDialog() {
    showInputDialog = false;
    inputDialogValue = '';
    inputDialogCallback = null;
  }

  // ============================================
  // MCP 服务器操作函数
  // ============================================

  function openMCPDialog(server: MCPServer | null = null) {
    currentEditingMCPServer = server;
    mcpDialogIsEdit = server !== null;
    const defaultJSON = `{
  "mcpServers": {
    "${server?.name || "mcp-server"}": {
      "command": "${server?.command || "npx"}",
      "args": ${server?.args ? JSON.stringify(server.args, null, 2) : `[
        "@modelcontextprotocol/server-filesystem",
        "/path/to/allowed/files"
      ]`},
      "env": ${server?.env ? JSON.stringify(server.env, null, 2) : `{}`}
    }
  }
}`;
    mcpDialogJson = defaultJSON;
    showMCPDialogState = true;
  }

  function closeMCPDialog() {
    showMCPDialogState = false;
    currentEditingMCPServer = null;
    mcpDialogJson = '';
  }

  function saveMCPServer() {
    const jsonText = mcpDialogJson.trim();
    if (!jsonText) {
      alert('请输入 MCP JSON 配置');
      return;
    }

    let parsed: any;
    try {
      parsed = JSON.parse(jsonText);
    } catch (error: any) {
      alert('JSON 格式错误：' + error.message);
      return;
    }

    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      alert('JSON 必须是对象');
      return;
    }

    const servers = parsed.mcpServers;
    if (!servers || typeof servers !== 'object' || Array.isArray(servers)) {
      alert('缺少 mcpServers 对象');
      return;
    }

    const serverNames = Object.keys(servers);
    if (serverNames.length === 0) {
      alert('mcpServers 不能为空');
      return;
    }

    if (serverNames.length > 1 && mcpDialogIsEdit) {
      alert('编辑模式仅支持一个服务器');
      return;
    }

    const saveServer = (name: string, cfg: any, isUpdate: boolean): boolean => {
      if (!cfg || typeof cfg !== 'object') {
        alert(`服务器 ${name} 配置无效`);
        return false;
      }
      const command = String(cfg.command || '').trim();
      if (!command) {
        alert(`服务器 ${name} 缺少 command`);
        return false;
      }

      const args = cfg.args ?? [];
      if (!Array.isArray(args)) {
        alert(`服务器 ${name} 的 args 必须是数组`);
        return false;
      }

      const env = cfg.env ?? {};
      if (typeof env !== 'object' || Array.isArray(env)) {
        alert(`服务器 ${name} 的 env 必须是对象`);
        return false;
      }

      const serverData = {
        name,
        command,
        args,
        env,
        enabled: cfg.enabled !== false,
        type: 'stdio'
      };

      if (isUpdate && currentEditingMCPServer) {
        vscode.postMessage({
          type: 'updateMCPServer',
          serverId: currentEditingMCPServer.id,
          updates: { ...serverData, id: currentEditingMCPServer.id }
        });
      } else {
        vscode.postMessage({
          type: 'addMCPServer',
          server: serverData
        });
      }

      return true;
    };

    let savedCount = 0;
    if (mcpDialogIsEdit && currentEditingMCPServer) {
      const name = serverNames[0];
      if (saveServer(name, servers[name], true)) savedCount += 1;
    } else {
      serverNames.forEach((name) => {
        if (saveServer(name, servers[name], false)) savedCount += 1;
      });
    }

    if (savedCount > 0) {
      vscode.postMessage({ type: 'loadMCPServers' });
      closeMCPDialog();
    }
  }

  function deleteMCPServer(serverId: string) {
    showConfirm('删除 MCP 服务器', '确定要删除此 MCP 服务器吗？', () => {
      vscode.postMessage({ type: 'deleteMCPServer', serverId });
    });
  }

  function toggleMCPServer(serverId: string, enabled: boolean) {
    const server = mcpServers.find(s => s.id === serverId);
    if (server) {
      vscode.postMessage({
        type: 'updateMCPServer',
        serverId,
        updates: { ...server, enabled: !enabled }
      });
    }
  }

  function toggleMCPExpand(serverId: string) {
    if (mcpExpandedServer === serverId) {
      mcpExpandedServer = null;
    } else {
      mcpExpandedServer = serverId;
      // 加载工具列表（如果尚未加载）
      if (!mcpServerTools[serverId]) {
        // 设置加载状态
        mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
        vscode.postMessage({ type: 'getMCPServerTools', serverId });
      }
    }
  }

  function refreshMCPTools(serverId: string) {
    // 设置刷新状态
    mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
    vscode.postMessage({ type: 'refreshMCPTools', serverId });
  }

  // 切换 MCP 工具描述展开状态
  function toggleMCPToolDesc(toolKey: string, e: Event) {
    e.stopPropagation();
    if (mcpExpandedTool === toolKey) {
      mcpExpandedTool = null;
    } else {
      mcpExpandedTool = toolKey;
    }
  }

  // ============================================
  // 仓库管理操作函数
  // ============================================

  function openRepoDialog() {
    showRepoDialogState = true;
    repoAddUrl = '';
    repositoriesLoading = true; // 设置加载状态
    vscode.postMessage({ type: 'loadRepositories' });
  }

  function closeRepoDialog() {
    showRepoDialogState = false;
    repoAddUrl = '';
    repoAddLoading = false;
    repositoriesLoading = false;
  }

  function addRepository() {
    const url = repoAddUrl.trim();
    if (!url) {
      alert('请输入仓库 URL');
      return;
    }
    repoAddLoading = true;
    vscode.postMessage({ type: 'addRepository', url });
  }

  function deleteRepository(repositoryId: string) {
    showConfirm('删除仓库', '确定要删除此仓库吗？', () => {
      vscode.postMessage({ type: 'deleteRepository', repositoryId });
    });
  }

  function refreshRepository(repositoryId: string) {
    vscode.postMessage({ type: 'refreshRepository', repositoryId });
  }

  // ============================================
  // Skill 库操作函数
  // ============================================

  function openSkillLibraryDialog() {
    showSkillLibraryDialogState = true;
    skillSearchQuery = '';
    skillLibraryLoading = true; // 设置加载状态
    vscode.postMessage({ type: 'loadSkillLibrary' });
  }

  function closeSkillLibraryDialog() {
    showSkillLibraryDialogState = false;
    skillSearchQuery = '';
    skillLibraryLoading = false;
  }

  function installSkill(skillFullName: string) {
    vscode.postMessage({ type: 'installSkill', skillId: skillFullName });
  }

  // 切换 Skill 启用状态
  function toggleSkill(skill: SkillItem) {
    if (skill.source === 'builtin') {
      // 内置工具使用 toggleBuiltInTool
      vscode.postMessage({ type: 'toggleBuiltInTool', tool: skill.name, enabled: !skill.enabled });
    } else if (skill.source === 'custom') {
      // 自定义工具暂不支持切换，只能删除
      return;
    } else if (skill.source === 'instruction') {
      // Instruction skill 暂不支持切换
      return;
    }
  }

  // 删除 Skill
  function deleteSkill(skill: SkillItem) {
    if (skill.source === 'builtin') {
      // 内置工具使用禁用
      showConfirm('禁用工具', `确定要禁用工具 "${skill.name}" 吗？`, () => {
        vscode.postMessage({ type: 'toggleBuiltInTool', tool: skill.name, enabled: false });
      });
    } else if (skill.source === 'custom') {
      // 删除自定义工具
      showConfirm('删除自定义工具', `确定要删除自定义工具 "${skill.name}" 吗？`, () => {
        vscode.postMessage({ type: 'removeCustomTool', toolName: skill.name });
      });
    } else if (skill.source === 'instruction') {
      // Instruction skill 删除
      showConfirm('删除 Instruction Skill', `确定要删除 Instruction Skill "${skill.name}" 吗？`, () => {
        vscode.postMessage({ type: 'removeInstructionSkill', skillName: skill.name });
      });
    }
  }

  // Skill 搜索过滤
  let filteredLibrarySkills = $derived(
    librarySkills.filter(skill => {
      if (!skillSearchQuery) return true;
      const query = skillSearchQuery.toLowerCase();
      const name = (skill.name || '').toLowerCase();
      const desc = (skill.description || '').toLowerCase();
      return name.includes(query) || desc.includes(query);
    })
  );

  // 按仓库分组
  let skillsByRepo = $derived(() => {
    const groups: Record<string, { name: string; skills: LibrarySkill[] }> = {};
    for (const skill of filteredLibrarySkills) {
      const repoId = skill.repositoryId || 'unknown';
      if (!groups[repoId]) {
        groups[repoId] = {
          name: skill.repositoryName || '未知仓库',
          skills: []
        };
      }
      groups[repoId].skills.push(skill);
    }
    return groups;
  });

  // 执行统计数据
  let executionStats = $state<Array<{
    worker: string;
    totalExecutions: number;
    successCount: number;
    failureCount: number;
    successRate: number;
    avgDuration: number;
    totalInputTokens: number;
    totalOutputTokens: number;
  }>>([]);

  // 监听来自扩展的状态更新
  $effect(() => {
    const handler = (event: MessageEvent) => {
      const message = event.data;

      // Worker 状态更新 (模型连接状态)
      if (message.type === 'workerStatusUpdate') {
        if (message.statuses) {
          modelStatuses = { ...modelStatuses, ...message.statuses };
        }
        isRefreshing = false;
      }
      // 执行统计更新
      else if (message.type === 'executionStatsUpdate') {
        if (message.stats) {
          executionStats = message.stats;
        }
        if (message.orchestratorStats) {
          totalTokens = (message.orchestratorStats.totalInputTokens || 0) + (message.orchestratorStats.totalOutputTokens || 0);
        }
      }
      // 用户信息
      else if (message.type === 'userInfo') {
        userInfo = message.data;
      }
      // 画像配置加载
      else if (message.type === 'profileConfig') {
        const config = message.config;
        if (config?.workers) {
          // 存储所有 workers 的配置
          for (const [worker, profile] of Object.entries(config.workers) as [string, any][]) {
            if (profile && allWorkerProfiles[worker]) {
              allWorkerProfiles[worker] = {
                role: profile.role || '',
                focus: profile.focus || [],
                constraints: profile.constraints || []
              };
            }
          }
          // 触发响应式更新
          allWorkerProfiles = { ...allWorkerProfiles };
        }
        if (config?.categories) {
          taskCategories = { ...taskCategories, ...config.categories };
        }
      }
      // 画像保存结果
      else if (message.type === 'profileConfigSaved') {
        // 保存成功/失败由 toast 消息处理
      }
      // Worker 配置加载
      else if (message.type === 'allWorkerConfigsLoaded') {
        if (message.configs) {
          // 将后端配置格式转换为前端格式
          const configs = message.configs;
          for (const [worker, config] of Object.entries(configs) as [string, any][]) {
            if (config && workerConfigs[worker]) {
              workerConfigs[worker] = {
                baseUrl: config.baseUrl || '',
                apiKey: config.apiKey || '',
                model: config.model || '',
                provider: config.provider || 'anthropic',
                enabled: config.enabled !== false
              };
            }
          }
        }
      }
      // 编排者配置加载
      else if (message.type === 'orchestratorConfigLoaded') {
        if (message.config) {
          orchConfig = {
            baseUrl: message.config.baseUrl || '',
            apiKey: message.config.apiKey || '',
            model: message.config.model || '',
            provider: message.config.provider || 'anthropic'
          };
        }
      }
      // 压缩器配置加载
      else if (message.type === 'compressorConfigLoaded') {
        if (message.config) {
          compConfig = {
            baseUrl: message.config.baseUrl || '',
            apiKey: message.config.apiKey || '',
            model: message.config.model || '',
            provider: message.config.provider || 'anthropic'
          };
        }
      }
      // Worker 连接测试结果 - 更新统计 Tab 状态和测试按钮状态
      else if (message.type === 'workerConnectionTestResult') {
        const worker = message.worker;
        if (worker && modelStatuses[worker]) {
          if (message.success) {
            modelStatuses[worker] = {
              status: 'available',
              model: workerConfigs[worker]?.model || modelStatuses[worker]?.model
            };
            testStatus[worker] = 'success';
          } else {
            modelStatuses[worker] = {
              status: 'error',
              model: workerConfigs[worker]?.model || modelStatuses[worker]?.model,
              error: message.error
            };
            testStatus[worker] = 'error';
          }
          modelStatuses = { ...modelStatuses };
          testStatus = { ...testStatus };
          resetTestStatus(worker);
        }
      }
      // 编排者连接测试结果 - 更新统计 Tab 状态和测试按钮状态
      else if (message.type === 'orchestratorConnectionTestResult') {
        if (message.success) {
          modelStatuses.orchestrator = {
            status: 'available',
            model: orchConfig.model || modelStatuses.orchestrator?.model
          };
          testStatus.orch = 'success';
        } else {
          modelStatuses.orchestrator = {
            status: 'error',
            model: orchConfig.model || modelStatuses.orchestrator?.model,
            error: message.error
          };
          testStatus.orch = 'error';
        }
        modelStatuses = { ...modelStatuses };
        testStatus = { ...testStatus };
        resetTestStatus('orch');
      }
      // 压缩器连接测试结果 - 更新统计 Tab 状态和测试按钮状态
      else if (message.type === 'compressorConnectionTestResult') {
        if (message.success) {
          modelStatuses.compressor = {
            status: 'available',
            model: compConfig.model || modelStatuses.compressor?.model
          };
          testStatus.comp = 'success';
        } else {
          modelStatuses.compressor = {
            status: 'error',
            model: compConfig.model || modelStatuses.compressor?.model,
            error: message.error
          };
          testStatus.comp = 'error';
        }
        modelStatuses = { ...modelStatuses };
        testStatus = { ...testStatus };
        resetTestStatus('comp');
      }
      // MCP 服务器列表加载
      else if (message.type === 'mcpServersLoaded') {
        const servers = ensureArray<any>(message.servers);
        mcpServers = servers.map((s: any) => ({
          id: s.id || s.name,
          name: s.name || s.id,
          command: s.command || '',
          args: s.args || [],
          env: s.env || {},
          enabled: s.enabled !== false,
          connected: s.connected || false,
          error: s.error
        }));
      }
      // MCP 服务器添加成功
      else if (message.type === 'mcpServerAdded') {
        vscode.postMessage({ type: 'loadMCPServers' });
      }
      // MCP 服务器更新成功
      else if (message.type === 'mcpServerUpdated') {
        vscode.postMessage({ type: 'loadMCPServers' });
      }
      // MCP 服务器删除成功
      else if (message.type === 'mcpServerDeleted') {
        mcpServers = mcpServers.filter(s => s.id !== message.serverId);
      }
      // MCP 工具列表加载（首次获取）
      else if (message.type === 'mcpServerTools') {
        if (message.serverId) {
          const tools = ensureArray(message.tools);
          mcpServerTools = { ...mcpServerTools, [message.serverId]: tools };
          // 清除刷新状态
          const newSet = new Set(mcpRefreshingServers);
          newSet.delete(message.serverId);
          mcpRefreshingServers = newSet;
        }
      }
      // MCP 工具列表刷新
      else if (message.type === 'mcpToolsRefreshed') {
        if (message.serverId) {
          const tools = ensureArray(message.tools);
          mcpServerTools = { ...mcpServerTools, [message.serverId]: tools };
          // 清除刷新状态
          const newSet = new Set(mcpRefreshingServers);
          newSet.delete(message.serverId);
          mcpRefreshingServers = newSet;
        }
      }
      // Skills 配置加载
      else if (message.type === 'skillsConfigLoaded') {
        const skillList: SkillItem[] = [];
        // 内置工具 - 显示所有工具（包括禁用的）
        if (message.config?.builtInTools) {
          for (const [id, tool] of Object.entries(message.config.builtInTools) as [string, any][]) {
            skillList.push({
              name: id,
              description: tool?.description || '',
              source: 'builtin',
              enabled: !!tool?.enabled
            });
          }
        }
        // 自定义工具
        if (Array.isArray(message.config?.customTools)) {
          for (const tool of message.config.customTools) {
            skillList.push({ name: tool.name, description: tool.description || '', source: 'custom', enabled: true });
          }
        }
        // Instruction Skills
        if (Array.isArray(message.config?.instructionSkills)) {
          for (const skill of message.config.instructionSkills) {
            skillList.push({ name: skill.name, description: skill.description || '', source: 'instruction', enabled: true });
          }
        }
        skills = skillList;
      }
      // 仓库列表加载
      else if (message.type === 'repositoriesLoaded') {
        const repoList = ensureArray<any>(message.repositories);
        repositories = repoList.map((r: any) => ({
          id: r.id,
          url: r.url,
          name: r.name || r.url,
          skillCount: r.skillCount || 0,
          lastUpdated: r.lastUpdated
        }));
        repoAddLoading = false;
        repositoriesLoading = false; // 清除加载状态
      }
      // 仓库添加成功
      else if (message.type === 'repositoryAdded') {
        vscode.postMessage({ type: 'loadRepositories' });
        repoAddLoading = false;
        repoAddUrl = '';
      }
      // 仓库删除成功
      else if (message.type === 'repositoryDeleted') {
        repositories = repositories.filter(r => r.id !== message.repositoryId);
      }
      // 仓库刷新成功
      else if (message.type === 'repositoryRefreshed') {
        vscode.postMessage({ type: 'loadRepositories' });
      }
      // Skill 库加载
      else if (message.type === 'skillLibraryLoaded') {
        const skillsList = ensureArray<any>(message.skills);
        librarySkills = skillsList.map((s: any) => ({
          name: s.name,
          fullName: s.fullName || s.name,
          description: s.description || '',
          author: s.author,
          version: s.version,
          category: s.category,
          skillType: s.skillType,
          repositoryId: s.repositoryId,
          repositoryName: s.repositoryName,
          installed: s.installed || false,
          icon: s.icon
        }));
        skillLibraryLoading = false; // 清除加载状态
      }
      // Skill 安装成功
      else if (message.type === 'skillInstalled') {
        vscode.postMessage({ type: 'loadSkillsConfig' });
        vscode.postMessage({ type: 'loadSkillLibrary' });
        showSkillLibraryDialogState = false;
      }
      // 内置工具切换成功
      else if (message.type === 'builtInToolToggled') {
        vscode.postMessage({ type: 'loadSkillsConfig' });
      }
      // 自定义工具删除成功
      else if (message.type === 'customToolRemoved') {
        vscode.postMessage({ type: 'loadSkillsConfig' });
      }
    };

    window.addEventListener('message', handler);

    // 初始化请求数据
    vscode.postMessage({ type: 'checkWorkerStatus', force: false });
    vscode.postMessage({ type: 'requestExecutionStats' });
    vscode.postMessage({ type: 'getProfileConfig' });
    vscode.postMessage({ type: 'loadAllWorkerConfigs' });
    vscode.postMessage({ type: 'loadOrchestratorConfig' });
    vscode.postMessage({ type: 'loadCompressorConfig' });
    vscode.postMessage({ type: 'loadMCPServers' });
    vscode.postMessage({ type: 'loadSkillsConfig' });
    vscode.postMessage({ type: 'loadRepositories' });

    return () => window.removeEventListener('message', handler);
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="settings-overlay" onclick={closeSettings}>
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="settings-panel" onclick={(e) => e.stopPropagation()}>
    <div class="settings-header">
      <span class="settings-title">设置</span>
      {#if userInfo}
        <div class="logout-section">
          <span class="user-info-text">{userInfo}</span>
          <button class="settings-btn secondary logout-btn" onclick={logout}>退出</button>
        </div>
      {/if}
      <button class="btn-icon btn-icon--sm" onclick={closeSettings} title="关闭设置">
        <Icon name="close" size={14} />
      </button>
    </div>

    <!-- Tab 切换栏 -->
    <div class="settings-tabs">
      <button class="settings-tab" class:active={activeTab === 'stats'} onclick={() => activeTab = 'stats'}>
        <Icon name="stats" size={14} />
        统计
      </button>
      <button class="settings-tab" class:active={activeTab === 'model'} onclick={() => activeTab = 'model'}>
        <Icon name="model" size={14} />
        模型
      </button>
      <button class="settings-tab" class:active={activeTab === 'profile'} onclick={() => activeTab = 'profile'}>
        <Icon name="profile" size={14} />
        画像
      </button>
      <button class="settings-tab" class:active={activeTab === 'tools'} onclick={() => activeTab = 'tools'}>
        <Icon name="tools" size={14} />
        工具
      </button>
    </div>

    <!-- Tab 内容区域 -->
    <div class="settings-tab-content">
      {#if activeTab === 'stats'}
        <!-- 统计 Tab -->
        <div class="settings-section stats-section">
          <div class="settings-section-header">
            <div class="settings-section-title">模型状态与统计</div>
            <div class="settings-section-actions">
              <div class="settings-summary-chip">总 Token: {formatTokens(totalTokens)}</div>
              <button class="model-refresh-btn" class:loading={isRefreshing} onclick={refreshConnections} disabled={isRefreshing}>
                <Icon name="refresh" size={14} />
                {isRefreshing ? '检测中...' : '检测'}
              </button>
              <button class="settings-btn secondary" onclick={showResetConfirmDialog}>重置 Token</button>
            </div>
          </div>

          <div class="model-connection-list">
            {#each ['orchestrator', 'compressor', 'claude', 'codex', 'gemini'] as worker}
              {@const status = modelStatuses[worker]}
              {@const workerStats = getWorkerStats(worker)}
              {@const statusClass = getStatusClass(status?.status || 'checking')}
              <div class="model-connection-item {statusClass}" data-worker={worker}>
                <div class="model-connection-icon {worker}">
                  <Icon name="circle" size={14} />
                </div>
                <div class="model-connection-info">
                  <div class="model-connection-header">
                    <span class="model-connection-name">
                      {worker === 'orchestrator' ? '编排模型' : worker === 'compressor' ? '压缩模型' : worker.charAt(0).toUpperCase() + worker.slice(1)}
                      {#if worker === 'orchestrator' || worker === 'compressor'}
                        <span class="required-badge">必需</span>
                      {/if}
                    </span>
                    <span class="model-connection-badge {statusClass}">{statusTexts[status?.status] || statusTexts['checking']}</span>
                  </div>
                  <div class="model-connection-model">{status?.model || '未配置'}</div>
                  <div class="model-connection-stats-inline">
                    <span>任务: {workerStats?.totalExecutions ?? '--'}</span>
                    <span class="stats-divider">|</span>
                    <span>成功率: {workerStats?.successRate != null ? `${Math.round(workerStats.successRate * 100)}%` : '--'}</span>
                    <span class="stats-divider">|</span>
                    <span>Token: {formatTokens((workerStats?.totalInputTokens ?? 0) + (workerStats?.totalOutputTokens ?? 0))}</span>
                  </div>
                </div>
              </div>
            {/each}
          </div>
        </div>
      {:else if activeTab === 'model'}
        <!-- 模型配置 Tab -->
        <div class="model-config-grid">
          <div class="model-config-stack">
            <div class="model-config-tabs">
              <button class="model-config-tab" class:active={modelConfigTab === 'orch'} onclick={() => modelConfigTab = 'orch'}>编排模型</button>
              <button class="model-config-tab" class:active={modelConfigTab === 'comp'} onclick={() => modelConfigTab = 'comp'}>压缩模型</button>
              <button class="model-config-tab" class:active={modelConfigTab === 'ace'} onclick={() => modelConfigTab = 'ace'}>ACE配置</button>
            </div>

            {#if modelConfigTab === 'orch'}
              <div class="model-config-card">
                <div class="model-config-header">
                  <div class="model-config-title">编排模型</div>
                  <div class="model-config-desc">驱动任务规划与协调</div>
                </div>
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <div class="llm-config-form">
                  <div class="llm-config-field">
                    <label class="llm-config-label">Base URL</label>
                    <input type="text" class="llm-config-input" bind:value={orchConfig.baseUrl} placeholder="https://api.anthropic.com">
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">API Key</label>
                    <input type="password" class="llm-config-input" bind:value={orchConfig.apiKey} placeholder="sk-ant-...">
                  </div>
                  <div class="llm-config-field-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">Model</label>
                      <input type="text" class="llm-config-input" bind:value={orchConfig.model} placeholder="claude-3-5-sonnet-20241022">
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">Provider</label>
                      <select class="llm-config-select" bind:value={orchConfig.provider}>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                      </select>
                    </div>
                  </div>
                  <div class="llm-config-actions">
                    <button class="llm-config-save-btn" onclick={() => saveModelConfig('orch')}>保存配置</button>
                    <button
                      class="llm-config-test-btn"
                      class:testing={testStatus.orch === 'testing'}
                      class:success={testStatus.orch === 'success'}
                      class:error={testStatus.orch === 'error'}
                      onclick={() => testModelConnection('orch')}
                      disabled={testStatus.orch === 'testing'}
                    >
                      {#if testStatus.orch === 'testing'}
                        <Icon name="refresh" size={14} />
                        测试中...
                      {:else if testStatus.orch === 'success'}
                        <Icon name="check" size={14} />
                        连接成功
                      {:else if testStatus.orch === 'error'}
                        <Icon name="close" size={14} />
                        连接失败
                      {:else}
                        <Icon name="check" size={14} />
                        测试连接
                      {/if}
                    </button>
                  </div>
                </div>
              </div>
            {:else if modelConfigTab === 'comp'}
              <div class="model-config-card">
                <div class="model-config-header">
                  <div class="model-config-title">压缩模型</div>
                  <div class="model-config-desc">用于上下文压缩与知识提取</div>
                </div>
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <div class="llm-config-form">
                  <div class="llm-config-field">
                    <label class="llm-config-label">Base URL</label>
                    <input type="text" class="llm-config-input" bind:value={compConfig.baseUrl} placeholder="https://api.anthropic.com">
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">API Key</label>
                    <input type="password" class="llm-config-input" bind:value={compConfig.apiKey} placeholder="sk-ant-...">
                  </div>
                  <div class="llm-config-field-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">Model</label>
                      <input type="text" class="llm-config-input" bind:value={compConfig.model} placeholder="claude-3-haiku-20240307">
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">Provider</label>
                      <select class="llm-config-select" bind:value={compConfig.provider}>
                        <option value="openai">OpenAI</option>
                        <option value="anthropic">Anthropic</option>
                      </select>
                    </div>
                  </div>
                  <div class="llm-config-actions">
                    <button class="llm-config-save-btn" onclick={() => saveModelConfig('comp')}>保存配置</button>
                    <button
                      class="llm-config-test-btn"
                      class:testing={testStatus.comp === 'testing'}
                      class:success={testStatus.comp === 'success'}
                      class:error={testStatus.comp === 'error'}
                      onclick={() => testModelConnection('comp')}
                      disabled={testStatus.comp === 'testing'}
                    >
                      {#if testStatus.comp === 'testing'}
                        <Icon name="refresh" size={14} />
                        测试中...
                      {:else if testStatus.comp === 'success'}
                        <Icon name="check" size={14} />
                        连接成功
                      {:else if testStatus.comp === 'error'}
                        <Icon name="close" size={14} />
                        连接失败
                      {:else}
                        <Icon name="check" size={14} />
                        测试连接
                      {/if}
                    </button>
                  </div>
                </div>
              </div>
            {:else if modelConfigTab === 'ace'}
              <div class="model-config-card">
                <div class="model-config-header">
                  <div class="model-config-title">ACE 配置</div>
                  <div class="model-config-desc">Augment 工具的接口与密钥配置</div>
                </div>
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <div class="llm-config-form">
                  <div class="llm-config-field">
                    <label class="llm-config-label">API 地址</label>
                    <input type="text" class="llm-config-input" bind:value={aceConfig.url} placeholder="https://api.example.com/v1">
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">API 密钥</label>
                    <input type="password" class="llm-config-input" bind:value={aceConfig.key} placeholder="sk-...">
                  </div>
                  <div class="llm-config-actions">
                    <button class="llm-config-save-btn" onclick={() => saveModelConfig('ace')}>保存配置</button>
                    <button
                      class="llm-config-test-btn"
                      class:testing={testStatus.ace === 'testing'}
                      class:success={testStatus.ace === 'success'}
                      class:error={testStatus.ace === 'error'}
                      onclick={() => testModelConnection('ace')}
                      disabled={testStatus.ace === 'testing'}
                    >
                      {#if testStatus.ace === 'testing'}
                        <Icon name="refresh" size={14} />
                        测试中...
                      {:else if testStatus.ace === 'success'}
                        <Icon name="check" size={14} />
                        连接成功
                      {:else if testStatus.ace === 'error'}
                        <Icon name="close" size={14} />
                        连接失败
                      {:else}
                        <Icon name="check" size={14} />
                        测试连接
                      {/if}
                    </button>
                  </div>
                </div>
              </div>
            {/if}
          </div>

          <!-- Worker 模型配置 -->
          <div class="model-config-card">
            <div class="model-config-header-row">
              <div class="model-config-title">Worker 模型</div>
              <div class="worker-model-tabs">
                <button class="worker-model-tab" class:active={workerModelTab === 'claude'} onclick={() => workerModelTab = 'claude'}>
                  <span class="worker-dot claude"></span>
                  Claude
                </button>
                <button class="worker-model-tab" class:active={workerModelTab === 'codex'} onclick={() => workerModelTab = 'codex'}>
                  <span class="worker-dot codex"></span>
                  Codex
                </button>
                <button class="worker-model-tab" class:active={workerModelTab === 'gemini'} onclick={() => workerModelTab = 'gemini'}>
                  <span class="worker-dot gemini"></span>
                  Gemini
                </button>
              </div>
            </div>
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <div class="llm-config-form">
              <div class="llm-config-field">
                <label class="llm-config-label">Base URL</label>
                <input type="text" class="llm-config-input" bind:value={workerConfigs[workerModelTab].baseUrl} placeholder="https://api.anthropic.com">
              </div>
              <div class="llm-config-field">
                <label class="llm-config-label">API Key</label>
                <input type="password" class="llm-config-input" bind:value={workerConfigs[workerModelTab].apiKey} placeholder="sk-ant-...">
              </div>
              <div class="llm-config-field-row inline-toggle-row">
                <div class="llm-config-field">
                  <label class="llm-config-label">Model</label>
                  <input type="text" class="llm-config-input" bind:value={workerConfigs[workerModelTab].model} placeholder="claude-3-5-sonnet-20241022">
                </div>
                <div class="llm-config-field">
                  <label class="llm-config-label">Provider</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].provider}>
                    <option value="openai">OpenAI</option>
                    <option value="anthropic">Anthropic</option>
                  </select>
                </div>
                <div class="llm-config-field inline-toggle">
                  <label class="llm-config-label">状态</label>
                  <button type="button" class="llm-config-toggle-btn" onclick={() => workerConfigs[workerModelTab].enabled = !workerConfigs[workerModelTab].enabled}>
                    <span class="toggle-switch" class:active={workerConfigs[workerModelTab].enabled}></span>
                    <span>启用</span>
                  </button>
                </div>
              </div>
              <div class="llm-config-actions">
                <button class="llm-config-save-btn" onclick={() => saveModelConfig('worker')}>保存配置</button>
                <button
                  class="llm-config-test-btn"
                  class:testing={testStatus[workerModelTab] === 'testing'}
                  class:success={testStatus[workerModelTab] === 'success'}
                  class:error={testStatus[workerModelTab] === 'error'}
                  onclick={() => testModelConnection('worker')}
                  disabled={testStatus[workerModelTab] === 'testing'}
                >
                  {#if testStatus[workerModelTab] === 'testing'}
                    <Icon name="refresh" size={14} />
                    测试中...
                  {:else if testStatus[workerModelTab] === 'success'}
                    <Icon name="check" size={14} />
                    连接成功
                  {:else if testStatus[workerModelTab] === 'error'}
                    <Icon name="close" size={14} />
                    连接失败
                  {:else}
                    <Icon name="check" size={14} />
                    测试连接
                  {/if}
                </button>
              </div>
            </div>
          </div>
        </div>
      {:else if activeTab === 'profile'}
        <!-- 画像配置 Tab -->
        <!-- Worker 画像卡片 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">Worker 画像配置</div>
            <div class="profile-worker-tabs">
              <button class="profile-worker-tab" class:active={profileWorker === 'claude'} onclick={() => profileWorker = 'claude'}>
                <span class="profile-worker-dot claude"></span>
                Claude
              </button>
              <button class="profile-worker-tab" class:active={profileWorker === 'codex'} onclick={() => profileWorker = 'codex'}>
                <span class="profile-worker-dot codex"></span>
                Codex
              </button>
              <button class="profile-worker-tab" class:active={profileWorker === 'gemini'} onclick={() => profileWorker = 'gemini'}>
                <span class="profile-worker-dot gemini"></span>
                Gemini
              </button>
            </div>
          </div>

          <!-- Worker 画像编辑区 -->
          <div class="profile-editor">
            <!-- 角色定位 -->
            <div class="profile-field">
              <div class="profile-field-label">角色定位</div>
              <textarea
                class="profile-textarea"
                value={profileRole}
                oninput={(e) => updateProfileRole((e.target as HTMLTextAreaElement).value)}
                placeholder="描述该 Worker 的角色定位..."
              ></textarea>
            </div>

            <!-- 两列布局：专注领域 + 行为约束 -->
            <div class="profile-two-columns">
              <!-- 专注领域 -->
              <div class="profile-field">
                <div class="profile-field-header">
                  <span class="profile-field-label">专注领域</span>
                  <button class="profile-add-btn" onclick={addFocusArea}>
                    <Icon name="plus" size={12} />
                  </button>
                </div>
                <div class="profile-tags">
                  {#each profileFocusAreas as area, i}
                    <span class="profile-tag">
                      {area}
                      <button class="profile-tag-remove" onclick={() => removeFocusArea(i)}>
                        <Icon name="close" size={10} />
                      </button>
                    </span>
                  {/each}
                  {#if profileFocusAreas.length === 0}
                    <span class="profile-tags-empty">点击 + 添加专注领域</span>
                  {/if}
                </div>
              </div>

              <!-- 行为约束 -->
              <div class="profile-field">
                <div class="profile-field-header">
                  <span class="profile-field-label">行为约束</span>
                  <button class="profile-add-btn" onclick={addConstraint}>
                    <Icon name="plus" size={12} />
                  </button>
                </div>
                <div class="profile-tags">
                  {#each profileConstraints as constraint, i}
                    <span class="profile-tag">
                      {constraint}
                      <button class="profile-tag-remove" onclick={() => removeConstraint(i)}>
                        <Icon name="close" size={10} />
                      </button>
                    </span>
                  {/each}
                  {#if profileConstraints.length === 0}
                    <span class="profile-tags-empty">点击 + 添加行为约束</span>
                  {/if}
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 全局任务分类配置 -->
        <div class="settings-section" style="margin-top: var(--space-4);">
          <div class="settings-section-header">
            <div class="settings-section-title">任务分类默认配置</div>
            <div class="settings-section-actions">
              <button class="settings-btn primary" onclick={saveProfile}>
                保存配置
              </button>
              <button class="settings-btn secondary" onclick={resetProfile}>重置</button>
            </div>
          </div>
          <div class="settings-section-desc">配置存储：<code>~/.multicli/</code></div>
          <div class="profile-categories-grid">
            {#each Object.entries(categoryLabels) as [category, label]}
              <div class="profile-category-row">
                <span class="profile-category-label">{label}</span>
                <select class="profile-category-select" bind:value={taskCategories[category]}>
                  <option value="claude">Claude</option>
                  <option value="codex">Codex</option>
                  <option value="gemini">Gemini</option>
                </select>
              </div>
            {/each}
          </div>
        </div>
      {:else if activeTab === 'tools'}
        <!-- 工具 Tab -->
        <!-- MCP 工具 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">MCP 工具</div>
            <button class="settings-btn primary" onclick={() => openMCPDialog(null)}>
              <Icon name="plus" size={14} />
              <span>添加服务器</span>
            </button>
          </div>
          <div class="settings-section-desc">通过 JSON 配置 MCP 服务器，自动解析和使用工具</div>
          <div class="mcp-server-list">
            {#if mcpServers.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>暂无 MCP 服务器</p>
                <p class="empty-state-hint">点击"添加服务器"开始配置</p>
              </div>
            {:else}
              {#each mcpServers as server (server.id)}
                <div class="mcp-server-item" class:expanded={mcpExpandedServer === server.id}>
                  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                  <div class="mcp-server-header" onclick={() => toggleMCPExpand(server.id)}>
                    <div class="mcp-server-info">
                      <div class="mcp-server-name">{server.name}</div>
                      <div class="mcp-server-command">{server.command || ''}</div>
                    </div>
                    <div class="mcp-server-actions">
                      <span class="mcp-server-badge" class:enabled={server.enabled} class:disabled={!server.enabled}>
                        {server.enabled ? '已启用' : '已禁用'}
                      </span>
                      <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                      <span onclick={(e) => e.stopPropagation()}>
                        <Toggle
                          checked={server.enabled}
                          title={server.enabled ? '点击禁用' : '点击启用'}
                          onchange={() => toggleMCPServer(server.id, server.enabled)}
                        />
                      </span>
                      <button class="btn-icon btn-icon--sm" title="编辑"
                        onclick={(e) => { e.stopPropagation(); openMCPDialog(server); }}>
                        <Icon name="edit" size={14} />
                      </button>
                      <button class="btn-icon btn-icon--sm btn-icon--danger" title="删除"
                        onclick={(e) => { e.stopPropagation(); deleteMCPServer(server.id); }}>
                        <Icon name="trash" size={14} />
                      </button>
                      <span class="mcp-expand-icon" class:expanded={mcpExpandedServer === server.id}>
                        <Icon name="chevronDown" size={14} />
                      </span>
                    </div>
                  </div>
                  {#if mcpExpandedServer === server.id}
                    <div class="mcp-tools-panel">
                      <div class="mcp-tools-header">
                        <span>工具列表 {mcpServerTools[server.id]?.length ? `(${mcpServerTools[server.id].length})` : ''}</span>
                        <button class="btn-icon btn-icon--sm" class:refreshing={mcpRefreshingServers.has(server.id)} title="刷新工具"
                          onclick={() => refreshMCPTools(server.id)} disabled={mcpRefreshingServers.has(server.id)}>
                          <Icon name="refresh" size={14} />
                        </button>
                      </div>
                      <div class="mcp-tools-list">
                        {#if mcpRefreshingServers.has(server.id)}
                          <div class="mcp-tools-empty">加载中...</div>
                        {:else if mcpServerTools[server.id] && mcpServerTools[server.id].length > 0}
                          {#each mcpServerTools[server.id] as tool, toolIndex}
                            {@const toolKey = `${server.id}-${toolIndex}`}
                            <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
                            <div class="mcp-tool-item" class:show-desc={mcpExpandedTool === toolKey}>
                              <div class="mcp-tool-row">
                                <div class="mcp-tool-name">{tool.name}</div>
                                {#if tool.description}
                                  <button class="mcp-tool-desc-btn" title="查看描述" onclick={(e) => toggleMCPToolDesc(toolKey, e)}>
                                    <Icon name="info" size={14} />
                                  </button>
                                {/if}
                              </div>
                              {#if tool.description}
                                <div class="mcp-tool-desc">{tool.description}</div>
                                <div class="mcp-tool-desc-pop">{tool.description}</div>
                              {/if}
                            </div>
                          {/each}
                        {:else}
                          <div class="mcp-tools-empty">暂无工具，点击刷新按钮加载</div>
                        {/if}
                      </div>
                    </div>
                  {/if}
                </div>
              {/each}
            {/if}
          </div>
        </div>

        <!-- Claude Skills 工具 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">Claude Skills 工具</div>
            <div class="settings-btn-group">
              <button class="settings-btn primary" onclick={() => openSkillLibraryDialog()}>
                <Icon name="plus" size={14} />
                <span>安装 Skill</span>
              </button>
              <button class="settings-btn secondary" onclick={() => openRepoDialog()}>
                <Icon name="grid" size={14} />
                <span>管理技能仓库</span>
              </button>
            </div>
          </div>
          <div class="settings-section-desc">通过 Skill 库安装的技能工具，由 Anthropic 提供</div>
          <div class="skills-tool-list">
            {#if skills.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>暂无已安装的 Skill</p>
                <p class="empty-state-hint">点击"安装 Skill"从库中安装</p>
              </div>
            {:else}
              {#each skills as skill}
                <div class="skill-item">
                  <div class="skill-info">
                    <div class="skill-name">{skill.name}</div>
                    <div class="skill-desc" title={skill.description}>{skill.description}</div>
                  </div>
                  <div class="skill-actions">
                    <span class="skill-source-badge" class:builtin={skill.source === 'builtin'} class:custom={skill.source === 'custom'} class:instruction={skill.source === 'instruction'}>
                      {skill.source === 'builtin' ? '内置' : skill.source === 'custom' ? '自定义' : 'Instruction'}
                    </span>
                    {#if skill.source === 'builtin'}
                      <Toggle
                        checked={skill.enabled}
                        title={skill.enabled ? '点击禁用' : '点击启用'}
                        onchange={() => toggleSkill(skill)}
                      />
                    {:else}
                      <button class="btn-icon btn-icon--sm btn-icon--danger" title="删除" onclick={() => deleteSkill(skill)}>
                        <Icon name="trash" size={14} />
                      </button>
                    {/if}
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </div>

        <!-- 终端工具 -->
        <div class="settings-section">
          <div class="settings-section-title">终端工具</div>
          <div class="settings-section-desc">在 VS Code 终端中执行命令，提供可视化的执行过程</div>
          <div class="builtin-tool-list">
            <div class="builtin-tool-item">
              <div class="builtin-tool-icon">
                <Icon name="tools" size={20} />
              </div>
              <div class="builtin-tool-info">
                <div class="builtin-tool-name">VSCode 终端执行器</div>
                <div class="builtin-tool-desc">在 VS Code 终端中可视化执行命令</div>
              </div>
              <div class="builtin-tool-badge enabled">已启用</div>
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<!-- 输入对话框 -->
{#if showInputDialog}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" onclick={cancelInputDialog} onkeydown={(e) => e.key === 'Escape' && cancelInputDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog--sm" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{inputDialogTitle}</h3>
      </div>
      <div class="modal-body">
        <div class="form-field">
          <input type="text" bind:value={inputDialogValue} placeholder="请输入内容...">
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={cancelInputDialog}>取消</button>
        <button class="settings-btn primary" onclick={confirmInputDialog}>确定</button>
      </div>
    </div>
  </div>
{/if}

<!-- MCP 对话框 -->
{#if showMCPDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={closeMCPDialog} onkeydown={(e) => e.key === 'Escape' && closeMCPDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{mcpDialogIsEdit ? '编辑 MCP 服务器' : '添加 MCP 服务器'}</h3>
        <button class="modal-close" onclick={closeMCPDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="form-field">
          <label for="mcp-json">MCP 服务器 JSON</label>
          <textarea id="mcp-json" rows="12" placeholder="粘贴 MCP JSON 配置" bind:value={mcpDialogJson}></textarea>
          <div class="form-help">
            支持格式：{'{'} "mcpServers": {'{'} "name": {'{'} "command": "...", "args": [...], "env": {'{'} ... {'}'} {'}'} {'}'} {'}'}
          </div>
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeMCPDialog}>取消</button>
        <button class="settings-btn primary" onclick={saveMCPServer}>保存</button>
      </div>
    </div>
  </div>
{/if}

<!-- 仓库管理对话框 -->
{#if showRepoDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={closeRepoDialog} onkeydown={(e) => e.key === 'Escape' && closeRepoDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog-lg" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>管理技能仓库</h3>
        <button class="modal-close" onclick={closeRepoDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="repo-add-section">
          <div class="repo-add-form">
            <div class="form-field" style="flex: 1; margin-bottom: 0;">
              <label for="repo-url">仓库 URL</label>
              <input type="text" id="repo-url" placeholder="https://example.com/skills.json" bind:value={repoAddUrl}>
            </div>
            <button class="settings-btn primary" onclick={addRepository} disabled={repoAddLoading}>
              <Icon name="plus" size={14} />
              <span>{repoAddLoading ? '添加中' : '添加'}</span>
            </button>
          </div>
        </div>
        <div class="repo-list-title">已添加的仓库</div>
        <div class="repo-manage-list">
          {#if repositoriesLoading}
            <div class="loading-state">
              <Icon name="refresh" size={24} />
              <span>加载中...</span>
            </div>
          {:else if repositories.length === 0}
            <div class="empty-state-sm">暂无仓库</div>
          {:else}
            {#each repositories as repo (repo.id)}
              <div class="repo-item">
                <div class="repo-info">
                  <div class="repo-name">{repo.name || repo.url}</div>
                  <div class="repo-url">{repo.url}</div>
                  {#if repo.skillCount}
                    <div class="repo-meta">{repo.skillCount} 个技能</div>
                  {/if}
                </div>
                <div class="repo-actions">
                  <button class="btn-icon btn-icon--sm" title="刷新" onclick={() => refreshRepository(repo.id)}>
                    <Icon name="refresh" size={14} />
                  </button>
                  <button class="btn-icon btn-icon--sm" title="删除" onclick={() => deleteRepository(repo.id)}>
                    <Icon name="close" size={14} />
                  </button>
                </div>
              </div>
            {/each}
          {/if}
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeRepoDialog}>关闭</button>
      </div>
    </div>
  </div>
{/if}

<!-- Skill 库对话框 -->
{#if showSkillLibraryDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={closeSkillLibraryDialog} onkeydown={(e) => e.key === 'Escape' && closeSkillLibraryDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog-lg" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>Skill 库</h3>
        <button class="modal-close" onclick={closeSkillLibraryDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="skill-library-search">
          <input type="text" placeholder="搜索 Skill..." bind:value={skillSearchQuery}>
        </div>
        <div class="skill-library-list">
          {#if skillLibraryLoading}
            <div class="loading-state">
              <Icon name="refresh" size={32} />
              <span>正在加载技能列表...</span>
            </div>
          {:else if filteredLibrarySkills.length === 0}
            <div class="empty-state">
              <Icon name="tools" size={48} />
              <p>暂无可用的 Skill</p>
              <p class="empty-state-hint">请先添加 Skill 仓库</p>
            </div>
          {:else}
            {#each Object.entries(skillsByRepo()) as [_, repoData]}
              <div class="skill-repo-group">
                <div class="skill-repo-title">{repoData.name} ({repoData.skills.length} 个技能)</div>
                {#each repoData.skills as skill}
                  <div class="skill-library-item">
                    <div class="skill-library-icon">
                      <Icon name="tools" size={14} />
                    </div>
                    <div class="skill-library-info">
                      <div class="skill-library-name">{skill.name}</div>
                      <div class="skill-library-desc" title={skill.description || ''}>{skill.description || ''}</div>
                      {#if skill.author || skill.version || skill.category}
                        <div class="skill-library-meta">
                          {#if skill.author}<span class="skill-library-meta-item">作者: {skill.author}</span>{/if}
                          {#if skill.version}<span class="skill-library-meta-item">版本: {skill.version}</span>{/if}
                          {#if skill.category}<span class="skill-library-meta-item">分类: {skill.category}</span>{/if}
                        </div>
                      {/if}
                    </div>
                    <div class="skill-library-actions">
                      <button class="settings-btn" class:primary={!skill.installed} onclick={() => installSkill(skill.fullName)} disabled={skill.installed}>
                        {skill.installed ? '已安装' : '安装'}
                      </button>
                    </div>
                  </div>
                {/each}
              </div>
            {/each}
          {/if}
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeSkillLibraryDialog}>关闭</button>
      </div>
    </div>
  </div>
{/if}

<!-- 重置 Token 确认对话框 -->
{#if showResetConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" onclick={cancelResetStats} role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1" onclick={(e) => e.stopPropagation()}>
      <div class="modal-header">
        <div class="modal-title">确认重置</div>
        <button class="modal-close" onclick={cancelResetStats}>×</button>
      </div>
      <div class="modal-body">
        <p style="margin: 0; color: var(--foreground);">确定要重置所有 Token 统计数据吗？</p>
        <p style="margin: var(--space-2) 0 0; color: var(--foreground-muted); font-size: var(--text-sm);">此操作不可撤销。</p>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={cancelResetStats}>取消</button>
        <button class="settings-btn primary" onclick={confirmResetStats}>确认重置</button>
      </div>
    </div>
  </div>
{/if}

<!-- 通用确认对话框 -->
{#if showConfirmDialog}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" onclick={handleConfirmNo} role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1" onclick={(e) => e.stopPropagation()}>
      <div class="modal-header">
        <div class="modal-title">{confirmDialogTitle}</div>
        <button class="modal-close" onclick={handleConfirmNo}>×</button>
      </div>
      <div class="modal-body">
        <p style="margin: 0; color: var(--foreground);">{confirmDialogMessage}</p>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={handleConfirmNo}>取消</button>
        <button class="settings-btn primary" onclick={handleConfirmYes}>确认</button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* ============================================
     Settings Panel - 优化后的样式
     ============================================ */

  /* 基础面板布局 */
  .settings-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  .settings-panel {
    width: 90%;
    max-width: 640px;
    max-height: 85vh;
    background: var(--vscode-editor-background, #1e1e1e);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow:
      0 25px 50px -12px rgba(0, 0, 0, 0.25),
      0 0 0 1px rgba(255, 255, 255, 0.05);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  .settings-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }

  .settings-title {
    font-size: var(--text-xl);
    font-weight: var(--font-bold);
    color: var(--foreground);
  }

  .settings-tabs {
    display: flex;
    gap: 0;
    padding: 0;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }

  .settings-tab {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    flex: 1;
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: all var(--transition-fast);
    position: relative;
  }

  .settings-tab:hover {
    color: var(--foreground);
    background: var(--surface-hover);
  }
  .settings-tab.active {
    color: var(--primary);
    border-bottom-color: var(--primary);
    font-weight: var(--font-semibold);
  }

  .settings-tab-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .settings-tab-content::-webkit-scrollbar { width: 6px; }
  .settings-tab-content::-webkit-scrollbar-track { background: transparent; }
  .settings-tab-content::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.15); border-radius: 3px; }
  .settings-tab-content::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.25); }

  /* 动画 */
  @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }
  @keyframes slideUp { from { opacity: 0; transform: translateY(20px) scale(0.98); } to { opacity: 1; transform: translateY(0) scale(1); } }
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* Section 容器 */
  .settings-section {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-5);
    transition: border-color var(--transition-fast);
  }
  .settings-section:hover {
    border-color: var(--primary-muted);
  }

  .settings-section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-4);
    flex-wrap: wrap;
    gap: var(--space-3);
  }

  .settings-section-title {
    font-size: var(--text-base);
    font-weight: var(--font-bold);
    color: var(--foreground);
  }

  .settings-section-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin-bottom: var(--space-4);
    line-height: 1.5;
  }

  .settings-section-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .settings-btn-group {
    display: flex;
    gap: var(--space-2);
  }

  /* 统一按钮样式 */
  .settings-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    background: var(--primary);
    border: none;
    border-radius: var(--radius-md);
    color: white;
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .settings-btn:hover:not(:disabled) {
    background: var(--primary-hover);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(var(--primary-rgb, 99, 102, 241), 0.3);
  }
  .settings-btn:active:not(:disabled) {
    transform: translateY(0);
  }
  .settings-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .settings-btn.secondary {
    background: var(--surface-3);
    color: var(--foreground);
    border: 1px solid var(--border);
  }
  .settings-btn.secondary:hover:not(:disabled) {
    background: var(--surface-hover);
    border-color: var(--primary-muted);
    transform: translateY(-1px);
    box-shadow: none;
  }

  /* 统计 Tab 专用样式 */
  .stats-section { padding: var(--space-4); }
  .stats-section .settings-section-header { margin-bottom: var(--space-3); }

  .settings-summary-chip {
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    background: var(--primary-muted);
    color: var(--primary);
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    white-space: nowrap;
  }

  /* 模型连接状态 - 紧凑布局 */
  .model-connection-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .model-connection-item {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }

  .model-connection-item:hover {
    border-color: var(--primary-muted);
  }
  .model-connection-item.success {
    border-left: 3px solid var(--success);
  }
  .model-connection-item.error {
    border-left: 3px solid var(--error);
  }
  .model-connection-item.disabled {
    border-left: 3px solid var(--foreground-muted);
    opacity: 0.6;
  }
  .model-connection-item.checking {
    border-left: 3px solid var(--info);
  }

  .model-connection-icon {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius-md);
    flex-shrink: 0;
  }

  .model-connection-icon.claude { background: var(--color-claude-muted); color: var(--color-claude); }
  .model-connection-icon.codex { background: var(--color-codex-muted); color: var(--color-codex); }
  .model-connection-icon.gemini { background: var(--color-gemini-muted); color: var(--color-gemini); }
  .model-connection-icon.orchestrator { background: var(--color-orchestrator-muted); color: var(--color-orchestrator); }
  .model-connection-icon.compressor { background: var(--color-compressor-muted); color: var(--color-compressor); }

  .model-connection-info { min-width: 0; overflow: hidden; flex: 1; }

  .model-connection-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .model-connection-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    display: flex;
    align-items: center;
  }

  .required-badge {
    font-size: 10px;
    color: var(--warning);
    background: rgba(245, 158, 11, 0.15);
    padding: 1px 4px;
    border-radius: var(--radius-sm);
    margin-left: var(--space-1);
  }

  .model-connection-model {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 2px;
  }

  .model-connection-stats-inline {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: 4px;
  }
  .stats-divider {
    opacity: 0.3;
  }

  .model-connection-badge {
    height: 20px;
    padding: 0 var(--space-2);
    font-size: 10px;
    font-weight: var(--font-medium);
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .model-connection-badge.success { background: var(--success-muted); color: var(--success); }
  .model-connection-badge.checking { background: var(--info-muted); color: var(--info); }
  .model-connection-badge.disabled { background: var(--surface-3); color: var(--foreground-muted); }
  .model-connection-badge.error { background: var(--error-muted); color: var(--error); }

  .model-refresh-btn {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    height: var(--btn-height-sm);
    padding: 0 var(--space-2);
    font-size: var(--text-xs);
    background: var(--secondary);
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .model-refresh-btn:hover:not(:disabled) { background: var(--secondary-hover); }
  .model-refresh-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .model-refresh-btn.loading :global(svg) { animation: spin 1s linear infinite; }

  /* Worker Tab 切换 - 统一样式 */
  .worker-tabs {
    display: flex;
    gap: var(--space-2);
  }

  .worker-tab {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .worker-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .worker-tab.active { background: var(--primary); border-color: var(--primary); color: white; }

  .worker-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .worker-dot.claude { background: var(--color-claude); }
  .worker-dot.codex { background: var(--color-codex); }
  .worker-dot.gemini { background: var(--color-gemini); }

  /* 兼容旧类名 - Worker Model Tabs 放在标题右侧 */
  .worker-model-tabs { display: flex; gap: var(--space-2); }
  .worker-model-tab { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: transparent; border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .worker-model-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .worker-model-tab.active { background: var(--primary); border-color: var(--primary); color: white; }
  .profile-worker-tabs { display: flex; gap: var(--space-2); }
  .profile-worker-tab { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: transparent; border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .profile-worker-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .profile-worker-tab.active { background: var(--primary); border-color: var(--primary); color: white; }
  .profile-worker-dot { width: 8px; height: 8px; border-radius: var(--radius-full); }
  .profile-worker-dot.claude { background: var(--color-claude); }
  .profile-worker-dot.codex { background: var(--color-codex); }
  .profile-worker-dot.gemini { background: var(--color-gemini); }

  /* 模型配置表单 */
  .model-config-card {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-4);
  }

  .model-config-header-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-4);
    flex-wrap: wrap;
    gap: var(--space-3);
  }

  .model-config-header { margin-bottom: var(--space-4); }
  .model-config-title { font-size: var(--text-base); font-weight: var(--font-semibold); color: var(--foreground); }
  .model-config-desc { font-size: var(--text-sm); color: var(--foreground-muted); margin-top: var(--space-1); }

  /* 模型配置顶部 Tab 切换 */
  .model-config-grid { display: flex; flex-direction: column; gap: var(--space-4); }
  .model-config-stack { display: flex; flex-direction: column; gap: var(--space-4); }
  .model-config-tabs { display: flex; gap: var(--space-1); border-bottom: 1px solid var(--border); margin-bottom: var(--space-4); }
  .model-config-tab {
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .model-config-tab:hover { color: var(--foreground); }
  .model-config-tab.active { color: var(--primary); border-bottom-color: var(--primary); }

  .llm-config-form { display: flex; flex-direction: column; gap: var(--space-3); }
  .llm-config-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .llm-config-field-row { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); }
  .llm-config-field-row.inline-toggle-row { grid-template-columns: 1fr 140px 100px; align-items: end; }
  .llm-config-label { font-size: var(--text-sm); color: var(--foreground-muted); }

  .llm-config-input, .llm-config-select {
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    outline: none;
    transition: border-color var(--transition-fast);
    width: 100%;
    box-sizing: border-box;
  }

  .llm-config-input:focus, .llm-config-select:focus { border-color: var(--primary); }

  .llm-config-actions { display: flex; justify-content: flex-end; gap: var(--space-2); margin-top: var(--space-3); }
  .llm-config-test-btn { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: var(--secondary); border: none; border-radius: var(--radius-sm); color: var(--foreground); cursor: pointer; transition: all var(--transition-fast); }
  .llm-config-test-btn:hover { background: var(--secondary-hover); }
  .llm-config-test-btn:disabled { opacity: 0.7; cursor: wait; }
  .llm-config-test-btn.testing :global(svg) { animation: spin 1s linear infinite; }
  .llm-config-test-btn.success { background: var(--success-muted); color: var(--success); }
  .llm-config-test-btn.error { background: var(--error-muted); color: var(--error); }
  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
  .llm-config-save-btn { height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); font-weight: var(--font-medium); background: var(--primary); border: none; border-radius: var(--radius-sm); color: white; cursor: pointer; transition: all var(--transition-fast); }
  .llm-config-save-btn:hover { background: var(--primary-hover); }

  .llm-config-toggle-label { display: flex; align-items: center; gap: var(--space-2); font-size: var(--text-sm); cursor: pointer; }
  .llm-config-toggle-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .inline-toggle { display: flex; flex-direction: column; gap: var(--space-2); }
  .toggle-switch { width: 32px; height: 18px; background: var(--secondary); border-radius: var(--radius-full); position: relative; transition: background var(--transition-fast); cursor: pointer; flex-shrink: 0; }
  .toggle-switch::after { content: ''; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; background: white; border-radius: var(--radius-full); transition: transform var(--transition-fast); }
  .toggle-switch.active { background: var(--primary); }
  .toggle-switch.active::after { transform: translateX(14px); }

  /* Profile 编辑器 */
  .profile-editor { display: flex; flex-direction: column; gap: var(--space-4); margin-top: var(--space-4); }
  .profile-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .profile-field-header { display: flex; justify-content: space-between; align-items: center; }
  .profile-field-label { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); }

  .profile-textarea {
    min-height: 80px;
    padding: var(--space-3);
    font-size: var(--text-sm);
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    resize: vertical;
    outline: none;
    transition: border-color var(--transition-fast);
  }

  .profile-textarea:focus { border-color: var(--primary); }
  .profile-two-columns { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-4); }
  .profile-column { min-width: 0; }

  .profile-add-btn { display: flex; align-items: center; justify-content: center; width: var(--btn-height-xs); height: var(--btn-height-xs); padding: 0; background: var(--primary); border: none; border-radius: var(--radius-sm); color: white; cursor: pointer; transition: all var(--transition-fast); }
  .profile-add-btn:hover { background: var(--primary-hover); }

  .profile-tags { display: flex; flex-wrap: wrap; gap: var(--space-2); min-height: var(--btn-height-lg); padding: var(--space-3); background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  .profile-tag { display: inline-flex; align-items: center; gap: var(--space-2); height: var(--btn-height-xs); padding: 0 var(--space-3); font-size: var(--text-sm); background: var(--primary); border-radius: var(--radius-full); color: white; }
  .profile-tag-remove { display: flex; align-items: center; justify-content: center; width: 14px; height: 14px; padding: 0; background: rgba(0, 0, 0, 0.2); border: none; border-radius: var(--radius-full); color: white; cursor: pointer; transition: background var(--transition-fast); }
  .profile-tag-remove:hover { background: rgba(0, 0, 0, 0.4); }
  .profile-tags-empty { font-size: var(--text-sm); color: var(--foreground-muted); font-style: italic; }

  .profile-categories-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: var(--space-2); margin-top: var(--space-3); }
  .profile-category-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); height: var(--btn-height-md); padding: 0 var(--space-3); background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  .profile-category-label { font-size: var(--text-sm); color: var(--foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; }
  .profile-category-select { height: var(--btn-height-xs); padding: 0 var(--space-2); font-size: var(--text-xs); background: var(--vscode-input-background, #3c3c3c); border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground); outline: none; }

  /* MCP 服务器列表 */
  .mcp-server-list { display: flex; flex-direction: column; gap: var(--space-2); }

  .mcp-server-item {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }

  .mcp-server-header {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .mcp-server-header:hover { background: var(--surface-hover); }
  .mcp-server-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .mcp-server-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .mcp-server-command { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-server-actions { display: flex; align-items: center; gap: var(--space-2); flex-shrink: 0; }

  /* MCP 操作按钮样式 */
  .mcp-server-actions .btn-icon {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .mcp-server-actions .btn-icon:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
  }
  .mcp-server-actions .btn-icon--danger:hover {
    background: var(--error-muted);
    border-color: var(--error);
    color: var(--error);
  }
  .mcp-server-actions .btn-icon:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .mcp-server-actions .btn-icon.refreshing :global(svg) {
    animation: spin 1s linear infinite;
  }

  .mcp-server-badge { padding: 2px 8px; font-size: var(--text-xs); border-radius: var(--radius-full); white-space: nowrap; }
  .mcp-server-badge.enabled { background: var(--success-muted); color: var(--success); }
  .mcp-server-badge.disabled { background: var(--surface-3); color: var(--foreground-muted); }
  .mcp-expand-icon { transition: transform var(--transition-fast); display: flex; color: var(--foreground-muted); margin-left: var(--space-1); }
  .mcp-expand-icon.expanded { transform: rotate(180deg); }

  .mcp-tools-panel { border-top: 1px solid var(--border); padding: var(--space-3) var(--space-4); background: var(--surface-1); max-height: 280px; overflow-y: auto; }
  .mcp-tools-header { display: flex; justify-content: space-between; align-items: center; font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground-muted); margin-bottom: var(--space-2); }
  .mcp-tools-list { display: flex; flex-direction: column; gap: var(--space-2); }

  /* MCP 工具项样式 */
  .mcp-tool-item {
    position: relative;
    padding: var(--space-2) var(--space-3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }
  .mcp-tool-item:hover { border-color: var(--primary-muted); background: var(--surface-hover); }
  .mcp-tool-row { display: flex; align-items: center; gap: var(--space-2); }
  .mcp-tool-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-tool-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 2px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }

  /* MCP 工具描述查看按钮 */
  .mcp-tool-desc-btn {
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--foreground-muted);
    transition: all var(--transition-fast);
    flex-shrink: 0;
  }
  .mcp-tool-desc-btn:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
  }

  /* MCP 工具描述弹出框 */
  .mcp-tool-desc-pop {
    position: absolute;
    right: var(--space-3);
    top: calc(100% + var(--space-2));
    width: min(360px, 80vw);
    max-height: 160px;
    overflow-y: auto;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-3);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: 1.5;
    z-index: 10;
    display: none;
  }
  .mcp-tool-item.show-desc .mcp-tool-desc-pop { display: block; }

  .mcp-tools-empty { font-size: var(--text-sm); color: var(--foreground-muted); text-align: center; padding: var(--space-4); }

  /* Skills 列表 */
  .skills-tool-list { display: flex; flex-direction: column; gap: var(--space-2); }

  .skill-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }
  .skill-item:hover { border-color: var(--primary-muted); }

  .skill-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .skill-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-desc { font-size: var(--text-xs); color: var(--foreground-muted); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; }

  .skill-actions { display: flex; align-items: center; gap: var(--space-2); flex-shrink: 0; }

  .skill-source-badge { padding: 2px 8px; font-size: var(--text-xs); border-radius: var(--radius-full); background: var(--surface-3); color: var(--foreground-muted); white-space: nowrap; flex-shrink: 0; }
  .skill-source-badge.builtin { background: var(--info-muted); color: var(--info); }
  .skill-source-badge.custom { background: var(--success-muted); color: var(--success); }
  .skill-source-badge.instruction { background: var(--primary-muted); color: var(--primary); }

  /* Skill 删除按钮 */
  .skill-actions .btn-icon--danger {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--foreground-muted);
  }
  .skill-actions .btn-icon--danger:hover {
    background: var(--error-muted);
    border-color: var(--error);
    color: var(--error);
  }

  /* 内置工具列表 */
  .builtin-tool-list { display: flex; flex-direction: column; gap: var(--space-2); }
  .builtin-tool-item { display: flex; align-items: center; gap: var(--space-4); padding: var(--space-4); background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius-md); }
  .builtin-tool-icon { display: flex; align-items: center; justify-content: center; width: var(--avatar-lg); height: var(--avatar-lg); background: var(--primary-muted); border-radius: var(--radius-md); color: var(--primary); flex-shrink: 0; }
  .builtin-tool-info { flex: 1; min-width: 0; }
  .builtin-tool-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .builtin-tool-desc { font-size: var(--text-sm); color: var(--foreground-muted); margin-top: 2px; }
  .builtin-tool-badge { height: var(--btn-height-xs); padding: 0 var(--space-3); font-size: var(--text-xs); border-radius: var(--radius-full); display: flex; align-items: center; }
  .builtin-tool-badge.enabled { background: var(--success-muted); color: var(--success); }

  /* 空状态 */
  .empty-state { display: flex; flex-direction: column; align-items: center; justify-content: center; padding: var(--space-8); color: var(--foreground-muted); text-align: center; }
  .empty-state p { margin: var(--space-2) 0 0 0; }
  .empty-state-hint { font-size: var(--text-sm); opacity: 0.7; }
  .empty-state-sm { padding: var(--space-6); text-align: center; color: var(--foreground-muted); font-size: var(--text-sm); }

  /* 加载状态 */
  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8);
    color: var(--foreground-muted);
    gap: var(--space-3);
  }
  .loading-state :global(svg) {
    animation: spin 1s linear infinite;
  }
  .loading-state span {
    font-size: var(--text-sm);
  }

  /* Modal 对话框 */
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: calc(var(--z-modal) + 10);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  .modal-dialog {
    width: 480px;
    max-width: 90vw;
    max-height: 80vh;
    background: var(--vscode-editor-background, #1e1e1e);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow:
      0 25px 50px -12px rgba(0, 0, 0, 0.25),
      0 0 0 1px rgba(255, 255, 255, 0.05);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  .modal-dialog-lg { width: 600px; }
  .modal-dialog--sm { width: 360px; }
  .modal-overlay--top { z-index: calc(var(--z-modal) + 20); }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .modal-header h3 {
    font-size: var(--text-lg);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0;
  }
  .modal-close {
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    color: var(--foreground-muted);
    font-size: 20px;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .modal-close:hover {
    background: var(--surface-hover);
    border-color: var(--border);
    color: var(--foreground);
  }
  .modal-body {
    flex: 1;
    padding: var(--space-5);
    overflow-y: auto;
  }
  .modal-body::-webkit-scrollbar { width: 6px; }
  .modal-body::-webkit-scrollbar-track { background: transparent; }
  .modal-body::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.15); border-radius: 3px; }
  .modal-body::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.25); }

  .modal-footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
    border-top: 1px solid var(--border);
    background: var(--surface-2);
  }

  /* 表单字段 */
  .form-field { margin-bottom: var(--space-4); }
  .form-field:last-child { margin-bottom: 0; }
  .form-field label {
    display: block;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }
  .form-field input, .form-field textarea {
    width: 100%;
    padding: var(--space-3);
    font-size: var(--text-sm);
    background: var(--surface-2);
    color: var(--foreground);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    outline: none;
    box-sizing: border-box;
    transition: all var(--transition-fast);
  }
  .form-field input { height: var(--btn-height-lg); padding: 0 var(--space-3); }
  .form-field textarea { font-family: var(--font-mono); resize: vertical; min-height: 120px; }
  .form-field input:hover, .form-field textarea:hover { border-color: var(--primary-muted); }
  .form-field input:focus, .form-field textarea:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px rgba(var(--primary-rgb, 99, 102, 241), 0.15);
  }
  .form-help { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: var(--space-2); line-height: 1.5; }

  /* 仓库管理 */
  .repo-add-section {
    margin-bottom: var(--space-5);
    padding: var(--space-4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .repo-add-form { display: flex; gap: var(--space-3); align-items: flex-end; }
  .repo-add-form .form-field { flex: 1; margin-bottom: 0; }
  .repo-list-title { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); margin-bottom: var(--space-3); }
  .repo-manage-list { display: flex; flex-direction: column; gap: var(--space-2); max-height: 320px; overflow-y: auto; }
  .repo-manage-list::-webkit-scrollbar { width: 6px; }
  .repo-manage-list::-webkit-scrollbar-track { background: transparent; }
  .repo-manage-list::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.15); border-radius: 3px; }
  .repo-manage-list::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.25); }
  .repo-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }
  .repo-item:hover {
    border-color: var(--primary-muted);
    background: var(--surface-hover);
  }
  .repo-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .repo-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .repo-url { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .repo-meta { font-size: var(--text-xs); color: var(--foreground-muted); }
  .repo-actions { display: flex; gap: var(--space-2); flex-shrink: 0; }

  /* Skill 库 */
  .skill-library-search { margin-bottom: var(--space-4); }
  .skill-library-search input {
    width: 100%;
    height: var(--btn-height-lg);
    padding: 0 var(--space-4);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    outline: none;
    box-sizing: border-box;
    transition: all var(--transition-fast);
  }
  .skill-library-search input::placeholder { color: var(--foreground-muted); }
  .skill-library-search input:hover { border-color: var(--primary-muted); }
  .skill-library-search input:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px rgba(var(--primary-rgb, 99, 102, 241), 0.15);
  }
  .skill-library-list { max-height: 400px; overflow-y: auto; }
  .skill-library-list::-webkit-scrollbar { width: 6px; }
  .skill-library-list::-webkit-scrollbar-track { background: transparent; }
  .skill-library-list::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.15); border-radius: 3px; }
  .skill-library-list::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.25); }
  .skill-repo-group { margin-bottom: var(--space-5); }
  .skill-repo-group:last-child { margin-bottom: 0; }
  .skill-repo-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-3);
    padding-bottom: var(--space-2);
    border-bottom: 1px solid var(--border);
  }
  .skill-library-item {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-bottom: var(--space-2);
    transition: all var(--transition-fast);
  }
  .skill-library-item:last-child { margin-bottom: 0; }
  .skill-library-item:hover {
    border-color: var(--primary-muted);
    background: var(--surface-hover);
  }
  .skill-library-icon {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--primary-muted);
    border-radius: var(--radius-md);
    color: var(--primary);
    flex-shrink: 0;
  }
  .skill-library-info { min-width: 0; overflow: hidden; }
  .skill-library-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-library-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 4px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; line-height: 1.5; }
  .skill-library-meta { display: flex; gap: var(--space-3); margin-top: var(--space-2); flex-wrap: wrap; }
  .skill-library-meta-item { font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-library-actions { flex-shrink: 0; align-self: center; }
</style>
