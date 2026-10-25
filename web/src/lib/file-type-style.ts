/** File identity is intentionally colored; interface actions remain monochrome.
 * Brand paths: Simple Icons 16.35.0 (CC0), see public/assets/licenses/simple-icons.txt.
 */
import type { FileTypeVisualKind } from './file-type';

interface FileTypeStyle {
  light: string;
  dark: string;
  badge?: string;
  brandPath?: string;
}

export const fileTypeStyles: Record<FileTypeVisualKind, FileTypeStyle> = {
  svelte: { light: '#e44930', dark: '#ff795d', brandPath: 'M10.354 21.125a4.44 4.44 0 0 1-4.765-1.767 4.109 4.109 0 0 1-.703-3.107 3.898 3.898 0 0 1 .134-.522l.105-.321.287.21a7.21 7.21 0 0 0 2.186 1.092l.208.063-.02.208a1.253 1.253 0 0 0 .226.83 1.337 1.337 0 0 0 1.435.533 1.231 1.231 0 0 0 .343-.15l5.59-3.562a1.164 1.164 0 0 0 .524-.778 1.242 1.242 0 0 0-.211-.937 1.338 1.338 0 0 0-1.435-.533 1.23 1.23 0 0 0-.343.15l-2.133 1.36a4.078 4.078 0 0 1-1.135.499 4.44 4.44 0 0 1-4.765-1.766 4.108 4.108 0 0 1-.702-3.108 3.855 3.855 0 0 1 1.742-2.582l5.589-3.563a4.072 4.072 0 0 1 1.135-.499 4.44 4.44 0 0 1 4.765 1.767 4.109 4.109 0 0 1 .703 3.107 3.943 3.943 0 0 1-.134.522l-.105.321-.286-.21a7.204 7.204 0 0 0-2.187-1.093l-.208-.063.02-.207a1.255 1.255 0 0 0-.226-.831 1.337 1.337 0 0 0-1.435-.532 1.231 1.231 0 0 0-.343.15L8.62 9.368a1.162 1.162 0 0 0-.524.778 1.24 1.24 0 0 0 .211.937 1.338 1.338 0 0 0 1.435.533 1.235 1.235 0 0 0 .344-.151l2.132-1.36a4.067 4.067 0 0 1 1.135-.498 4.44 4.44 0 0 1 4.765 1.766 4.108 4.108 0 0 1 .702 3.108 3.857 3.857 0 0 1-1.742 2.583l-5.589 3.562a4.072 4.072 0 0 1-1.135.499m10.358-17.95C18.484-.015 14.082-.96 10.9 1.068L5.31 4.63a6.412 6.412 0 0 0-2.896 4.295 6.753 6.753 0 0 0 .666 4.336 6.43 6.43 0 0 0-.96 2.396 6.833 6.833 0 0 0 1.168 5.167c2.229 3.19 6.63 4.135 9.812 2.108l5.59-3.562a6.41 6.41 0 0 0 2.896-4.295 6.756 6.756 0 0 0-.665-4.336 6.429 6.429 0 0 0 .958-2.396 6.831 6.831 0 0 0-1.167-5.168Z' },
  vue: { light: '#25845d', dark: '#58c998', brandPath: 'M24,1.61H14.06L12,5.16,9.94,1.61H0L12,22.39ZM12,14.08,5.16,2.23H9.59L12,6.41l2.41-4.18h4.43Z' },
  typescript: { light: '#2474b9', dark: '#67b5f3', badge: 'TS' },
  javascript: { light: '#a47a00', dark: '#ebc64a', badge: 'JS' },
  rust: { light: '#a95d2c', dark: '#e8a778', badge: 'R' },
  python: { light: '#3879a8', dark: '#7cb7e0', badge: 'PY' },
  go: { light: '#087f98', dark: '#5ac4d6', badge: 'GO' },
  java: { light: '#bc5c2b', dark: '#ed9b6b', badge: 'J' },
  kotlin: { light: '#8854bd', dark: '#c099ee', badge: 'KT' },
  markdown: { light: '#397e54', dark: '#7bc696' },
  json: { light: '#a66e18', dark: '#dfb263' },
  html: { light: '#c55a31', dark: '#ee946f' },
  css: { light: '#5f62bc', dark: '#aaa5ef', badge: 'CSS' },
  yaml: { light: '#99669c', dark: '#c6a2d3' },
  toml: { light: '#976030', dark: '#d6ab77' },
  shell: { light: '#448350', dark: '#88c28f' },
  sql: { light: '#8b65a9', dark: '#bca0d7' },
  docker: { light: '#1c80ac', dark: '#6fbddc' },
  git: { light: '#c65937', dark: '#ef9a77' },
  env: { light: '#66863a', dark: '#a8c980' },
  text: { light: '#657b8c', dark: '#9eb3c3' },
  image: { light: '#a461aa', dark: '#cf9ad7' },
  pdf: { light: '#c4434a', dark: '#ee9299', badge: 'PDF' },
  document: { light: '#2868b2', dark: '#80b5ee', badge: 'W' },
  spreadsheet: { light: '#278052', dark: '#77c99e', badge: 'X' },
  presentation: { light: '#c15c33', dark: '#efa17e', badge: 'P' },
  binary: { light: '#8868a3', dark: '#baa1d2' },
  'react': { light: '#1682a0', dark: '#78d2e7', badge: 'RX' },
  'c': { light: '#4771b5', dark: '#96b8ef', badge: 'C' },
  'cpp': { light: '#3766a8', dark: '#87b4ed', badge: 'C+' },
  'csharp': { light: '#7550ac', dark: '#bc9ce5', badge: 'C#' },
  'swift': { light: '#c65c36', dark: '#f69f7c', badge: 'SW' },
  'dart': { light: '#087e9f', dark: '#71cce0', badge: 'DA' },
  'ruby': { light: '#b64048', dark: '#ec939a', badge: 'RB' },
  'php': { light: '#6468a5', dark: '#b0b3ea', badge: 'PHP' },
  'lua': { light: '#5154a4', dark: '#a5a9e5', badge: 'LUA' },
  'r': { light: '#3675b4', dark: '#89b9eb', badge: 'R' },
  'functional': { light: '#8757a7', dark: '#c4a0df' },
  'code': { light: '#4d7da1', dark: '#98bfd9' },
  'config': { light: '#80723c', dark: '#c7b97c' },
  'build': { light: '#9a6d37', dark: '#d9b07f' },
  'lock': { light: '#7c758b', dark: '#b9b0ca' },
  'database': { light: '#47799f', dark: '#8bb8db' },
  'data': { light: '#35817d', dark: '#87c9c3' },
  'ebook': { light: '#8b683a', dark: '#d7b583', badge: 'BK' },
  'email': { light: '#467da5', dark: '#92bede' },
  'vector': { light: '#9c7132', dark: '#e2b86c' },
  'design': { light: '#995eaa', dark: '#d2a0e0' },
  'audio': { light: '#a25792', dark: '#dc9bcb' },
  'video': { light: '#7865b3', dark: '#b7a6e8' },
  'disk': { light: '#687f92', dark: '#a8bfd1' },
  'font': { light: '#a05d74', dark: '#dda1b7' },
  'executable': { light: '#557e99', dark: '#9bbdd3' },
  'binary-code': { light: '#72778a', dark: '#b0b6cd' },
  'certificate': { light: '#9b7931', dark: '#dfc078' },
  'model': { light: '#378e85', dark: '#86d2c7' },
  'ai-model': { light: '#8b5cba', dark: '#c8a0ed' },
  'diagram': { light: '#4d8598', dark: '#94c9da' },
  'notebook': { light: '#99704c', dark: '#d9b18c' },
  generic: { light: 'var(--foreground-muted)', dark: 'var(--foreground-muted)' },
};
