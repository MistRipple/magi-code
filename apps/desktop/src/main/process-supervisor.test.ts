import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { ProcessSupervisor } from "./process-supervisor.js";

const DAEMON_IDENTITY = {
  serviceName: "magi-rust-backend",
  productVersion: "3.0.51",
  buildIdentity: "build-test-1",
};

function healthPayload(overrides: Record<string, unknown> = {}): string {
  return JSON.stringify({
    status: "ok",
    runtimeEpoch: "runtime-test-1",
    serviceName: DAEMON_IDENTITY.serviceName,
    productVersion: DAEMON_IDENTITY.productVersion,
    buildIdentity: DAEMON_IDENTITY.buildIdentity,
    startupNonce: "external-daemon-start-1",
    ...overrides,
  });
}

test("development supervisor reuses a healthy external daemon without spawning one", async () => {
  const server = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(healthPayload());
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
    });

    await supervisor.start();
    assert.equal(supervisor.status, "ready");
    assert.equal(supervisor.processId, null);
    await supervisor.stop();
    assert.equal(supervisor.status, "stopped");
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

test("development supervisor re-registers the desktop connection after an external daemon restart", async () => {
  let healthy = true;
  let runtimeEpoch = "runtime-test-1";
  let readyCalls = 0;
  const server = createServer((request, response) => {
    if (request.url === "/health") {
      response.writeHead(healthy ? 200 : 503, { "content-type": "application/json" });
      response.end(
        healthy
          ? healthPayload({ runtimeEpoch })
          : healthPayload({ status: "restarting", runtimeEpoch }),
      );
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end('{"status":"ok"}');
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
      onReady: async () => {
        readyCalls += 1;
      },
    });

    await supervisor.start();
    assert.equal(readyCalls, 1);
    healthy = false;
    await waitUntil(() => supervisor.status === "restarting");
    runtimeEpoch = "runtime-test-2";
    healthy = true;
    await waitUntil(() => readyCalls === 2);
    assert.equal(supervisor.status, "ready");
    await new Promise((resolve) => setTimeout(resolve, 1_100));
    assert.equal(readyCalls, 2);
    await supervisor.stop();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

test("development supervisor retries a transient registration failure within the same runtime epoch", async () => {
  const server = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(healthPayload({ runtimeEpoch: "runtime-registration-retry" }));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    let readyAttempts = 0;
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
      onReady: async () => {
        readyAttempts += 1;
        if (readyAttempts === 1) throw new Error("transient registration failure");
      },
    });

    await supervisor.start();
    assert.equal(supervisor.status, "starting");
    await new Promise((resolve) => setTimeout(resolve, 400));
    assert.equal(readyAttempts, 1);
    await waitUntil(() => readyAttempts === 2);
    assert.equal(supervisor.status, "ready");
    await new Promise((resolve) => setTimeout(resolve, 400));
    assert.equal(readyAttempts, 2);
    await supervisor.stop();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

test("development supervisor rejects an old daemon that has no Magi identity", async () => {
  const server = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end('{"status":"ok","runtimeEpoch":"legacy-runtime"}');
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
    });

    await assert.rejects(
      supervisor.start(),
      /magi_daemon_identity_mismatch:identity_fields_missing/u,
    );
    assert.equal(supervisor.status, "failed");
    await supervisor.stop();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

test("development supervisor rejects a foreign process with a mismatched Magi identity", async () => {
  const server = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(healthPayload({ serviceName: "foreign-service" }));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
    });

    await assert.rejects(
      supervisor.start(),
      /magi_daemon_identity_mismatch:service_name_mismatch/u,
    );
    assert.equal(supervisor.status, "failed");
    await supervisor.stop();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

test("external daemon identity mismatch is never re-registered and recovers only after identity is restored", async () => {
  let identity: Record<string, unknown> = { ...DAEMON_IDENTITY };
  let readyCalls = 0;
  const server = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(healthPayload(identity));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  try {
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const supervisor = new ProcessSupervisor({
      daemonPath: process.execPath,
      agentOrigin: `http://127.0.0.1:${address.port}`,
      environment: { MAGI_DESKTOP_REUSE_DAEMON: "1" },
      daemonIdentity: DAEMON_IDENTITY,
      onReady: async () => {
        readyCalls += 1;
      },
    });

    await supervisor.start();
    assert.equal(supervisor.status, "ready");
    assert.equal(readyCalls, 1);
    identity = { ...DAEMON_IDENTITY, buildIdentity: "foreign-build" };
    await waitUntil(() => supervisor.status === "failed");
    assert.equal(readyCalls, 1);

    identity = { ...DAEMON_IDENTITY };
    await waitUntil(() => readyCalls === 2);
    assert.equal(supervisor.status, "ready");
    await supervisor.stop();
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
});

async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.fail("等待条件超时");
}
