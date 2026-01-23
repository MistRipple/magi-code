import * as fs from 'fs';
import * as path from 'path';
import { PlanRecord } from './plan-storage';
import { Mission } from './mission/types';

/**
 * 计划 TODO 文件管理器
 *
 * 存储位置：
 * - 旧架构：.multicli/sessions/{sessionId}/plans/{planId}.md
 * - 新架构：.multicli/sessions/{sessionId}/missions/{missionId}.md
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

  /** 获取会话的 missions 目录 */
  private getMissionsDir(sessionId: string): string {
    return path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId, 'missions');
  }

  private ensureDir(sessionId: string): void {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) {
      fs.mkdirSync(plansDir, { recursive: true });
    }
  }

  private ensureMissionsDir(sessionId: string): void {
    const missionsDir = this.getMissionsDir(sessionId);
    if (!fs.existsSync(missionsDir)) {
      fs.mkdirSync(missionsDir, { recursive: true });
    }
  }

  private getTodoPath(sessionId: string, planId: string): string {
    return path.join(this.getPlansDir(sessionId), `${planId}.md`);
  }

  private getMissionTodoPath(sessionId: string, missionId: string): string {
    return path.join(this.getMissionsDir(sessionId), `${missionId}.md`);
  }

  // ============================================================================
  // Mission 支持（新架构）
  // ============================================================================

  /**
   * 为 Mission 生成 TODO 文件
   */
  ensureMissionTodoFile(mission: Mission, sessionId: string): void {
    this.ensureMissionsDir(sessionId);
    const todoPath = this.getMissionTodoPath(sessionId, mission.id);
    if (fs.existsSync(todoPath)) {
      return;
    }

    const lines: string[] = [];
    lines.push(`# Mission: ${mission.goal}`);
    lines.push('');
    lines.push(`**ID**: ${mission.id}`);
    lines.push(`**Status**: ${mission.status}`);
    lines.push(`**Phase**: ${mission.phase}`);
    lines.push(`**Created**: ${new Date(mission.createdAt).toISOString()}`);
    lines.push('');

    if (mission.analysis) {
      lines.push('## Analysis');
      lines.push(mission.analysis);
      lines.push('');
    }

    if (mission.constraints && mission.constraints.length > 0) {
      lines.push('## Constraints');
      for (const constraint of mission.constraints) {
        lines.push(`- **${constraint.type}**: ${constraint.description}`);
      }
      lines.push('');
    }

    lines.push('## Assignments');
    lines.push('');

    for (const assignment of mission.assignments || []) {
      const worker = assignment.workerId || 'unknown';
      lines.push(`### ${assignment.responsibility} [${worker}]`);
      lines.push('');

      if (assignment.todos && assignment.todos.length > 0) {
        for (const todo of assignment.todos) {
          const marker = todo.status === 'completed' ? 'x' : ' ';
          const failedMarker = todo.status === 'failed' ? ' [FAILED]' : '';
          lines.push(`- [${marker}] (${todo.id}) ${todo.content}${failedMarker}`);
        }
      } else {
        lines.push('_No todos yet_');
      }
      lines.push('');
    }

    fs.writeFileSync(todoPath, lines.join('\n'), 'utf-8');
  }

  /**
   * 更新 Mission 中某个 Todo 的状态
   */
  updateMissionTodoStatus(
    sessionId: string,
    missionId: string,
    todoId: string,
    status: 'completed' | 'failed'
  ): void {
    const todoPath = this.getMissionTodoPath(sessionId, missionId);
    if (!fs.existsSync(todoPath)) {
      return;
    }

    const content = fs.readFileSync(todoPath, 'utf-8');
    const lines = content.split('\n');
    const nextLines = lines.map(line => {
      if (!line.startsWith('- [')) {
        return line;
      }
      if (!line.includes(`(${todoId})`)) {
        return line;
      }
      const marker = status === 'completed' ? 'x' : '!';
      const stripped = line.replace(/^- \[[ x!]?\]\s*/, '');
      const suffix = status === 'failed' && !stripped.includes('[FAILED]') ? ' [FAILED]' : '';
      return `- [${marker}] ${stripped}${suffix}`;
    });

    fs.writeFileSync(todoPath, nextLines.join('\n'), 'utf-8');
  }

  // ============================================================================
  // 旧架构支持（保留以兼容）
  // ============================================================================

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
      const worker = task.assignedWorker || 'unknown';
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
