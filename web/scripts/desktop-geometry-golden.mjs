import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const source = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
  'utf8',
);

function section(startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  assert.notEqual(start, -1, `缺少区段起点: ${startMarker}`);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `缺少区段终点: ${endMarker}`);
  return source.slice(start, end);
}

const pump = section('function pumpDesktopGeometryReport()', 'function reportDesktopGeometry()');
const report = section('function reportDesktopGeometry()', 'function scheduleDesktopGeometryReport()');
const schedule = section('function scheduleDesktopGeometryReport()', 'function sameDesktopVisibilityTarget(');
const geometryEffect = section('  $effect(() => {\n    if (!desktopAppSurface || !desktopSnapshot || !workbenchElement)', '  onMount(() => {\n    if (!desktopAppSurface) return;');
const mount = section('  onMount(() => {\n    if (!desktopAppSurface) return;', '  onMount(() => {\n    applyViewportMode();');

assert.match(source, /let desktopGeometryInFlight: DesktopGeometryReport \| null = null/u);
assert.match(pump, /snapshot\.layout\.layoutRevision === pending\.frame\.layoutRevision/u);
assert.match(pump, /rendererGeometry\?\.revision === pending\.frame\.revision/u);
assert.match(pump, /if \(acknowledged\) \{[\s\S]*?desktopGeometryLastKey = pending\.key/u);
assert.doesNotMatch(pump, /desktopGeometryLastKey = pending\.key[\s\S]*?if \(!acknowledged\)/u);
assert.match(pump, /desktopGeometryPending\?\.frame\.layoutRevision === pending\.frame\.layoutRevision[\s\S]*?desktopGeometryPending = null/u);
assert.match(pump, /retryAfterNonAck = true/u);
assert.match(pump, /desktopGeometryInFlight = null;[\s\S]*?if \(retryAfterNonAck\) scheduleDesktopGeometryReport\(\)/u);

assert.match(report, /const browserPanelActive = layout\.rightPaneVisible[\s\S]*?layout\.activePanelKind === 'browser'/u);
assert.match(report, /layout\.rightPaneVisible && !rightPaneBounds\) return;/u);
assert.match(report, /browserTabId !== layout\.activeTabId[\s\S]*?return;/u);
assert.match(report, /browserContentSlot: MagiDesktopRendererGeometryFrame\['browserContentSlot'\] = null/u);
assert.doesNotMatch(report, /desktopGeometryLastKey =/u);
assert.match(report, /desktopGeometryPending\?\.key === key[\s\S]*?desktopGeometryInFlight\?\.key === key/u);

assert.doesNotMatch(`${pump}\n${report}\n${schedule}`, /window\.setTimeout|window\.clearTimeout/u);
assert.match(schedule, /desktopGeometrySchedulePending/u);
assert.match(schedule, /tick\(\)\.then/u);
assert.match(geometryEffect, /desktopSnapshot\.layout\.rendererGeometry\?\.revision/u);
assert.match(geometryEffect, /desktopSnapshot\.layout\.rendererGeometry\?\.browserContentSlot\?\.tabId/u);
assert.match(mount, /geometryResizeObserver/u);
assert.match(mount, /geometryMutationObserver/u);
assert.match(mount, /scheduleDesktopGeometryReport\(\)/u);

console.log('desktop geometry golden passed');
