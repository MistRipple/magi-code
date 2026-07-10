import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const slashCommands = await server.ssrLoadModule('/src/lib/slash-commands.ts');

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
  const commands = slashCommands.buildSlashCommands(skills, {
    name: '目标模式',
    description: '创建长期目标并持续推进',
  });

  assert.deepEqual(
    commands.map((command) => [command.kind, command.id]),
    [
      ['goal', 'goal'],
      ['skill', 'cn-engineering-standard'],
      ['skill', 'browser-control'],
    ],
    'slash command list must keep built-in goal first and append configured skills',
  );
  assert.deepEqual(
    slashCommands.filterSlashCommands(commands, 'go').map((command) => command.id),
    ['goal'],
    '/go must resolve the built-in goal command',
  );
  assert.deepEqual(
    slashCommands.filterSlashCommands(commands, '目标').map((command) => command.id),
    ['goal'],
    'Chinese goal keywords must resolve the built-in goal command',
  );
  assert.deepEqual(
    slashCommands.filterSlashCommands(commands, '工程').map((command) => command.id),
    ['cn-engineering-standard'],
    'skill names and descriptions must stay searchable',
  );
  assert.deepEqual(
    slashCommands.filterSlashCommands(commands, 'bct').map((command) => command.id),
    ['browser-control'],
    'skill search must preserve fuzzy matching',
  );
  assert.deepEqual(
    slashCommands.resolveSlashTrigger('/', 1),
    { triggerStart: 0, filter: '' },
    'a leading slash must open the command menu',
  );
  assert.deepEqual(
    slashCommands.resolveSlashTrigger('继续 /go', 6),
    { triggerStart: 3, filter: 'go' },
    'a slash after whitespace must filter commands',
  );
  assert.equal(
    slashCommands.resolveSlashTrigger('https://example.com', 19),
    null,
    'slashes inside URLs must not open the command menu',
  );
  assert.equal(
    slashCommands.resolveSlashTrigger('/goal 后续内容', 10),
    null,
    'a completed slash token must close after whitespace',
  );

  console.log('slash command golden replay passed');
}, { configFile: 'vite.web.config.ts' });
