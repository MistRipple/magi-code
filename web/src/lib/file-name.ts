/** Split only the basename suffix; dotfiles and directory names stay intact. */
export function splitFileName(name: string): { stem: string; extension: string } {
  const separator = Math.max(name.lastIndexOf('/'), name.lastIndexOf('\\'));
  const basename = name.slice(separator + 1);
  const suffix = basename.match(/(?:\.d\.[cm]?tsx?|\.tar\.(?:gz|bz2|xz|zst)|\.[^.]+)$/iu)?.[0] ?? '';
  if (!suffix || suffix.length === basename.length || suffix === '.') {
    return { stem: name, extension: '' };
  }
  return { stem: name.slice(0, -suffix.length), extension: suffix };
}
