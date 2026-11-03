#!/usr/bin/env node
// 用法：node scripts/compat/cli.mjs inspect <上游插件目录> [--kind dsh] [--require llm,tools] [--json]
import { renderText } from './core/report.mjs';
import { createRegistry } from './index.mjs';

const [command, directory, ...flags] = process.argv.slice(2);
const flag = (name) => {
  const index = flags.indexOf(`--${name}`);
  return index === -1 ? undefined : flags[index + 1];
};
if (command !== 'inspect' || !directory) {
  console.error('用法: node scripts/compat/cli.mjs inspect <目录> [--kind dsh] [--require llm,tools] [--json]');
  process.exit(2);
}
const registry = createRegistry();
const kind = flag('kind');
const required = flag('require')?.split(',').filter(Boolean);
try {
  const translator = kind ? registry.create(kind, { required }) : await registry.detect(directory, { required });
  const report = await translator.inspect(directory, { required });
  console.log(flags.includes('--json') ? JSON.stringify(report, null, 2) : renderText(report));
  process.exit(report.verdict.blocked.length === 0 ? 0 : 1);
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
