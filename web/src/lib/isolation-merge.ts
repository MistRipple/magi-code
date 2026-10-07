import type {
  IsolationConflictKindDto,
  IsolationConflictResolutionDto,
  IsolationMergeEntryDto,
  SessionIsolationOriginDto,
} from '../shared/rust-backend-types';

export interface IsolationMergeSummary {
  clean: number;
  alreadyApplied: number;
  conflict: number;
}

export function summarizeMergePlan(entries: IsolationMergeEntryDto[]): IsolationMergeSummary {
  const summary: IsolationMergeSummary = { clean: 0, alreadyApplied: 0, conflict: 0 };
  for (const entry of entries) {
    if (entry.state === 'clean') summary.clean += 1;
    else if (entry.state === 'already_applied') summary.alreadyApplied += 1;
    else summary.conflict += 1;
  }
  return summary;
}

export interface IsolationMergeChoice {
  /** 没有冲突的文件默认全部勾选；用户可以取消。 */
  excluded: ReadonlySet<string>;
  /** 冲突文件的处理方式；没有选择的冲突不会被合并。 */
  resolutions: Readonly<Record<string, IsolationConflictResolutionDto>>;
}

/**
 * 把用户在合并面板里的选择整理成后端请求：只合并勾选的干净文件，
 * 以及明确选择了「用副本的版本」的冲突文件。选择「保留主工作区」的冲突不发给后端处理，
 * 它们继续留在副本里。
 */
export function buildMergeRequest(
  entries: IsolationMergeEntryDto[],
  choice: IsolationMergeChoice,
): { paths: string[]; resolutions: Record<string, IsolationConflictResolutionDto> } {
  const paths: string[] = [];
  const resolutions: Record<string, IsolationConflictResolutionDto> = {};
  for (const entry of entries) {
    if (entry.state === 'conflict') {
      if (choice.resolutions[entry.path] === 'use_session') {
        paths.push(entry.path);
        resolutions[entry.path] = 'use_session';
      }
      continue;
    }
    if (!choice.excluded.has(entry.path)) {
      paths.push(entry.path);
    }
  }
  return { paths, resolutions };
}

const CONFLICT_LABEL_KEYS: Record<IsolationConflictKindDto, string> = {
  both_added: 'isolation.merge.conflict.bothAdded',
  both_modified: 'isolation.merge.conflict.bothModified',
  deleted_in_source: 'isolation.merge.conflict.deletedInSource',
  modified_in_source: 'isolation.merge.conflict.modifiedInSource',
  unsupported: 'isolation.merge.conflict.unsupported',
};

export function conflictLabelKey(kind: IsolationConflictKindDto | undefined): string {
  return CONFLICT_LABEL_KEYS[kind ?? 'both_modified'] ?? CONFLICT_LABEL_KEYS.both_modified;
}

/** 这个会话为什么运行在隔离副本里；自动隔离时给出占用工作区的会话。 */
export function isolationOriginBlockingSession(origin: SessionIsolationOriginDto): string | null {
  return origin.kind === 'contention' ? origin.blocking_session_id : null;
}
