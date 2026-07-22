<script lang="ts">
  import { highlightCode } from '../../lib/code-highlighter';
  import { i18n } from '../../stores/i18n.svelte';

  interface Props {
    diff?: string | null;
    originalContent?: string | null;
    currentContent?: string | null;
    ariaLabel?: string;
    language?: string | null;
    fill?: boolean;
  }

  type DiffLineKind = 'context' | 'addition' | 'deletion' | 'meta';

  interface DiffLine {
    id: string;
    kind: DiffLineKind;
    text: string;
    oldLineNumber: number | null;
    newLineNumber: number | null;
  }

  interface DiffHunk {
    id: string;
    oldStart: number | null;
    newStart: number | null;
    lines: DiffLine[];
  }

  interface DiffGap {
    kind: 'gap';
    id: string;
    startLine: number;
    endLine: number;
    lines: DiffLine[];
  }

  interface DiffHunkSection {
    kind: 'hunk';
    id: string;
    hunk: DiffHunk;
  }

  type DiffSection = DiffGap | DiffHunkSection;

  const EXT_LANG_MAP: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
    py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java',
    cpp: 'cpp', c: 'c', cs: 'csharp', kt: 'kotlin', swift: 'swift',
    html: 'xml', vue: 'xml', svelte: 'xml', xml: 'xml', svg: 'xml',
    css: 'css', scss: 'scss', less: 'less',
    json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini',
    md: 'markdown', sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql', graphql: 'graphql', dockerfile: 'dockerfile',
  };

  let {
    diff = '',
    originalContent = null,
    currentContent = null,
    ariaLabel = '',
    language = null,
    fill = false,
  }: Props = $props();

  const diffCode = $derived(typeof diff === 'string' ? diff.trimEnd() : '');
  const sourceLanguage = $derived.by(() => {
    const explicitLanguage = language?.trim().toLowerCase();
    if (explicitLanguage) return explicitLanguage;
    const normalizedPath = ariaLabel.split('→').pop()?.trim() ?? ariaLabel;
    const filename = normalizedPath.split(/[\\/]/u).pop()?.toLowerCase() ?? '';
    if (filename === 'dockerfile') return 'dockerfile';
    const extension = filename.split('.').pop() ?? '';
    return EXT_LANG_MAP[extension] ?? '';
  });
  const hunks = $derived(parseUnifiedDiff(diffCode));
  const sections = $derived(buildDiffSections(hunks, originalContent, currentContent));

  let highlightedLines = $state<Record<string, string>>({});
  let expandedGaps = $state<Set<string>>(new Set());
  let activeDiffFingerprint = '';

  const visibleLines = $derived.by(() => sections.flatMap((section) => {
    if (section.kind === 'hunk') return section.hunk.lines;
    return expandedGaps.has(section.id) ? section.lines : [];
  }));

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  function renderedLine(line: DiffLine): string {
    const value = highlightedLines[line.id] ?? escapeHtml(line.text);
    return value || '&nbsp;';
  }

  function displayLineNumber(line: DiffLine): number | '' {
    return line.newLineNumber ?? line.oldLineNumber ?? '';
  }

  function contentLines(content: string): string[] {
    const lines = content.split('\n');
    if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();
    return lines;
  }

  function parseUnifiedDiff(source: string): DiffHunk[] {
    if (!source) return [];

    const parsedHunks: DiffHunk[] = [];
    let currentHunk: DiffHunk | null = null;
    let oldLine: number | null = null;
    let newLine: number | null = null;
    let hunkIndex = 0;
    let lineIndex = 0;

    function pushCurrentHunk(): void {
      if (currentHunk && currentHunk.lines.length > 0) {
        parsedHunks.push(currentHunk);
      }
      currentHunk = null;
    }

    function ensureHunk(): DiffHunk {
      if (!currentHunk) {
        currentHunk = {
          id: `hunk-${hunkIndex++}`,
          oldStart: oldLine,
          newStart: newLine,
          lines: [],
        };
      }
      return currentHunk;
    }

    for (const rawLine of source.split(/\r?\n/u)) {
      const range = rawLine.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/u);
      if (rawLine.startsWith('@@')) {
        pushCurrentHunk();
        oldLine = range ? Number(range[1]) : null;
        newLine = range ? Number(range[3]) : null;
        currentHunk = {
          id: `hunk-${hunkIndex++}`,
          oldStart: oldLine,
          newStart: newLine,
          lines: [],
        };
        continue;
      }

      if (!currentHunk && (
        rawLine.startsWith('diff --git ')
        || rawLine.startsWith('index ')
        || rawLine.startsWith('--- ')
        || rawLine.startsWith('+++ ')
      )) {
        continue;
      }

      const hunk = ensureHunk();
      const id = `${hunk.id}-line-${lineIndex++}`;

      if (rawLine.startsWith('\\ No newline at end of file')) {
        hunk.lines.push({
          id,
          kind: 'meta',
          text: rawLine,
          oldLineNumber: null,
          newLineNumber: null,
        });
        continue;
      }
      if (rawLine.startsWith('+')) {
        hunk.lines.push({
          id,
          kind: 'addition',
          text: rawLine.slice(1),
          oldLineNumber: null,
          newLineNumber: newLine,
        });
        if (newLine !== null) newLine += 1;
        continue;
      }
      if (rawLine.startsWith('-')) {
        hunk.lines.push({
          id,
          kind: 'deletion',
          text: rawLine.slice(1),
          oldLineNumber: oldLine,
          newLineNumber: null,
        });
        if (oldLine !== null) oldLine += 1;
        continue;
      }

      const text = rawLine.startsWith(' ') ? rawLine.slice(1) : rawLine;
      hunk.lines.push({
        id,
        kind: 'context',
        text,
        oldLineNumber: oldLine,
        newLineNumber: newLine,
      });
      if (oldLine !== null) oldLine += 1;
      if (newLine !== null) newLine += 1;
    }

    pushCurrentHunk();
    return parsedHunks;
  }

  function buildDiffSections(
    parsedHunks: DiffHunk[],
    baselineContent: string | null,
    latestContent: string | null,
  ): DiffSection[] {
    if (parsedHunks.length === 0) return [];

    const useLatestContent = typeof latestContent === 'string';
    const fullContent = useLatestContent ? latestContent : baselineContent;
    if (typeof fullContent !== 'string') {
      return parsedHunks.map((hunk) => ({ kind: 'hunk', id: hunk.id, hunk }));
    }

    const fullLines = contentLines(fullContent);
    const result: DiffSection[] = [];
    let nextUnchangedLine = 1;

    function addGap(startLine: number, endLine: number): void {
      if (endLine < startLine || startLine < 1) return;
      const lines = fullLines.slice(startLine - 1, endLine).map((text, index): DiffLine => {
        const lineNumber = startLine + index;
        return {
          id: `gap-${startLine}-${endLine}-line-${lineNumber}`,
          kind: 'context',
          text,
          oldLineNumber: useLatestContent ? null : lineNumber,
          newLineNumber: useLatestContent ? lineNumber : null,
        };
      });
      result.push({
        kind: 'gap',
        id: `gap-${startLine}-${endLine}`,
        startLine,
        endLine,
        lines,
      });
    }

    for (const hunk of parsedHunks) {
      const visibleLineNumbers = hunk.lines
        .map((line) => useLatestContent ? line.newLineNumber : line.oldLineNumber)
        .filter((lineNumber): lineNumber is number => lineNumber !== null);
      const hunkStart = visibleLineNumbers.length > 0
        ? Math.min(...visibleLineNumbers)
        : (useLatestContent ? hunk.newStart : hunk.oldStart);
      if (hunkStart !== null && hunkStart > nextUnchangedLine) {
        addGap(nextUnchangedLine, Math.min(hunkStart - 1, fullLines.length));
      }

      result.push({ kind: 'hunk', id: hunk.id, hunk });

      if (visibleLineNumbers.length > 0) {
        nextUnchangedLine = Math.max(nextUnchangedLine, Math.max(...visibleLineNumbers) + 1);
      } else if (hunkStart !== null) {
        nextUnchangedLine = Math.max(nextUnchangedLine, hunkStart);
      }
    }

    if (nextUnchangedLine <= fullLines.length) {
      addGap(nextUnchangedLine, fullLines.length);
    }
    return result;
  }

  function toggleGap(gapId: string): void {
    const next = new Set(expandedGaps);
    if (next.has(gapId)) next.delete(gapId);
    else next.add(gapId);
    expandedGaps = next;
  }

  $effect(() => {
    const fingerprint = `${ariaLabel}\u0000${diffCode}`;
    if (fingerprint === activeDiffFingerprint) return;
    activeDiffFingerprint = fingerprint;
    expandedGaps = new Set();
  });

  $effect(() => {
    const languageToUse = sourceLanguage;
    const lines = visibleLines.filter((line) => line.kind !== 'meta');
    highlightedLines = {};
    if (!languageToUse || lines.length === 0) return;

    const sourceLength = lines.reduce((total, line) => total + line.text.length, 0);
    if (sourceLength > 100_000) return;

    let cancelled = false;
    void Promise.all(lines.map(async (line) => {
      const highlighted = await highlightCode(line.text, languageToUse);
      return [line.id, highlighted ?? escapeHtml(line.text)] as const;
    })).then((entries) => {
      if (!cancelled) highlightedLines = Object.fromEntries(entries);
    }).catch((error) => {
      console.warn('[DiffCodeBlock] 代码高亮失败:', error);
    });
    return () => {
      cancelled = true;
    };
  });
