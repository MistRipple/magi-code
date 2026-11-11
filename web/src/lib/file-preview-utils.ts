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

// 可在 <img> 中直接预览的图片扩展名白名单，与后端 /api/files/raw 的
// image_mime_for_path 保持一致。tiff 不在内（浏览器原生不支持）。
const IMAGE_EXTENSIONS = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'bmp', 'ico', 'svg',
]);

export function getFileExtension(path: string): string {
  const normalized = path.split(/[\\/]/).pop() || path;
  const index = normalized.lastIndexOf('.');
  return index >= 0 ? normalized.slice(index + 1).toLowerCase() : normalized.toLowerCase();
}

export function isMarkdownFile(path: string): boolean {
  const ext = getFileExtension(path);
  return ext === 'md' || ext === 'markdown';
}

export function isHtmlFile(path: string): boolean {
  const ext = getFileExtension(path);
  return ext === 'html' || ext === 'htm';
}

export function isKnownBinaryFile(path: string): boolean {
  return BINARY_EXTENSIONS.has(getFileExtension(path));
}

export function isWordFile(path: string): boolean {
  return WORD_EXTENSIONS.has(getFileExtension(path));
}

export function isImageFile(path: string): boolean {
  return IMAGE_EXTENSIONS.has(getFileExtension(path));
}
