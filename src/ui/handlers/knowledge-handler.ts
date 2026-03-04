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
import type { CommandHandler, CommandHandlerContext } from './types';

type Msg<T extends string> = Extract<WebviewToExtensionMessage, { type: T }>;

const SUPPORTED = new Set([
  'getProjectKnowledge', 'clearProjectKnowledge',
  'getADRs', 'getFAQs', 'searchFAQs',
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
      case 'getADRs':
        await this.handleGetADRs(message as Msg<'getADRs'>, ctx);
        break;
      case 'getFAQs':
        await this.handleGetFAQs(message as Msg<'getFAQs'>, ctx);
        break;
      case 'searchFAQs':
        await this.handleSearchFAQs(message as Msg<'searchFAQs'>, ctx);
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

  private async handleGetProjectKnowledge(ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) {
        ctx.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const codeIndex = kb.getCodeIndex();
      const adrs = kb.getADRs();
      const faqs = kb.getFAQs();
      const learnings = kb.getLearnings();

      ctx.sendData('projectKnowledgeLoaded', { codeIndex, adrs, faqs, learnings });
      logger.info('项目知识.已加载', {
        files: codeIndex ? codeIndex.files.length : 0,
        adrs: adrs.length,
        faqs: faqs.length,
        learnings: learnings.length,
      }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('项目知识.加载失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`加载项目知识失败: ${error.message}`, 'error');
    }
  }

  private async handleClearProjectKnowledge(ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) {
        ctx.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const counts = kb.clearAll();
      const total = counts.adrs + counts.faqs + counts.learnings;
      ctx.sendToast(`已清空 ${total} 条知识记录（ADR: ${counts.adrs}, FAQ: ${counts.faqs}, 经验: ${counts.learnings}）`, 'success');
      logger.info('项目知识库.已清空', counts, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('项目知识库.清空失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`清空失败: ${error.message}`, 'error');
    }
  }

  private async handleGetADRs(message: Msg<'getADRs'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const adrs = kb.getADRs(message.filter as any);
      logger.info('ADR.已加载', { count: adrs.length, filter: message.filter }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('ADR.加载失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`加载 ADR 失败: ${error.message}`, 'error');
    }
  }

  private async handleGetFAQs(message: Msg<'getFAQs'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const faqs = kb.getFAQs(message.filter);
      logger.info('FAQ.已加载', { count: faqs.length, filter: message.filter }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('FAQ.加载失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`加载 FAQ 失败: ${error.message}`, 'error');
    }
  }

  private async handleSearchFAQs(message: Msg<'searchFAQs'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const results = kb.searchFAQs(message.keyword);
      logger.info('FAQ.搜索完成', { keyword: message.keyword, count: results.length }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('FAQ.搜索失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`搜索 FAQ 失败: ${error.message}`, 'error');
    }
  }

  private async handleDeleteADR(message: Msg<'deleteADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const success = kb.deleteADR(message.id);
      if (success) {
        ctx.sendToast('ADR 已删除', 'success');
        logger.info('ADR.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast('ADR 删除失败', 'error');
      }
    } catch (error: any) {
      logger.error('ADR.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`删除失败: ${error.message}`, 'error');
    }
  }

  private async handleDeleteFAQ(message: Msg<'deleteFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const success = kb.deleteFAQ(message.id);
      if (success) {
        ctx.sendToast('FAQ 已删除', 'success');
        logger.info('FAQ.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast('FAQ 删除失败', 'error');
      }
    } catch (error: any) {
      logger.error('FAQ.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`删除失败: ${error.message}`, 'error');
    }
  }

  private async handleDeleteLearning(message: Msg<'deleteLearning'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const success = kb.deleteLearning(message.id);
      if (success) {
        ctx.sendToast('经验记录已删除', 'success');
        logger.info('Learning.删除成功', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast('经验记录删除失败', 'error');
      }
    } catch (error: any) {
      logger.error('Learning.删除失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`删除失败: ${error.message}`, 'error');
    }
  }

  private async handleAddADR(message: Msg<'addADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      kb.addADR(message.adr);
      ctx.sendToast('ADR 已添加', 'success');
      logger.info('ADR.已添加', { id: message.adr.id, title: message.adr.title }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('ADR.添加失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`添加 ADR 失败: ${error.message}`, 'error');
    }
  }

  private async handleUpdateADR(message: Msg<'updateADR'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const success = kb.updateADR(message.id, message.updates);
      if (success) {
        ctx.sendToast('ADR 已更新', 'success');
        logger.info('ADR.已更新', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast('ADR 不存在', 'warning');
      }
    } catch (error: any) {
      logger.error('ADR.更新失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`更新 ADR 失败: ${error.message}`, 'error');
    }
  }

  private async handleAddFAQ(message: Msg<'addFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      kb.addFAQ(message.faq);
      ctx.sendToast('FAQ 已添加', 'success');
      logger.info('FAQ.已添加', { id: message.faq.id, question: message.faq.question }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge(ctx);
    } catch (error: any) {
      logger.error('FAQ.添加失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`添加 FAQ 失败: ${error.message}`, 'error');
    }
  }

  private async handleUpdateFAQ(message: Msg<'updateFAQ'>, ctx: CommandHandlerContext): Promise<void> {
    try {
      const kb = ctx.getProjectKnowledgeBase();
      if (!kb) { ctx.sendToast('项目知识库未初始化', 'warning'); return; }
      const success = kb.updateFAQ(message.id, message.updates);
      if (success) {
        ctx.sendToast('FAQ 已更新', 'success');
        logger.info('FAQ.已更新', { id: message.id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge(ctx);
      } else {
        ctx.sendToast('FAQ 不存在', 'warning');
      }
    } catch (error: any) {
      logger.error('FAQ.更新失败', { error: error.message }, LogCategory.SESSION);
      ctx.sendToast(`更新 FAQ 失败: ${error.message}`, 'error');
    }
  }
}
