import { generateSessionSuggestions } from '../web/agent-api';
import type { AgentBindingOverride } from '../web/agent-binding-context';
import type {
  SessionSuggestionDto,
  SessionSuggestionGroupDto,
} from '../shared/rust-backend-types';

export type SessionSuggestion = SessionSuggestionDto;
export type SessionSuggestionGroup = SessionSuggestionGroupDto;

export interface SessionSuggestionScope {
  key: string;
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
  locale: string;
}

interface CachedSuggestionEntry {
  active: SessionSuggestionGroup | null;
  standby: SessionSuggestionGroup | null;
  activeInteracted: boolean;
  initialized: boolean;
  generating: boolean;
  controller: AbortController | null;
}

/** 一次生成请求的上限；辅助模型偶发慢响应不应让空状态长期停在骨架屏。 */
const GENERATE_TIMEOUT_MS = 20_000;
const SUGGESTIONS_PER_GROUP = 3;

/**
 * 合规判定由 daemon 独占：后端已做 deny_unknown_fields、分类枚举、长度、
 * Markdown 与重复校验。这里只做协议解包，确认形状可渲染即可，避免前后端
 * 出现两套阈值导致合规建议被静默丢弃。
 */
function unwrapGroups(value: unknown): SessionSuggestionGroup[] {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
  const groups = (value as Record<string, unknown>).groups;
  if (!Array.isArray(groups)) return [];
  const unwrapped: SessionSuggestionGroup[] = [];
  for (const group of groups) {
    if (!group || typeof group !== 'object' || Array.isArray(group)) continue;
    const suggestions = (group as Record<string, unknown>).suggestions;
    if (!Array.isArray(suggestions) || suggestions.length === 0) continue;
    unwrapped.push({ suggestions: suggestions as SessionSuggestion[] });
  }
  return unwrapped;
}

class SessionSuggestionsStore {
  activeGroup = $state<SessionSuggestionGroup | null>(null);
  standbyGroup = $state<SessionSuggestionGroup | null>(null);
  scopeKey = $state('');
  generating = $state(false);
  /** 首次生成尚未落地时为 true，用于渲染骨架而不是空白区域。 */
  loadingInitial = $state(false);
  unavailable = $state(false);

  readonly suggestionsPerGroup = SUGGESTIONS_PER_GROUP;

  private readonly entries = new Map<string, CachedSuggestionEntry>();

  ensure(scope: SessionSuggestionScope): void {
    let entry = this.entries.get(scope.key);
    if (!entry) {
      entry = {
        active: null,
        standby: null,
        activeInteracted: false,
        initialized: false,
        generating: false,
        controller: null,
      };
      this.entries.set(scope.key, entry);
    }
    this.sync(scope.key, entry, true);
    if (!entry.initialized) {
      entry.initialized = true;
      void this.generate(scope, entry, true);
    }
  }

  markActiveSelected(): void {
    const entry = this.entries.get(this.scopeKey);
    if (entry) entry.activeInteracted = true;
  }

  /** 换一组：备用组已就绪时立即切换，同时后台补下一组；否则直接重新生成。 */
  rotate(scope: SessionSuggestionScope): void {
    const entry = this.entries.get(scope.key);
    if (!entry || entry.generating) return;
    if (entry.standby) {
      entry.active = entry.standby;
      entry.standby = null;
      entry.activeInteracted = true;
      this.sync(scope.key, entry);
    }
    void this.generate(scope, entry, false);
  }

  private sync(scopeKey: string, entry: CachedSuggestionEntry, force = false): void {
    if (!force && this.scopeKey !== scopeKey) return;
    this.scopeKey = scopeKey;
    this.activeGroup = entry.active;
    this.standbyGroup = entry.standby;
    this.generating = entry.generating;
    this.loadingInitial = entry.generating && !entry.active;
    this.unavailable = !entry.generating && !entry.active;
  }

  private async generate(
    scope: SessionSuggestionScope,
    entry: CachedSuggestionEntry,
    initial: boolean,
  ): Promise<void> {
    if (entry.generating) return;
    const controller = new AbortController();
    entry.controller = controller;
    entry.generating = true;
    this.sync(scope.key, entry);
    const timeoutId = setTimeout(() => controller.abort(), GENERATE_TIMEOUT_MS);
    try {
      const response = await generateSessionSuggestions(
        {
          locale: scope.locale,
          count: SUGGESTIONS_PER_GROUP,
          requestedGroups: initial ? 2 : 1,
          excludePrompts: (entry.active?.suggestions || []).map((suggestion) => suggestion.prompt),
        },
        controller.signal,
        scope.workspaceId || scope.workspacePath
          ? {
              scope: 'workspace',
              workspaceId: scope.workspaceId,
              workspacePath: scope.workspacePath,
              sessionId: scope.sessionId,
            }
          : ({ scope: 'personal', sessionId: scope.sessionId } satisfies AgentBindingOverride),
      );
      if (controller.signal.aborted || this.entries.get(scope.key) !== entry) return;
      const groups = unwrapGroups(response);
      if (initial) {
        if (!entry.activeInteracted && groups[0]) entry.active = groups[0];
        entry.standby = entry.activeInteracted ? (groups[0] || groups[1] || null) : (groups[1] || null);
      } else if (groups[0]) {
        if (entry.active) {
          entry.standby = groups[0];
        } else {
          entry.active = groups[0];
        }
      }
    } catch (error) {
      // 建议是空状态的增强项，失败不阻断主流程；但必须留下可诊断信号，
      // 否则辅助模型未配置、超时和返回不合规在界面上无法区分。
      if (!controller.signal.aborted) {
        console.warn('[session-suggestions] 会话起步建议生成失败', error);
      }
    } finally {
      clearTimeout(timeoutId);
      if (this.entries.get(scope.key) === entry) {
        entry.generating = false;
        entry.controller = null;
        this.sync(scope.key, entry);
      }
    }
  }
}

export const sessionSuggestions = new SessionSuggestionsStore();
