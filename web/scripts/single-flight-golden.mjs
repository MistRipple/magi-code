import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const { createSingleFlight } = await server.ssrLoadModule('/src/lib/single-flight.ts');

  function deferred() {
    let resolve;
    const promise = new Promise((done) => { resolve = done; });
    return { promise, resolve };
  }

  // 同一 key 的并发调用共用同一个在途请求，只真正执行一次。
  {
    const run = createSingleFlight();
    const gate = deferred();
    let started = 0;
    const start = () => { started += 1; return gate.promise; };
    const first = run('a', false, start);
    const second = run('a', false, start);
    const forcedWhileForced = run('a', false, start);
    assert.equal(started, 1, '并发的相同请求只能执行一次');
    gate.resolve('done');
    assert.deepEqual(await Promise.all([first, second, forcedWhileForced]), ['done', 'done', 'done']);
    // 完成后不再共用：下一次会重新执行。
    await run('a', false, () => { started += 1; return Promise.resolve('again'); });
    assert.equal(started, 2, '请求结束后必须释放，下一次调用重新执行');
  }

  // 不同 key 互不影响。
  {
    const run = createSingleFlight();
    let started = 0;
    const gate = deferred();
    const start = () => { started += 1; return gate.promise; };
    void run('a', false, start);
    void run('b', false, start);
    assert.equal(started, 2, '不同作用域必须各自请求');
    gate.resolve(1);
  }

  // 强制刷新不能被非强制的在途请求满足；强制的在途请求可以满足后来的非强制/强制调用。
  {
    const run = createSingleFlight();
    const gate = deferred();
    let started = 0;
    const start = () => { started += 1; return gate.promise; };
    void run('a', false, start);
    void run('a', true, start);
    assert.equal(started, 2, '非强制在途请求不能满足强制刷新');
    void run('a', true, start);
    void run('a', false, start);
    assert.equal(started, 2, '强制在途请求可以被后续调用共用');
    gate.resolve(1);
  }

  // 失败后同样释放，且失败会传给所有共用者。
  {
    const run = createSingleFlight();
    const gate = deferred();
    let started = 0;
    const start = () => { started += 1; return gate.promise.then(() => { throw new Error('boom'); }); };
    const first = run('a', false, start);
    const second = run('a', false, start);
    gate.resolve();
    await assert.rejects(first, /boom/);
    await assert.rejects(second, /boom/);
    await run('a', false, () => { started += 1; return Promise.resolve('recovered'); });
    assert.equal(started, 2, '失败后必须释放在途记录');
  }
});

console.log('single-flight golden passed');
