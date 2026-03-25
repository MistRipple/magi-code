import fs from 'fs';
import path from 'path';
import { logger, LogCategory } from '../logging';
import type {
  GovernedKnowledgePurpose,
  GovernedKnowledgeReference,
} from './governed-knowledge-context-service';

export interface KnowledgeGovernanceAuditRecord {
  schemaVersion: 'knowledge-governance-audit.v1';
  eventId: string;
  timestamp: number;
  generatedAt: string;
  resultKind: string;
  version: string;
  sourceUpdatedAt?: string;
  purpose: GovernedKnowledgePurpose;
  consumer?: string;
  sessionId?: string;
  requestId?: string;
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
  agentId?: string;
  workerId?: string;
  referenceCount: number;
  references: GovernedKnowledgeReference[];
}

export interface KnowledgeGovernanceAuditQuery {
  sessionId?: string;
  requestId?: string;
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
  workerId?: string;
  limit?: number;
}

export class FileKnowledgeGovernanceAuditStore {
  private readonly storagePath: string;

  constructor(workspaceRoot: string) {
    this.storagePath = path.join(workspaceRoot, '.magi', 'observability', 'knowledge', 'context-access.jsonl');
  }

  append(record: KnowledgeGovernanceAuditRecord): void {
    try {
      fs.mkdirSync(path.dirname(this.storagePath), { recursive: true });
      fs.appendFileSync(this.storagePath, `${JSON.stringify(record)}\n`, 'utf8');
    } catch (error) {
      logger.warn('知识治理.审计落盘失败', {
        storagePath: this.storagePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.SESSION);
    }
  }

  readAll(): KnowledgeGovernanceAuditRecord[] {
    if (!fs.existsSync(this.storagePath)) {
      return [];
    }

    try {
      const content = fs.readFileSync(this.storagePath, 'utf8');
      return content
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line) as KnowledgeGovernanceAuditRecord);
    } catch (error) {
      logger.warn('知识治理.审计读取失败', {
        storagePath: this.storagePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.SESSION);
      return [];
    }
  }

  query(input: KnowledgeGovernanceAuditQuery): KnowledgeGovernanceAuditRecord[] {
    const limit = this.normalizeLimit(input.limit, 12, 1, 50);
    const scoped = this.readAll().filter((record) => this.matchesQuery(record, input));
    return scoped.slice(-limit);
  }

  count(input: KnowledgeGovernanceAuditQuery): number {
    return this.readAll().filter((record) => this.matchesQuery(record, input)).length;
  }

  getStoragePath(): string {
    return this.storagePath;
  }

  private matchesQuery(record: KnowledgeGovernanceAuditRecord, query: KnowledgeGovernanceAuditQuery): boolean {
    const conditions: Array<[string | undefined, string | undefined]> = [
      [record.sessionId, this.normalizeString(query.sessionId)],
      [record.requestId, this.normalizeString(query.requestId)],
      [record.missionId, this.normalizeString(query.missionId)],
      [record.assignmentId, this.normalizeString(query.assignmentId)],
      [record.todoId, this.normalizeString(query.todoId)],
      [record.workerId, this.normalizeString(query.workerId)],
    ];

    for (const [actual, expected] of conditions) {
      if (!expected) {
        continue;
      }
      if (actual !== expected) {
        return false;
      }
    }
    return true;
  }

  private normalizeString(value: unknown): string | undefined {
    if (typeof value !== 'string') {
      return undefined;
    }
    const normalized = value.trim();
    return normalized || undefined;
  }

  private normalizeLimit(value: unknown, fallback: number, min: number, max: number): number {
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return fallback;
    }
    return Math.max(min, Math.min(max, Math.floor(parsed)));
  }
}
