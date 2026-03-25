/**
 * 恢复快照存储
 *
 * 内存态存储，按 chainId 索引。
 * 阶段四将升级为持久化到 session.json。
 */

import type { ResumeSnapshot } from './resume-snapshot-types';

export class ResumeSnapshotStore {
  /** chainId → snapshotId → snapshot */
  private snapshots = new Map<string, Map<string, ResumeSnapshot>>();

  /**
   * 写入快照
   */
  save(snapshot: ResumeSnapshot): void {
    const chainMap = this.snapshots.get(snapshot.chainId) ?? new Map<string, ResumeSnapshot>();
    chainMap.set(snapshot.id, snapshot);
    this.snapshots.set(snapshot.chainId, chainMap);
  }

  /**
   * 读取指定快照
   */
  get(snapshotId: string): ResumeSnapshot | undefined {
    for (const chainMap of this.snapshots.values()) {
      const snapshot = chainMap.get(snapshotId);
      if (snapshot) return snapshot;
    }
    return undefined;
  }

  /**
   * 获取执行链的最新快照（按 checkpointSeq 降序）
   */
  getLatest(chainId: string): ResumeSnapshot | undefined {
    const chainMap = this.snapshots.get(chainId);
    if (!chainMap || chainMap.size === 0) return undefined;

    let latest: ResumeSnapshot | undefined;
    for (const snapshot of chainMap.values()) {
      if (!latest || snapshot.checkpointSeq > latest.checkpointSeq) {
        latest = snapshot;
      }
    }
    return latest;
  }

  /**
   * 获取执行链的所有快照（按 checkpointSeq 升序）
   */
  listByChain(chainId: string): ResumeSnapshot[] {
    const chainMap = this.snapshots.get(chainId);
    if (!chainMap) return [];
    return Array.from(chainMap.values()).sort((a, b) => a.checkpointSeq - b.checkpointSeq);
  }

  /**
   * 获取指定 attempt 的下一个 checkpointSeq
   */
  getNextCheckpointSeq(chainId: string, attempt: number): number {
    const chainMap = this.snapshots.get(chainId);
    if (!chainMap) return 1;

    let maxSeq = 0;
    for (const snapshot of chainMap.values()) {
      if (snapshot.attempt === attempt && snapshot.checkpointSeq > maxSeq) {
        maxSeq = snapshot.checkpointSeq;
      }
    }
    return maxSeq + 1;
  }

  /**
   * 清除指定执行链的所有快照
   */
  clearChain(chainId: string): void {
    this.snapshots.delete(chainId);
  }

  dispose(): void {
    this.snapshots.clear();
  }

  // ===== 持久化序列化/反序列化 =====

  /**
   * 导出指定链 ID 列表涉及的所有快照
   */
  exportByChainIds(chainIds: string[]): ResumeSnapshot[] {
    const result: ResumeSnapshot[] = [];
    for (const chainId of chainIds) {
      const chainMap = this.snapshots.get(chainId);
      if (chainMap) {
        for (const snapshot of chainMap.values()) {
          result.push(snapshot);
        }
      }
    }
    return result;
  }

  /**
   * 批量导入快照（用于 session 恢复）
   */
  importSnapshots(snapshots: ResumeSnapshot[]): void {
    for (const snapshot of snapshots) {
      this.save(snapshot);
    }
  }
}

