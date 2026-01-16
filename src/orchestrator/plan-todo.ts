import * as fs from 'fs';
import * as path from 'path';
import { PlanRecord } from './plan-storage';

/**
 * 计划 TODO 文件管理器
 *
 * 存储位置：.multicli/sessions/{sessionId}/plans/{planId}.md
 * 每个会话的计划 TODO 文件存储在对应会话目录下
 */
export class PlanTodoManager {
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /** 获取会话的计划目录 */
  private getPlansDir(sessionId: string): string {
    return path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId, 'plans');
  }

  private ensureDir(sessionId: string): void {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) {
      fs.mkdirSync(plansDir, { recursive: true });
    }
  }

  private getTodoPath(sessionId: string, planId: string): string {
    return path.join(this.getPlansDir(sessionId), `${planId}.md`);
  }

  ensurePlanFile(record: PlanRecord): void {
    this.ensureDir(record.sessionId);
    const todoPath = this.getTodoPath(record.sessionId, record.id);
    if (fs.existsSync(todoPath)) {
      return;
    }

    const lines: string[] = [];
    lines.push(`# Execution Plan: ${record.id}`);
    lines.push('');
    lines.push(`Prompt: ${record.prompt}`);
    lines.push(`Updated: ${new Date(record.updatedAt).toISOString()}`);
    if (record.review?.summary) {
      lines.push(`Review: ${record.review.status} - ${record.review.summary}`);
    }
    lines.push('');
    lines.push('## Tasks');

    for (const task of record.plan.subTasks || []) {
      const worker = task.assignedWorker || task.assignedCli || 'unknown';
      const files = task.targetFiles && task.targetFiles.length > 0
        ? ` | files: ${task.targetFiles.join(', ')}`
        : '';
      lines.push(`- [ ] (${task.id}) ${task.description} [${worker}]${files}`);
    }

    fs.writeFileSync(todoPath, lines.join('\n'), 'utf-8');
  }

  updateSubTaskStatus(sessionId: string, planId: string, subTaskId: string, status: 'completed' | 'failed'): void {
    const todoPath = this.getTodoPath(sessionId, planId);
    if (!fs.existsSync(todoPath)) {
      return;
    }

    const content = fs.readFileSync(todoPath, 'utf-8');
    const lines = content.split('\n');
    const nextLines = lines.map(line => {
      if (!line.startsWith('- [')) {
        return line;
      }
      if (!line.includes(`(${subTaskId})`)) {
        return line;
      }
      const marker = status === 'completed' ? 'x' : '!';
      const stripped = line.replace(/^- \[[ x!]?\]\s*/, '');
      const suffix = status === 'failed' && !stripped.includes('[FAILED]') ? ' [FAILED]' : '';
      return `- [${marker}] ${stripped}${suffix}`;
    });

    fs.writeFileSync(todoPath, nextLines.join('\n'), 'utf-8');
  }
}
