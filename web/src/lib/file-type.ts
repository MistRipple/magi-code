import { getFileExtension } from './file-preview-utils';
import type { IconName } from './icons';

export type FileTypeVisualKind =
  | 'svelte'
  | 'typescript'
  | 'javascript'
  | 'rust'
  | 'python'
  | 'go'
  | 'java'
  | 'markdown'
  | 'json'
  | 'html'
  | 'css'
  | 'yaml'
  | 'toml'
  | 'shell'
  | 'sql'
  | 'docker'
  | 'git'
  | 'env'
  | 'text'
  | 'image'
  | 'binary'
  | 'generic';

export interface FileTypeVisual {
  kind: FileTypeVisualKind;
  glyph?: string;
  icon?: IconName;
  label: string;
}

const FILE_NAME_VISUALS: Record<string, FileTypeVisual> = {
  dockerfile: { kind: 'docker', glyph: '▣', label: 'Dockerfile' },
  '.gitignore': { kind: 'git', glyph: '◆', label: 'Git ignore' },
  '.gitattributes': { kind: 'git', glyph: '◆', label: 'Git attributes' },
  '.gitmodules': { kind: 'git', glyph: '◆', label: 'Git modules' },
  '.env': { kind: 'env', glyph: '●', label: 'Environment file' },
  '.env.local': { kind: 'env', glyph: '●', label: 'Local environment file' },
  'cargo.toml': { kind: 'rust', glyph: 'R', label: 'Rust manifest' },
  'cargo.lock': { kind: 'rust', glyph: 'R', label: 'Rust lockfile' },
  'package.json': { kind: 'json', glyph: '{}', label: 'JSON file' },
  'package-lock.json': { kind: 'json', glyph: '{}', label: 'JSON lockfile' },
  'pnpm-lock.yaml': { kind: 'yaml', glyph: 'YML', label: 'YAML lockfile' },
  'yarn.lock': { kind: 'generic', glyph: 'L', label: 'Lockfile' },
  license: { kind: 'text', icon: 'file-text', label: 'License text file' },
  notice: { kind: 'text', icon: 'file-text', label: 'Notice text file' },
  authors: { kind: 'text', icon: 'file-text', label: 'Authors text file' },
  contributors: { kind: 'text', icon: 'file-text', label: 'Contributors text file' },
  changelog: { kind: 'text', icon: 'file-text', label: 'Changelog text file' },
  changes: { kind: 'text', icon: 'file-text', label: 'Changes text file' },
  copying: { kind: 'text', icon: 'file-text', label: 'Copyright text file' },
};

const EXTENSION_VISUALS: Record<string, FileTypeVisual> = {
  svelte: { kind: 'svelte', glyph: 'S', label: 'Svelte file' },
  vue: { kind: 'svelte', glyph: 'V', label: 'Vue file' },
  ts: { kind: 'typescript', glyph: 'TS', label: 'TypeScript file' },
  tsx: { kind: 'typescript', glyph: 'TS', label: 'TypeScript React file' },
  js: { kind: 'javascript', glyph: 'JS', label: 'JavaScript file' },
  jsx: { kind: 'javascript', glyph: 'JS', label: 'JavaScript React file' },
  mjs: { kind: 'javascript', glyph: 'JS', label: 'JavaScript module' },
  cjs: { kind: 'javascript', glyph: 'JS', label: 'CommonJS module' },
  rs: { kind: 'rust', glyph: 'R', label: 'Rust file' },
  py: { kind: 'python', glyph: 'PY', label: 'Python file' },
  go: { kind: 'go', glyph: 'GO', label: 'Go file' },
  java: { kind: 'java', glyph: 'J', label: 'Java file' },
  kt: { kind: 'java', glyph: 'K', label: 'Kotlin file' },
  kts: { kind: 'java', glyph: 'K', label: 'Kotlin script' },
  md: { kind: 'markdown', glyph: 'M↓', label: 'Markdown file' },
  markdown: { kind: 'markdown', glyph: 'M↓', label: 'Markdown file' },
  mdx: { kind: 'markdown', glyph: 'M↓', label: 'MDX file' },
  json: { kind: 'json', glyph: '{}', label: 'JSON file' },
  jsonc: { kind: 'json', glyph: '{}', label: 'JSON with comments file' },
  html: { kind: 'html', glyph: '#', label: 'HTML file' },
  htm: { kind: 'html', glyph: '#', label: 'HTML file' },
  xml: { kind: 'html', glyph: '<>', label: 'XML file' },
  css: { kind: 'css', glyph: '#', label: 'CSS file' },
  scss: { kind: 'css', glyph: '#', label: 'SCSS file' },
  sass: { kind: 'css', glyph: '#', label: 'Sass file' },
  less: { kind: 'css', glyph: '#', label: 'Less file' },
  yaml: { kind: 'yaml', glyph: 'YML', label: 'YAML file' },
  yml: { kind: 'yaml', glyph: 'YML', label: 'YAML file' },
  toml: { kind: 'toml', glyph: 'T', label: 'TOML file' },
  ini: { kind: 'toml', glyph: 'INI', label: 'INI file' },
  sh: { kind: 'shell', glyph: '$', label: 'Shell script' },
  bash: { kind: 'shell', glyph: '$', label: 'Bash script' },
  zsh: { kind: 'shell', glyph: '$', label: 'Zsh script' },
  fish: { kind: 'shell', glyph: '$', label: 'Fish script' },
  sql: { kind: 'sql', glyph: 'SQL', label: 'SQL file' },
  txt: { kind: 'text', icon: 'file-text', label: 'Text file' },
  text: { kind: 'text', icon: 'file-text', label: 'Text file' },
  log: { kind: 'text', icon: 'file-text', label: 'Log file' },
  csv: { kind: 'text', icon: 'file-text', label: 'CSV text file' },
  tsv: { kind: 'text', icon: 'file-text', label: 'TSV text file' },
  rst: { kind: 'text', icon: 'file-text', label: 'reStructuredText file' },
  adoc: { kind: 'text', icon: 'file-text', label: 'AsciiDoc file' },
  nfo: { kind: 'text', icon: 'file-text', label: 'Text information file' },
  png: { kind: 'image', glyph: '▧', label: 'Image file' },
  jpg: { kind: 'image', glyph: '▧', label: 'Image file' },
  jpeg: { kind: 'image', glyph: '▧', label: 'Image file' },
  gif: { kind: 'image', glyph: '▧', label: 'Image file' },
  webp: { kind: 'image', glyph: '▧', label: 'Image file' },
  svg: { kind: 'image', glyph: '◇', label: 'SVG image' },
  pdf: { kind: 'binary', glyph: 'PDF', label: 'PDF file' },
  zip: { kind: 'binary', glyph: 'ZIP', label: 'Archive file' },
  gz: { kind: 'binary', glyph: 'ZIP', label: 'Compressed archive' },
};

function baseName(path: string): string {
  return path.split(/[\\/]/u).pop()?.toLowerCase() || path.toLowerCase();
}

export function getFileTypeVisual(path: string): FileTypeVisual {
  const name = baseName(path);
  const exact = FILE_NAME_VISUALS[name];
  if (exact) {
    return exact;
  }
  return EXTENSION_VISUALS[getFileExtension(name)] ?? {
    kind: 'generic',
    icon: 'file',
    label: 'File',
  };
}
