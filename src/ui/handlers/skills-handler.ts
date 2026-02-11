/**
 * SkillsHandler - Skills 配置与仓库管理消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：Skills 配置 CRUD + 自定义工具管理 + 仓库管理 + Skill 安装。
 */

import { logger, LogCategory } from '../../logging';
import type { WebviewToExtensionMessage } from '../../types';
import { applySkillInstall } from '../../tools/skill-installation';
import type { CommandHandler, CommandHandlerContext } from './types';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'loadSkillsConfig', 'saveSkillsConfig',
  'addCustomTool', 'removeCustomTool', 'removeInstructionSkill', 'installSkill',
  'loadRepositories', 'addRepository', 'updateRepository', 'deleteRepository',
  'refreshRepository', 'loadSkillLibrary',
]);

export class SkillsCommandHandler implements CommandHandler {
  readonly supportedTypes: ReadonlySet<string> = SUPPORTED;

  async handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void> {
    switch (message.type) {
      case 'loadSkillsConfig':
        await this.handleLoadSkillsConfig(ctx);
        break;
      case 'saveSkillsConfig':
        await this.handleSaveSkillsConfig(message as Msg<'saveSkillsConfig'>, ctx);
        break;
      case 'addCustomTool':
        await this.handleAddCustomTool(message as Msg<'addCustomTool'>, ctx);
        break;
      case 'removeCustomTool':
        await this.handleRemoveCustomTool(message as Msg<'removeCustomTool'>, ctx);
        break;
      case 'removeInstructionSkill':
        await this.handleRemoveInstructionSkill(message as Msg<'removeInstructionSkill'>, ctx);
        break;
      case 'installSkill':
        await this.handleInstallSkill(message as Msg<'installSkill'>, ctx);
        break;
      case 'loadRepositories':
        await this.handleLoadRepositories(ctx);
        break;
      case 'addRepository':
        await this.handleAddRepository(message as Msg<'addRepository'>, ctx);
        break;
      case 'updateRepository':
        await this.handleUpdateRepository(message as Msg<'updateRepository'>, ctx);
        break;
      case 'deleteRepository':
        await this.handleDeleteRepository(message as Msg<'deleteRepository'>, ctx);
        break;
      case 'refreshRepository':
        await this.handleRefreshRepository(message as Msg<'refreshRepository'>, ctx);
        break;
      case 'loadSkillLibrary':
        await this.handleLoadSkillLibrary(ctx);
        break;
    }
  }

  private async reloadSkills(ctx: CommandHandlerContext, reason: string): Promise<void> {
    await ctx.getAdapterFactory().reloadSkills();
    logger.info('Skills reloaded in adapter factory', { reason }, LogCategory.TOOLS);
  }

