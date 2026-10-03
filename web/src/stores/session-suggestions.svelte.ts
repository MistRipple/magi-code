import { i18n } from './i18n.svelte';
import type {
  SessionSuggestionDto,
  SessionSuggestionGroupDto,
} from '../shared/rust-backend-types';

export type SessionSuggestion = SessionSuggestionDto;
export type SessionSuggestionGroup = SessionSuggestionGroupDto;

export interface SessionSuggestionScope {
  key: string;
  locale: string;
}

interface CachedSuggestionEntry {
  active: SessionSuggestionGroup | null;
  initialized: boolean;
}

type BuiltInSuggestionSeed = {
  category: SessionSuggestion['category'];
  labelKey: string;
  promptKey: string;
};

/**
 * 新会话的固定建议池。词条放在 i18n 字典中，运行时只做本地抽样，
 * 因而不会因为网络、daemon 或 auxiliary 模型状态而延迟空状态展示。
 */
const BUILT_IN_SUGGESTIONS: readonly BuiltInSuggestionSeed[] = [
  {
    category: 'understand',
    labelKey: 'messageList.suggestions.items.projectOverview.label',
    promptKey: 'messageList.suggestions.items.projectOverview.prompt',
  },
  {
    category: 'understand',
    labelKey: 'messageList.suggestions.items.architecture.label',
    promptKey: 'messageList.suggestions.items.architecture.prompt',
  },
  {
    category: 'understand',
    labelKey: 'messageList.suggestions.items.startingPoint.label',
    promptKey: 'messageList.suggestions.items.startingPoint.prompt',
  },
  {
    category: 'understand',
    labelKey: 'messageList.suggestions.items.executionFlow.label',
    promptKey: 'messageList.suggestions.items.executionFlow.prompt',
  },
  {
    category: 'understand',
    labelKey: 'messageList.suggestions.items.dependencyMap.label',
    promptKey: 'messageList.suggestions.items.dependencyMap.prompt',
  },
  {
    category: 'inspect',
    labelKey: 'messageList.suggestions.items.inspectChanges.label',
    promptKey: 'messageList.suggestions.items.inspectChanges.prompt',
  },
  {
    category: 'inspect',
    labelKey: 'messageList.suggestions.items.findRisks.label',
    promptKey: 'messageList.suggestions.items.findRisks.prompt',
  },
  {
    category: 'inspect',
    labelKey: 'messageList.suggestions.items.readRelevantCode.label',
    promptKey: 'messageList.suggestions.items.readRelevantCode.prompt',
  },
  {
    category: 'inspect',
    labelKey: 'messageList.suggestions.items.checkConventions.label',
    promptKey: 'messageList.suggestions.items.checkConventions.prompt',
  },
  {
    category: 'inspect',
    labelKey: 'messageList.suggestions.items.reviewChanges.label',
    promptKey: 'messageList.suggestions.items.reviewChanges.prompt',
  },
  {
    category: 'plan',
    labelKey: 'messageList.suggestions.items.makePlan.label',
    promptKey: 'messageList.suggestions.items.makePlan.prompt',
  },
  {
    category: 'plan',
    labelKey: 'messageList.suggestions.items.breakDown.label',
    promptKey: 'messageList.suggestions.items.breakDown.prompt',
  },
  {
    category: 'plan',
    labelKey: 'messageList.suggestions.items.compareSolutions.label',
    promptKey: 'messageList.suggestions.items.compareSolutions.prompt',
  },
  {
    category: 'plan',
    labelKey: 'messageList.suggestions.items.acceptanceCriteria.label',
    promptKey: 'messageList.suggestions.items.acceptanceCriteria.prompt',
  },
  {
    category: 'plan',
    labelKey: 'messageList.suggestions.items.edgeCases.label',
    promptKey: 'messageList.suggestions.items.edgeCases.prompt',
  },
  {
    category: 'execute',
    labelKey: 'messageList.suggestions.items.implementImprovement.label',
    promptKey: 'messageList.suggestions.items.implementImprovement.prompt',
  },
  {
    category: 'execute',
    labelKey: 'messageList.suggestions.items.fixIssue.label',
    promptKey: 'messageList.suggestions.items.fixIssue.prompt',
  },
  {
    category: 'execute',
    labelKey: 'messageList.suggestions.items.addTests.label',
    promptKey: 'messageList.suggestions.items.addTests.prompt',
  },
  {
    category: 'execute',
    labelKey: 'messageList.suggestions.items.improveDocumentation.label',
    promptKey: 'messageList.suggestions.items.improveDocumentation.prompt',
  },
  {
    category: 'execute',
    labelKey: 'messageList.suggestions.items.verifyImplementation.label',
    promptKey: 'messageList.suggestions.items.verifyImplementation.prompt',
  },
  {
    category: 'record',
    labelKey: 'messageList.suggestions.items.workLog.label',
    promptKey: 'messageList.suggestions.items.workLog.prompt',
  },
  {
    category: 'record',
    labelKey: 'messageList.suggestions.items.changeSummary.label',
    promptKey: 'messageList.suggestions.items.changeSummary.prompt',
  },
  {
    category: 'record',
    labelKey: 'messageList.suggestions.items.decisionLog.label',
    promptKey: 'messageList.suggestions.items.decisionLog.prompt',
  },
  {
    category: 'record',
    labelKey: 'messageList.suggestions.items.releaseNotes.label',
    promptKey: 'messageList.suggestions.items.releaseNotes.prompt',
  },
  {
    category: 'record',
    labelKey: 'messageList.suggestions.items.nextSteps.label',
    promptKey: 'messageList.suggestions.items.nextSteps.prompt',
  },
  {
    category: 'learn',
    labelKey: 'messageList.suggestions.items.explainConcept.label',
    promptKey: 'messageList.suggestions.items.explainConcept.prompt',
  },
  {
    category: 'learn',
    labelKey: 'messageList.suggestions.items.extractRules.label',
    promptKey: 'messageList.suggestions.items.extractRules.prompt',
  },
  {
    category: 'learn',
    labelKey: 'messageList.suggestions.items.explainTradeoffs.label',
    promptKey: 'messageList.suggestions.items.explainTradeoffs.prompt',
  },
  {
    category: 'learn',
    labelKey: 'messageList.suggestions.items.explainApi.label',
    promptKey: 'messageList.suggestions.items.explainApi.prompt',
  },
  {
    category: 'learn',
    labelKey: 'messageList.suggestions.items.debugApproach.label',
    promptKey: 'messageList.suggestions.items.debugApproach.prompt',
  },
];

