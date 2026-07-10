export interface SlashSkillOption {
  skillId: string;
  name: string;
  description: string;
}

export interface SlashGoalLabels {
  name: string;
  description: string;
}

export type SlashCommand =
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
      skill: SlashSkillOption;
    };

function fuzzyMatch(text: string, query: string): boolean {
  if (!query) return true;
  let queryIndex = 0;
  for (let index = 0; index < text.length && queryIndex < query.length; index += 1) {
    if (text[index] === query[queryIndex]) queryIndex += 1;
  }
  return queryIndex === query.length;
}

export function buildSlashCommands(
  skills: SlashSkillOption[],
  goalLabels: SlashGoalLabels,
): SlashCommand[] {
  return [
    {
      kind: 'goal',
      id: 'goal',
      name: goalLabels.name,
      description: goalLabels.description,
      aliases: ['goal', 'goal mode', 'goalmode', '目标', '目标模式', '长期目标'],
    },
    ...skills.map<SlashCommand>((skill) => ({
      kind: 'skill',
      id: skill.skillId,
      name: skill.name,
      description: skill.description,
      skill,
    })),
  ];
}

export function filterSlashCommands(commands: SlashCommand[], rawQuery: string): SlashCommand[] {
  const query = rawQuery.trim().toLowerCase();
  if (!query) return commands;
  return commands.filter((command) => {
    const searchParts = command.kind === 'goal'
      ? [command.name, command.description, ...command.aliases]
      : [command.id, command.name, command.description];
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
