import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';

async function moduleUrl(relative, dependencies = {}) {
  let source = await readFile(new URL(relative, import.meta.url), 'utf8');
  for (const [specifier, url] of Object.entries(dependencies)) {
    source = source.replaceAll(`'${specifier}'`, `'${url}'`);
  }
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
}
const previewUrl = await moduleUrl('../src/lib/file-preview-utils.ts');
const { getFileTypeVisual, fileTypeCatalog } = await import(await moduleUrl('../src/lib/file-type.ts', {
  './file-preview-utils': previewUrl,
}));
const { fileTypeStyles } = await import(await moduleUrl('../src/lib/file-type-style.ts'));
const { icons } = await import(await moduleUrl('../src/lib/icons.ts'));

// Check path normalization, case, special names, compound suffixes and ambiguous suffix precedence.
for (const [path, kind] of [
  ['C:\\work\\APP.TSX', 'react'], ['/tmp/PHOTO.HEIC', 'image'],
  ['types.d.ts', 'typescript'], ['video.ts', 'typescript'], ['build.mts', 'typescript'],
  ['backup.TAR.GZ', 'binary'], ['backup.tar.zst', 'binary'],
  ['Cargo.toml', 'rust'], ['go.mod', 'go'], ['Gemfile.lock', 'ruby'],
  ['.env.production', 'env'], ['.editorconfig', 'config'], ['.gitignore', 'git'],
  ['Dockerfile.dev', 'docker'], ['build.Dockerfile', 'docker'], ['Containerfile', 'docker'],
  ['CMakeLists.txt', 'build'], ['MAKEFILE', 'build'], ['yarn.lock', 'lock'],
  ['brief.docm', 'document'], ['data.xlsb', 'spreadsheet'], ['slides.ppsx', 'presentation'],
  ['music.opus', 'audio'], ['clip.mkv', 'video'], ['captions.vtt', 'video'],
  ['font.woff2', 'font'], ['app.AppImage', 'executable'], ['library.so', 'binary-code'],
  ['certificate.key', 'certificate'], ['mesh.obj', 'model'], ['source.mm', 'swift'],
  ['template.dot', 'document'], ['model.safetensors', 'ai-model'],
  ['unknown.xyzabc', 'generic'], ['constructor', 'generic'], ['__proto__', 'generic'],
  ['', 'generic'],
]) assert.equal(getFileTypeVisual(path).kind, kind, path);

// Every registered extension/example must have a renderable identity and both theme colors.
const extensions = new Set();
const filenames = new Set();
for (const definition of fileTypeCatalog) {
  assert.equal(getFileTypeVisual(definition.example).kind, definition.kind, definition.example);
  for (const extension of definition.extensions) {
    assert.ok(!extensions.has(extension), `duplicate extension: ${extension}`);
    extensions.add(extension);
    const visual = getFileTypeVisual(`example.${extension.toUpperCase()}`);
    assert.equal(visual.kind, definition.kind, extension);
    assert.ok(icons[visual.icon], `missing glyph: ${visual.icon}`);
  }
  for (const filename of definition.filenames ?? []) {
    assert.ok(!filenames.has(filename.toLowerCase()), `duplicate name: ${filename}`);
    filenames.add(filename.toLowerCase());
    assert.equal(getFileTypeVisual(filename).kind, definition.kind, filename);
  }
  const style = fileTypeStyles[definition.kind];
  assert.match(style.light, /^#[\da-f]{6}$/i);
  assert.match(style.dark, /^#[\da-f]{6}$/i);
}
assert.equal(getFileTypeVisual('file.unknown').icon, 'file');
console.log(`File type checks passed: ${fileTypeCatalog.length} identities, ${extensions.size} extensions, ${filenames.size} filenames.`);
