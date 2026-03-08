import fs from 'fs';
import path from 'path';
import type { WorkerSlot } from '../../types';
import { logger, LogCategory } from '../../logging';

export type DispatchIdempotencyStatus = 'dispatched' | 'completed' | 'failed' | 'cancelled';

export interface DispatchIdempotencyRecord {
  key: string;
  sessionId: string;
  missionId: string;
  taskId: string;
  worker: WorkerSlot;
  category: string;
  taskName: string;
  routingReason: string;
  degraded: boolean;
  status: DispatchIdempotencyStatus;
  createdAt: number;
  updatedAt: number;
}

interface DispatchIdempotencyStoreFile {
  version: 1;
  records: DispatchIdempotencyRecord[];
}

export interface DispatchIdempotencyClaimInput {
  key: string;
  sessionId: string;
  missionId: string;
  taskId: string;
  worker: WorkerSlot;
  category: string;
  taskName: string;
  routingReason: string;
  degraded: boolean;
  status: DispatchIdempotencyStatus;
  createdAt?: number;
  updatedAt?: number;
}

export interface DispatchIdempotencyClaimResult {
  claimed: boolean;
  record: DispatchIdempotencyRecord;
}

/**
 * Dispatch 幂等账本（本地持久化）
 *
 * 目标：
 * - 跨进程重放时，避免相同 idempotency_key 重复派发
 * - 保留 taskId/status，支持恢复阶段做重复判定
 */
export class DispatchIdempotencyStore {
  private readonly filePath: string;
  private readonly lockPath: string;
  private readonly ttlMs: number;
  private readonly maxRecords: number;
  private readonly lockAcquireTimeoutMs: number;
  private readonly lockStaleMs: number;
  private readonly lockRetryMs: number;
  private readonly byKey = new Map<string, DispatchIdempotencyRecord>();
  private readonly keyByTaskId = new Map<string, string>();

  constructor(
    workspaceRoot: string,
    options?: {
      ttlMs?: number;
      maxRecords?: number;
      lockAcquireTimeoutMs?: number;
      lockStaleMs?: number;
      lockRetryMs?: number;
    },
  ) {
    this.filePath = path.join(workspaceRoot, '.magi', 'runtime', 'dispatch-idempotency.json');
    this.lockPath = `${this.filePath}.lock`;
    this.ttlMs = Math.max(60_000, options?.ttlMs ?? 24 * 60 * 60 * 1000);
    this.maxRecords = Math.max(100, options?.maxRecords ?? 5000);
    this.lockAcquireTimeoutMs = Math.max(500, options?.lockAcquireTimeoutMs ?? 5000);
    this.lockStaleMs = Math.max(1000, options?.lockStaleMs ?? 15_000);
    this.lockRetryMs = Math.max(5, options?.lockRetryMs ?? 25);
  }

  resolveByKey(key: string): DispatchIdempotencyRecord | null {
    const normalized = key.trim();
    if (!normalized) {
      return null;
    }
    return this.withExclusiveLock(() => {
      this.loadFromDiskUnsafe();
      const record = this.byKey.get(normalized);
      if (!record) {
        return null;
      }
      if (this.isExpired(record, Date.now())) {
        this.byKey.delete(normalized);
        this.keyByTaskId.delete(record.taskId);
        this.persistUnsafe();
        return null;
      }
      return { ...record };
    });
  }

