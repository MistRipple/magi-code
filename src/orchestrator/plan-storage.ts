import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';
import { ExecutionPlan } from './protocols/types';

export interface PlanRecord {
  id: string;
  sessionId: string;
  taskId: string;
  prompt: string;
  createdAt: number;
  updatedAt: number;
  plan: ExecutionPlan;
  formattedPlan: string;
  review?: PlanReview;
}

export interface PlanReview {
  status: 'approved' | 'rejected' | 'skipped';
  summary: string;
  reviewer: string;
  reviewedAt: number;
}

/**
 * 计划存储管理器
 *
 * 存储位置：.magi/sessions/{sessionId}/plans/
 * 每个会话的计划存储在对应会话目录下
 */
export class PlanStorage {
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /** 获取会话的计划目录 */
  private getPlansDir(sessionId: string): string {
    return path.join(this.workspaceRoot, '.magi', 'sessions', sessionId, 'plans');
  }

  private ensureDir(sessionId: string): void {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) {
      fs.mkdirSync(plansDir, { recursive: true });
    }
  }

  savePlan(record: PlanRecord): PlanRecord {
    this.ensureDir(record.sessionId);
    const filePath = path.join(this.getPlansDir(record.sessionId), `${record.id}.json`);
    fs.writeFileSync(filePath, JSON.stringify(record, null, 2), 'utf-8');
    return record;
  }

  getPlan(planId: string, sessionId: string): PlanRecord | null {
    const filePath = path.join(this.getPlansDir(sessionId), `${planId}.json`);
    if (!fs.existsSync(filePath)) return null;
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      return JSON.parse(content) as PlanRecord;
    } catch (error) {
      logger.warn('编排器.计划.加载_失败', { planId, error }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  listPlansForSession(sessionId: string): PlanRecord[] {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) return [];
    const files = fs.readdirSync(plansDir).filter(f => f.endsWith('.json'));
    const records: PlanRecord[] = [];
    for (const file of files) {
      const planId = file.replace(/\.json$/, '');
      const record = this.getPlan(planId, sessionId);
      if (record) {
        records.push(record);
      }
    }
    return records.sort((a, b) => b.updatedAt - a.updatedAt);
  }

  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    const records = this.listPlansForSession(sessionId);
    return records[0] ?? null;
  }
}
