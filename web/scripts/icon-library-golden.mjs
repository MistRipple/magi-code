import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { relative } from 'node:path';
import { parse } from 'svelte/compiler';
import ts from 'typescript';

const root = new URL('../src/', import.meta.url);
const iconSource = await readFile(new URL('lib/icons.ts', root), 'utf8');
const { outputText } = ts.transpileModule(iconSource, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { icons, iconStyle } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
assert.equal(iconStyle.strokeWidth, 1.75);
assert.equal(iconStyle.fill, 'none');
for (const [name, glyph] of Object.entries(icons)) {
  assert.ok(glyph.length > 0, `empty icon: ${name}`);
  assert.ok(!/\b(?:fill|stroke|stroke-width)=/u.test(glyph), `drawing override: ${name}`);
}

// These SVGs encode measured progress, a cursor, or file brand identity, rather than UI actions.
const dedicatedSvg = new Map([
  ['components/Icon.svelte', 'shared icon renderer'],
  ['components/FileTypeIcon.svelte', 'file brand silhouette'],
  ['components/ContextUsageRing.svelte', 'context usage geometry'],
  ['components/DesktopUpdateStatus.svelte', 'download progress geometry'],
  ['components/tabs/AgentCursorOverlay.svelte', 'agent pointer geometry'],
]);
const encounteredSvg = new Set();
const errors = [];
let components = 0;
let references = 0;
function attribute(node, name) {
  const attr = node?.attributes?.find(item => item.type === 'Attribute' && item.name === name);
  return Array.isArray(attr?.value) && attr.value.every(item => item.type === 'Text')
    ? attr.value.map(item => item.data).join('') : null;
}
function visit(node, path, parent) {
  if (!node || typeof node !== 'object') return;
  if (Array.isArray(node)) { for (const child of node) visit(child, path, parent); return; }
  if (node.type === 'RegularElement' && node.name === 'svg') {
    if (!dedicatedSvg.has(path)) errors.push(`${path}: inline SVG should use Icon`);
    encounteredSvg.add(path);
  }
  if (node.type === 'Component' && node.name === 'Icon') {
    references++;
    const name = attribute(node, 'name');
    if (name !== null && !Object.hasOwn(icons, name)) errors.push(`${path}: unknown icon ${name}`);
  }
  if (node.type === 'Text' && node.data.trim() && /^[\s×✕✖✓✔▸▾▴◀▶▼▲⌕↻↗↙←→↑↓⋮⋯☰]+$/u.test(node.data)) {
    // A multiplication sign between width and height is text, not a close control.
    const dimensions = path === 'components/tabs/BrowserTabContent.svelte'
      && attribute(parent, 'class') === 'viewport-dimension-separator';
    if (!dimensions) errors.push(`${path}: text glyph ${node.data.trim()} should use Icon`);
  }
  for (const [key, value] of Object.entries(node)) {
    if (['attributes', 'expression', 'metadata', 'name_loc', 'loc'].includes(key)) continue;
    if (value && typeof value === 'object') visit(value, path, node.type === 'RegularElement' ? node : parent);
  }
}
async function scan(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const url = new URL(entry.name + (entry.isDirectory() ? '/' : ''), directory);
    if (entry.isDirectory()) { await scan(url); continue; }
    if (!entry.name.endsWith('.svelte')) continue;
    const path = relative(root.pathname, url.pathname);
    const source = await readFile(url, 'utf8');
    assert.ok(!/border-top-color:[^;]+;[\s\S]{0,100}animation:\s*[\w-]*spin/u.test(source), `${path}: use the shared loader glyph`);
    const ast = parse(source, { modern: true });
    visit(ast.fragment, path, null);
    components++;
    if (source.includes('<Icon ')) {
      assert.match(source, /import Icon from ['"][^'"]*\bIcon\.svelte['"]/u, `${path}: shared Icon import`);
    }
  }
}
await scan(root);
assert.deepEqual([...encounteredSvg].sort(), [...dedicatedSvg.keys()].sort(), 'Review dedicated SVG exceptions explicitly');
assert.deepEqual(errors, [], errors.join('\n'));

const pane = await readFile(new URL('web/RightPane.svelte', root), 'utf8');
assert.match(pane, /tab\.kind === 'code'[\s\S]*?<FileTypeIcon/u, 'code tabs share the file icon renderer');
assert.match(pane, /<FileTypeIcon path=\{\(tab\.payload as CodeTabPayload\)\.displayPath \?\?/u, 'file identity uses the readable path rather than an opaque host path');
const legacy = await readFile(new URL('demos/message-card-preview.ts', root), 'utf8');
assert.match(legacy, /mount\(FileTypeIcon/u, 'legacy demo must reuse file identity renderer');
assert.match(legacy, /import \{ icons, iconStyle,/u, 'legacy symbols must use the shared catalog');
const legacyMarkup = (await readFile(new URL('demos/message-cards.html', root), 'utf8')).replace(/<script\b[^>]*>[\s\S]*?<\/script>/gu, '');
const ids = new Set([...legacy.matchAll(/(?:^|[,\s])([a-z]+): '[^']+'/gu)].map(match => match[1]));
for (const match of legacyMarkup.matchAll(/<svg\b[^>]*>[\s\S]*?<\/svg>/gu)) {
  const drawing = match[0];
  const use = drawing.match(/<use href="#([^"]+)"/u);
  if (use) assert.ok(ids.has(use[1]), `legacy demo icon missing from shared map: ${use[1]}`);
  else assert.ok(/role="img"|id="demo-icon-definitions"/u.test(drawing), 'legacy inline SVG must be chart content or shared symbol definitions');
}
for (const path of ['components/KnowledgePanel.svelte', 'components/MermaidRenderer.svelte', 'components/diagram/GraphvizRenderer.svelte', 'web/WebFolderPicker.svelte']) {
  const source = await readFile(new URL(path, root), 'utf8');
  assert.match(source, /class="(?:kp-|mac-)?spinner"><Icon name="loader"/u, `${path}: shared loading glyph`);
}

console.log(`Icon library checks passed: ${components} components, ${references} shared icon references, ${Object.keys(icons).length} names.`);
