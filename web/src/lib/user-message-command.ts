export interface UserMessageCommandMetadata {
  [key: string]: unknown;
  sessionCommand?: unknown;
  pluginCommandId?: unknown;
  goalMode?: unknown;
  skillName?: unknown;
  skillTitle?: unknown;
}

/** 命令展示读取本条消息的身份和接纳时名称，不依赖当前技能设置。 */
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
  const skillTitle = typeof metadata?.skillTitle === 'string' ? metadata.skillTitle.trim() : '';
  return [metadata?.goalMode === true ? '/goal' : '', skillId ? '/' + (skillTitle || skillId) : '']
    .filter(Boolean).join(' ');
}