  claimOrGet(input: DispatchIdempotencyClaimInput): DispatchIdempotencyClaimResult {
    return this.withExclusiveLock(() => {
      this.loadFromDiskUnsafe();
      const now = Date.now();
      const normalizedKey = input.key.trim();
      const existing = this.byKey.get(normalizedKey);
      if (existing && !this.isExpired(existing, now)) {
        return {
          claimed: false,
          record: { ...existing },
        };
      }

      const createdAt = Number.isFinite(input.createdAt) ? Math.floor(input.createdAt!) : now;
      const updatedAt = Number.isFinite(input.updatedAt) ? Math.floor(input.updatedAt!) : now;
      const next: DispatchIdempotencyRecord = {
        key: normalizedKey,
        sessionId: input.sessionId.trim(),
        missionId: input.missionId.trim(),
        taskId: input.taskId.trim(),
        worker: input.worker,
        category: input.category.trim(),
        taskName: input.taskName.trim(),
        routingReason: input.routingReason.trim(),
        degraded: input.degraded === true,
        status: this.normalizeStatus(input.status),
        createdAt,
        updatedAt,
      };

      if (!next.key || !next.taskId || !next.sessionId || !next.missionId) {
        throw new Error('幂等 claim 记录缺少关键字段');
      }

      this.byKey.set(next.key, next);
      this.keyByTaskId.set(next.taskId, next.key);
      this.prune(now);
      this.persistUnsafe();

      return {
        claimed: true,
        record: { ...next },
      };
    });
  }

  remember(record: DispatchIdempotencyClaimInput): DispatchIdempotencyRecord {
    const result = this.claimOrGet(record);
    if (!result.claimed) {
      // 保持历史接口语义：remember 在 key 已存在时返回既有记录，不覆写。
      return result.record;
    }
    return result.record;
  }

  updateStatusByTaskId(taskId: string, status: DispatchIdempotencyStatus): DispatchIdempotencyRecord | null {
    const normalizedTaskId = taskId.trim();
    if (!normalizedTaskId) {
      return null;
    }
    return this.withExclusiveLock(() => {
      this.loadFromDiskUnsafe();
      const key = this.keyByTaskId.get(normalizedTaskId);
      if (!key) {
        return null;
      }
      const current = this.byKey.get(key);
      if (!current) {
        this.keyByTaskId.delete(normalizedTaskId);
        this.persistUnsafe();
        return null;
      }
      if (current.status === status) {
        return { ...current };
      }
      const next: DispatchIdempotencyRecord = {
        ...current,
        status,
        updatedAt: Date.now(),
      };
      this.byKey.set(key, next);
      this.persistUnsafe();
      return { ...next };
    });
  }

  removeByTaskId(taskId: string): boolean {
    const normalizedTaskId = taskId.trim();
    if (!normalizedTaskId) {
      return false;
    }
    return this.withExclusiveLock(() => {
      this.loadFromDiskUnsafe();
      const key = this.keyByTaskId.get(normalizedTaskId);
      if (!key) {
        return false;
      }
      const removed = this.byKey.delete(key);
      this.keyByTaskId.delete(normalizedTaskId);
      this.persistUnsafe();
      return removed;
    });
  }

  clear(): void {
    this.withExclusiveLock(() => {
      this.byKey.clear();
      this.keyByTaskId.clear();
      this.persistUnsafe();
      return null;
    });
  }

