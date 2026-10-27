interface WorkspaceDisplay {
  workspaceId: string;
  name: string;
  rootPath: string;
}

/** 同名工作区使用最短可区分的路径后缀；只生成展示文字，不改变工作区身份。 */
export function workspacePathHints(workspaces: readonly WorkspaceDisplay[]): Record<string, string> {
  const groups = new Map<string, WorkspaceDisplay[]>();
  for (const workspace of workspaces) {
    const group = groups.get(workspace.name) ?? [];
    group.push(workspace);
    groups.set(workspace.name, group);
  }
  const hints: Record<string, string> = {};
  for (const group of groups.values()) {
    if (group.length < 2) continue;
    const paths = group.map(workspace => workspace.rootPath.replace(/\\/g, '/').replace(/\/+$/, '').split('/'));
    for (const [index, workspace] of group.entries()) {
      const parts = paths[index];
      let depth = 1;
      while (depth < parts.length && paths.some((other, otherIndex) => (
        otherIndex !== index && other.slice(-depth).join('/') === parts.slice(-depth).join('/')
      ))) depth++;
      const suffix = parts.slice(-depth);
      // 名称已展示末级目录时，副标题只保留有辨识作用的上级路径。
      if (suffix.length > 1 && suffix.at(-1) === workspace.name) suffix.pop();
      hints[workspace.workspaceId] = suffix.join('/') || workspace.rootPath;
    }
  }
  return hints;
}
