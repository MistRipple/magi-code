import * as fs from 'fs';
import * as path from 'path';
import { PlanRecord } from './plan-storage';

export class PlanTodoManager {
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

  private getTodoPath(planId: string): string {
    return path.join(this.plansDir, `${planId}.md`);
  }

  ensurePlanFile(record: PlanRecord): void {
    this.ensureDir();
    const todoPath = this.getTodoPath(record.id);
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

  updateSubTaskStatus(planId: string, subTaskId: string, status: 'completed' | 'failed'): void {
    const todoPath = this.getTodoPath(planId);
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
