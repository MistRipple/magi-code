import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { INSTALL_WEB_SETUP_INPUT } from "./web-setup-input.js";
import { DESKTOP_BROWSER_PROTOCOL_VERSION } from "@magi/desktop-browser-contracts";
import { parseBrowserHostRequest } from "@magi/desktop-browser-contracts/validation";

const flush = () => new Promise<void>(resolve => setImmediate(resolve));

test("配置写命令必须携带 Agent lease，缺失控制或用户控制均被协议拒绝", () => {
  for (const command of [
    { type: "web_platform_setup", payload: { tab_id: "home", action: "create_tunnel" } },
    { type: "web_configure_connector", payload: { tab_id: "home", name: "Magi", tunnel_id: "tunnel_1" } },
  ]) {
    const envelope = { request_id: "setup", protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION, command };
    assert.throws(() => parseBrowserHostRequest(envelope));
    assert.throws(() => parseBrowserHostRequest({ ...envelope,
      command: { ...command, payload: { ...command.payload, control: { mode: "user", fence: 1 } } },
    }));
    assert.doesNotThrow(() => parseBrowserHostRequest({ ...envelope,
      command: { ...command, payload: { ...command.payload, control: { mode: "agent", lease_id: "lease", fence: 1 } } },
    }));
  }
});

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
