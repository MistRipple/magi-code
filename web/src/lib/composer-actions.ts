import type { SessionCommandDisabledReason } from './composer-policy';

export interface ComposerSkillOption {
  skillId: string;
  name: string;
  description: string;
}

/** 会话命令：由 daemon 执行的独立轮次，不调用主模型回复。 */
export type ComposerSessionCommand = 'compact';

export interface ComposerActionLabels {
  goal: {
    name: string;
    description: string;
  };
  compact: {
    name: string;
    description: string;
  };
  context: {
    name: string;
    description: string;
  };
}

export type ComposerAction =
  | {
      kind: 'resource';
      id: 'file-or-directory';
      name: string;
      description: string;
    }
  | {
      kind: 'goal';
      id: 'goal';
      name: string;
      description: string;
      aliases: string[];
    }
  | {
      kind: 'command';
      id: ComposerSessionCommand;
      name: string;
      description: string;
      aliases: string[];
      /** 当前不可用的原因；菜单仍展示该项并说明原因，而不是让它凭空消失。 */
      disabledReason?: SessionCommandDisabledReason;
    }
  | {
      kind: 'skill';
      id: string;
      name: string;
      description: string;
      skill: ComposerSkillOption;
    };

function fuzzyMatch(text: string, query: string): boolean {
  if (!query) return true;
  let queryIndex = 0;
  for (let index = 0; index < text.length && queryIndex < query.length; index += 1) {
    if (text[index] === query[queryIndex]) queryIndex += 1;
  }
  return queryIndex === query.length;
}

export function buildComposerActions(
  skills: ComposerSkillOption[],
  labels: ComposerActionLabels,
  options: { sessionCommandDisabledReason?: SessionCommandDisabledReason | null } = {},
): ComposerAction[] {
  return [
    {
      kind: 'resource',
      id: 'file-or-directory',
      name: labels.context.name,
      description: labels.context.description,
    },
    {
      kind: 'goal',
      id: 'goal',
      name: labels.goal.name,
      description: labels.goal.description,
      aliases: ['goal', 'goal mode', 'goalmode', '目标', '目标模式', '长期目标'],
    },
    {
      kind: 'command' as const,
      id: 'compact' as const,
      name: labels.compact.name,
      description: labels.compact.description,
      aliases: ['compact', 'compress', 'summarize', '压缩', '压缩上下文', '上下文', '总结'],
      ...(options.sessionCommandDisabledReason
        ? { disabledReason: options.sessionCommandDisabledReason }
        : {}),
    },
    ...skills.map<ComposerAction>((skill) => ({
      kind: 'skill',
      id: skill.skillId,
      name: skill.name,
      description: skill.description,
      skill,
    })),
  ];
}

export function filterSlashCommands(
  actions: ComposerAction[],
  rawQuery: string,
): Array<Exclude<ComposerAction, { kind: 'resource' }>> {
  const query = rawQuery.trim().toLowerCase();
  return actions
    .filter((action): action is Exclude<ComposerAction, { kind: 'resource' }> => (
      action.kind !== 'resource'
    ))
    .filter((action) => {
      if (!query) return true;
      const searchParts = action.kind === 'goal' || action.kind === 'command'
        ? [action.id, action.name, action.description, ...action.aliases]
        : [action.id, action.name, action.description];
      return searchParts.some((part) => {
        const normalized = part.toLowerCase();
        return normalized.includes(query) || fuzzyMatch(normalized, query);
      });
    });
}

export function resolveSlashTrigger(
  value: string,
  rawCursor: number,
): { triggerStart: number; filter: string } | null {
  const cursor = Math.max(0, Math.min(value.length, rawCursor));
  if (cursor === 0) return null;
  let index = cursor - 1;
  while (index >= 0) {
    const character = value[index];
    if (character === '/') {
      const previous = index > 0 ? value[index - 1] : '';
      const isTokenStart = index === 0 || previous === '\n' || previous === ' ' || previous === '\t';
      return isTokenStart
        ? { triggerStart: index, filter: value.slice(index + 1, cursor) }
        : null;
    }
    if (character === ' ' || character === '\n' || character === '\t') return null;
    index -= 1;
  }
  return null;
}
