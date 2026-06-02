import assert from 'node:assert/strict';
import { createServer } from 'vite';

const server = await createServer({
  root: process.cwd(),
  configFile: false,
  logLevel: 'silent',
  server: { middlewareMode: true },
});

try {
  const toolCatalog = await server.ssrLoadModule('/src/shared/tool-catalog.ts');

  assert.equal(
    toolCatalog.normalizeToolRuntimeStatus('ready'),
    'ready',
    'explicit ready runtime status must be preserved',
  );

  assert.equal(
    toolCatalog.normalizeToolRuntimeStatus(' degraded '),
    'degraded',
    'runtime status must be trimmed before display/counting',
  );

  assert.equal(
    toolCatalog.normalizeToolRuntimeStatus(undefined),
    'unknown',
    'missing runtime status must not be treated as ready',
  );

  assert.equal(
    toolCatalog.normalizeToolRuntimeStatus(''),
    'unknown',
    'blank runtime status must not be treated as ready',
  );

  console.log('tool catalog normalization golden replay passed');
} finally {
  await server.close();
}
