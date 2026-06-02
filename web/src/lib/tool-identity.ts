export interface ParsedToolIdentity {
  source: 'builtin' | 'mcp' | 'skill';
  baseName: string;
  qualifier?: string;
  displayName: string;
}

export function parseToolIdentity(toolName: string): ParsedToolIdentity {
  if (typeof toolName !== 'string') {
    return {
      source: 'builtin',
      baseName: '',
      displayName: '',
    };
  }

  const parts = toolName.split('__');
  if (parts.length >= 3 && parts[0] === 'mcp') {
    const qualifier = parts[1] || 'mcp';
    const baseName = parts.slice(2).join('__') || toolName;
    return {
      source: 'mcp',
      baseName,
      qualifier,
      displayName: `${baseName} · ${qualifier}`,
    };
  }
  if (parts.length >= 3 && parts[0] === 'skill') {
    const qualifier = parts[1] || 'skill';
    const baseName = parts[2] || toolName;
    return {
      source: 'skill',
      baseName,
      qualifier,
      displayName: `${baseName} · ${qualifier}`,
    };
  }

  return {
    source: 'builtin',
    baseName: toolName,
    displayName: toolName,
  };
}
