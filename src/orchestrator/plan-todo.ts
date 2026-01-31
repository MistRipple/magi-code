import * as fs from 'fs';
import * as path from 'path';
import { Mission } from './mission/types';

/**
 * Mission TODO 文件管理器
 *
 * 存储位置：.multicli/sessions/{sessionId}/missions/{missionId}.md
 */
export class PlanTodoManager {
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /** 获取会话的 missions 目录 */
  private getMissionsDir(sessionId: string): string {
    return path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId, 'missions');
  }

  private ensureMissionsDir(sessionId: string): void {
    const missionsDir = this.getMissionsDir(sessionId);
    if (!fs.existsSync(missionsDir)) {
      fs.mkdirSync(missionsDir, { recursive: true });
    }
  }

  private getMissionTodoPath(sessionId: string, missionId: string): string {
    return path.join(this.getMissionsDir(sessionId), `${missionId}.md`);
  }

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
}
