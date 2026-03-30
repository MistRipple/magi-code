import { logger, LogCategory } from '../logging';

export const DEFAULT_MAX_UNIFIED_DIFF_LINES = 1200;

export interface UnifiedFileDiffResult {
  additions: number;
  deletions: number;
  diff: string;
}

export function buildUnifiedFileDiff(
  originalContent: string,
  newContent: string,
  displayPath: string,
  maxDiffLines: number = DEFAULT_MAX_UNIFIED_DIFF_LINES,
): UnifiedFileDiffResult {
  if (originalContent === newContent) {
    return { additions: 0, deletions: 0, diff: '' };
  }

  const diffLib = require('diff') as {
    structuredPatch: (
      oldFileName: string,
      newFileName: string,
      oldStr: string,
      newStr: string,
      oldHeader?: string,
      newHeader?: string,
      options?: { context?: number }
    ) => {
      hunks?: Array<{
        oldStart: number;
        oldLines: number;
        newStart: number;
        newLines: number;
        lines: string[];
      }>;
    };
  };

  const patch = diffLib.structuredPatch(
    displayPath,
    displayPath,
    originalContent,
    newContent,
    '',
    '',
    { context: 3 },
  );
  const hunks = Array.isArray(patch.hunks) ? patch.hunks : [];
  if (hunks.length === 0) {
    return { additions: 0, deletions: 0, diff: '' };
  }

  let additions = 0;
  let deletions = 0;
  const diffLines: string[] = [`--- ${displayPath}`, `+++ ${displayPath}`];

  for (const hunk of hunks) {
    diffLines.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`);
    for (const line of hunk.lines) {
      diffLines.push(line);
      if (line.startsWith('+')) additions += 1;
      if (line.startsWith('-')) deletions += 1;
    }
  }

  if (diffLines.length <= maxDiffLines) {
    return { additions, deletions, diff: diffLines.join('\n') };
  }

  logger.warn('统一 diff 已截断', {
    filePath: displayPath,
    totalDiffLines: diffLines.length,
    maxDiffLines,
    additions,
    deletions,
  }, LogCategory.TOOLS);

  return {
    additions,
    deletions,
    diff: [
      ...diffLines.slice(0, maxDiffLines),
      `... (diff truncated: ${diffLines.length - maxDiffLines} more lines)`,
    ].join('\n'),
  };
}
