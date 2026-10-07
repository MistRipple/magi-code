/** Skill 说明文件的展示辅助：把 YAML front matter 与正文分开，并格式化文件大小。 */

export interface SkillInstructionParts {
  /** front matter 里的 `key: value` 行（只做展示，不当作结构化事实）。 */
  meta: Array<{ key: string; value: string }>;
  /** 去掉 front matter 之后的正文。 */
  body: string;
}

export function splitSkillInstruction(text: string): SkillInstructionParts {
  const normalized = text.replace(/\r\n/g, '\n');
  if (!normalized.startsWith('---\n')) {
    return { meta: [], body: normalized };
  }
  const end = normalized.indexOf('\n---', 4);
  if (end < 0) {
    return { meta: [], body: normalized };
  }
  const meta = normalized
    .slice(4, end)
    .split('\n')
    .flatMap((line) => {
      const match = /^([A-Za-z0-9_-]+):\s*(.*)$/.exec(line.trim());
      if (!match) return [];
      return [{ key: match[1], value: match[2].replace(/^["']|["']$/g, '') }];
    });
  const rest = normalized.slice(end + 4);
  return { meta, body: rest.replace(/^\n+/, '') };
}

export function formatSkillFileSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
