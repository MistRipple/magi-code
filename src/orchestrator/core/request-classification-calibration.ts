import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import type { InteractionMode } from '../../types';
import type { ModelAutonomyCapability } from '../../types/agent-types';
import { logger, LogCategory } from '../../logging';
import type { PlanMode } from '../plan-ledger';
import {
  classifyRequest,
  REQUEST_CLASSIFIER_VERSION,
  type RequestClassification,
  type RequestEntryPath,
} from './request-classifier';

export interface RequestClassificationDecisionRecord {
  kind: 'decision';
  timestamp: string;
  decision_id: string;
  session_id: string;
  turn_id: string | null;
  request_id: string | null;
  prompt: string;
  prompt_hash: string;
  requested_planning_mode: PlanMode;
  effective_planning_mode: PlanMode;
  requested_interaction_mode: InteractionMode;
  effective_interaction_mode: InteractionMode;
  model_capability: ModelAutonomyCapability;
  classifier_version: string;
  entry_path: RequestEntryPath;
  requires_modification: boolean;
  include_thinking: boolean;
  include_tool_calls: boolean;
  history_mode: 'session' | 'isolated';
  reason: string;
  decision_factors: string[];
  signals: {
    has_read_only_intent: boolean;
    has_write_intent: boolean;
    has_high_impact_intent: boolean;
    has_workspace_scoped_intent: boolean;
    has_assistant_meta_intent: boolean;
    has_conversational_intent: boolean;
    is_short_conversational_turn: boolean;
  };
}

export interface RequestClassificationFeedbackRecord {
  kind: 'feedback';
  timestamp: string;
  decision_id: string;
  expected_entry_path: RequestEntryPath;
  actual_entry_path?: RequestEntryPath;
  verdict: 'confirmed' | 'misclassified';
  note?: string;
}

export type RequestClassificationCalibrationEvent =
  | RequestClassificationDecisionRecord
  | RequestClassificationFeedbackRecord;

export interface RequestClassificationReplayItem {
  decisionId: string;
  recordedEntryPath: RequestEntryPath;
  replayedEntryPath: RequestEntryPath;
  recordedReason: string;
  replayedReason: string;
  recordedDecisionFactors: string[];
  replayedDecisionFactors: string[];
  changedFields: string[];
}

export interface RequestClassificationReplayReport {
  classifierVersion: string;
  total: number;
  changed: number;
  unchanged: number;
  items: RequestClassificationReplayItem[];
}

export interface RequestClassificationCalibrationStore {
  appendDecision(record: RequestClassificationDecisionRecord): void;
  appendFeedback(record: RequestClassificationFeedbackRecord): void;
  readAll(): RequestClassificationCalibrationEvent[];
  readDecisions(): RequestClassificationDecisionRecord[];
  readFeedback(): RequestClassificationFeedbackRecord[];
  buildReplayReport(): RequestClassificationReplayReport;
  getStoragePath(): string;
}

export function buildRequestClassificationDecisionRecord(input: {
  sessionId: string;
  turnId?: string | null;
  requestId?: string | null;
  prompt: string;
  requestedPlanningMode: PlanMode;
  effectivePlanningMode: PlanMode;
  requestedInteractionMode: InteractionMode;
  effectiveInteractionMode: InteractionMode;
  modelCapability: ModelAutonomyCapability;
  classification: RequestClassification;
}): RequestClassificationDecisionRecord {
  const promptHash = createHash('sha1').update(input.prompt).digest('hex');
  const turnId = typeof input.turnId === 'string' && input.turnId.trim() ? input.turnId.trim() : null;
  const requestId = typeof input.requestId === 'string' && input.requestId.trim() ? input.requestId.trim() : null;

  return {
    kind: 'decision',
    timestamp: new Date().toISOString(),
    decision_id: `${turnId || 'turnless'}:${requestId || promptHash.slice(0, 12)}`,
    session_id: input.sessionId,
    turn_id: turnId,
    request_id: requestId,
    prompt: input.prompt,
    prompt_hash: promptHash,
    requested_planning_mode: input.requestedPlanningMode,
    effective_planning_mode: input.effectivePlanningMode,
    requested_interaction_mode: input.requestedInteractionMode,
    effective_interaction_mode: input.effectiveInteractionMode,
    model_capability: input.modelCapability,
    classifier_version: input.classification.classifierVersion,
    entry_path: input.classification.entryPolicy.entryPath,
    requires_modification: input.classification.requiresModification,
    include_thinking: input.classification.entryPolicy.includeThinking,
    include_tool_calls: input.classification.entryPolicy.includeToolCalls,
    history_mode: input.classification.entryPolicy.historyMode,
    reason: input.classification.reason,
    decision_factors: [...input.classification.decisionFactors],
    signals: {
      has_read_only_intent: input.classification.hasReadOnlyIntent,
      has_write_intent: input.classification.hasWriteIntent,
      has_high_impact_intent: input.classification.hasHighImpactIntent,
      has_workspace_scoped_intent: input.classification.hasWorkspaceScopedIntent,
      has_assistant_meta_intent: input.classification.hasAssistantMetaIntent,
      has_conversational_intent: input.classification.hasConversationalIntent,
      is_short_conversational_turn: input.classification.isShortConversationalTurn,
    },
  };
}

