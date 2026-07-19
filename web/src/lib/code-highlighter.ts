type HighlightJs = typeof import('highlight.js')['default'];

let highlighterPromise: Promise<HighlightJs> | null = null;

function loadHighlighter(): Promise<HighlightJs> {
  highlighterPromise ??= import('highlight.js').then((module) => module.default);
  return highlighterPromise;
}

export async function highlightCode(code: string, language: string): Promise<string | null> {
  const normalizedLanguage = language.trim().toLowerCase();
  if (!normalizedLanguage) return null;

  const highlighter = await loadHighlighter();
  if (!highlighter.getLanguage(normalizedLanguage)) return null;
  return highlighter.highlight(code, { language: normalizedLanguage }).value;
}