  private async handleLoadSkillsConfig(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig();
      ctx.sendData('skillsConfigLoaded', {
        config: config || { customTools: [], instructionSkills: [], repositories: [] },
      });
      logger.info('Skills 配置已加载', {}, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('加载 Skills 配置失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`加载 Skills 配置失败: ${error.message}`, 'error');
    }
  }

  private async handleSaveSkillsConfig(message: Msg<'saveSkillsConfig'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.saveSkillsConfig(message.config);
      ctx.sendToast('Skills 配置已保存', 'success');
      await this.reloadSkills(ctx, 'saveSkillsConfig');
      logger.info('Skills 配置已保存', {}, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('保存 Skills 配置失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`保存 Skills 配置失败: ${error.message}`, 'error');
    }
  }

  private async handleAddCustomTool(message: Msg<'addCustomTool'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };

      const existingIndex = config.customTools.findIndex((t: any) => t.name === message.tool.name);
      if (existingIndex >= 0) {
        config.customTools[existingIndex] = message.tool;
      } else {
        config.customTools.push(message.tool);
      }
      LLMConfigLoader.saveSkillsConfig(config);
      ctx.sendData('customToolAdded', { tool: message.tool });
      ctx.sendToast(`自定义工具 "${message.tool.name}" 已添加`, 'success');
      await this.reloadSkills(ctx, 'addCustomTool');
      logger.info('自定义工具已添加', { name: message.tool.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('添加自定义工具失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`添加自定义工具失败: ${error.message}`, 'error');
    }
  }

  private async handleRemoveCustomTool(message: Msg<'removeCustomTool'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
      config.customTools = config.customTools.filter((t: any) => t.name !== message.toolName);
      LLMConfigLoader.saveSkillsConfig(config);
      ctx.sendData('customToolRemoved', { toolName: message.toolName });
      ctx.sendToast(`自定义工具 "${message.toolName}" 已删除`, 'success');
      await this.reloadSkills(ctx, 'removeCustomTool');
      logger.info('自定义工具已删除', { name: message.toolName }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除自定义工具失败', { toolName: message.toolName, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`删除自定义工具失败: ${error.message}`, 'error');
    }
  }

  private async handleRemoveInstructionSkill(message: Msg<'removeInstructionSkill'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
      config.instructionSkills = (config.instructionSkills || []).filter((s: any) => s.name !== message.skillName);
      LLMConfigLoader.saveSkillsConfig(config);
      ctx.sendData('instructionSkillRemoved', { skillName: message.skillName });
      ctx.sendToast(`Instruction Skill "${message.skillName}" 已删除`, 'success');
      await this.reloadSkills(ctx, 'removeInstructionSkill');
      logger.info('Instruction Skill 已删除', { name: message.skillName }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除 Instruction Skill 失败', { skillName: message.skillName, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`删除 Instruction Skill 失败: ${error.message}`, 'error');
    }
  }

  private async handleInstallSkill(message: Msg<'installSkill'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');

      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      const skills = await manager.getAllSkills(repositories);
      const skill = skills.find((item: any) => item.fullName === message.skillId || item.id === message.skillId);
      if (!skill) throw new Error(`未找到 Skill: ${message.skillId}`);

      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, skill);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);

      ctx.sendData('skillInstalled', { skillId: message.skillId, skill });
      ctx.sendToast(`Skill "${skill.description}" 已安装`, 'success');
      await this.handleLoadSkillsConfig(ctx);
      await this.reloadSkills(ctx, 'installSkill');
      logger.info('Skill 已安装', { skillId: message.skillId, name: skill.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('安装 Skill 失败', { skillId: message.skillId, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`安装 Skill 失败: ${error.message}`, 'error');
    }
  }

  private async handleLoadRepositories(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const repositories = LLMConfigLoader.loadRepositories();
      ctx.sendData('repositoriesLoaded', { repositories });
      logger.info('Repositories loaded', { count: repositories.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load repositories', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`加载仓库失败: ${error.message}`, 'error');
    }
  }

  private async handleAddRepository(message: Msg<'addRepository'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');

      const manager = new SkillRepositoryManager();
      const repoInfo = await manager.validateRepository(message.url);
      const result = await LLMConfigLoader.addRepository(message.url);
      LLMConfigLoader.updateRepositoryName(result.id, repoInfo.name);
      LLMConfigLoader.updateRepository(result.id, { type: repoInfo.type });

      ctx.sendData('repositoryAdded', {
        repository: { id: result.id, url: message.url, name: repoInfo.name, type: repoInfo.type, enabled: true },
      });
      ctx.sendToast(`仓库 "${repoInfo.name}" 已添加（${repoInfo.skillCount} 个技能）`, 'success');
      logger.info('Repository added', { url: message.url, name: repoInfo.name, type: repoInfo.type }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to add repository', { url: message.url, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`添加仓库失败: ${error.message}`, 'error');
      ctx.sendData('repositoryAddFailed', { error: error.message });
    }
  }

  private async handleUpdateRepository(message: Msg<'updateRepository'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateRepository(message.repositoryId, message.updates);
      ctx.sendToast('仓库已更新', 'success');
      await this.handleLoadRepositories(ctx);
      logger.info('Repository updated', { id: message.repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to update repository', { repositoryId: message.repositoryId, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`更新仓库失败: ${error.message}`, 'error');
    }
  }

  private async handleDeleteRepository(message: Msg<'deleteRepository'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.deleteRepository(message.repositoryId);
      ctx.sendData('repositoryDeleted', { repositoryId: message.repositoryId });
      ctx.sendToast('仓库已删除', 'success');
      await this.handleLoadRepositories(ctx);
      logger.info('Repository deleted', { id: message.repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to delete repository', { repositoryId: message.repositoryId, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`删除仓库失败: ${error.message}`, 'error');
    }
  }

  private async handleRefreshRepository(message: Msg<'refreshRepository'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');
      const manager = new SkillRepositoryManager();
      manager.clearCache(message.repositoryId);
      ctx.sendData('repositoryRefreshed', { repositoryId: message.repositoryId });
      ctx.sendToast('仓库缓存已清除', 'success');
      logger.info('Repository cache cleared', { id: message.repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to refresh repository', { repositoryId: message.repositoryId, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`刷新仓库失败: ${error.message}`, 'error');
    }
  }

  private async handleLoadSkillLibrary(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');

      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      const skills = await manager.getAllSkills(repositories);

      const skillsConfig = LLMConfigLoader.loadSkillsConfig();
      const installedSkills = new Set<string>();
      if (skillsConfig && Array.isArray(skillsConfig.customTools)) {
        skillsConfig.customTools.forEach((tool: any) => { if (tool?.name) installedSkills.add(tool.name); });
      }
      if (skillsConfig && Array.isArray(skillsConfig.instructionSkills)) {
        skillsConfig.instructionSkills.forEach((skill: any) => { if (skill?.name) installedSkills.add(skill.name); });
      }

      const skillsWithStatus = skills.map(skill => ({ ...skill, installed: installedSkills.has(skill.fullName) }));
      ctx.sendData('skillLibraryLoaded', { skills: skillsWithStatus });
      logger.info('Skill library loaded', {
        totalSkills: skillsWithStatus.length,
        installedCount: skillsWithStatus.filter(s => s.installed).length,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load skill library', { error: error.message, stack: error.stack }, LogCategory.TOOLS);
      ctx.sendToast(`加载 Skill 库失败: ${error.message}`, 'error');
    }
  }
}
