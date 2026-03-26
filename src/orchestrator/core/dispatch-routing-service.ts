import type { WorkerSlot } from '../../types';
import type { ProfileLoader } from '../profile/profile-loader';
import { LLMConfigLoader } from '../../llm/config';

export interface WorkerAvailabilitySnapshot {
  availableWorkers: Set<WorkerSlot>;
  unavailableReasons: Map<WorkerSlot, string>;
}

interface ResolveExecutionOptions {
  busyWorkers?: Set<WorkerSlot>;
  excludedWorkers?: Set<WorkerSlot>;
  allowBusyFallback?: boolean;
}

export class DispatchRoutingService {
  private runtimeUnavailableWorkers = new Map<WorkerSlot, { until: number; reason: string }>();

  constructor(
    private readonly profileLoader: ProfileLoader,
    private readonly workerSlots: readonly WorkerSlot[],
    private readonly fallbackPriority: Readonly<Record<WorkerSlot, WorkerSlot[]>>,
    private readonly runtimeUnavailableCooldownMs: number,
  ) {}

  getWorkerAvailability(): { availableWorkers: WorkerSlot[]; unavailableReasons: Record<string, string> } {
    const snapshot = this.getWorkerAvailabilitySnapshot();
    return {
      availableWorkers: Array.from(snapshot.availableWorkers),
      unavailableReasons: Object.fromEntries(snapshot.unavailableReasons.entries()),
    };
  }

  markWorkerRuntimeUnavailable(worker: WorkerSlot, reason: string): void {
    this.runtimeUnavailableWorkers.set(worker, {
      until: Date.now() + this.runtimeUnavailableCooldownMs,
      reason,
    });
  }

  clearWorkerRuntimeUnavailable(worker: WorkerSlot): void {
    this.runtimeUnavailableWorkers.delete(worker);
  }

  clearAllRuntimeUnavailable(): void {
    this.runtimeUnavailableWorkers.clear();
  }

  shouldMarkRuntimeUnavailable(errorMessage: string): boolean {
    const normalized = (errorMessage || '').toLowerCase();
    if (!normalized) {
      return false;
    }

    const infraErrorPattern =
      /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|billing|payment|rate limit|limit|insufficient|suspended|disabled|timeout|timed out|network|connection|fetch failed|socket|econnreset|econnrefused|enotfound|eai_again|tls|certificate|overloaded|service unavailable|502|503|504/;

    return infraErrorPattern.test(normalized);
  }

  shouldAutoFailoverRuntime(errorMessage: string): boolean {
    const normalized = (errorMessage || '').toLowerCase();
    if (!normalized) {
      return false;
    }

    const transientInfraErrorPattern =
      /rate limit|limit exceeded|限流|timeout|timed out|超时|network|connection|fetch failed|socket|econnreset|econnrefused|enotfound|eai_again|tls|certificate|overloaded|service unavailable|502|503|504/;

    return transientInfraErrorPattern.test(normalized);
  }

  resolveExecutionWorker(
    preferredWorker: WorkerSlot,
    options: ResolveExecutionOptions = {},
  ): { ok: true; selectedWorker: WorkerSlot; degraded: boolean; routingReason: string } | { ok: false; error: string } {
    const availability = this.getWorkerAvailabilitySnapshot();
    const busyWorkers = options.busyWorkers || new Set<WorkerSlot>();
    const excludedWorkers = options.excludedWorkers || new Set<WorkerSlot>();

    const isPreferredBusy = busyWorkers.has(preferredWorker);
    const isPreferredExcluded = excludedWorkers.has(preferredWorker);
    const isPreferredUnavailable = !availability.availableWorkers.has(preferredWorker);

    if (!isPreferredBusy && !isPreferredExcluded && !isPreferredUnavailable) {
      return {
        ok: true,
        selectedWorker: preferredWorker,
        degraded: false,
        routingReason: `执行前校验通过，继续由 ${preferredWorker} 执行`,
      };
    }

    const preferredUnavailableReason = availability.unavailableReasons.get(preferredWorker) || '当前不可用';
    const preferredReason = isPreferredUnavailable
      ? preferredUnavailableReason
      : isPreferredBusy
        ? '当前 worker lane 忙碌'
        : '被调度器排除';

    if (isPreferredBusy && options.allowBusyFallback === false) {
      return {
        ok: false,
        error: `任务目标 Worker ${preferredWorker} 忙碌（${preferredReason}），且当前策略不允许忙碌时降级`,
      };
    }

    const fallbackWorker = this.fallbackPriority[preferredWorker].find(worker => {
      if (busyWorkers.has(worker)) return false;
      if (excludedWorkers.has(worker)) return false;
      return availability.availableWorkers.has(worker);
    });
    if (!fallbackWorker) {
      return {
        ok: false,
        error: `任务目标 Worker ${preferredWorker} 不可执行（${preferredReason}），且无可用降级 Worker`,
      };
    }

    return {
      ok: true,
      selectedWorker: fallbackWorker,
      degraded: true,
      routingReason: `目标 Worker ${preferredWorker} 当前不可执行（${preferredReason}），执行时降级到 ${fallbackWorker}`,
    };
  }

  private getRuntimeUnavailableReason(worker: WorkerSlot): string | null {
    const status = this.runtimeUnavailableWorkers.get(worker);
    if (!status) {
      return null;
    }
    const now = Date.now();
    if (now >= status.until) {
      this.runtimeUnavailableWorkers.delete(worker);
      return null;
    }
    const remainSeconds = Math.ceil((status.until - now) / 1000);
    return `${status.reason}（冷却 ${remainSeconds}s）`;
  }

  private getWorkerAvailabilitySnapshot(): WorkerAvailabilitySnapshot {
    const availableWorkers = new Set<WorkerSlot>();
    const unavailableReasons = new Map<WorkerSlot, string>();
    const enabledProfiles = this.profileLoader.getEnabledProfiles();
    const fullConfig = LLMConfigLoader.loadFullConfig();

    for (const worker of this.workerSlots) {
      const workerConfig = fullConfig.workers[worker];
      if (!enabledProfiles.has(worker)) {
        unavailableReasons.set(worker, '未启用');
        continue;
      }
      if (!workerConfig) {
        unavailableReasons.set(worker, '缺少模型配置');
        continue;
      }
      if (!workerConfig.apiKey?.trim()) {
        unavailableReasons.set(worker, 'API Key 未配置');
        continue;
      }
      if (!workerConfig.baseUrl?.trim()) {
        unavailableReasons.set(worker, 'Base URL 未配置');
        continue;
      }
      if (!workerConfig.model?.trim()) {
        unavailableReasons.set(worker, '模型未配置');
        continue;
      }
      if (workerConfig.provider !== 'openai' && workerConfig.provider !== 'anthropic') {
        unavailableReasons.set(worker, `Provider 无效: ${workerConfig.provider}`);
        continue;
      }
      const runtimeReason = this.getRuntimeUnavailableReason(worker);
      if (runtimeReason) {
        unavailableReasons.set(worker, runtimeReason);
        continue;
      }
      availableWorkers.add(worker);
    }

    return { availableWorkers, unavailableReasons };
  }

  private pickFallbackWorker(
    preferredWorker: WorkerSlot,
    availableWorkers: Set<WorkerSlot>,
  ): WorkerSlot | undefined {
    return this.fallbackPriority[preferredWorker]
      .find(worker => availableWorkers.has(worker));
  }
}