export const BUILT_IN_SUGGESTION_COUNT = BUILT_IN_SUGGESTIONS.length;

const SUGGESTIONS_PER_GROUP = 3;
const SUGGESTION_CATEGORIES: readonly SessionSuggestion['category'][] = [
  'understand',
  'inspect',
  'plan',
  'execute',
  'record',
  'learn',
];

function localizeSuggestion(seed: BuiltInSuggestionSeed): SessionSuggestion {
  return {
    category: seed.category,
    label: i18n.t(seed.labelKey),
    prompt: i18n.t(seed.promptKey),
  };
}

function shuffle<T>(items: T[]): T[] {
  for (let index = items.length - 1; index > 0; index -= 1) {
    const swapIndex = Math.floor(Math.random() * (index + 1));
    [items[index], items[swapIndex]] = [items[swapIndex], items[index]];
  }
  return items;
}

function pickSuggestions(excludedPrompts: readonly string[] = []): SessionSuggestion[] {
  const localized = BUILT_IN_SUGGESTIONS.map(localizeSuggestion);
  const excluded = new Set(excludedPrompts.map((prompt) => prompt.trim().toLowerCase()));
  const candidates = localized.filter((suggestion) => (
    !excluded.has(suggestion.prompt.trim().toLowerCase())
  ));
  const selected: SessionSuggestion[] = [];
  const selectedCategories = shuffle([...SUGGESTION_CATEGORIES]).slice(0, SUGGESTIONS_PER_GROUP);

  for (const category of selectedCategories) {
    const categoryCandidates = candidates.filter((suggestion) => suggestion.category === category);
    if (categoryCandidates.length > 0) {
      selected.push(categoryCandidates[Math.floor(Math.random() * categoryCandidates.length)]);
    }
  }

  if (selected.length < SUGGESTIONS_PER_GROUP) {
    const selectedPrompts = new Set(selected.map((suggestion) => suggestion.prompt));
    for (const suggestion of shuffle([...candidates])) {
      if (selectedPrompts.has(suggestion.prompt)) continue;
      selected.push(suggestion);
      selectedPrompts.add(suggestion.prompt);
      if (selected.length === SUGGESTIONS_PER_GROUP) break;
    }
  }

  return shuffle(selected).slice(0, SUGGESTIONS_PER_GROUP);
}

class SessionSuggestionsStore {
  activeGroup = $state<SessionSuggestionGroup | null>(null);
  scopeKey = $state('');

  readonly suggestionsPerGroup = SUGGESTIONS_PER_GROUP;

  private readonly entries = new Map<string, CachedSuggestionEntry>();

  ensure(scope: SessionSuggestionScope): void {
    let entry = this.entries.get(scope.key);
    if (!entry) {
      entry = {
        active: null,
        initialized: false,
      };
      this.entries.set(scope.key, entry);
    }
    if (!entry.initialized) {
      entry.initialized = true;
      entry.active = { suggestions: pickSuggestions() };
    }
    this.sync(scope.key, entry);
  }

  /** 换一组：从固定建议池中同步抽取 3 条，排除当前展示的词条。 */
  rotate(scope: SessionSuggestionScope): void {
    let entry = this.entries.get(scope.key);
    if (!entry) {
      entry = { active: null, initialized: false };
      this.entries.set(scope.key, entry);
    }
    const excludedPrompts = entry.active?.suggestions.map((suggestion) => suggestion.prompt) || [];
    entry.active = { suggestions: pickSuggestions(excludedPrompts) };
    entry.initialized = true;
    this.sync(scope.key, entry);
  }

  private sync(scopeKey: string, entry: CachedSuggestionEntry): void {
    this.scopeKey = scopeKey;
    this.activeGroup = entry.active;
  }
}

export const sessionSuggestions = new SessionSuggestionsStore();
