import fs from 'fs';
import path from 'path';
import { logger, LogCategory } from '../../logging';

export interface TerminationMetricsRecord {
  timestamp: string;
  session_id: string;
  plan_id: string | null;
  turn_id: string | null;
  mode: string;
  final_status: string;
  reason: string;
  rounds: number;
  duration_ms: number;
  token_used: number;
  evidence_ids: string[];
  progress_vector: unknown;
  review_state: unknown;
  blocker_state: unknown;
  budget_state: unknown;
  required_total: number | null;
  failed_required: number | null;
  running_or_pending_required: number | null;
  shadow: unknown;
  decision_trace: unknown;
}

export interface TerminationMetricsRepository {
  append(record: TerminationMetricsRecord): void;
  readAll(): TerminationMetricsRecord[];
  getStoragePath(): string;
}

export class FileTerminationMetricsRepository implements TerminationMetricsRepository {
  private readonly metricsPath: string;

  constructor(workspaceRoot: string) {
    this.metricsPath = path.join(workspaceRoot, '.magi', 'metrics', 'termination.jsonl');
  }

  append(record: TerminationMetricsRecord): void {
    try {
      fs.mkdirSync(path.dirname(this.metricsPath), { recursive: true });
      fs.appendFileSync(this.metricsPath, `${JSON.stringify(record)}\n`, 'utf8');
    } catch (error) {
      logger.warn('编排器.终止指标.落盘失败', {
        metricsPath: this.metricsPath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  readAll(): TerminationMetricsRecord[] {
    if (!fs.existsSync(this.metricsPath)) {
      return [];
    }
    try {
      const content = fs.readFileSync(this.metricsPath, 'utf8');
      return content
        .split('\n')
        .map(line => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line) as TerminationMetricsRecord);
    } catch (error) {
      logger.warn('编排器.终止指标.读取失败', {
        metricsPath: this.metricsPath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return [];
    }
  }

  getStoragePath(): string {
    return this.metricsPath;
  }
}