  private loadFromDiskUnsafe(): void {
    this.byKey.clear();
    this.keyByTaskId.clear();
    if (!fs.existsSync(this.filePath)) {
      return;
    }

    try {
      const raw = fs.readFileSync(this.filePath, 'utf-8');
      const parsed = JSON.parse(raw) as DispatchIdempotencyStoreFile;
      const records = Array.isArray(parsed?.records) ? parsed.records : [];
      const now = Date.now();
      for (const item of records) {
        if (!item || typeof item !== 'object') {
          continue;
        }
        if (!item.key || !item.taskId || !item.sessionId || !item.missionId) {
          continue;
        }
        const record: DispatchIdempotencyRecord = {
          key: String(item.key).trim(),
          sessionId: String(item.sessionId).trim(),
          missionId: String(item.missionId).trim(),
          taskId: String(item.taskId).trim(),
          worker: item.worker as WorkerSlot,
          category: String(item.category || '').trim(),
          taskName: String(item.taskName || '').trim(),
          routingReason: String(item.routingReason || '').trim(),
          degraded: item.degraded === true,
          status: this.normalizeStatus(item.status),
          createdAt: Number.isFinite(item.createdAt) ? Math.floor(item.createdAt) : now,
          updatedAt: Number.isFinite(item.updatedAt) ? Math.floor(item.updatedAt) : now,
        };
        if (!record.key || !record.taskId || this.isExpired(record, now)) {
          continue;
        }
        this.byKey.set(record.key, record);
        this.keyByTaskId.set(record.taskId, record.key);
      }
      this.prune(now);
      this.persistUnsafe();
    } catch (error) {
      logger.warn('Dispatch.IdempotencyStore.加载失败', {
        filePath: this.filePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private normalizeStatus(input: unknown): DispatchIdempotencyStatus {
    if (input === 'completed' || input === 'failed' || input === 'cancelled') {
      return input;
    }
    return 'dispatched';
  }

  private isExpired(record: DispatchIdempotencyRecord, now: number): boolean {
    return now - record.updatedAt > this.ttlMs;
  }

  private prune(now: number): void {
    for (const [key, record] of this.byKey.entries()) {
      if (this.isExpired(record, now)) {
        this.byKey.delete(key);
        this.keyByTaskId.delete(record.taskId);
      }
    }

    if (this.byKey.size <= this.maxRecords) {
      return;
    }

    const records = Array.from(this.byKey.values())
      .sort((a, b) => a.updatedAt - b.updatedAt);
    const overflow = this.byKey.size - this.maxRecords;
    for (let i = 0; i < overflow; i++) {
      const record = records[i];
      if (!record) {
        continue;
      }
      this.byKey.delete(record.key);
      this.keyByTaskId.delete(record.taskId);
    }
  }

  private persistUnsafe(): void {
    try {
      fs.mkdirSync(path.dirname(this.filePath), { recursive: true });
      const payload: DispatchIdempotencyStoreFile = {
        version: 1,
        records: Array.from(this.byKey.values())
          .sort((a, b) => b.updatedAt - a.updatedAt),
      };
      const tmpPath = `${this.filePath}.${process.pid}.${Date.now()}.${Math.random().toString(16).slice(2)}.tmp`;
      fs.writeFileSync(tmpPath, JSON.stringify(payload, null, 2), 'utf-8');
      fs.renameSync(tmpPath, this.filePath);
    } catch (error) {
      logger.warn('Dispatch.IdempotencyStore.持久化失败', {
        filePath: this.filePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private withExclusiveLock<T>(work: () => T): T {
    const lockFd = this.acquireLock();
    try {
      return work();
    } finally {
      this.releaseLock(lockFd);
    }
  }

  private acquireLock(): number {
    const startedAt = Date.now();
    fs.mkdirSync(path.dirname(this.lockPath), { recursive: true });
    while (true) {
      try {
        const fd = fs.openSync(this.lockPath, 'wx');
        const payload = JSON.stringify({
          pid: process.pid,
          acquiredAt: Date.now(),
        });
        fs.writeFileSync(fd, payload, 'utf8');
        return fd;
      } catch (error) {
        const nodeError = error as NodeJS.ErrnoException;
        if (nodeError?.code !== 'EEXIST') {
          throw error;
        }

        this.tryCleanStaleLock();
        if (Date.now() - startedAt >= this.lockAcquireTimeoutMs) {
          throw new Error(`幂等账本锁获取超时: ${this.lockPath}`);
        }
        this.spinWait(this.lockRetryMs);
      }
    }
  }

  private releaseLock(lockFd: number): void {
    try {
      fs.closeSync(lockFd);
    } catch {
      // ignore close errors
    }
    try {
      fs.unlinkSync(this.lockPath);
    } catch {
      // ignore unlink errors
    }
  }

  private tryCleanStaleLock(): void {
    try {
      const stat = fs.statSync(this.lockPath);
      const age = Date.now() - stat.mtimeMs;
      if (age > this.lockStaleMs) {
        fs.unlinkSync(this.lockPath);
        logger.warn('Dispatch.IdempotencyStore.检测到陈旧锁并已清理', {
          lockPath: this.lockPath,
          ageMs: age,
        }, LogCategory.ORCHESTRATOR);
      }
    } catch {
      // lock vanished or stat failed, ignore
    }
  }

  private spinWait(ms: number): void {
    const start = Date.now();
    while (Date.now() - start < ms) {
      // busy wait for short lock retry window
    }
  }
}
