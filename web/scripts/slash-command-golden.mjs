import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const composerActions = await server.ssrLoadModule('/src/lib/composer-actions.ts');
  const contextReferences = await server.ssrLoadModule('/src/lib/composer-context-references.ts');

  const skills = [
    {
      skillId: 'cn-engineering-standard',
      name: '中文工程规范',
      description: '按严格工程闭环处理代码问题',
    },
    {
      skillId: 'browser-control',
      name: '浏览器控制',
      description: '执行真实浏览器场景验收',
    },
  ];
  const commands = composerActions.buildComposerActions(skills, {
    goal: {
      name: '目标模式',
      description: '创建长期目标并持续推进',
    },
    context: {
      name: '文件和文件夹',
      description: '添加本轮上下文引用',
    },
  });

  assert.deepEqual(
    commands.map((command) => [command.kind, command.id]),
    [
      ['resource', 'file-or-directory'],
      ['goal', 'goal'],
      ['skill', 'cn-engineering-standard'],
      ['skill', 'browser-control'],
    ],
    'composer action list must keep one resource entry, goal mode, and configured skills',
  );
  assert.deepEqual(
    composerActions.filterSlashCommands(commands, 'go').map((command) => command.id),
    ['goal'],
    '/go must resolve the built-in goal command',
  );
  assert.deepEqual(
    composerActions.filterSlashCommands(commands, '目标').map((command) => command.id),
    ['goal'],
    'Chinese goal keywords must resolve the built-in goal command',
  );
  assert.deepEqual(
    composerActions.filterSlashCommands(commands, '工程').map((command) => command.id),
    ['cn-engineering-standard'],
    'skill names and descriptions must stay searchable',
  );
  assert.deepEqual(
    composerActions.filterSlashCommands(commands, 'bct').map((command) => command.id),
    ['browser-control'],
    'skill search must preserve fuzzy matching',
  );
  assert.deepEqual(
    composerActions.resolveSlashTrigger('/', 1),
    { triggerStart: 0, filter: '' },
    'a leading slash must open the command menu',
  );
  assert.deepEqual(
    composerActions.resolveSlashTrigger('继续 /go', 6),
    { triggerStart: 3, filter: 'go' },
    'a slash after whitespace must filter commands',
  );
  assert.equal(
    composerActions.resolveSlashTrigger('https://example.com', 19),
    null,
    'slashes inside URLs must not open the command menu',
  );
  assert.equal(
    composerActions.resolveSlashTrigger('/goal 后续内容', 10),
    null,
    'a completed slash token must close after whitespace',
  );

  const firstReference = contextReferences.addComposerContextReference([], {
    kind: 'file',
    path: '/Users/xie/code/TEST/README.md',
    name: 'README.md',
  });
  assert.equal(firstReference.length, 1, 'a file reference must be stored as structured composer state');
  assert.equal(firstReference[0].id, 'file:/Users/xie/code/TEST/README.md');
  assert.deepEqual(
    contextReferences.addComposerContextReference(firstReference, {
      kind: 'file',
      path: '/Users/xie/code/TEST/README.md',
      name: 'README.md',
    }),
    firstReference,
    'duplicate context references must not create duplicate chips or request payloads',
  );
  assert.deepEqual(
    contextReferences.toSessionContextReferencePayload(firstReference),
    [{ kind: 'file', path: '/Users/xie/code/TEST/README.md', name: 'README.md' }],
    'transport payload must exclude frontend-only identity fields',
  );

  console.log('slash command golden replay passed');
}, { configFile: 'vite.web.config.ts' });
