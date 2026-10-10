export interface UserMessageCommandMetadata {
  [key: string]: unknown;
  sessionCommand?: unknown;
  pluginCommandId?: unknown;
  goalMode?: unknown;
  skillName?: unknown;
}

/** 命令展示只读取本条消息的结构化身份，不依赖当前技能设置。 */
export function resolveUserMessageCommandLabel(
  metadata: UserMessageCommandMetadata | undefined,
): string {
  if (metadata?.sessionCommand === 'compact') return '/compact';

  const pluginCommandId = typeof metadata?.pluginCommandId === 'string'
    ? metadata.pluginCommandId.trim()
    : '';
  if (pluginCommandId) return `/${pluginCommandId}`;

  const skillId = typeof metadata?.skillName === 'string'
    ? metadata.skillName.trim()
    : '';
  return [metadata?.goalMode === true ? '/goal' : '', skillId ? '/' + skillId : '']
    .filter(Boolean).join(' ');
}
