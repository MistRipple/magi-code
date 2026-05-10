const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  c: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cxx: 'cpp',
  h: 'c',
  hpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  scss: 'scss',
  sass: 'scss',
  html: 'html',
  htm: 'html',
  xml: 'xml',
  svelte: 'svelte',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  ts: 'typescript',
  tsx: 'typescript',
  json: 'json',
  jsonc: 'json',
  md: 'markdown',
  markdown: 'markdown',
  py: 'python',
  rb: 'ruby',
  go: 'go',
  rs: 'rust',
  java: 'java',
  kt: 'kotlin',
  kts: 'kotlin',
  swift: 'swift',
  php: 'php',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  fish: 'bash',
  ps1: 'powershell',
  sql: 'sql',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  ini: 'ini',
  dockerfile: 'dockerfile',
  diff: 'diff',
  patch: 'diff',
};

const BINARY_EXTENSIONS = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'ico', 'bmp', 'tiff', 'svg',
  'zip', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'rar', '7z',
  'pdf', 'wasm', 'exe', 'dll', 'so', 'dylib', 'bin',
  'woff', 'woff2', 'ttf', 'otf', 'eot',
  'mp3', 'wav', 'flac', 'ogg', 'mp4', 'mov', 'avi', 'mkv', 'webm',
  'db', 'sqlite', 'sqlite3',
  'xls', 'xlsx', 'ppt', 'pptx',
]);

const WORD_EXTENSIONS = new Set(['doc', 'docx']);

export function getFileExtension(path: string): string {
  const normalized = path.split(/[\\/]/).pop() || path;
  const index = normalized.lastIndexOf('.');
  return index >= 0 ? normalized.slice(index + 1).toLowerCase() : normalized.toLowerCase();
}

export function getLanguageFromPath(path: string): string {
  const filename = path.split(/[\\/]/).pop()?.toLowerCase() || '';
  if (filename === 'dockerfile' || filename.endsWith('.dockerfile')) {
    return 'dockerfile';
  }
  return LANGUAGE_BY_EXTENSION[getFileExtension(path)] || '';
}

export function isMarkdownFile(path: string): boolean {
  const ext = getFileExtension(path);
  return ext === 'md' || ext === 'markdown';
}

export function isKnownBinaryFile(path: string): boolean {
  return BINARY_EXTENSIONS.has(getFileExtension(path));
}

export function isWordFile(path: string): boolean {
  return WORD_EXTENSIONS.has(getFileExtension(path));
}
