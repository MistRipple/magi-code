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

export class PlanStorage {
  private plansDir: string;

  constructor(workspaceRoot: string) {
    this.plansDir = path.join(workspaceRoot, '.multicli', 'plans');
    this.ensureDir();
  }

  private ensureDir(): void {
    if (!fs.existsSync(this.plansDir)) {
      fs.mkdirSync(this.plansDir, { recursive: true });
    }
  }

  savePlan(record: PlanRecord): PlanRecord {
    this.ensureDir();
    const filePath = path.join(this.plansDir, `${record.id}.json`);
    fs.writeFileSync(filePath, JSON.stringify(record, null, 2), 'utf-8');
    return record;
  }

  getPlan(planId: string): PlanRecord | null {
    const filePath = path.join(this.plansDir, `${planId}.json`);
    if (!fs.existsSync(filePath)) return null;
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      return JSON.parse(content) as PlanRecord;
    } catch (error) {
      console.warn('[PlanStorage] 读取计划失败:', planId, error);
      return null;
    }
  }

  listPlansForSession(sessionId: string): PlanRecord[] {
    if (!fs.existsSync(this.plansDir)) return [];
    const files = fs.readdirSync(this.plansDir).filter(f => f.endsWith('.json'));
    const records: PlanRecord[] = [];
    for (const file of files) {
      const planId = file.replace(/\.json$/, '');
      const record = this.getPlan(planId);
      if (record && record.sessionId === sessionId) {
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
