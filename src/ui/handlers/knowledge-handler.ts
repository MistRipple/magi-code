/**
 * KnowledgeHandler - 项目知识库消息处理器（P1-3 修复）
 *
 * 从 WebviewProvider 提取的独立 Handler。
 * 职责：ADR / FAQ / 知识库 CRUD + 知识库内文件打开。
 */

import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import { logger, LogCategory } from '../../logging';
import type { WebviewToExtensionMessage } from '../../types';
import { GovernedKnowledgeContextService } from '../../knowledge/governed-knowledge-context-service';
import type { CommandHandler, CommandHandlerContext } from './types';
import { t } from '../../i18n';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'getProjectKnowledge', 'clearProjectKnowledge',
  'deleteADR', 'deleteFAQ', 'deleteLearning',
  'addADR', 'updateADR', 'addFAQ', 'updateFAQ',
]);

export class KnowledgeCommandHandler implements CommandHandler {
  readonly supportedTypes: ReadonlySet<string> = SUPPORTED;

  async handle(message: WebviewToExtensionMessage, ctx: CommandHandlerContext): Promise<void> {
    switch (message.type) {
      case 'getProjectKnowledge':
        await this.handleGetProjectKnowledge(ctx);
        break;
      case 'clearProjectKnowledge':
        await this.handleClearProjectKnowledge(ctx);
        break;
      case 'deleteADR':
        await this.handleDeleteADR(message as Msg<'deleteADR'>, ctx);
        break;
      case 'deleteFAQ':
        await this.handleDeleteFAQ(message as Msg<'deleteFAQ'>, ctx);
        break;
      case 'deleteLearning':
        await this.handleDeleteLearning(message as Msg<'deleteLearning'>, ctx);
        break;
      case 'addADR':
        await this.handleAddADR(message as Msg<'addADR'>, ctx);
        break;
      case 'updateADR':
        await this.handleUpdateADR(message as Msg<'updateADR'>, ctx);
        break;
      case 'addFAQ':
        await this.handleAddFAQ(message as Msg<'addFAQ'>, ctx);
        break;
      case 'updateFAQ':
        await this.handleUpdateFAQ(message as Msg<'updateFAQ'>, ctx);
        break;
    }
  }

  private async resolveKnowledgeBase(ctx: CommandHandlerContext) {
    await ctx.ensureProjectKnowledgeBaseReady();
    const kb = ctx.getProjectKnowledgeBase();
    if (!kb) {
      ctx.sendToast(t('knowledge.toast.notInitialized'), 'warning');
      return undefined;
    }
    return kb;
  }

  private async handleGetProjectKnowledge(ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) {
        return;
      }

      const payload = new GovernedKnowledgeContextService(kb).buildKnowledgeSnapshot({
        purpose: 'ui_panel',
        consumer: 'knowledge_handler',
      });

      ctx.sendData('projectKnowledgeLoaded', payload as unknown as Record<string, unknown>);
      logger.info('项目知识.已加载', {
        files: payload.codeIndex ? payload.codeIndex.files.length : 0,
        adrs: payload.adrs.length,
        faqs: payload.faqs.length,
        learnings: payload.learnings.length,
      }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('项目知识.加载失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.loadFailed', { error: error.message }), 'error');
    }
  }

  private async handleClearProjectKnowledge(ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;

      const counts = kb.clearAll();
      const total = counts.adrs + counts.faqs + counts.learnings;
      ctx.sendToast(t('knowledge.toast.cleared', { total, adrs: counts.adrs, faqs: counts.faqs, learnings: counts.learnings }), 'success');
      logger.info('项目知识库.已清空', counts, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('项目知识库.清空失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.clearFailed', { error: error.message }), 'error');
    }
  }

  private async handleDeleteADR(message: Msg<'deleteADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      const success = kb.deleteADR(message.id);
      if (success) {
        ctx.sendToast(t('knowledge.toast.adrDeleted'), 'success');
        logger.info('ADR.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast(t('knowledge.toast.adrDeleteFailed'), 'error');
      }
    } catch (error: any) {
      logger.error('ADR.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.deleteFailed', { error: error.message }), 'error');
    }
  }

  private async handleDeleteFAQ(message: Msg<'deleteFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      const success = kb.deleteFAQ(message.id);
      if (success) {
        ctx.sendToast(t('knowledge.toast.faqDeleted'), 'success');
        logger.info('FAQ.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast(t('knowledge.toast.faqDeleteFailed'), 'error');
      }
    } catch (error: any) {
      logger.error('FAQ.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.deleteFailed', { error: error.message }), 'error');
    }
  }

  private async handleDeleteLearning(message: Msg<'deleteLearning'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      const success = kb.deleteLearning(message.id);
      if (success) {
        ctx.sendToast(t('knowledge.toast.learningDeleted'), 'success');
        logger.info('Learning.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast(t('knowledge.toast.learningDeleteFailed'), 'error');
      }
    } catch (error: any) {
      logger.error('Learning.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.deleteFailed', { error: error.message }), 'error');
    }
  }

  private async handleAddADR(message: Msg<'addADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      kb.addADR(message.adr);
      ctx.sendToast(t('knowledge.toast.adrAdded'), 'success');
      logger.info('ADR.已添加', { id: message.adr.id, title: message.adr.title }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('ADR.添加失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.addAdrFailed', { error: error.message }), 'error');
    }
  }

  private async handleUpdateADR(message: Msg<'updateADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      const success = kb.updateADR(message.id, message.updates);
      if (success) {
        ctx.sendToast(t('knowledge.toast.adrUpdated'), 'success');
        logger.info('ADR.已更新', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast(t('knowledge.toast.adrNotExist'), 'warning');
      }
    } catch (error: any) {
      logger.error('ADR.更新失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.updateAdrFailed', { error: error.message }), 'error');
    }
  }

  private async handleAddFAQ(message: Msg<'addFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      kb.addFAQ(message.faq);
      ctx.sendToast(t('knowledge.toast.faqAdded'), 'success');
      logger.info('FAQ.已添加', { id: message.faq.id, question: message.faq.question }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('FAQ.添加失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.addFaqFailed', { error: error.message }), 'error');
    }
  }

  private async handleUpdateFAQ(message: Msg<'updateFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = await this.resolveKnowledgeBase(ctx);
      if (!kb) return;
      const success = kb.updateFAQ(message.id, message.updates);
      if (success) {
        ctx.sendToast(t('knowledge.toast.faqUpdated'), 'success');
        logger.info('FAQ.已更新', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast(t('knowledge.toast.faqNotExist'), 'warning');
      }
    } catch (error: any) {
      logger.error('FAQ.更新失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(t('knowledge.toast.updateFaqFailed', { error: error.message }), 'error');
    }
  }
}
