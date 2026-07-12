export interface ComposerSkillOption {
  skillId: string;
  name: string;
  description: string;
}

export interface ComposerActionLabels {
  goal: {
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
      const searchParts = action.kind === 'goal'
        ? [action.name, action.description, ...action.aliases]
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
