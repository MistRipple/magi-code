import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { INSTALL_WEB_SETUP_INPUT } from "./web-setup-input.js";
const flush = () => new Promise<void>(resolve => setImmediate(resolve));

function fixture() {
  const context = createContext({ __magiBrowserAutomation: { reference: (element: string) => element } });
  const start = (id: string, expression: string) => runInContext(
    `${INSTALL_WEB_SETUP_INPUT}(${JSON.stringify(id)}, ${JSON.stringify(expression)})`, context,
  );
  return { context, start };
}

test("配置脚本等待原生动作确认，不能提前提交后续动作", async () => {
  const { context, start } = fixture();
  start("first", `(async () => {
    const input = globalThis.__magiSetupInput;
    await input.perform('click', 'create');
    await input.perform('type', 'name', 'Magi');
    return {status:'ready'};
  })()`);
  await flush();
  assert.equal(context.__magiSetupInput.snapshot().request.ref, "create");
  assert.equal(context.__magiSetupInput.done, false);
  context.__magiSetupInput.settle();
  await flush();
  assert.equal(context.__magiSetupInput.snapshot().request.kind, "type");
  context.__magiSetupInput.settle();
  await flush();
  assert.equal(context.__magiSetupInput.done, true);
  assert.equal(context.__magiSetupInput.value.status, "ready");
});

test("用户接管或取消后关闭待执行动作，旧脚本不能借下一条命令继续提交", async () => {
  const { context, start } = fixture();
  start("old", `(async () => {
    const input = globalThis.__magiSetupInput;
    await input.perform('click', 'open');
    await input.perform('click', 'save');
  })()`);
  await flush();
  const old = context.__magiSetupInput;
  start("new", `(async () => { await globalThis.__magiSetupInput.perform('click', 'inspect'); })()`);
  await flush();
  assert.equal(old.disposed, true);
  assert.equal(old.failed, true);
  assert.equal(old.pending, null);
  assert.equal(context.__magiSetupInput.snapshot().request.ref, "inspect");
  context.__magiSetupInput.dispose();
  await flush();
  assert.equal(context.__magiSetupInput.failed, true);
});
