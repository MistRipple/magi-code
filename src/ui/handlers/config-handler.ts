/**
 * ConfigHandler - 配置管理消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：Profile / Worker / Orchestrator / Auxiliary 配置 CRUD + 模型列表获取 + 提示词增强触发。
 */

import { logger, LogCategory } from '../../logging';
import { fetchWithRetry, isRetryableNetworkError, toErrorMessage } from '../../tools/network-utils';
import type { WebviewToExtensionMessage, WorkerSlot } from '../../types';
import type { CommandHandler, CommandHandlerContext } from './types';
import { t } from '../../i18n';
import { resolveModelsBaseUrl } from '../../llm/url-mode';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'saveProfileConfig', 'resetProfileConfig',
  'enhancePrompt',
  'saveWorkerConfig', 'testWorkerConnection',
  'saveOrchestratorConfig', 'testOrchestratorConnection',
  'saveAuxiliaryConfig', 'testAuxiliaryConnection',
  'fetchModelList',
]);

export class ConfigCommandHandler implements CommandHandler {
  readonly supportedTypes: ReadonlySet<string> = SUPPORTED;

  async handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void> {
    switch (message.type) {
      case 'saveProfileConfig':
        await this.handleSaveProfileConfig(message as Msg<'saveProfileConfig'>, ctx);
        break;
      case 'resetProfileConfig':
        await this.handleResetProfileConfig(ctx);
        break;
      case 'enhancePrompt': {
        const result = await ctx.getPromptEnhancer().enhance((message as Msg<'enhancePrompt'>).prompt);
        ctx.sendData('promptEnhanced', { enhancedPrompt: result.enhancedPrompt, error: result.error || '' });
        break;
      }
      case 'saveWorkerConfig':
        await this.handleSaveWorkerConfig(message as Msg<'saveWorkerConfig'>, ctx);
        break;
      case 'testWorkerConnection':
        await this.handleTestWorkerConnection(message as Msg<'testWorkerConnection'>, ctx);
        break;
      case 'saveOrchestratorConfig':
        await this.handleSaveOrchestratorConfig(message as Msg<'saveOrchestratorConfig'>, ctx);
        break;
      case 'testOrchestratorConnection':
        await this.handleTestOrchestratorConnection(message as Msg<'testOrchestratorConnection'>, ctx);
        break;
      case 'saveAuxiliaryConfig':
        await this.handleSaveAuxiliaryConfig(message as Msg<'saveAuxiliaryConfig'>, ctx);
        break;
      case 'testAuxiliaryConnection':
        await this.handleTestAuxiliaryConnection(message as Msg<'testAuxiliaryConnection'>, ctx);
        break;
      case 'fetchModelList':
        await this.handleFetchModelList(message as Msg<'fetchModelList'>, ctx);
        break;
    }
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
      ctx.sendToast(t('config.toast.profileSaved'), 'success');

      try {
        await ctx.getOrchestratorEngine().reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        ctx.sendToast(t('config.toast.profileReloadFailed', { error: reloadMsg }), 'warning');
      }

      await ctx.refreshSettingsBootstrap();
      logger.info('界面.画像.配置.已保存', { path: WorkerAssignmentStorage.getConfigPath() }, LogCategory.UI);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      ctx.sendData('profileConfigSaved', { success: false, error: errorMsg });
      ctx.sendToast(t('config.toast.saveFailed', { error: errorMsg }), 'error');
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
        ctx.sendToast(t('config.toast.profileReloadFailed', { error: reloadMsg }), 'warning');
      }