</script>

<div class="diff-code-block" class:fill aria-label={ariaLabel || undefined}>
  <div class="diff-code-scroll">
    <div class="diff-code-content">
      {#each sections as section, sectionIndex (section.id)}
        {#if section.kind === 'gap'}
          <button
            class="diff-fold-control"
            class:expanded={expandedGaps.has(section.id)}
            type="button"
            aria-expanded={expandedGaps.has(section.id)}
            onclick={() => toggleGap(section.id)}
          >
            <span class="diff-fold-gutter" aria-hidden="true">···</span>
            <span class="diff-fold-label">
              {expandedGaps.has(section.id)
                ? i18n.t('edits.diff.collapseUnchanged', { count: section.lines.length })
                : i18n.t('edits.diff.expandUnchanged', { count: section.lines.length })}
            </span>
          </button>
          {#if expandedGaps.has(section.id)}
            <div class="diff-hunk diff-hunk--expanded">
              {#each section.lines as line (line.id)}
                <div class="diff-line diff-line--{line.kind}">
                  <span class="diff-line-number" aria-hidden="true">{displayLineNumber(line)}</span>
                  <code class="diff-line-code language-{sourceLanguage}">{@html renderedLine(line)}</code>
                </div>
              {/each}
            </div>
          {/if}
        {:else}
          {#if sectionIndex > 0 && sections[sectionIndex - 1]?.kind === 'hunk'}
            <div class="diff-hunk-separator" aria-hidden="true"><span>···</span></div>
          {/if}
          <div class="diff-hunk">
            {#each section.hunk.lines as line (line.id)}
              <div class="diff-line diff-line--{line.kind}">
                <span class="diff-line-number" aria-hidden="true">{displayLineNumber(line)}</span>
                <code class="diff-line-code language-{sourceLanguage}">{@html renderedLine(line)}</code>
              </div>
            {/each}
          </div>
        {/if}
      {/each}
    </div>
  </div>
</div>

<style>
  .diff-code-block {
    min-width: 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md, 8px);
    background: var(--surface-1);
    color: var(--foreground);
    overflow: hidden;
  }

  .diff-code-block.fill {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .diff-code-scroll {
    width: 100%;
    min-width: 0;
    max-height: min(60vh, 640px);
    overflow: auto;
    background: color-mix(in srgb, var(--surface-1) 94%, black);
    color: inherit;
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.55;
    tab-size: 2;
  }

  .diff-code-block.fill .diff-code-scroll {
    flex: 1;
    min-height: 0;
    max-height: none;
  }

  .diff-code-content {
    min-width: max-content;
  }

  .diff-hunk-separator {
    display: flex;
    align-items: center;
    height: 20px;
    padding-left: 14px;
    border-top: 1px solid var(--border-subtle);
    border-bottom: 1px solid var(--border-subtle);
    background: color-mix(in srgb, var(--info) 6%, var(--surface-1));
    color: var(--foreground-muted);
    user-select: none;
  }

  .diff-fold-control {
    display: grid;
    grid-template-columns: 58px minmax(max-content, 1fr);
    width: 100%;
    min-width: max-content;
    min-height: 26px;
    padding: 0;
    border: none;
    border-top: 1px solid var(--border-subtle);
    border-bottom: 1px solid var(--border-subtle);
    background: color-mix(in srgb, var(--info) 8%, var(--surface-1));
    color: var(--foreground-muted);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .diff-fold-control:hover,
  .diff-fold-control.expanded {
    background: color-mix(in srgb, var(--info) 14%, var(--surface-1));
    color: var(--foreground);
  }

  .diff-fold-gutter {
    display: flex;
    align-items: center;
    justify-content: center;
    border-right: 1px solid color-mix(in srgb, var(--border-subtle) 72%, transparent);
    color: var(--info);
    user-select: none;
  }

  .diff-fold-label {
    padding: 3px 14px;
    white-space: nowrap;
  }

  .diff-line {
    display: grid;
    grid-template-columns: 58px minmax(max-content, 1fr);
    width: 100%;
    min-width: max-content;
    min-height: 1.55em;
  }

  .diff-line--addition {
    background: color-mix(in srgb, var(--success) 15%, transparent);
    box-shadow: inset 3px 0 0 color-mix(in srgb, var(--success) 84%, transparent);
  }

  .diff-line--deletion {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    box-shadow: inset 3px 0 0 color-mix(in srgb, var(--error) 84%, transparent);
  }

  .diff-line--meta {
    background: color-mix(in srgb, var(--warning) 8%, transparent);
    color: var(--foreground-muted);
    font-style: italic;
  }

  .diff-line-number {
    display: block;
    padding: 0 10px 0 6px;
    border-right: 1px solid color-mix(in srgb, var(--border-subtle) 72%, transparent);
    color: color-mix(in srgb, var(--foreground-muted) 72%, transparent);
    text-align: right;
    font-variant-numeric: tabular-nums;
    user-select: none;
  }

  .diff-line--addition .diff-line-number {
    color: var(--success);
  }

  .diff-line--deletion .diff-line-number {
    color: var(--error);
  }

  .diff-line-code {
    display: block;
    min-width: max-content;
    padding: 0 14px;
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    white-space: pre;
  }
</style>