export function replayRequestClassificationDecisions(
  records: RequestClassificationDecisionRecord[],
): RequestClassificationReplayReport {
  const items = records.map((record) => {
    const replayed = classifyRequest(record.prompt, record.effective_planning_mode);
    const changedFields: string[] = [];

    if (record.entry_path !== replayed.entryPolicy.entryPath) {
      changedFields.push('entry_path');
    }
    if (record.requires_modification !== replayed.requiresModification) {
      changedFields.push('requires_modification');
    }
    if (record.include_thinking !== replayed.entryPolicy.includeThinking) {
      changedFields.push('include_thinking');
    }
    if (record.include_tool_calls !== replayed.entryPolicy.includeToolCalls) {
      changedFields.push('include_tool_calls');
    }
    if (record.history_mode !== replayed.entryPolicy.historyMode) {
      changedFields.push('history_mode');
    }
    if (record.reason !== replayed.reason) {
      changedFields.push('reason');
    }

    return {
      decisionId: record.decision_id,
      recordedEntryPath: record.entry_path,
      replayedEntryPath: replayed.entryPolicy.entryPath,
      recordedReason: record.reason,
      replayedReason: replayed.reason,
      recordedDecisionFactors: [...record.decision_factors],
      replayedDecisionFactors: [...replayed.decisionFactors],
      changedFields,
    };
  });

  const changed = items.filter((item) => item.changedFields.length > 0).length;
  return {
    classifierVersion: REQUEST_CLASSIFIER_VERSION,
    total: items.length,
    changed,
    unchanged: items.length - changed,
    items,
  };
}

export class FileRequestClassificationCalibrationStore implements RequestClassificationCalibrationStore {
  private readonly storagePath: string;

  constructor(workspaceRoot: string) {
    this.storagePath = path.join(workspaceRoot, '.magi', 'metrics', 'request-classification.jsonl');
  }

  appendDecision(record: RequestClassificationDecisionRecord): void {
    this.append(record);
  }

  appendFeedback(record: RequestClassificationFeedbackRecord): void {
    this.append(record);
  }

  readAll(): RequestClassificationCalibrationEvent[] {
    if (!fs.existsSync(this.storagePath)) {
      return [];
    }

    try {
      const content = fs.readFileSync(this.storagePath, 'utf8');
      return content
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line) as RequestClassificationCalibrationEvent);
    } catch (error) {
      logger.warn('编排器.请求分类.校准读取失败', {
        storagePath: this.storagePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return [];
    }
  }

  readDecisions(): RequestClassificationDecisionRecord[] {
    return this.readAll().filter((event): event is RequestClassificationDecisionRecord => event.kind === 'decision');
  }

  readFeedback(): RequestClassificationFeedbackRecord[] {
    return this.readAll().filter((event): event is RequestClassificationFeedbackRecord => event.kind === 'feedback');
  }

  buildReplayReport(): RequestClassificationReplayReport {
    return replayRequestClassificationDecisions(this.readDecisions());
  }

  getStoragePath(): string {
    return this.storagePath;
  }

  private append(record: RequestClassificationCalibrationEvent): void {
    try {
      fs.mkdirSync(path.dirname(this.storagePath), { recursive: true });
      fs.appendFileSync(this.storagePath, `${JSON.stringify(record)}\n`, 'utf8');
    } catch (error) {
      logger.warn('编排器.请求分类.校准落盘失败', {
        storagePath: this.storagePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }
}