      ctx.sendToast(t('config.toast.profileResetDone'), 'success');
      ctx.sendData('profileConfigReset', { success: true });
      await ctx.refreshSettingsBootstrap();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      ctx.sendData('profileConfigReset', { success: false, error: errorMsg });
      ctx.sendToast(t('config.toast.resetFailed', { error: errorMsg }), 'error');
    }
  }

  // ============================================================================
  // LLM 配置管理
  // ============================================================================

  private async handleSaveWorkerConfig(message: Msg<'saveWorkerConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateWorkerConfig(message.worker, message.config);
      await ctx.getAdapterFactory().clearAdapter(message.worker);
      const { clearClientCache } = await import('../../llm/clients/client-factory');
      clearClientCache();
      try {
        // #7 根修复：Worker enabled 变化后，立即重建 worker_dispatch/worker_send_message 的 worker enum
        await ctx.getOrchestratorEngine().reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        ctx.sendToast(t('config.toast.workerRefreshFailed', { error: reloadMsg }), 'warning');
      }
      ctx.sendToast(t('config.toast.workerConfigSaved', { worker: message.worker }), 'success');
      await ctx.refreshSettingsBootstrap(true);
      logger.info('Worker 配置已保存', { worker: message.worker }, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存 Worker 配置失败', { worker: message.worker, error: error.message }, LogCategory.LLM);
      ctx.sendToast(t('config.toast.saveConfigFailed', { error: error.message }), 'error');
    }
  }

  private async handleTestWorkerConnection(message: Msg<'testWorkerConnection'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const normalizedConfig = { ...message.config, enabled: message.config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        ctx.sendToast(t('config.toast.workerNotEnabled', { worker: message.worker }), 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });

      if (response) {
        ctx.sendData('workerConnectionTestResult', { worker: message.worker, success: true });
        ctx.sendToast(t('config.toast.workerConnected', { worker: message.worker }), 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('Worker 连接测试失败', { worker: message.worker, error: error.message }, LogCategory.LLM);
      ctx.sendData('workerConnectionTestResult', { worker: message.worker, success: false, error: error.message });
      ctx.sendToast(t('config.toast.workerConnectFailed', { worker: message.worker, error: error.message }), 'error');
    }
  }

  private async handleSaveOrchestratorConfig(message: Msg<'saveOrchestratorConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateOrchestratorConfig(message.config);
      await ctx.getAdapterFactory().clearAdapter('orchestrator');
      const { clearClientCache } = await import('../../llm/clients/client-factory');
      clearClientCache();
      ctx.sendToast(t('config.toast.orchestratorSaved'), 'success');
      await ctx.refreshSettingsBootstrap(true);
      logger.info('编排者配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存编排者配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(t('config.toast.saveConfigFailed', { error: error.message }), 'error');
    }
  }

  private async handleTestOrchestratorConnection(message: Msg<'testOrchestratorConnection'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const normalizedConfig = { ...message.config, enabled: message.config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        ctx.sendToast(t('config.toast.orchestratorNotEnabled'), 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });

      if (response) {
        ctx.sendData('orchestratorConnectionTestResult', { success: true });
        ctx.sendToast(t('config.toast.orchestratorConnected'), 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('编模型连接测试失败', { error: error.message }, LogCategory.LLM);
      ctx.sendData('orchestratorConnectionTestResult', { success: false, error: error.message });
      ctx.sendToast(t('config.toast.orchestratorConnectFailed', { error: error.message }), 'error');
    }
  }

  private async handleSaveAuxiliaryConfig(message: Msg<'saveAuxiliaryConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateAuxiliaryConfig(message.config);
      const { clearClientCache } = await import('../../llm/clients/client-factory');
      clearClientCache();
      await ctx.getOrchestratorEngine().reloadCompressionAdapter();
      ctx.sendToast(t('config.toast.auxiliarySaved'), 'success');
      await ctx.refreshSettingsBootstrap(true);
      logger.info('辅助模型配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存辅助模型配置失败', { error: error.message }, LogCategory.LLM);
      ctx.sendToast(t('config.toast.saveConfigFailed', { error: error.message }), 'error');
    }
  }

  private async handleTestAuxiliaryConnection(message: Msg<'testAuxiliaryConnection'>, ctx: CommandHandlerContext): Promise<void> {
    let orchestratorModel: string | undefined;
    try {
      const normalizedConfig = { ...message.config, enabled: Boolean(message.config?.apiKey) || message.config?.enabled === true };
      const { LLMConfigLoader } = await import('../../llm/config');
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();
      orchestratorModel = orchestratorConfig?.provider && orchestratorConfig?.model
        ? `${orchestratorConfig.provider} - ${orchestratorConfig.model}`
        : undefined;

      if (!normalizedConfig.enabled || !normalizedConfig.apiKey || !normalizedConfig.model) {
        ctx.sendData('auxiliaryConnectionTestResult', {
          success: false, error: t('config.toast.auxiliaryNotAvailable'), orchestratorModel,
        });
        ctx.sendToast(t('config.toast.auxiliaryUnavailable'), 'warning');
        return;
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });

      if (response) {
        ctx.sendData('auxiliaryConnectionTestResult', { success: true });
        ctx.sendToast(t('config.toast.auxiliaryConnected'), 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('辅助模型连接测试失败', { error: error.message }, LogCategory.LLM);
      ctx.sendData('auxiliaryConnectionTestResult', {
        success: false, error: error.message, orchestratorModel,
      });
      ctx.sendToast(t('config.toast.auxiliaryConnectFailed'), 'warning');
    }
  }

  private async handleFetchModelList(message: Msg<'fetchModelList'>, ctx: CommandHandlerContext): Promise<void> {
    const { config, target } = message;
    try {
      if (!config?.baseUrl || !config?.apiKey) {
        ctx.sendData('modelListFetched', { target, success: false, models: [], error: t('config.toast.fillBaseUrlFirst') });
        return;
      }

      const provider = config.provider === 'anthropic' ? 'anthropic' : 'openai';
      let modelsUrl = resolveModelsBaseUrl(provider, config.baseUrl, config.urlMode);
      if (!modelsUrl) {
        const error = t('config.toast.modelListUnsupportedInFullMode');
        ctx.sendData('modelListFetched', { target, success: false, models: [], error });
        ctx.sendToast(error, 'warning');
        return;
      }
      modelsUrl += '/models';

      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
      };
      if (provider === 'anthropic') {
        headers['x-api-key'] = config.apiKey;
        headers['anthropic-version'] = '2023-06-01';
      } else {
        headers['Authorization'] = `Bearer ${config.apiKey}`;
      }

      const response = await fetchWithRetry(modelsUrl, {
        method: 'GET',
        headers,
      }, {
        timeoutMs: 10000,
        attempts: 2,
        retryOnStatuses: [429, 500, 502, 503, 504],
      });

      if (!response.ok) {
        const status = response.status;
        let error = `HTTP ${status}`;
        if (status === 401 || status === 403) error = t('config.toast.invalidApiKey');
        else if (status === 404) error = t('config.toast.apiNotSupportModelList');
        ctx.sendData('modelListFetched', { target, success: false, models: [], error });
        ctx.sendToast(t('config.toast.fetchModelsFailed', { error }), 'error');
        return;
      }

      const data = await response.json();
      const models: string[] = (data?.data || [])
        .map((m: any) => m.id)
        .filter((id: any) => typeof id === 'string' && id.length > 0)
        .sort();

      ctx.sendData('modelListFetched', { target, success: true, models });
      ctx.sendToast(t('config.toast.modelsFetched', { count: models.length }), 'success');
    } catch (error: any) {
      const errorMessage = toErrorMessage(error);
      let displayError = errorMessage;
      const lower = errorMessage.toLowerCase();
      if (lower.includes('timeout') || lower.includes('timed out')) displayError = t('config.toast.connectionTimeout');
      else if (isRetryableNetworkError(errorMessage)) displayError = t('config.toast.networkFailed');

      logger.error('获取模型列表失败', { target, error: errorMessage }, LogCategory.LLM);
      ctx.sendData('modelListFetched', { target, success: false, models: [], error: displayError });
      ctx.sendToast(t('config.toast.fetchModelsFailed', { error: displayError }), 'error');
    }
  }
}
