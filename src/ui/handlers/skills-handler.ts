/**
 * SkillsHandler - Skills 配置与仓库管理消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：Skills 配置 CRUD + 自定义工具管理 + 仓库管理 + Skill 安装。
 */

import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { logger, LogCategory } from '../../logging';
import type { WebviewToExtensionMessage } from '../../types';
import { applySkillInstall } from '../../tools/skill-installation';
import type { CommandHandler, CommandHandlerContext } from './types';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'loadSkillsConfig', 'saveSkillsConfig',
  'addCustomTool', 'removeCustomTool', 'removeInstructionSkill', 'installSkill',
  'installLocalSkill',
  'updateSkill', 'updateAllSkills',
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
      case 'installLocalSkill':
        await this.handleInstallLocalSkill(ctx);
        break;
      case 'updateSkill':
        await this.handleUpdateSkill(message as Msg<'updateSkill'>, ctx);
        break;
      case 'updateAllSkills':
        await this.handleUpdateAllSkills(ctx);
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
      ctx.sendData('skillInstallFailed', { skillId: message.skillId, error: error.message, source: 'repository' });
      ctx.sendToast(`安装 Skill 失败: ${error.message}`, 'error');
    }
  }

  private parseSkillMarkdown(content: string): { meta: Record<string, any>; body: string } {
    const trimmed = content.trim();
    if (!trimmed.startsWith('---')) {
      return { meta: {}, body: content.trim() };
    }

    const lines = trimmed.split('\n');
    const metaLines: string[] = [];
    let i = 1;
    for (; i < lines.length; i++) {
      const line = lines[i].trim();
      if (line === '---') {
        i++;
        break;
      }
      metaLines.push(lines[i]);
    }

    const meta: Record<string, any> = {};
    let currentKey: string | null = null;

    for (const line of metaLines) {
      const trimmedLine = line.trim();
      if (!trimmedLine) continue;

      if (trimmedLine.startsWith('- ') && currentKey) {
        if (!Array.isArray(meta[currentKey])) {
          meta[currentKey] = [];
        }
        const listValue = trimmedLine.slice(2).trim().replace(/^['"]|['"]$/g, '');
        meta[currentKey].push(listValue);
        continue;
      }

      const match = trimmedLine.match(/^([A-Za-z0-9_-]+)\s*:\s*(.*)$/);
      if (!match) continue;

      const key = match[1];
      let value: any = match[2].trim();
      if (value === '') {
        meta[key] = [];
      } else if (value === 'true' || value === 'false') {
        meta[key] = value === 'true';
      } else {
        value = value.replace(/^['"]|['"]$/g, '');
        meta[key] = value;
      }
      currentKey = key;
    }

    const body = lines.slice(i).join('\n').trim();
    return { meta, body };
  }

  private toBoolean(value: unknown, defaultValue: boolean): boolean {
    if (typeof value === 'boolean') return value;
    if (typeof value === 'string') {
      const lower = value.trim().toLowerCase();
      if (lower === 'true') return true;
      if (lower === 'false') return false;
    }
    return defaultValue;
  }

  private toStringArray(value: unknown): string[] | undefined {
    const normalizeItem = (item: string): string => item.trim().replace(/^['"]|['"]$/g, '').trim();

    if (Array.isArray(value)) {
      const items = value.map((item) => normalizeItem(String(item))).filter(Boolean);
      return items.length > 0 ? items : undefined;
    }
    if (typeof value === 'string') {
      const trimmed = value.trim();
      const rawItems = trimmed.startsWith('[') && trimmed.endsWith(']')
        ? trimmed.slice(1, -1).split(',')
        : trimmed.split(',');
      const items = rawItems.map((item) => normalizeItem(item)).filter(Boolean);
      return items.length > 0 ? items : undefined;
    }
    return undefined;
  }

  private normalizeSkillSlug(rawName: string): string {
    const trimmed = rawName.trim().toLowerCase();
    const replaced = trimmed.replace(/\s+/g, '-').replace(/[^a-z0-9._-]/g, '-');
    return replaced.replace(/-+/g, '-').replace(/^[-_.]+|[-_.]+$/g, '');
  }

  private shortHash(input: string): string {
    let hash = 2166136261;
    for (let i = 0; i < input.length; i++) {
      hash ^= input.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return (hash >>> 0).toString(16).padStart(8, '0').slice(0, 8);
  }

  private buildLocalSkillName(rawName: string, filePath: string): string {
    const slug = this.normalizeSkillSlug(rawName);
    const safe = slug || `skill-${this.shortHash(`${rawName}|${filePath}`)}`;
    return `local/${safe}`;
  }

  private extractDescription(body: string): string {
    const lines = body.split('\n').map((line) => line.trim()).filter(Boolean);
    for (const line of lines) {
      if (line.startsWith('#')) {
        const title = line.replace(/^#+\s*/, '').trim();
        if (title) return title;
        continue;
      }
      return line.length > 120 ? `${line.slice(0, 120)}...` : line;
    }
    return 'Local instruction skill';
  }

  private async handleInstallLocalSkill(ctx: CommandHandlerContext): Promise<void> {
    const selection = await vscode.window.showOpenDialog({
      canSelectFiles: true,
      canSelectFolders: false,
      canSelectMany: false,
      openLabel: '导入 Skill',
      filters: {
        Markdown: ['md'],
      },
    });

    if (!selection || selection.length === 0) {
      ctx.sendData('skillInstallFailed', { source: 'local', canceled: true });
      return;
    }

    const fileUri = selection[0];

    try {
      const content = fs.readFileSync(fileUri.fsPath, 'utf-8');
      const { meta, body } = this.parseSkillMarkdown(content);
      const instruction = body.trim();
      if (!instruction) {
        throw new Error('SKILL.md 内容为空');
      }

      const fileBaseName = path.basename(fileUri.fsPath, path.extname(fileUri.fsPath));
      const rawName = String(meta.name || fileBaseName || 'local-skill');
      const normalizedName = this.buildLocalSkillName(rawName, fileUri.fsPath);
      const description = String(meta.description || this.extractDescription(instruction));
      const version = meta.version ? String(meta.version) : undefined;
      const allowedTools = this.toStringArray(meta['allowed-tools'] ?? meta.allowedTools ?? meta.allowed_tools);
      const disableModelInvocation = this.toBoolean(
        meta['disable-model-invocation'] ?? meta.disableModelInvocation ?? meta.disable_model_invocation,
        false,
      );
      const userInvocable = this.toBoolean(
        meta['user-invocable'] ?? meta.userInvocable ?? meta.user_invocable,
        true,
      );
      const argumentHint = meta['argument-hint'] || meta.argumentHint || meta.argument_hint
        ? String(meta['argument-hint'] || meta.argumentHint || meta.argument_hint)
        : undefined;

      const localSkill = {
        id: normalizedName,
        name: normalizedName,
        fullName: normalizedName,
        description,
        version,
        repositoryId: 'local',
        repositoryName: 'Local Skills',
        skillType: 'instruction' as const,
        instruction,
        allowedTools,
        disableModelInvocation,
        userInvocable,
        argumentHint,
      };

      const { LLMConfigLoader } = await import('../../llm/config');
      const repositories = LLMConfigLoader.loadRepositories();
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, localSkill as any);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);

      ctx.sendData('skillInstalled', { skillId: normalizedName, skill: localSkill, source: 'local' });
      ctx.sendToast(`本地 Skill "${normalizedName}" 已安装`, 'success');
      await this.handleLoadSkillsConfig(ctx);
      await this.reloadSkills(ctx, 'installLocalSkill');
      logger.info('本地 Skill 已安装', {
        filePath: fileUri.fsPath,
        skillName: normalizedName,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      const message = error?.message || String(error);
      logger.error('本地 Skill 安装失败', {
        filePath: fileUri.fsPath,
        error: message,
      }, LogCategory.TOOLS);
      ctx.sendData('skillInstallFailed', { source: 'local', filePath: fileUri.fsPath, error: message });
      ctx.sendToast(`本地 Skill 安装失败: ${message}`, 'error');
    }
  }

  private async handleUpdateSkill(message: Msg<'updateSkill'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');

      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      // 清除所有仓库缓存，确保拉取到最新版本
      for (const repo of repositories) {
        manager.clearCache(repo.id);
      }

      const skills = await manager.getAllSkills(repositories);
      const latestSkill = skills.find((item: any) => item.fullName === message.skillName || item.name === message.skillName);
      if (!latestSkill) throw new Error(`远程仓库中未找到 Skill: ${message.skillName}`);

      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, latestSkill);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);

      ctx.sendData('skillUpdated', { skillName: message.skillName, version: latestSkill.version });
      ctx.sendToast(`Skill "${latestSkill.name}" 已更新${latestSkill.version ? ` (v${latestSkill.version})` : ''}`, 'success');
      await this.handleLoadSkillsConfig(ctx);
      await this.reloadSkills(ctx, 'updateSkill');
      logger.info('Skill 已更新', { name: message.skillName, version: latestSkill.version }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('更新 Skill 失败', { skillName: message.skillName, error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`更新 Skill 失败: ${error.message}`, 'error');
      ctx.sendData('skillUpdated', { skillName: message.skillName, error: error.message });
    }
  }

  private async handleUpdateAllSkills(ctx: CommandHandlerContext): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');

      let config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
      const installedNames = new Set<string>();
      (config.instructionSkills || []).forEach((s: any) => { if (s?.name) installedNames.add(s.name); });
      (config.customTools || []).forEach((t: any) => { if (t?.name) installedNames.add(t.name); });

      if (installedNames.size === 0) {
        ctx.sendData('allSkillsUpdated', { updatedCount: 0 });
        ctx.sendToast('没有已安装的 Skill 需要更新', 'info');
        return;
      }

      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      for (const repo of repositories) {
        manager.clearCache(repo.id);
      }

      const remoteSkills = await manager.getAllSkills(repositories);
      let updatedCount = 0;

      for (const remoteSkill of remoteSkills) {
        if (installedNames.has(remoteSkill.fullName) || installedNames.has(remoteSkill.name)) {
          config = applySkillInstall(config, remoteSkill);
          updatedCount++;
        }
      }

      LLMConfigLoader.saveSkillsConfig(config);

      ctx.sendData('allSkillsUpdated', { updatedCount });
      ctx.sendToast(`已更新 ${updatedCount} 个 Skill`, 'success');
      await this.handleLoadSkillsConfig(ctx);
      await this.reloadSkills(ctx, 'updateAllSkills');
      logger.info('所有 Skill 已更新', { updatedCount }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('批量更新 Skill 失败', { error: error.message }, LogCategory.TOOLS);
      ctx.sendToast(`批量更新 Skill 失败: ${error.message}`, 'error');
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
      const { skills, failedRepositories } = await manager.getAllSkillsWithReport(repositories);

      const skillsConfig = LLMConfigLoader.loadSkillsConfig();
      const installedSkills = new Set<string>();
      if (skillsConfig && Array.isArray(skillsConfig.customTools)) {
        skillsConfig.customTools.forEach((tool: any) => { if (tool?.name) installedSkills.add(tool.name); });
      }
      if (skillsConfig && Array.isArray(skillsConfig.instructionSkills)) {
        skillsConfig.instructionSkills.forEach((skill: any) => { if (skill?.name) installedSkills.add(skill.name); });
      }

      const skillsWithStatus = skills.map(skill => ({ ...skill, installed: installedSkills.has(skill.fullName) }));
      ctx.sendData('skillLibraryLoaded', {
        skills: skillsWithStatus,
        failedRepositories,
        totalRepositories: repositories.length,
      });

      if (failedRepositories.length > 0) {
        const preview = failedRepositories
          .slice(0, 2)
          .map((repo) => repo.repositoryId)
          .join(', ');
        const suffix = failedRepositories.length > 2 ? '...' : '';
        ctx.sendToast(
          `有 ${failedRepositories.length} 个仓库加载失败：${preview}${suffix}`,
          'warning',
        );
      }

      logger.info('Skill library loaded', {
        totalSkills: skillsWithStatus.length,
        installedCount: skillsWithStatus.filter(s => s.installed).length,
        failedRepositories: failedRepositories.length,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load skill library', { error: error.message, stack: error.stack }, LogCategory.TOOLS);
      ctx.sendToast(`加载 Skill 库失败: ${error.message}`, 'error');
    }
  }
}
