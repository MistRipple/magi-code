import { getFileExtension } from './file-preview-utils';
import type { IconName } from './icons';

export type FileTypeVisualKind =
  | 'svelte'
  | 'vue'
  | 'typescript'
  | 'javascript'
  | 'rust'
  | 'python'
  | 'go'
  | 'java'
  | 'kotlin'
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
  | 'pdf'
  | 'binary'
  | 'generic';

export interface FileTypeVisual {
  kind: FileTypeVisualKind;
  icon?: IconName;
  label: string;
}

const FILE_NAME_VISUALS: Record<string, FileTypeVisual> = {
  dockerfile: { kind: 'docker', icon: 'settings', label: 'Dockerfile' },
  '.gitignore': { kind: 'git', icon: 'git-branch', label: 'Git ignore' },
  '.gitattributes': { kind: 'git', icon: 'git-branch', label: 'Git attributes' },
  '.gitmodules': { kind: 'git', icon: 'git-branch', label: 'Git modules' },
  '.env': { kind: 'env', icon: 'file', label: 'Environment file' },
  '.env.local': { kind: 'env', icon: 'file', label: 'Local environment file' },
  'cargo.toml': { kind: 'rust', icon: 'code', label: 'Rust manifest' },
  'cargo.lock': { kind: 'rust', icon: 'code', label: 'Rust lockfile' },
  'package.json': { kind: 'json', icon: 'code', label: 'JSON file' },
  'package-lock.json': { kind: 'json', icon: 'code', label: 'JSON lockfile' },
  'pnpm-lock.yaml': { kind: 'yaml', icon: 'settings', label: 'YAML lockfile' },
  'yarn.lock': { kind: 'generic', icon: 'file', label: 'Lockfile' },
  license: { kind: 'text', icon: 'file-text', label: 'License text file' },
  notice: { kind: 'text', icon: 'file-text', label: 'Notice text file' },
  authors: { kind: 'text', icon: 'file-text', label: 'Authors text file' },
  contributors: { kind: 'text', icon: 'file-text', label: 'Contributors text file' },
  changelog: { kind: 'text', icon: 'file-text', label: 'Changelog text file' },
  changes: { kind: 'text', icon: 'file-text', label: 'Changes text file' },
  copying: { kind: 'text', icon: 'file-text', label: 'Copyright text file' },
};

const EXTENSION_VISUALS: Record<string, FileTypeVisual> = {
  svelte: { kind: 'svelte', icon: 'code', label: 'Svelte file' },
  vue: { kind: 'vue', icon: 'code', label: 'Vue file' },
  ts: { kind: 'typescript', icon: 'code', label: 'TypeScript file' },
  tsx: { kind: 'typescript', icon: 'code', label: 'TypeScript React file' },
  js: { kind: 'javascript', icon: 'code', label: 'JavaScript file' },
  jsx: { kind: 'javascript', icon: 'code', label: 'JavaScript React file' },
  mjs: { kind: 'javascript', icon: 'code', label: 'JavaScript module' },
  cjs: { kind: 'javascript', icon: 'code', label: 'CommonJS module' },
  rs: { kind: 'rust', icon: 'code', label: 'Rust file' },
  py: { kind: 'python', icon: 'code', label: 'Python file' },
  go: { kind: 'go', icon: 'code', label: 'Go file' },
  java: { kind: 'java', icon: 'code', label: 'Java file' },
  kt: { kind: 'kotlin', icon: 'code', label: 'Kotlin file' },
  kts: { kind: 'kotlin', icon: 'code', label: 'Kotlin script' },
  md: { kind: 'markdown', icon: 'file-text', label: 'Markdown file' },
  markdown: { kind: 'markdown', icon: 'file-text', label: 'Markdown file' },
  mdx: { kind: 'markdown', icon: 'file-text', label: 'MDX file' },
  json: { kind: 'json', icon: 'code', label: 'JSON file' },
  jsonc: { kind: 'json', icon: 'code', label: 'JSON with comments file' },
  html: { kind: 'html', icon: 'code', label: 'HTML file' },
  htm: { kind: 'html', icon: 'code', label: 'HTML file' },
  xml: { kind: 'html', icon: 'code', label: 'XML file' },
  css: { kind: 'css', icon: 'code', label: 'CSS file' },
  scss: { kind: 'css', icon: 'code', label: 'SCSS file' },
  sass: { kind: 'css', icon: 'code', label: 'Sass file' },
  less: { kind: 'css', icon: 'code', label: 'Less file' },
  yaml: { kind: 'yaml', icon: 'settings', label: 'YAML file' },
  yml: { kind: 'yaml', icon: 'settings', label: 'YAML file' },
  toml: { kind: 'toml', icon: 'settings', label: 'TOML file' },
  ini: { kind: 'toml', icon: 'settings', label: 'INI file' },
  sh: { kind: 'shell', icon: 'terminal', label: 'Shell script' },
  bash: { kind: 'shell', icon: 'terminal', label: 'Bash script' },
  zsh: { kind: 'shell', icon: 'terminal', label: 'Zsh script' },
  fish: { kind: 'shell', icon: 'terminal', label: 'Fish script' },
  sql: { kind: 'sql', icon: 'database', label: 'SQL file' },
  txt: { kind: 'text', icon: 'file-text', label: 'Text file' },
  text: { kind: 'text', icon: 'file-text', label: 'Text file' },
  log: { kind: 'text', icon: 'file-text', label: 'Log file' },
  csv: { kind: 'text', icon: 'file-text', label: 'CSV text file' },
  tsv: { kind: 'text', icon: 'file-text', label: 'TSV text file' },
  rst: { kind: 'text', icon: 'file-text', label: 'reStructuredText file' },
  adoc: { kind: 'text', icon: 'file-text', label: 'AsciiDoc file' },
  nfo: { kind: 'text', icon: 'file-text', label: 'Text information file' },
  png: { kind: 'image', icon: 'eye', label: 'Image file' },
  jpg: { kind: 'image', icon: 'eye', label: 'Image file' },
  jpeg: { kind: 'image', icon: 'eye', label: 'Image file' },
  gif: { kind: 'image', icon: 'eye', label: 'Image file' },
  webp: { kind: 'image', icon: 'eye', label: 'Image file' },
  svg: { kind: 'image', icon: 'eye', label: 'SVG image' },
  pdf: { kind: 'pdf', icon: 'file-text', label: 'PDF file' },
  zip: { kind: 'binary', icon: 'file', label: 'Archive file' },
  gz: { kind: 'binary', icon: 'file', label: 'Compressed archive' },
};

function baseName(path: string): string {
  return path.split(/[\\/]/u).pop()?.toLowerCase() || path.toLowerCase();
}

export function getFileTypeVisual(path: string): FileTypeVisual {
  const name = baseName(path);
  const exact = FILE_NAME_VISUALS[name]
    ?? (name.startsWith('.env.') ? FILE_NAME_VISUALS['.env'] : undefined)
    ?? (name.startsWith('dockerfile.') ? FILE_NAME_VISUALS.dockerfile : undefined);
  if (exact) {
    return exact;
  }
  return EXTENSION_VISUALS[getFileExtension(name)] ?? {
    kind: 'generic',
    icon: 'file',
    label: 'File',
  };
}
