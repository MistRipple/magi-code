/**
 * ConfigHandler - 配置管理消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：Profile / PromptEnhance / Worker / Orchestrator / Compressor 配置 CRUD + 模型列表获取。
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { logger, LogCategory } from '../../logging';
import type { WebviewToExtensionMessage, WorkerSlot } from '../../types';
import { loadAceConfigFromFile } from '../../tools/tool-manager';
import type { CommandHandler, CommandHandlerContext } from './types';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'getProfileConfig', 'saveProfileConfig', 'resetProfileConfig',
  'getPromptEnhanceConfig', 'updatePromptEnhance', 'testPromptEnhance', 'enhancePrompt',
  'loadAllWorkerConfigs', 'saveWorkerConfig', 'testWorkerConnection',
  'loadOrchestratorConfig', 'saveOrchestratorConfig', 'testOrchestratorConnection',
  'loadCompressorConfig', 'saveCompressorConfig', 'testCompressorConnection',
  'fetchModelList',
]);

export class ConfigCommandHandler implements CommandHandler {
  readonly supportedTypes: ReadonlySet<string> = SUPPORTED;

  async handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void> {
    switch (message.type) {
      case 'getProfileConfig':
        await this.sendProfileConfig(ctx);
        break;
      case 'saveProfileConfig':
        await this.handleSaveProfileConfig(message as Msg<'saveProfileConfig'>, ctx);
        break;
      case 'resetProfileConfig':
        await this.handleResetProfileConfig(ctx);
        break;
      case 'getPromptEnhanceConfig':
        await this.sendPromptEnhanceConfig(ctx);
        break;
      case 'updatePromptEnhance':
        await this.handleUpdatePromptEnhance(message as Msg<'updatePromptEnhance'>, ctx);
        break;
      case 'testPromptEnhance':
        await this.handleTestPromptEnhance(message as Msg<'testPromptEnhance'>, ctx);
        break;
      case 'enhancePrompt': {
        const result = await ctx.getPromptEnhancer().enhance((message as Msg<'enhancePrompt'>).prompt);
        ctx.sendData('promptEnhanced', { enhancedPrompt: result.enhancedPrompt, error: result.error || '' });
        break;
      }
      case 'loadAllWorkerConfigs':
        await this.handleLoadAllWorkerConfigs(ctx);
        break;
      case 'saveWorkerConfig':
        await this.handleSaveWorkerConfig(message as Msg<'saveWorkerConfig'>, ctx);
        break;
      case 'testWorkerConnection':
        await this.handleTestWorkerConnection(message as Msg<'testWorkerConnection'>, ctx);
        break;
      case 'loadOrchestratorConfig':
        await this.handleLoadOrchestratorConfig(ctx);
        break;
      case 'saveOrchestratorConfig':
        await this.handleSaveOrchestratorConfig(message as Msg<'saveOrchestratorConfig'>, ctx);
        break;
      case 'testOrchestratorConnection':
        await this.handleTestOrchestratorConnection(message as Msg<'testOrchestratorConnection'>, ctx);
        break;
      case 'loadCompressorConfig':
        await this.handleLoadCompressorConfig(ctx);
        break;
      case 'saveCompressorConfig':
        await this.handleSaveCompressorConfig(message as Msg<'saveCompressorConfig'>, ctx);
        break;
      case 'testCompressorConnection':
        await this.handleTestCompressorConnection(message as Msg<'testCompressorConnection'>, ctx);
        break;
      case 'fetchModelList':
        await this.handleFetchModelList(message as Msg<'fetchModelList'>, ctx);
        break;
    }
  }

  // ============================================================================
  // Profile 配置
  // ============================================================================

  private async sendProfileConfig(ctx: CommandHandlerContext): Promise<void> {
    const { WorkerAssignmentStorage, CATEGORY_DEFINITIONS, CATEGORY_RULES } = await import('../../orchestrator/profile');
    const assignments = WorkerAssignmentStorage.ensureDefaults();
    const assignmentMap: Record<string, string> = {};

    for (const [worker, categories] of Object.entries(assignments.assignments)) {
      for (const category of categories) {
        assignmentMap[category] = worker;
      }
    }

    const categoryGuidance: Record<string, any> = {};
    for (const [category, definition] of Object.entries(CATEGORY_DEFINITIONS)) {
      categoryGuidance[category] = {
        displayName: definition.displayName,
        description: definition.description,
        guidance: {
          focus: definition.guidance?.focus || [],
          constraints: definition.guidance?.constraints || [],
        },
        priority: definition.priority,
        riskLevel: definition.riskLevel,
      };
    }

    const uiConfig: any = {
      assignments: assignmentMap,
      categoryGuidance,
      categoryPriority: CATEGORY_RULES.categoryPriority,
      configPath: WorkerAssignmentStorage.getConfigPath(),
    };

    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const userRules = LLMConfigLoader.loadUserRules();
      uiConfig.userRules = userRules.content || '';
    } catch (error) {
      logger.warn('加载用户规则失败', { error }, LogCategory.LLM);
    }

    ctx.sendData('profileConfig', { config: uiConfig });
  }

  private async handleSaveProfileConfig(message: Msg<'saveProfileConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { WorkerAssignmentStorage, WORKER_ASSIGNMENTS_VERSION } = await import('../../orchestrator/profile');

      const workerAssignments: Record<WorkerSlot, string[]> = {
        claude: [],
        codex: [],
        gemini: [],
      };

      const assignmentMap = message.data.assignments || {};
      for (const [category, worker] of Object.entries(assignmentMap)) {
        const normalizedWorker = String(worker).toLowerCase() as WorkerSlot;
        if (!['claude', 'codex', 'gemini'].includes(normalizedWorker)) {
          throw new Error(`未知 Worker: ${worker}`);
        }
        workerAssignments[normalizedWorker].push(category);
      }

      WorkerAssignmentStorage.save({
        version: WORKER_ASSIGNMENTS_VERSION,
        assignments: workerAssignments,
      });

      try {
        const { LLMConfigLoader } = await import('../../llm/config');
        const userRulesContent = typeof message.data.userRules === 'string' ? message.data.userRules : '';
        const trimmed = userRulesContent.trim();
        LLMConfigLoader.updateUserRules({
          enabled: trimmed.length > 0,
          content: userRulesContent,
        });
        await ctx.getAdapterFactory().refreshUserRules();
      } catch (rulesError) {
        logger.warn('保存用户规则失败', { error: rulesError }, LogCategory.LLM);
      }

      ctx.sendData('profileConfigSaved', { success: true });
      ctx.sendToast('画像配置已保存', 'success');

      try {
        await ctx.getOrchestratorEngine().reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        ctx.sendToast(`画像重载失败: ${reloadMsg}`, 'warning');
      }

      await this.sendProfileConfig(ctx);
      logger.info('界面.画像.配置.已保存', { path: WorkerAssignmentStorage.getConfigPath() }, LogCategory.UI);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      ctx.sendData('profileConfigSaved', { success: false, error: errorMsg });
      ctx.sendToast(`保存失败: ${errorMsg}`, 'error');
    }
  }

  private async handleResetProfileConfig(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { WorkerAssignmentStorage } = await import('../../orchestrator/profile');
      const defaults = WorkerAssignmentStorage.buildDefault();
      WorkerAssignmentStorage.save(defaults);

      try {
        const { LLMConfigLoader } = await import('../../llm/config');
        LLMConfigLoader.updateUserRules({ enabled: false, content: '' });
        await ctx.getAdapterFactory().refreshUserRules();
      } catch (rulesError) {
        logger.warn('重置用户规则失败', { error: rulesError }, LogCategory.LLM);
      }

      try {
        await ctx.getOrchestratorEngine().reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        ctx.sendToast(`画像重载失败: ${reloadMsg}`, 'warning');
      }

      ctx.sendToast('画像配置已重置为默认值', 'success');
      ctx.sendData('profileConfigReset', { success: true });
      await this.sendProfileConfig(ctx);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      ctx.sendData('profileConfigReset', { success: false, error: errorMsg });
      ctx.sendToast(`重置失败: ${errorMsg}`, 'error');
    }
  }

  // ============================================================================
  // Prompt Enhance 配置
  // ============================================================================

  private async sendPromptEnhanceConfig(ctx: CommandHandlerContext): Promise<void> {
    const config = loadAceConfigFromFile();
    ctx.sendData('promptEnhanceConfigLoaded', {
      config: { baseUrl: config.baseUrl, apiKey: config.apiKey },
    });
  }

  private async handleUpdatePromptEnhance(message: Msg<'updatePromptEnhance'>, ctx: CommandHandlerContext): Promise<void> {
    const config = message.config;
    const source = message.source ?? 'auto';
    try {
      const configPath = path.join(os.homedir(), '.magi', 'config.json');
      const configDir = path.dirname(configPath);

      if (!fs.existsSync(configDir)) {
        fs.mkdirSync(configDir, { recursive: true });
      }

      let existingConfig: any = {};
      if (fs.existsSync(configPath)) {
        try {
          existingConfig = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
        } catch { existingConfig = {}; }
      }

      existingConfig.promptEnhance = { baseUrl: config.baseUrl, apiKey: config.apiKey };
      fs.writeFileSync(configPath, JSON.stringify(existingConfig, null, 2), 'utf-8');
      logger.info('界面.提示词_增强.配置.已保存', { path: configPath }, LogCategory.UI);

      const toolManager = ctx.getAdapterFactory().getToolManager?.();
      if (toolManager && config.baseUrl && config.apiKey) {
        toolManager.configureAce(config.baseUrl, config.apiKey);
        logger.info('界面.提示词_增强.配置.已同步到ToolManager', undefined, LogCategory.UI);
      }

      if (source === 'manual') {
        ctx.sendToast('ACE 配置已保存', 'success');
      }
    } catch (error) {
      logger.error('界面.提示词_增强.配置.保存_失败', error, LogCategory.UI);
      if (source === 'manual') {
        const errorMsg = error instanceof Error ? error.message : String(error);
        ctx.sendToast(`ACE 配置保存失败: ${errorMsg}`, 'error');
      }
    }
  }

  private async handleTestPromptEnhance(message: Msg<'testPromptEnhance'>, ctx: CommandHandlerContext): Promise<void> {
    if (!message.baseUrl || !message.apiKey) {
      ctx.sendData('promptEnhanceResult', { success: false, message: '请填写 API 地址和密钥' });
      return;
    }

    try {
      const testUrl = message.baseUrl.replace(/\/$/, '') + '/prompt-enhancer';
      const response = await fetch(testUrl, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${message.apiKey}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          nodes: [{ id: 1, type: 0, text_node: { content: 'test' } }],
          chat_history: [],
          blobs: { checkpoint_id: null, added_blobs: [], deleted_blobs: [] },
          conversation_id: null,
          model: 'claude-sonnet-4-5',
          mode: 'CHAT',
          user_guided_blobs: [],
          external_source_ids: [],
          user_guidelines: '',
          workspace_guidelines: '',
          rules: [],
        }),
        signal: AbortSignal.timeout(15000),
      });

      if (response.ok) {
        ctx.sendData('promptEnhanceResult', { success: true, message: '连接成功' });
      } else if (response.status === 401) {
        ctx.sendData('promptEnhanceResult', { success: false, message: 'Token 无效或已过期' });
      } else if (response.status === 403) {
        ctx.sendData('promptEnhanceResult', { success: false, message: '访问被拒绝' });
      } else {
        ctx.sendData('promptEnhanceResult', { success: false, message: `连接失败: ${response.status}` });
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      ctx.sendData('promptEnhanceResult', { success: false, message: `连接错误: ${errorMsg}` });
    }
  }

  // ============================================================================
  // LLM 配置管理
  // ============================================================================

  private async handleLoadAllWorkerConfigs(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const fullConfig = LLMConfigLoader.loadFullConfig();
      ctx.sendData('allWorkerConfigsLoaded', { configs: fullConfig.workers });
    } catch (error: any) {
      logger.error('加载 Worker 配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  private async handleSaveWorkerConfig(message: Msg<'saveWorkerConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateWorkerConfig(message.worker, message.config);
      await ctx.getAdapterFactory().clearAdapter(message.worker);
      try {
        // #7 根修复：Worker enabled 变化后，立即重建 dispatch_task/send_worker_message 的 worker enum
        await ctx.getOrchestratorEngine().reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        ctx.sendToast(`Worker 可用列表刷新失败: ${reloadMsg}`, 'warning');
      }
      ctx.sendToast(`${message.worker} 配置已保存`, 'success');
      logger.info('Worker 配置已保存', { worker: message.worker }, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存 Worker 配置失败', { worker: message.worker, error: error.message }, LogCategory.LLM);
      ctx.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  private async handleTestWorkerConnection(message: Msg<'testWorkerConnection'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const normalizedConfig = { ...message.config, enabled: message.config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        ctx.sendToast(`${message.worker} 未启用，无法测试连接`, 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7,
      });

      if (response && response.content) {
        ctx.sendData('workerConnectionTestResult', { worker: message.worker, success: true });
        ctx.sendToast(`${message.worker} 连接成功`, 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('Worker 连接测试失败', { worker: message.worker, error: error.message }, LogCategory.LLM);
      ctx.sendData('workerConnectionTestResult', { worker: message.worker, success: false, error: error.message });
      ctx.sendToast(`${message.worker} 连接失败: ${error.message}`, 'error');
    }
  }

  private async handleLoadOrchestratorConfig(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadOrchestratorConfig();
      ctx.sendData('orchestratorConfigLoaded', { config });
    } catch (error: any) {
      logger.error('加载编排者配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  private async handleSaveOrchestratorConfig(message: Msg<'saveOrchestratorConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateOrchestratorConfig(message.config);
      await ctx.getAdapterFactory().clearAdapter('orchestrator');
      ctx.sendToast('编排者配置已保存', 'success');
      logger.info('编排者配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存编排者配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  private async handleTestOrchestratorConnection(message: Msg<'testOrchestratorConnection'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const normalizedConfig = { ...message.config, enabled: message.config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        ctx.sendToast('编排者未启用，无法测试连接', 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7,
      });

      if (response && response.content) {
        ctx.sendData('orchestratorConnectionTestResult', { success: true });
        ctx.sendToast('编排模型连接成功', 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('编模型连接测试失败', { error: error.message }, LogCategory.LLM);
      ctx.sendData('orchestratorConnectionTestResult', { success: false, error: error.message });
      ctx.sendToast(`编排模型连接失败: ${error.message}`, 'error');
    }
  }

  private async handleLoadCompressorConfig(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadCompressorConfig();
      ctx.sendData('compressorConfigLoaded', { config });
    } catch (error: any) {
      logger.error('加载压缩模型配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  private async handleSaveCompressorConfig(message: Msg<'saveCompressorConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateCompressorConfig(message.config);
      await ctx.getOrchestratorEngine().reloadCompressionAdapter();
      ctx.sendToast('压缩模型配置已保存', 'success');
      logger.info('压缩模型配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存压缩模型配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  private async handleTestCompressorConnection(message: Msg<'testCompressorConnection'>, ctx: CommandHandlerContext): Promise<void> {
    let fallbackModel: string | undefined;
    try {
      const normalizedConfig = { ...message.config, enabled: Boolean(message.config?.apiKey) || message.config?.enabled === true };
      const { LLMConfigLoader } = await import('../../llm/config');
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();
      fallbackModel = orchestratorConfig?.provider && orchestratorConfig?.model
        ? `${orchestratorConfig.provider} - ${orchestratorConfig.model}`
        : undefined;

      if (!normalizedConfig.enabled || !normalizedConfig.apiKey || !normalizedConfig.model) {
        ctx.sendData('compressorConnectionTestResult', {
          success: false, error: '压缩模型未配置或不可用', fallbackModel,
        });
        ctx.sendToast('压缩模型不可用，已降级使用编排者模型', 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7,
      });

      if (response && response.content) {
        ctx.sendData('compressorConnectionTestResult', { success: true });
        ctx.sendToast('压缩模型连接成功', 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('压缩模型连接测试失败', { error: error.message }, LogCategory.LLM);
      ctx.sendData('compressorConnectionTestResult', {
        success: false, error: error.message, fallbackModel,
      });
      ctx.sendToast('压缩模型连接失败，已降级使用编排者模型', 'warning');
    }
  }

  private async handleFetchModelList(message: Msg<'fetchModelList'>, ctx: CommandHandlerContext): Promise<void> {
    const { config, target } = message;
    try {
      if (!config?.baseUrl || !config?.apiKey) {
        ctx.sendData('modelListFetched', { target, success: false, models: [], error: '请先填写 Base URL 和 API Key' });
        return;
      }

      let modelsUrl = config.baseUrl;
      if (!modelsUrl.endsWith('/v1')) {
        modelsUrl = modelsUrl.replace(/\/$/, '') + '/v1';
      }
      modelsUrl += '/models';

      const response = await fetch(modelsUrl, {
        method: 'GET',
        headers: {
          'Authorization': `Bearer ${config.apiKey}`,
          'Content-Type': 'application/json',
        },
        signal: AbortSignal.timeout(10000),
      });

      if (!response.ok) {
        const status = response.status;
        let error = `HTTP ${status}`;
        if (status === 401 || status === 403) error = 'API Key 无效';
        else if (status === 404) error = '该 API 不支持模型列表查询';
        ctx.sendData('modelListFetched', { target, success: false, models: [], error });
        ctx.sendToast(`获取模型列表失败: ${error}`, 'error');
        return;
      }

      const data = await response.json();
      const models: string[] = (data?.data || [])
        .map((m: any) => m.id)
        .filter((id: any) => typeof id === 'string' && id.length > 0)
        .sort();

      ctx.sendData('modelListFetched', { target, success: true, models });
      ctx.sendToast(`获取到 ${models.length} 个模型`, 'success');
    } catch (error: any) {
      const errorMessage = error.message || String(error);
      let displayError = errorMessage;
      if (errorMessage.includes('timeout') || errorMessage.includes('TimeoutError')) displayError = '连接超时';
      else if (errorMessage.includes('ECONNREFUSED') || errorMessage.includes('ENOTFOUND')) displayError = '网络连接失败';

      logger.error('获取模型列表失败', { target, error: errorMessage }, LogCategory.LLM);
      ctx.sendData('modelListFetched', { target, success: false, models: [], error: displayError });
      ctx.sendToast(`获取模型列表失败: ${displayError}`, 'error');
    }
  }
}
