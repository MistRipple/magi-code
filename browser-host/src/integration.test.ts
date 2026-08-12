import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { deflateSync } from "node:zlib";
import { WebSocket } from "ws";
import { startBrowserHostServer } from "./index";
import type {
  EventEnvelope,
  HostSnapshot,
  PageState,
  RequestEnvelope,
  ResponseEnvelope,
} from "./protocol";
import { PROTOCOL_VERSION } from "./protocol";

const chromiumExecutable = process.env.MAGI_BROWSER_TEST_CHROMIUM;
const HOST_COMMAND_RESPONSE_TIMEOUT_MILLIS = 30_000;
const LONG_RUNNING_COMMAND_RESPONSE_TIMEOUT_MILLIS = 120_000;

function crc32(bytes: Buffer): number {
  let checksum = 0xffffffff;
  for (const byte of bytes) {
    checksum ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      checksum = (checksum >>> 1) ^ (checksum & 1 ? 0xedb88320 : 0);
    }
  }
  return (checksum ^ 0xffffffff) >>> 0;
}

function pngChunk(type: string, data: Buffer): Buffer {
  const checksumInput = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const chunk = Buffer.alloc(data.length + 12);
  chunk.writeUInt32BE(data.length, 0);
  chunk.write(type, 4, 4, "ascii");
  data.copy(chunk, 8);
  chunk.writeUInt32BE(crc32(checksumInput), data.length + 8);
  return chunk;
}

function solidPng(width: number, height: number): Buffer {
  const scanlines = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowOffset = y * (width * 4 + 1);
    for (let x = 0; x < width; x += 1) {
      const pixelOffset = rowOffset + 1 + x * 4;
      scanlines[pixelOffset] = 49;
      scanlines[pixelOffset + 1] = 130;
      scanlines[pixelOffset + 2] = 206;
      scanlines[pixelOffset + 3] = 255;
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(scanlines)),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

function pngDimensions(bytes: Buffer): { width: number; height: number } {
  assert.deepEqual(
    [...bytes.subarray(0, 8)],
    [137, 80, 78, 71, 13, 10, 26, 10],
  );
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
  };
}

function jpegDimensions(bytes: Buffer): { width: number; height: number } {
  const startOfFrameMarkers = new Set([
    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7,
    0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
  ]);
  let offset = 2;
  while (offset + 9 < bytes.length) {
    if (bytes[offset] !== 0xff) {
      offset += 1;
      continue;
    }
    const marker = bytes[offset + 1];
    offset += 2;
    if (marker === 0xd8 || marker === 0xd9) continue;
    const segmentLength = bytes.readUInt16BE(offset);
    if (startOfFrameMarkers.has(marker)) {
      return {
        width: bytes.readUInt16BE(offset + 5),
        height: bytes.readUInt16BE(offset + 3),
      };
    }
    offset += segmentLength;
  }
  throw new Error("JPEG frame does not contain dimensions");
}

function jsonValueFrom(response: ResponseEnvelope): Record<string, unknown> {
  assert.equal(response.outcome.status, "succeeded", JSON.stringify(response));
  if (response.outcome.status !== "succeeded") throw new Error("host command failed");
  assert.equal(response.outcome.payload.type, "json");
  if (response.outcome.payload.type !== "json") throw new Error("host command did not return JSON");
  const value = response.outcome.payload.payload.value;
  assert(value && typeof value === "object" && !Array.isArray(value));
  return value as Record<string, unknown>;
}

function pageStateFrom(response: ResponseEnvelope): PageState {
  assert.equal(response.outcome.status, "succeeded", JSON.stringify(response));
  if (response.outcome.status !== "succeeded") throw new Error("host command failed");
  assert.equal(response.outcome.payload.type, "page_state");
  if (response.outcome.payload.type !== "page_state") throw new Error("host command did not return page state");
  return response.outcome.payload.payload;
}

test(
  "real Chromium executes the private Host protocol end to end",
  { skip: !chromiumExecutable, timeout: 300_000 },
  async () => {
    const profilePath = await mkdtemp(join(tmpdir(), "magi-browser-host-"));
    const uploadFixturePath = join(profilePath, "upload-fixture.txt");
    await writeFile(uploadFixturePath, "Magi upload fixture", "utf8");
    const extensionPath = join(profilePath, "extension-fixture");
    const pwaIcon = solidPng(512, 512);
    await mkdir(extensionPath, { recursive: true });
    await writeFile(join(extensionPath, "manifest.json"), JSON.stringify({
      manifest_version: 3,
      name: "Magi Extension Fixture",
      version: "1.0.0",
      background: { service_worker: "background.js" },
      action: { default_title: "Magi fixture action" },
    }), "utf8");
    await writeFile(join(extensionPath, "background.js"), "chrome.runtime.onInstalled.addListener(() => {});", "utf8");
    let signalSlowRequestStarted: (() => void) | undefined;
    let cacheableRequestCount = 0;
    const slowRequestStarted = new Promise<void>((accept) => {
      signalSlowRequestStarted = accept;
    });
    const appServer = createServer((request, response) => {
      if (request.url === "/drop") {
        request.socket.destroy();
        return;
      }
      if (request.url === "/redirect") {
        const address = appServer.address();
        assert(address && typeof address !== "string");
        response
          .writeHead(302, { location: `http://localhost:${address.port}/` })
          .end();
        return;
      }
      if (request.url === "/slow") {
        signalSlowRequestStarted?.();
        setTimeout(() => {
          response
            .writeHead(200, { "content-type": "text/html; charset=utf-8" })
            .end("<!doctype html><title>Slow page</title>");
        }, 2_000);
        return;
      }
      if (request.url === "/init-script") {
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end(`<!doctype html><title>Init pending</title><script>
            document.title = globalThis.__magiInitMarker ?? 'Init missing';
          </script>`);
        return;
      }
      if (request.url === "/cacheable") {
        cacheableRequestCount += 1;
        response
          .writeHead(200, {
            "content-type": "text/html; charset=utf-8",
            "cache-control": "public, max-age=3600",
          })
          .end(`<!doctype html><title>Cache ${cacheableRequestCount}</title>`);
        return;
      }
      if (request.url === "/beforeunload") {
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end(`<!doctype html><title>Before unload</title>
            <button id="activate-beforeunload">Enable navigation guard</button>
            <script>
              document.querySelector('#activate-beforeunload').addEventListener('click', () => {
                window.addEventListener('beforeunload', (event) => {
                  event.preventDefault();
                  event.returnValue = '';
                });
              });
            </script>`);
        return;
      }
      if (request.url === "/responsive") {
        const mobile = /Android/.test(request.headers["user-agent"] ?? "");
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end(`<!doctype html>
<html>
  <head>
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Responsive device test</title>
    <style>
      body {
        min-height: 2000px;
      }
      #responsive-grid {
        display: grid;
        grid-template-columns: repeat(3, minmax(0, 1fr));
        gap: 8px;
      }
      @media (max-width: 600px) {
        #responsive-grid {
          grid-template-columns: 1fr;
        }
      }
    </style>
  </head>
  <body>
    <p>${mobile ? "Server mobile" : "Server desktop"}</p>
    <p id="client-state"></p>
    <div id="responsive-grid">
      <p>Responsive A</p>
      <p>Responsive B</p>
      <p>Responsive C</p>
    </div>
    <script>
      let resizeCount = 0;
      const renderClientState = () => {
        document.querySelector('#client-state').textContent = [
          'Viewport ' + window.innerWidth,
          matchMedia('(max-width: 600px)').matches ? 'Media mobile' : 'Media desktop',
          'Touch ' + navigator.maxTouchPoints,
          'Resize ' + resizeCount,
        ].join(' / ');
      };
      window.addEventListener('resize', () => {
        resizeCount += 1;
        renderClientState();
      });
      renderClientState();
    </script>
  </body>
</html>`);
        return;
      }
      if (request.url === "/spa") {
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end(`<!doctype html>
<html>
  <head><title>SPA repository</title></head>
  <body>
    <h1>Repository</h1>
    <a id="spa-issues" href="/spa/issues">Issues <span>75</span></a>
    <main id="spa-content">Repository overview</main>
    <script>
      document.querySelector('#spa-issues').addEventListener('click', (event) => {
        event.preventDefault();
        setTimeout(() => {
          history.pushState({}, '', '/spa/issues');
          document.querySelector('#spa-content').textContent = 'Issues page ready';
          setTimeout(() => {
            document.title = 'Issues | SPA repository';
          }, 150);
        }, 300);
      });
    </script>
  </body>
</html>`);
        return;
      }
      if (request.url === "/download") {
        response
          .writeHead(200, {
            "content-type": "text/plain; charset=utf-8",
            "content-disposition": 'attachment; filename="sample.txt"',
          })
          .end("Magi browser download");
        return;
      }
      if (request.url?.startsWith("/api/data")) {
        response
          .writeHead(200, { "content-type": "application/json" })
          .end(JSON.stringify({
            ok: true,
            source: "browser-host-fixture",
            magiHeader: request.headers["x-magi-test"] ?? null,
          }));
        return;
      }
      if (request.url === "/manifest.webmanifest") {
        response
          .writeHead(200, { "content-type": "application/manifest+json" })
          .end(JSON.stringify({
            id: "/",
            name: "Magi Browser Fixture",
            short_name: "Magi Fixture",
            start_url: "/",
            scope: "/",
            display: "standalone",
            background_color: "#ffffff",
            theme_color: "#3182ce",
            icons: [{ src: "/pwa-icon.png", sizes: "512x512", type: "image/png" }],
          }));
        return;
      }
      if (request.url === "/pwa-icon.png") {
        response
          .writeHead(200, { "content-type": "image/png", "content-length": pwaIcon.length })
          .end(pwaIcon);
        return;
      }
      if (request.url === "/sw.js") {
        response
          .writeHead(200, { "content-type": "text/javascript" })
          .end("self.addEventListener('fetch', () => {});");
        return;
      }
      if (request.url === "/popup") {
        response
          .writeHead(200, { "content-type": "text/html; charset=utf-8" })
          .end("<!doctype html><title>Popup</title><p>Popup content</p>");
        return;
      }
      response
        .writeHead(200, { "content-type": "text/html; charset=utf-8" })
        .end(`<!doctype html>
<html>
  <head>
    <title>Browser Host Fixture</title>
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <link rel="manifest" href="/manifest.webmanifest" />
  </head>
  <body>
    <div id="section">Section <span>Section child</span></div>
    <button id="increment">Increment</button>
    <button id="delete-account">Delete account</button>
    <button id="show-dialog" onclick="alert('Dialog content')">Show dialog</button>
    <button id="open-popup" onclick="window.open('/popup', '_blank')">Open popup</button>
    <a id="download" href="/download" download="sample.txt">Download sample</a>
    <a id="repository-forks" href="/forks"><span>Fork</span> <strong>3,403</strong></a>
    <a id="repository-stars" href="/stargazers" aria-label="You must be signed in to star this repository"><span>Star</span> <strong>18.2k</strong></a>
    <input aria-label="Upload file" type="file" />
    <input id="name-input" aria-label="Name" />
    <select id="color-select" aria-label="Color"><option value="red">Red</option><option value="blue">Blue</option></select>
    <input id="enabled-input" aria-label="Enabled" type="checkbox" />
    <button id="hover-target">Hover target</button>
    <p id="hover-status">Hover idle</p>
    <div id="drag-source" draggable="true">Drag source</div>
    <div id="drag-target">Drag target</div>
    <p id="drag-status">Drop idle</p>
    <input aria-label="Password" type="password" value="server-secret" />
    <input aria-label="Verification code" autocomplete="one-time-code" value="123456" />
    <input aria-label="Card number" autocomplete="cc-number" value="4111111111111111" />
    <a id="redirect" href="/redirect">Redirect</a>
    <div id="link-noise"></div>
    <h1>Semantic summary</h1>
    <p>Important page description</p>
    <p id="count">Count 0</p>
    <p id="submit-status">Submit idle</p>
    <script>
      document.querySelector('#link-noise').innerHTML = Array.from(
        { length: 200 },
        (_, index) => '<a href="#noise-' + index + '">Noise ' + index + '</a>',
      ).join('');
      let count = 0;
      document.querySelector('#increment').addEventListener('click', () => {
        count += 1;
        document.querySelector('#count').textContent = 'Count ' + count;
      });
      document.querySelector('#name-input').addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
          document.querySelector('#submit-status').textContent = 'Submitted ' + event.target.value;
        }
      });
      document.querySelector('#hover-target').addEventListener('mouseenter', () => {
        document.querySelector('#hover-status').textContent = 'Hover complete';
      });
      document.querySelector('#drag-source').addEventListener('dragstart', (event) => {
        event.dataTransfer.setData('text/plain', 'magi');
      });
      document.querySelector('#drag-target').addEventListener('dragover', (event) => event.preventDefault());
      document.querySelector('#drag-target').addEventListener('drop', (event) => {
        event.preventDefault();
        document.querySelector('#drag-status').textContent = 'Drop ' + event.dataTransfer.getData('text/plain');
      });
      window.__dtmcp = {
        listTools: () => [{ name: 'fixture_status' }],
        executeTool: (name, params) => ({ name, params, ready: true }),
      };
      Object.defineProperty(navigator, 'modelContext', {
        configurable: true,
        value: {
          tools: [{ name: 'fixture_webmcp', execute: (input) => ({ input, ready: true }) }],
        },
      });
      console.log('fixture-console-ready');
      fetch('/api/data').then(response => response.json()).then(value => console.log('fixture-network-ready', value.ok));
      navigator.serviceWorker?.register('/sw.js');
    </script>
  </body>
</html>`);
    });
    await new Promise<void>((accept) =>
      appServer.listen(0, "127.0.0.1", () => accept()),
    );
    const appAddress = appServer.address();
    assert(appAddress && typeof appAddress !== "string");
    const token = "browser-host-integration-token-000000000000";
    const downloadPath = join(profilePath, "downloads");
    let hostServer: Awaited<ReturnType<typeof startBrowserHostServer>> | undefined;
    let client: HostTestClient | undefined;

    try {
      hostServer = await startBrowserHostServer({
        profilePath,
        chromiumExecutable: chromiumExecutable!,
        runtimeVersion: "test-runtime",
        hostVersion: "0.1.0",
        playwrightVersion: "test-playwright",
        runtimeEpoch: 1,
        headless: true,
        deviceScaleFactor: 2,
        downloadPath,
        maxActivePages: 16,
        maxTabs: 16,
        bindHost: "127.0.0.1",
        port: 0,
        authToken: token,
      });
      if (process.platform !== "win32") {
        const browserCommands = execFileSync("/bin/ps", ["-axo", "command"], {
          encoding: "utf8",
        })
          .split("\n")
          .filter((line) => line.includes(profilePath));
        assert(browserCommands.length > 0, "sandbox test must locate the Chromium process");
        assert(
          browserCommands.every((line) => !line.includes("--no-sandbox")),
          "Browser Runtime must never disable the Chromium sandbox",
        );
      }
      client = new HostTestClient(
        `ws://127.0.0.1:${hostServer.port}/control`,
        token,
      );
      await client.open();
      await client.waitForEvent("ready");
      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });
      const failedPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-create-failure",
          initial_url: `http://127.0.0.1:${appAddress.port}/drop`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(failedPage.outcome.status, "failed");
      const retriedPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-create-failure",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(retriedPage.outcome.status, "succeeded");
      const slowPagePromise = client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-slow",
          initial_url: `http://127.0.0.1:${appAddress.port}/slow`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      await slowRequestStarted;
      const fastPageStartedAt = performance.now();
      const fastPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-fast",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(fastPage.outcome.status, "succeeded");
      assert(
        performance.now() - fastPageStartedAt < 1_000,
        "a slow page must not block commands for another tab",
      );
      assert.equal((await slowPagePromise).outcome.status, "succeeded");
      pageStateFrom(await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-navigation-options",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      }));
      const navigateOptions = async (
        navigation: Extract<RequestEnvelope["command"], { type: "navigate" }>["payload"]["navigation"],
      ) => client!.call({
        type: "navigate",
        payload: {
          tab_id: "tab-navigation-options",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation,
        },
      });
      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/init-script`,
        init_script: "globalThis.__magiInitMarker = 'Init injected';",
      })).title, "Init injected");
      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/init-script`,
      })).title, "Init missing", "navigation init_script must only affect one navigation");

      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/cacheable`,
      })).title, "Cache 1");
      assert.equal(pageStateFrom(await navigateOptions({
        action: "reload",
        ignore_cache: true,
      })).title, "Cache 2");
      assert.equal(cacheableRequestCount, 2);
      await navigateOptions({ action: "url", url: `http://127.0.0.1:${appAddress.port}/` });
      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/cacheable`,
      })).title, "Cache 3");

      const timedOutNavigation = await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/slow`,
        timeout_ms: 50,
      });
      assert.equal(timedOutNavigation.outcome.status, "indeterminate");
      const snapshotAfterTimedOutNavigation = snapshotFrom(await client.call({
        type: "snapshot",
        payload: {
          tab_id: "tab-navigation-options",
          limits: { max_nodes: 100, max_text_bytes: 8 * 1024 },
          subtree_ref: null,
        },
      }));
      assert.equal(snapshotAfterTimedOutNavigation.tab_id, "tab-navigation-options");
      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/beforeunload`,
      })).title, "Before unload");
      const beforeUnloadSnapshot = snapshotFrom(await client.call({
        type: "snapshot",
        payload: {
          tab_id: "tab-navigation-options",
          limits: { max_nodes: 100, max_text_bytes: 8 * 1024 },
          subtree_ref: null,
        },
      }));
      const enableBeforeUnload = beforeUnloadSnapshot.root.children.find(
        node => node.name === "Enable navigation guard",
      );
      assert(enableBeforeUnload);
      pageStateFrom(await client.call({
        type: "click",
        payload: {
          tab_id: "tab-navigation-options",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: beforeUnloadSnapshot.snapshot_revision,
            element_ref: enableBeforeUnload.element_ref,
          },
        },
      }));
      const dismissedBeforeUnload = await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/`,
        handle_before_unload: "dismiss",
      });
      assert.equal(
        pageStateFrom(dismissedBeforeUnload).url,
        `http://127.0.0.1:${appAddress.port}/beforeunload`,
      );
      const dismissedBeforeUnloadEvent = await client.waitForEvent("dialog");
      assert.equal(dismissedBeforeUnloadEvent.event.type, "dialog");
      if (dismissedBeforeUnloadEvent.event.type === "dialog") {
        assert.equal(dismissedBeforeUnloadEvent.event.payload.dialog_type, "beforeunload");
      }
      assert.equal(pageStateFrom(await navigateOptions({
        action: "url",
        url: `http://127.0.0.1:${appAddress.port}/`,
        handle_before_unload: "accept",
      })).title, "Browser Host Fixture");
      const acceptedBeforeUnloadEvent = await client.waitForEvent("dialog");
      assert.equal(acceptedBeforeUnloadEvent.event.type, "dialog");
      if (acceptedBeforeUnloadEvent.event.type === "dialog") {
        assert.equal(acceptedBeforeUnloadEvent.event.payload.dialog_type, "beforeunload");
      }
      const restoredPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-restored",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 41,
          snapshot_revision: 17,
        },
      });
      assert.equal(restoredPage.outcome.status, "succeeded");
      if (restoredPage.outcome.status === "succeeded") {
        assert.equal(restoredPage.outcome.payload.type, "page_state");
        if (restoredPage.outcome.payload.type === "page_state") {
          assert.equal(restoredPage.outcome.payload.payload.navigation_revision, 41);
        }
      }
      const restoredSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-restored",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(restoredSnapshot.snapshot_revision, 18);

      const sameUrlRestore = pageStateFrom(
        await client.call({
          type: "restore_page",
          payload: {
            tab_id: "tab-restored",
            initial_url: `http://127.0.0.1:${appAddress.port}/`,
            viewport: {
              width: 900,
              height: 700,
              surface_width: 900,
              surface_height: 700,
              device_scale_factor_millis: 1_000,
              device_type: "desktop",
            },
            navigation_revision: 60,
            snapshot_revision: 29,
            allow_streaming_eviction: true,
          },
        }),
      );
      assert.equal(sameUrlRestore.navigation_revision, 60);
      const snapshotAfterSameUrlRestore = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-restored",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(snapshotAfterSameUrlRestore.snapshot_revision, 30);

      const authorityUrl = `http://127.0.0.1:${appAddress.port}/responsive`;
      const differentUrlRestore = pageStateFrom(
        await client.call({
          type: "restore_page",
          payload: {
            tab_id: "tab-restored",
            initial_url: authorityUrl,
            viewport: {
              width: 900,
              height: 700,
              surface_width: 900,
              surface_height: 700,
              device_scale_factor_millis: 1_000,
              device_type: "desktop",
            },
            navigation_revision: 72,
            snapshot_revision: 33,
            allow_streaming_eviction: true,
          },
        }),
      );
      assert.equal(differentUrlRestore.url, authorityUrl);
      assert.equal(differentUrlRestore.navigation_revision, 72);
      const snapshotAfterDifferentUrlRestore = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-restored",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(snapshotAfterDifferentUrlRestore.snapshot_revision, 34);

      const responsivePage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-responsive",
          initial_url: `http://127.0.0.1:${appAddress.port}/responsive`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(responsivePage.outcome.status, "succeeded");
      const desktopResponsiveSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        desktopResponsiveSnapshot.root.children.some(
          (node) => node.name === "Server desktop",
        ),
        "desktop viewport must keep the desktop user agent",
      );
      assert(
        desktopResponsiveSnapshot.root.children.some(
          (node) => node.name === "Viewport 900 / Media desktop / Touch 0 / Resize 0",
        ),
        "desktop viewport must expose desktop layout metrics",
      );
      const desktopResponsiveA = desktopResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive A",
      );
      const desktopResponsiveB = desktopResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive B",
      );
      assert(desktopResponsiveA?.bounds && desktopResponsiveB?.bounds);
      assert.equal(desktopResponsiveA.bounds.y, desktopResponsiveB.bounds.y);
      assert(
        desktopResponsiveB.bounds.x > desktopResponsiveA.bounds.x,
        "desktop responsive content must use the wide multi-column layout",
      );

      const mobileViewport = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 390,
            height: 844,
            surface_width: 390,
            surface_height: 844,
            device_scale_factor_millis: 1_000,
            device_type: "mobile",
          },
        },
      });
      assert.equal(mobileViewport.outcome.status, "succeeded");
      const mobileResponsiveSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        mobileResponsiveSnapshot.root.children.some(
          (node) => node.name === "Server desktop",
        ),
        "device emulation must resize the loaded document without reloading it",
      );
      assert(
        mobileResponsiveSnapshot.root.children.some(
          (node) => node.name === "Viewport 390 / Media mobile / Touch 5 / Resize 1",
        ),
        "one viewport command must produce one native resize with the requested device metrics",
      );
      const mobileResponsiveA = mobileResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive A",
      );
      const mobileResponsiveB = mobileResponsiveSnapshot.root.children.find(
        (node) => node.name === "Responsive B",
      );
      assert(mobileResponsiveA?.bounds && mobileResponsiveB?.bounds);
      assert.equal(mobileResponsiveA.bounds.x, mobileResponsiveB.bounds.x);
      assert(
        mobileResponsiveB.bounds.y > mobileResponsiveA.bounds.y,
        "mobile responsive content must reflow vertically instead of being clipped",
      );
      const mobileScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(mobileScreenshotResponse.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 390, height: 844 },
        "CDP screenshot output must stay in logical viewport pixels at high DPI",
      );
      const fullPageScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          full_page: true,
          format: "png",
        },
      });
      assert.equal(fullPageScreenshotResponse.outcome.status, "succeeded");
      const fullPageDimensions = pngDimensions(await client.waitForBinary());
      assert.equal(fullPageDimensions.width, 390);
      assert(fullPageDimensions.height > 844);
      const clippedScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          clip: { x: 0.1, y: 0.25, width: 0.5, height: 0.25 },
          full_page: false,
          format: "png",
        },
      });
      assert.equal(clippedScreenshotResponse.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 195, height: 211 },
        "annotation screenshots must contain only the selected viewport region",
      );
      const jpegScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          full_page: false,
          format: "jpeg",
          quality: 72,
        },
      });
      assert.equal(jpegScreenshotResponse.outcome.status, "succeeded");
      assert.deepEqual(jpegDimensions(await client.waitForBinary()), { width: 390, height: 844 });
      const webpScreenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-responsive",
          target: null,
          clip: { x: 0, y: 0, width: 0.5, height: 0.5 },
          full_page: false,
          format: "webp",
          quality: 64,
        },
      });
      assert.equal(webpScreenshotResponse.outcome.status, "succeeded");
      const webpScreenshot = await client.waitForBinary();
      assert.equal(webpScreenshot.subarray(0, 4).toString("ascii"), "RIFF");
      assert.equal(webpScreenshot.subarray(8, 12).toString("ascii"), "WEBP");

      const fittedWideViewport = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_280,
            height: 800,
            surface_width: 511,
            surface_height: 1_099,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(fittedWideViewport.outcome.status, "succeeded");
      const fittedWideSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        fittedWideSnapshot.root.children.some(
          (node) => node.name === "Viewport 1280 / Media desktop / Touch 0 / Resize 3",
        ),
        "browser-native fitting must preserve the requested logical desktop viewport",
      );
      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-responsive",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      const fittedWideCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_280
          && event.event.payload.surface_width === 511,
      );
      const fittedWideFrame = fittedWideCapture.event;
      assert.equal(fittedWideFrame.event.type, "screencast_frame");
      if (fittedWideFrame.event.type === "screencast_frame") {
        assert.equal(fittedWideFrame.event.payload.width, 1_280);
        assert.equal(fittedWideFrame.event.payload.height, 800);
        assert.equal(fittedWideFrame.event.payload.surface_width, 511);
        assert.equal(fittedWideFrame.event.payload.surface_height, 319);
      }
      assert.deepEqual(
        jpegDimensions(fittedWideCapture.binary),
        { width: 2_560, height: 1_600 },
        "Chromium must keep a native high-DPI frame while the panel surface fits it for display",
      );
      const resizedSurface = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_280,
            height: 800,
            surface_width: 400,
            surface_height: 900,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(resizedSurface.outcome.status, "succeeded");
      const resizedSurfaceSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-responsive",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1_024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(
        resizedSurfaceSnapshot.snapshot_revision,
        fittedWideSnapshot.snapshot_revision + 1,
        "surface-only fitting must not invalidate the canonical DOM snapshot",
      );
      assert(
        resizedSurfaceSnapshot.root.children.some(
          (node) => node.name === "Viewport 1280 / Media desktop / Touch 0 / Resize 3",
        ),
        "resizing only the Chromium output surface must not resize the logical page viewport",
      );
      const resizedSurfaceScroll = await client.call({
        type: "scroll",
        payload: {
          tab_id: "tab-responsive",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: null,
          delta_x: 0,
          delta_y: 100,
        },
      });
      assert.equal(resizedSurfaceScroll.outcome.status, "succeeded");
      const resizedSurfaceCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_280
          && event.event.payload.surface_width === 400,
      );
      const resizedSurfaceFrame = resizedSurfaceCapture.event;
      assert.equal(resizedSurfaceFrame.event.type, "screencast_frame");
      if (resizedSurfaceFrame.event.type === "screencast_frame") {
        assert.equal(resizedSurfaceFrame.event.payload.width, 1_280);
        assert.equal(resizedSurfaceFrame.event.payload.height, 800);
        assert.equal(resizedSurfaceFrame.event.payload.surface_width, 400);
        assert.equal(resizedSurfaceFrame.event.payload.surface_height, 250);
      }
      assert.deepEqual(
        jpegDimensions(resizedSurfaceCapture.binary),
        { width: 2_560, height: 1_600 },
        "surface-only changes must not lower the stable screencast resolution",
      );
      const tallLogicalViewport = await client.call({
        type: "set_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_280,
            height: 2_930,
            surface_width: 400,
            surface_height: 900,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(tallLogicalViewport.outcome.status, "succeeded");
      const tallCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_280
          && event.event.payload.height === 2_930,
      );
      const tallScale = Math.min(1, 7_680 / (1_280 * 2), 4_320 / (2_930 * 2));
      assert.deepEqual(
        jpegDimensions(tallCapture.binary),
        {
          width: Math.round(1_280 * 2 * tallScale),
          height: Math.round(2_930 * 2 * tallScale),
        },
        "高逻辑视口必须由 Chromium 按画面流上限等比缩放，不能回退为尺寸不匹配错误",
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-responsive" },
      });

      const logicalOnlyViewport = await client.call({
        type: "set_logical_viewport",
        payload: {
          tab_id: "tab-responsive",
          viewport: {
            width: 1_600,
            height: 900,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
        },
      });
      assert.equal(logicalOnlyViewport.outcome.status, "succeeded");
      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-responsive",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      const logicalOnlyCapture = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-responsive"
          && event.event.payload.width === 1_600,
      );
      const logicalOnlyFrame = logicalOnlyCapture.event;
      assert.equal(logicalOnlyFrame.event.type, "screencast_frame");
      if (logicalOnlyFrame.event.type === "screencast_frame") {
        assert.equal(logicalOnlyFrame.event.payload.width, 1_600);
        assert.equal(logicalOnlyFrame.event.payload.height, 900);
        assert.equal(logicalOnlyFrame.event.payload.surface_width, 400);
        assert.equal(logicalOnlyFrame.event.payload.surface_height, 225);
      }
      assert.deepEqual(
        jpegDimensions(logicalOnlyCapture.binary),
        { width: 3_200, height: 1_800 },
        "logical viewport changes must preserve the existing display surface",
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-responsive" },
      });

      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });

      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-a",
          initial_url: "about:blank",
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: `http://127.0.0.1:${appAddress.port}/`,
          },
        },
      });
      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-spa",
          initial_url: `http://127.0.0.1:${appAddress.port}/spa`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      const spaSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-spa",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const spaIssues = spaSnapshot.root.children.find(
        (node) => node.name === "Issues 75",
      );
      assert(spaIssues, "SPA navigation link must be present in the snapshot");
      const spaClickStartedAt = performance.now();
      const spaClick = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-spa",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: spaSnapshot.snapshot_revision,
            element_ref: spaIssues.element_ref,
          },
        },
      });
      assert.equal(spaClick.outcome.status, "succeeded");
      assert(
        performance.now() - spaClickStartedAt < 1_500,
        "SPA click should settle promptly instead of waiting for the navigation timeout",
      );
      const spaAfterClick = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-spa",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        spaAfterClick.root.children.some((node) => node.name === "Issues page ready"),
        "SPA click must return after asynchronous content has settled",
      );
      if (spaClick.outcome.status === "succeeded") {
        assert.equal(spaClick.outcome.payload.type, "page_state");
        if (spaClick.outcome.payload.type === "page_state") {
          assert.equal(spaClick.outcome.payload.payload.url, `http://127.0.0.1:${appAddress.port}/spa/issues`);
          assert.equal(spaClick.outcome.payload.payload.title, "Issues | SPA repository");
        }
      }
      const firstSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert.equal(firstSnapshot.truncated, true);
      assert.equal(firstSnapshot.returned_nodes, 160);
      assert(firstSnapshot.total_nodes > firstSnapshot.returned_nodes);
      const button = firstSnapshot.root.children.find(
        (node) => node.name === "Increment",
      );
      const input = firstSnapshot.root.children.find(
        (node) => node.name === "Name",
      );
      const deleteAccount = firstSnapshot.root.children.find(
        (node) => node.name === "Delete account",
      );
      const showDialog = firstSnapshot.root.children.find(
        (node) => node.name === "Show dialog",
      );
      const openPopup = firstSnapshot.root.children.find(
        (node) => node.name === "Open popup",
      );
      const downloadSample = firstSnapshot.root.children.find(
        (node) => node.name === "Download sample",
      );
      const repositoryForks = firstSnapshot.root.children.find(
        (node) => node.name === "Fork 3,403",
      );
      const repositoryStars = firstSnapshot.root.children.find(
        (node) => node.name === "You must be signed in to star this repository",
      );
      const uploadFile = firstSnapshot.root.children.find(
        (node) => node.name === "Upload file",
      );
      const colorSelect = firstSnapshot.root.children.find(
        (node) => node.name === "Color",
      );
      const enabledInput = firstSnapshot.root.children.find(
        (node) => node.name === "Enabled",
      );
      const hoverTarget = firstSnapshot.root.children.find(
        (node) => node.name === "Hover target",
      );
      const dragSource = firstSnapshot.root.children.find(
        (node) => node.name === "Drag source",
      );
      const dragTarget = firstSnapshot.root.children.find(
        (node) => node.name === "Drag target",
      );
      const password = firstSnapshot.root.children.find(
        (node) => node.name === "Password",
      );
      const verificationCode = firstSnapshot.root.children.find(
        (node) => node.name === "Verification code",
      );
      const cardNumber = firstSnapshot.root.children.find(
        (node) => node.name === "Card number",
      );
      const semanticSummary = firstSnapshot.root.children.find(
        (node) => node.name === "Semantic summary",
      );
      const pageDescription = firstSnapshot.root.children.find(
        (node) => node.name === "Important page description",
      );
      assert(button, "button snapshot ref should exist");
      assert(input, "input snapshot ref should exist");
      assert(deleteAccount, "sensitive action snapshot ref should exist");
      assert(showDialog, "dialog test ref should exist");
      assert(openPopup, "popup test ref should exist");
      assert(downloadSample, "download test ref should exist");
      assert(repositoryForks, "nested control text should form one semantic name");
      assert.equal(
        repositoryStars?.description,
        "Star 18.2k",
        "visible control text must remain available when aria-label hides a count",
      );
      assert(
        !firstSnapshot.root.children.some(
          (node) => node.role === null && (node.name === "Fork" || node.name === "3,403"),
        ),
        "interactive controls must not expose duplicate plain-text descendants",
      );
      assert(uploadFile, "file chooser test ref should exist");
      assert(colorSelect, "select test ref should exist");
      assert(enabledInput, "checkbox test ref should exist");
      assert(hoverTarget, "hover test ref should exist");
      assert(dragSource, "drag source ref should exist");
      assert(dragTarget, "drag target ref should exist");
      assert(password, "password snapshot ref should exist");
      assert(verificationCode, "verification code snapshot ref should exist");
      assert(cardNumber, "card number snapshot ref should exist");
      assert(semanticSummary, "headings must survive dense interactive pages");
      assert(pageDescription, "page descriptions must survive dense interactive pages");
      assert.equal(password.editable, false);
      assert.equal(password.value, null);
      assert.equal(password.sensitive_input_kind, "password");
      assert.equal(verificationCode.editable, false);
      assert.equal(verificationCode.value, null);
      assert.equal(verificationCode.sensitive_input_kind, "one_time_code");
      assert.equal(cardNumber.editable, false);
      assert.equal(cardNumber.value, null);
      assert.equal(cardNumber.sensitive_input_kind, "payment_card");
      const continuationRef = firstSnapshot.continuation_refs[0];
      assert(continuationRef, "snapshot should expose a subtree continuation ref");
      const firstSubtreeSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: continuationRef,
          },
        }),
      );
      assert(firstSubtreeSnapshot.root.children.length > 0);
      const secondSubtreeSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: continuationRef,
          },
        }),
      );
      assert(secondSubtreeSnapshot.root.children.length > 0);
      const screenshotFromEarlierSnapshot = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-a",
          target: {
            snapshot_revision: firstSnapshot.snapshot_revision,
            element_ref: input.element_ref,
          },
          full_page: false,
          format: "png",
        },
      });
      assert.equal(
        screenshotFromEarlierSnapshot.outcome.status,
        "succeeded",
        "read-only snapshots must not invalidate earlier element refs",
      );
      const earlierScreenshot = await client.waitForBinary();
      assert.equal(earlierScreenshot[0], 0x89);
      assert.equal(earlierScreenshot[1], 0x50);
      const clickTestTarget = async (
        target: typeof showDialog,
        snapshotRevision = firstSnapshot.snapshot_revision,
        tabId = "tab-a",
      ) => {
        assert(target);
        const response = await client!.call({
          type: "click",
          payload: {
            tab_id: tabId,
            control: { mode: "agent", lease_id: "lease-a", fence: 1 },
            target: {
              snapshot_revision: snapshotRevision,
              element_ref: target.element_ref,
            },
          },
        });
        return pageStateFrom(response);
      };
      await clickTestTarget(showDialog);
      const clickCursor = await waitForAgentCursor(client, "tab-a", "click");
      assert.equal(clickCursor.event.type, "agent_cursor");
      if (clickCursor.event.type === "agent_cursor") {
        assert.equal(clickCursor.event.payload.tab_id, "tab-a");
        assert.equal(clickCursor.event.payload.visible, true);
        assert.equal(clickCursor.event.payload.action, "click");
        assert(clickCursor.event.payload.x !== null);
        assert(clickCursor.event.payload.y !== null);
        assert(clickCursor.event.payload.x >= 0 && clickCursor.event.payload.x <= 1);
        assert(clickCursor.event.payload.y >= 0 && clickCursor.event.payload.y <= 1);
      }
      const dialogEvent = await client.waitForEvent("dialog");
      assert.equal(dialogEvent.event.type, "dialog");
      if (dialogEvent.event.type === "dialog") {
        assert.equal(dialogEvent.event.payload.message, "Dialog content");
      }
      const pendingDialog = await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "dialog",
          arguments: { action: "list" },
        },
      });
      assert.equal(pendingDialog.outcome.status, "succeeded");
      const dismissedDialog = await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "dialog",
          arguments: { action: "dismiss" },
        },
      });
      assert.equal(dismissedDialog.outcome.status, "succeeded");
      await clickTestTarget(showDialog);
      await client.waitForEvent("dialog");
      const acceptedDialog = await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "dialog",
          arguments: { action: "accept" },
        },
      });
      assert.equal(acceptedDialog.outcome.status, "succeeded");
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "hover",
          arguments: {
            snapshot_revision: firstSnapshot.snapshot_revision,
            element_ref: hoverTarget.element_ref,
          },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "wait_for",
          arguments: { text: ["Hover complete"], timeout_ms: 2_000 },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "drag",
          arguments: {
            from: {
              snapshot_revision: firstSnapshot.snapshot_revision,
              element_ref: dragSource.element_ref,
            },
            to: {
              snapshot_revision: firstSnapshot.snapshot_revision,
              element_ref: dragTarget.element_ref,
            },
          },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "fill_form",
          arguments: {
            elements: [
              { snapshot_revision: firstSnapshot.snapshot_revision, element_ref: input.element_ref, value: "Batch" },
              { snapshot_revision: firstSnapshot.snapshot_revision, element_ref: colorSelect.element_ref, value: "blue" },
              { snapshot_revision: firstSnapshot.snapshot_revision, element_ref: enabledInput.element_ref, value: true },
            ],
          },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "upload_file",
          arguments: {
            snapshot_revision: firstSnapshot.snapshot_revision,
            element_ref: uploadFile.element_ref,
            file_path: uploadFixturePath,
          },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "click_at",
          arguments: { x: 3, y: 3 },
        },
      }));
      const evaluated = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "evaluate",
          arguments: { function: "() => ({ title: document.title, checked: document.querySelector('#enabled-input').checked })", wait_for_stable_dom: false },
        },
      }));
      assert.deepEqual(evaluated.value, { title: "Browser Host Fixture", checked: true });
      const consoleMessages = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "console", arguments: { action: "list", page_size: 50 } },
      }));
      assert(Array.isArray(consoleMessages.messages));
      assert(consoleMessages.messages.some((entry) => (entry as { text?: string }).text?.includes("fixture-console-ready")));
      const firstConsoleMessage = (consoleMessages.messages as Array<{ id?: number }>)[0];
      assert(firstConsoleMessage?.id);
      const consoleMessage = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "console",
          arguments: { action: "get", message_id: firstConsoleMessage.id },
        },
      }));
      assert.equal((consoleMessage.message as { id?: number }).id, firstConsoleMessage.id);
      const networkRequests = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "network", arguments: { action: "list", page_size: 100 } },
      }));
      assert(Array.isArray(networkRequests.requests));
      const apiRequest = networkRequests.requests.find((entry) => (entry as { url?: string }).url?.endsWith("/api/data")) as { id?: number } | undefined;
      assert(apiRequest?.id);
      const networkRequest = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "network", arguments: { action: "get", request_id: apiRequest.id, include_body: true } },
      }));
      assert(String(networkRequest.response_body).includes("browser-host-fixture"));
      const thirdPartyList = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "third_party",
          arguments: { action: "list" },
        },
      }));
      assert.equal((thirdPartyList.tools as Array<{ name?: string }>)[0]?.name, "fixture_status");
      const thirdParty = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "third_party",
          arguments: { action: "execute", tool_name: "fixture_status", params: { value: 1 } },
        },
      }));
      assert.equal((thirdParty.result as { ready?: boolean }).ready, true);
      const webMcpList = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "webmcp",
          arguments: { action: "list" },
        },
      }));
      assert.equal((webMcpList.tools as Array<{ name?: string }>)[0]?.name, "fixture_webmcp");
      const webMcp = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "webmcp",
          arguments: { action: "execute", tool_name: "fixture_webmcp", input: { value: 2 } },
        },
      }));
      assert.equal((webMcp.result as { ready?: boolean }).ready, true);
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "emulate",
          arguments: {
            color_scheme: "dark",
            cpu_throttling_rate: 2,
            user_agent: "MagiBrowserIntegration/1.0",
            geolocation: { latitude: 31.2304, longitude: 121.4737, accuracy: 5 },
            network_conditions: "fast 4g",
            extra_http_headers: { "x-magi-test": "enabled" },
          },
        },
      }));
      const emulatedState = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "evaluate",
          arguments: {
            function: "async () => ({ userAgent: navigator.userAgent, response: await (await fetch('/api/data?emulated=1')).json() })",
            wait_for_stable_dom: false,
          },
        },
      }));
      assert.equal((emulatedState.value as { userAgent?: string }).userAgent, "MagiBrowserIntegration/1.0");
      assert.equal(
        (emulatedState.value as { response?: { magiHeader?: string } }).response?.magiHeader,
        "enabled",
      );
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "emulate",
          arguments: { color_scheme: "auto", extra_http_headers: null },
        },
      }));
      const resetEmulation = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "evaluate",
          arguments: {
            function: "async () => ({ userAgent: navigator.userAgent, response: await (await fetch('/api/data?reset=1')).json() })",
            wait_for_stable_dom: false,
          },
        },
      }));
      assert.notEqual((resetEmulation.value as { userAgent?: string }).userAgent, "MagiBrowserIntegration/1.0");
      assert.equal(
        (resetEmulation.value as { response?: { magiHeader?: string | null } }).response?.magiHeader,
        null,
      );
      const performanceStart = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "performance",
          arguments: { action: "start", reload: false, auto_stop: false },
        },
      }));
      assert.equal(performanceStart.tracing, true);
      const tracePath = join(profilePath, "fixture-trace.json");
      const performanceStop = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "performance",
          arguments: { action: "stop", file_path: tracePath },
        },
      }));
      assert.equal(typeof performanceStop.event_count, "number");
      assert((await stat(tracePath)).size > 0);
      const performanceMetrics = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "performance", arguments: { action: "metrics" } },
      }));
      assert(Array.isArray(performanceMetrics.metrics));
      const performanceAnalysis = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "performance", arguments: { action: "analyze" } },
      }));
      assert(Array.isArray(performanceAnalysis.metrics));

      const lighthouseOutput = join(profilePath, "lighthouse");
      const lighthouse = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "lighthouse",
          arguments: { mode: "snapshot", device: "desktop", output_dir_path: lighthouseOutput },
        },
      }));
      assert.equal(lighthouse.mode, "snapshot");
      assert(Array.isArray(lighthouse.scores));
      assert.equal((await stat(join(lighthouseOutput, "report.json"))).isFile(), true);
      assert.equal((await stat(join(lighthouseOutput, "report.html"))).isFile(), true);
      const lighthouseNavigationOutput = join(profilePath, "lighthouse-navigation");
      const lighthouseNavigation = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "lighthouse",
          arguments: {
            mode: "navigation",
            device: "mobile",
            output_dir_path: lighthouseNavigationOutput,
          },
        },
      }));
      assert.equal(lighthouseNavigation.mode, "navigation");
      assert.equal(lighthouseNavigation.device, "mobile");
      assert.equal((await stat(join(lighthouseNavigationOutput, "report.json"))).isFile(), true);

      const recordingPath = join(profilePath, "fixture-recording.mp4");
      const recordingStarted = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "recording",
          arguments: { action: "start", file_path: recordingPath },
        },
      }));
      assert.equal(recordingStarted.recording, true);
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "evaluate",
          arguments: {
            function: "() => { document.body.animate([{ opacity: 1 }, { opacity: 0.8 }, { opacity: 1 }], { duration: 800 }); return true; }",
            wait_for_stable_dom: false,
          },
        },
      }));
      await new Promise(resolve => setTimeout(resolve, 1_200));
      const recordingStopped = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "recording",
          arguments: { action: "stop" },
        },
      }));
      assert.equal(recordingStopped.file_path, recordingPath);
      assert(Number(recordingStopped.byte_length) > 0);
      assert((await stat(recordingPath)).size > 0);

      const heapUsage = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "usage" } },
      }));
      assert(heapUsage.heap && typeof heapUsage.heap === "object");
      const heapPath = join(profilePath, "fixture.heapsnapshot");
      const heapCapture = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "heap",
          arguments: { action: "take_snapshot", file_path: heapPath },
        },
      }));
      assert.equal(heapCapture.file_path, heapPath);
      assert((await stat(heapPath)).size > 0);
      const heapSummary = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "summary", file_path: heapPath } },
      }));
      const heapDetails = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "details", file_path: heapPath, page_size: 5 } },
      }));
      assert(Array.isArray(heapSummary.types));
      const heapAggregate = (heapDetails.aggregates as { items: Array<{ id: number }> }).items[0];
      assert(heapAggregate);
      const heapClassNodes = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "class_nodes", file_path: heapPath, class_id: heapAggregate.id, page_size: 2 } },
      }));
      const heapNode = (heapClassNodes.nodes as { items: Array<{ node_id: number }> }).items[0];
      assert(heapNode);
      for (const action of ["edges", "object_details", "retainers", "dominators", "retaining_paths"] as const) {
        const result = jsonValueFrom(await client.call({
          type: "devtools",
          payload: {
            tab_id: "tab-a",
            operation: "heap",
            arguments: { action, file_path: heapPath, node_id: heapNode.node_id },
          },
        }));
        assert(result.node || result.chain || result.paths, `heap ${action} must return analysis data`);
      }
      const duplicateStrings = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "duplicate_strings", file_path: heapPath, page_size: 5 } },
      }));
      assert(duplicateStrings.duplicate_strings);
      const secondHeapPath = join(profilePath, "fixture-second.heapsnapshot");
      await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "heap",
          arguments: { action: "take_snapshot", file_path: secondHeapPath },
        },
      });
      const heapComparison = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "heap",
          arguments: { action: "compare_snapshots", base_file_path: heapPath, current_file_path: secondHeapPath },
        },
      }));
      assert(Array.isArray(heapComparison.changes));
      const closedHeap = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "heap", arguments: { action: "close_snapshot", file_path: heapPath } },
      }));
      assert.equal(closedHeap.closed, true);

      const extensionInstall = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "extensions",
          arguments: { action: "install", path: extensionPath },
        },
      }));
      assert(extensionInstall.result !== undefined);
      const extensionList = jsonValueFrom(await client.call({
        type: "devtools",
        payload: { tab_id: "tab-a", operation: "extensions", arguments: { action: "list" } },
      }));
      const extensions = extensionList.extensions as Array<{ id: string; name?: string }>;
      const extension = extensions.find(candidate => candidate.name === "Magi Extension Fixture");
      assert(extension, `installed extension must be visible in the browser extension list: ${JSON.stringify(extensionList)}`);
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "extensions",
          arguments: { action: "reload", extension_id: extension.id },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "extensions",
          arguments: { action: "trigger_action", extension_id: extension.id },
        },
      }));
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "extensions",
          arguments: { action: "uninstall", extension_id: extension.id },
        },
      }));
      const manifestId = `http://127.0.0.1:${appAddress.port}/`;
      const pwaInstall = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "pwa",
          arguments: {
            action: "install",
            manifest_id: manifestId,
            install_url: manifestId,
            display_mode: "browser",
          },
        },
      }));
      assert.equal(pwaInstall.manifest_id, manifestId);
      const pwaState = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          operation: "pwa",
          arguments: { action: "state", manifest_id: manifestId },
        },
      }));
      assert.equal(pwaState.badgeCount, 0);
      assert(Array.isArray(pwaState.fileHandlers));
      const pwaLaunch = jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "pwa",
          arguments: { action: "launch", manifest_id: manifestId },
        },
      }));
      assert.equal(typeof pwaLaunch.targetId, "string");
      jsonValueFrom(await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          operation: "pwa",
          arguments: { action: "uninstall", manifest_id: manifestId },
        },
      }));
      const finalInteractionSnapshot = snapshotFrom(await client.call({
        type: "snapshot",
        payload: {
          tab_id: "tab-a",
          limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
          subtree_ref: null,
        },
      }));
      const finalUploadFile = finalInteractionSnapshot.root.children.find(node => node.name === "Upload file");
      const finalDownloadSample = finalInteractionSnapshot.root.children.find(node => node.name === "Download sample");
      const finalDeleteAccount = finalInteractionSnapshot.root.children.find(node => node.name === "Delete account");
      const finalIncrement = finalInteractionSnapshot.root.children.find(node => node.name === "Increment");
      assert(finalUploadFile);
      assert(finalDownloadSample);
      assert(finalDeleteAccount);
      assert(finalIncrement);
      await clickTestTarget(finalUploadFile, finalInteractionSnapshot.snapshot_revision);
      await client.waitForEvent("file_chooser");
      await clickTestTarget(finalDownloadSample, finalInteractionSnapshot.snapshot_revision);
      const completedDownload = await client.waitForEvent("download");
      assert.equal(completedDownload.event.type, "download");
      const finalDownload = completedDownload.event.type === "download"
        && completedDownload.event.payload.state === "completed"
        ? completedDownload
        : await client.waitForEvent("download");
      assert.equal(finalDownload.event.type, "download");
      if (finalDownload.event.type === "download") {
        assert.equal(finalDownload.event.payload.state, "completed");
        assert.equal(finalDownload.event.payload.byte_length, 21);
      }
      const downloadedFiles = await readdir(downloadPath);
      assert.equal(downloadedFiles.length, 1);
      assert.equal(
        await readFile(join(downloadPath, downloadedFiles[0]), "utf8"),
        "Magi browser download",
      );
      const blockedSensitiveClick = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: finalInteractionSnapshot.snapshot_revision,
            element_ref: finalDeleteAccount.element_ref,
          },
        },
      });
      assert.equal(blockedSensitiveClick.outcome.status, "failed");
      if (blockedSensitiveClick.outcome.status === "failed") {
        assert.equal(
          blockedSensitiveClick.outcome.payload.code,
          "browser_sensitive_action_requires_user",
        );
        assert.equal(blockedSensitiveClick.outcome.payload.side_effect_started, false);
      }
      const clickResponse = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: finalInteractionSnapshot.snapshot_revision,
            element_ref: finalIncrement.element_ref,
          },
        },
      });
      assert.equal(clickResponse.outcome.status, "succeeded");
      if (clickResponse.outcome.status === "succeeded") {
        assert.equal(clickResponse.outcome.payload.type, "page_state");
      }
      const afterClick = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        afterClick.root.children.some((node) => node.name === "Count 1"),
        "click should update the page",
      );
      const currentInput = afterClick.root.children.find(
        (node) => node.name === "Name",
      );
      const redirectLink = afterClick.root.children.find(
        (node) => node.name === "Redirect",
      );
      assert(currentInput);
      assert(redirectLink);
      const typeResponse = await client.call({
        type: "type",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterClick.snapshot_revision,
            element_ref: currentInput.element_ref,
          },
          text: "Magi",
          replace: true,
          submit_key: "Enter",
        },
      });
      assert.equal(typeResponse.outcome.status, "succeeded");
      if (typeResponse.outcome.status === "succeeded") {
        assert.equal(typeResponse.outcome.payload.type, "page_state");
      }
      const afterTypeSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-a",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const currentPassword = afterTypeSnapshot.root.children.find(
        (node) => node.name === "Password",
      );
      const currentRedirectLink = afterTypeSnapshot.root.children.find(
        (node) => node.name === "Redirect",
      );
      assert(
        afterTypeSnapshot.root.children.some((node) => node.name === "Submitted Magi"),
        "type should optionally submit with a key in the same Host command",
      );
      assert(currentPassword, "password ref should remain available after typing");
      assert(currentRedirectLink, "redirect ref should remain available after typing");
      const blockedSensitiveType = await client.call({
        type: "type",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterTypeSnapshot.snapshot_revision,
            element_ref: currentPassword.element_ref,
          },
          text: "should-not-be-sent",
          replace: true,
        },
      });
      assert.equal(blockedSensitiveType.outcome.status, "failed");
      if (blockedSensitiveType.outcome.status === "failed") {
        assert.equal(
          blockedSensitiveType.outcome.payload.code,
          "browser_sensitive_input_blocked",
        );
        assert.equal(blockedSensitiveType.outcome.payload.side_effect_started, false);
      }
      const redirectClick = await client.call({
        type: "click",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          target: {
            snapshot_revision: afterTypeSnapshot.snapshot_revision,
            element_ref: currentRedirectLink.element_ref,
          },
        },
      });
      assert.equal(redirectClick.outcome.status, "succeeded");
      const screenshotResponse = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-a",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(screenshotResponse.outcome.status, "succeeded");
      const screenshot = await client.waitForBinary();
      assert.deepEqual(pngDimensions(screenshot), { width: 900, height: 700 });

      const crossOriginRedirect = await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: `http://127.0.0.1:${appAddress.port}/redirect`,
          },
        },
      });
      assert.equal(crossOriginRedirect.outcome.status, "succeeded");

      const blockedMetadataNavigation = await client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          navigation: {
            action: "url",
            url: "http://169.254.169.254/latest/meta-data",
          },
        },
      });
      assert.equal(blockedMetadataNavigation.outcome.status, "failed");
      if (blockedMetadataNavigation.outcome.status === "failed") {
        assert.equal(
          blockedMetadataNavigation.outcome.payload.code,
          "browser_navigation_target_blocked",
        );
        assert.equal(blockedMetadataNavigation.outcome.payload.side_effect_started, false);
      }

      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-a",
          format: "jpeg",
          quality: 70,
          max_width: 1_800,
          max_height: 1_400,
        },
      });
      const screencastFrame = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-a",
      );
      const screencastEvent = screencastFrame.event;
      assert.equal(screencastEvent.event.type, "screencast_frame");
      assert.equal(screencastEvent.event.payload.width, 900);
      assert.equal(screencastEvent.event.payload.height, 700);
      assert.equal(screencastEvent.event.payload.surface_width, 900);
      assert.equal(screencastEvent.event.payload.surface_height, 700);
      assert.equal(screencastEvent.event.payload.device_scale_factor_millis, 2_000);
      const frame = screencastFrame.binary;
      assert.equal(frame[0], 0xff);
      assert.equal(frame[1], 0xd8);
      assert.deepEqual(jpegDimensions(frame), { width: 1_800, height: 1_400 });
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-a" },
      });

      const popupPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-popup",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(popupPage.outcome.status, "succeeded");
      const popupSnapshot = snapshotFrom(await client.call({
        type: "snapshot",
        payload: {
          tab_id: "tab-popup",
          limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
          subtree_ref: null,
        },
      }));
      const popupTarget = popupSnapshot.root.children.find(node => node.name === "Open popup");
      assert(popupTarget, "popup target should exist");
      const popupClick = await clickTestTarget(popupTarget, popupSnapshot.snapshot_revision, "tab-popup");
      assert.equal(popupClick.url, `http://127.0.0.1:${appAddress.port}/popup`);
      assert.equal(popupClick.title, "Popup");
      const popupWait = await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-popup",
          operation: "wait_for",
          arguments: { url: "/popup", timeout_ms: 1_000 },
        },
      });
      assert.equal(popupWait.outcome.status, "succeeded");

      const waitTimeout = await client.call({
        type: "devtools",
        payload: {
          tab_id: "tab-popup",
          operation: "wait_for",
          arguments: { selector: "#does-not-exist", timeout_ms: 50 },
        },
      });
      assert.equal(waitTimeout.outcome.status, "failed");
      if (waitTimeout.outcome.status === "failed") {
        assert.equal(waitTimeout.outcome.payload.code, "browser_wait_timeout");
        assert.equal(waitTimeout.outcome.payload.diagnostic, null);
        assert.equal(waitTimeout.outcome.payload.side_effect_started, false);
      }

      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-clipboard",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      const clipboardTargetSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-clipboard",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      const clipboardInput = clipboardTargetSnapshot.root.children.find(
        (node) => node.name === "Name",
      );
      assert(clipboardInput?.bounds, "clipboard test input should remain visible");
      const clipboardPoint = {
        x: clipboardInput.bounds.x + clipboardInput.bounds.width / 2,
        y: clipboardInput.bounds.y + clipboardInput.bounds.height / 2,
      };
      await client.call({
        type: "update_control",
        payload: { fence: 2, mode: "user" },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "mouse_down",
            ...clipboardPoint,
            button: "left",
            click_count: 1,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "mouse_up",
            ...clipboardPoint,
            button: "left",
            click_count: 1,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: { type: "insert_text", text: "Magi" },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "a",
            code: "KeyA",
            key_code: 65,
            modifiers: 4,
          },
        },
      });
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_up",
            key: "a",
            code: "KeyA",
            key_code: 65,
            modifiers: 4,
          },
        },
      });
      const copyResponse = await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "c",
            code: "KeyC",
            key_code: 67,
            modifiers: 4,
          },
        },
      });
      assert.equal(copyResponse.outcome.status, "succeeded");
      if (copyResponse.outcome.status === "succeeded") {
        assert.equal(copyResponse.outcome.payload.type, "clipboard_text");
        if (copyResponse.outcome.payload.type === "clipboard_text") {
          assert.equal(copyResponse.outcome.payload.payload.operation, "copy");
          assert.equal(copyResponse.outcome.payload.payload.text, "Magi");
        }
      }
      const cutResponse = await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: {
            type: "key_down",
            key: "x",
            code: "KeyX",
            key_code: 88,
            modifiers: 4,
          },
        },
      });
      assert.equal(cutResponse.outcome.status, "succeeded");
      if (cutResponse.outcome.status === "succeeded") {
        assert.equal(cutResponse.outcome.payload.type, "clipboard_text");
        if (cutResponse.outcome.payload.type === "clipboard_text") {
          assert.equal(cutResponse.outcome.payload.payload.operation, "cut");
          assert.equal(cutResponse.outcome.payload.payload.text, "Magi");
        }
      }
      await client.call({
        type: "user_input",
        payload: {
          tab_id: "tab-clipboard",
          control: { mode: "user", fence: 2 },
          event: { type: "insert_text", text: "Pasted" },
        },
      });
      const afterClipboardSnapshot = snapshotFrom(
        await client.call({
          type: "snapshot",
          payload: {
            tab_id: "tab-clipboard",
            limits: { max_nodes: 400, max_text_bytes: 32 * 1024 },
            subtree_ref: null,
          },
        }),
      );
      assert(
        afterClipboardSnapshot.root.children.some(
          (node) => node.name === "Name" && node.value === "Pasted",
        ),
        "cut and paste should update the focused field",
      );
      const fenced = await client.call({
        type: "press",
        payload: {
          tab_id: "tab-a",
          control: { mode: "agent", lease_id: "lease-a", fence: 1 },
          key: "Enter",
        },
      });
      assert.equal(fenced.outcome.status, "failed");
      if (fenced.outcome.status === "failed") {
        assert.equal(fenced.outcome.payload.code, "browser_lease_fenced");
      }

      const scaledPage = await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-scaled-mobile",
          initial_url: `http://127.0.0.1:${appAddress.port}/`,
          viewport: {
            width: 600,
            height: 800,
            surface_width: 600,
            surface_height: 800,
            device_scale_factor_millis: 1_000,
            device_type: "mobile",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      assert.equal(scaledPage.outcome.status, "succeeded");
      const scaledScreenshot = await client.call({
        type: "screenshot",
        payload: {
          tab_id: "tab-scaled-mobile",
          target: null,
          full_page: false,
          format: "png",
        },
      });
      assert.equal(scaledScreenshot.outcome.status, "succeeded");
      assert.deepEqual(
        pngDimensions(await client.waitForBinary()),
        { width: 600, height: 800 },
      );
      const startScaledScreencast = await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-scaled-mobile",
          format: "jpeg",
          quality: 88,
          max_width: 7_680,
          max_height: 4_320,
        },
      });
      assert.equal(startScaledScreencast.outcome.status, "succeeded");
      const scaledFrame = await waitForScreencastFrame(
        client,
        (event) => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-scaled-mobile",
      );
      const scaledFrameEvent = scaledFrame.event;
      assert.equal(scaledFrameEvent.event.type, "screencast_frame");
      if (scaledFrameEvent.event.type === "screencast_frame") {
        assert.equal(scaledFrameEvent.event.payload.width, 600);
        assert.equal(scaledFrameEvent.event.payload.height, 800);
        assert.equal(scaledFrameEvent.event.payload.surface_width, 600);
        assert.equal(scaledFrameEvent.event.payload.surface_height, 800);
        assert.equal(
          scaledFrameEvent.event.payload.device_scale_factor_millis,
          2_000,
        );
      }
      assert.deepEqual(
        jpegDimensions(scaledFrame.binary),
        { width: 1_200, height: 1_600 },
      );
      await client.call({
        type: "stop_screencast",
        payload: { tab_id: "tab-scaled-mobile" },
      });
    } finally {
      client?.close();
      await hostServer?.close();
      await new Promise<void>((accept) => appServer.close(() => accept()));
      await rm(profilePath, { recursive: true, force: true });
    }
  },
);

test(
  "restored tabs occupy a Chromium slot only while materialized",
  { skip: !chromiumExecutable, timeout: 30_000 },
  async () => {
    const profilePath = await mkdtemp(join(tmpdir(), "magi-browser-host-restore-"));
    const token = "browser-host-restore-token-00000000000000";
    let hostServer: Awaited<ReturnType<typeof startBrowserHostServer>> | undefined;
    let client: HostTestClient | undefined;
    const viewport = {
      width: 900,
      height: 700,
      surface_width: 900,
      surface_height: 700,
      device_scale_factor_millis: 1_000,
      device_type: "desktop" as const,
    };

    try {
      hostServer = await startBrowserHostServer({
        profilePath,
        chromiumExecutable: chromiumExecutable!,
        runtimeVersion: "test-runtime",
        hostVersion: "0.1.0",
        playwrightVersion: "test-playwright",
        runtimeEpoch: 2,
        headless: true,
        deviceScaleFactor: 2,
        downloadPath: join(profilePath, "downloads"),
        maxActivePages: 1,
        maxTabs: 4,
        bindHost: "127.0.0.1",
        port: 0,
        authToken: token,
      });
      client = new HostTestClient(
        `ws://127.0.0.1:${hostServer.port}/control`,
        token,
      );
      await client.open();
      await client.waitForEvent("ready");

      const firstActivation = await client.call({
        type: "restore_page",
        payload: {
          tab_id: "restored-a",
          initial_url: "about:blank",
          viewport,
          navigation_revision: 0,
          snapshot_revision: 0,
          allow_streaming_eviction: true,
        },
      });
      assert.equal(firstActivation.outcome.status, "succeeded");
      const screencast = await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "restored-a",
          format: "jpeg",
          quality: 80,
          max_width: 900,
          max_height: 700,
        },
      });
      assert.equal(screencast.outcome.status, "succeeded");
      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "user" },
      });
      const hiddenCursor = await client.waitForEvent("agent_cursor");
      assert.equal(hiddenCursor.event.type, "agent_cursor");
      if (hiddenCursor.event.type === "agent_cursor") {
        assert.equal(hiddenCursor.event.payload.tab_id, "restored-a");
        assert.equal(hiddenCursor.event.payload.visible, false);
        assert.equal(hiddenCursor.event.payload.x, null);
        assert.equal(hiddenCursor.event.payload.y, null);
        assert.equal(hiddenCursor.event.payload.action, null);
      }
      await client.call({
        type: "update_control",
        payload: { fence: 2, mode: "agent" },
      });
      const restoredAgentCursor = await waitForAgentCursor(client, "restored-a");
      assert.equal(restoredAgentCursor.event.type, "agent_cursor");
      if (restoredAgentCursor.event.type === "agent_cursor") {
        assert.equal(restoredAgentCursor.event.payload.visible, true);
        assert.equal(restoredAgentCursor.event.payload.action, "move");
        assert.equal(restoredAgentCursor.event.payload.x, 0.5);
        assert.equal(restoredAgentCursor.event.payload.y, 0.5);
      }

      const secondActivation = await client.call({
        type: "restore_page",
        payload: {
          tab_id: "restored-b",
          initial_url: "about:blank",
          viewport,
          navigation_revision: 0,
          snapshot_revision: 0,
          allow_streaming_eviction: true,
        },
      });
      assert.equal(secondActivation.outcome.status, "succeeded");
      const suspended = await client.waitForEvent("page_suspended");
      assert.equal(suspended.event.type, "page_suspended");
      if (suspended.event.type === "page_suspended") {
        assert.equal(suspended.event.payload.tab_id, "restored-a");
      }

      const thirdActivation = await client.call({
        type: "restore_page",
        payload: {
          tab_id: "restored-a",
          initial_url: "about:blank",
          viewport,
          navigation_revision: 1,
          snapshot_revision: 1,
          allow_streaming_eviction: true,
        },
      });
      assert.equal(thirdActivation.outcome.status, "succeeded");
      const suspendedAgain = await client.waitForEvent("page_suspended");
      assert.equal(suspendedAgain.event.type, "page_suspended");
      if (suspendedAgain.event.type === "page_suspended") {
        assert.equal(suspendedAgain.event.payload.tab_id, "restored-b");
      }
    } finally {
      client?.close();
      await hostServer?.close();
      await rm(profilePath, { recursive: true, force: true });
    }
  },
);

test(
  "navigation keeps the last presented frame until the new document is stable",
  { skip: !chromiumExecutable, timeout: 30_000 },
  async () => {
    const profilePath = await mkdtemp(join(tmpdir(), "magi-browser-host-presentation-"));
    const token = "browser-host-presentation-token-00000000";
    const appServer = createServer((request, response) => {
      if (request.url === "/slow") {
        setTimeout(() => {
          response
            .writeHead(200, { "content-type": "text/html; charset=utf-8" })
            .end(`<!doctype html><html><head><title>Stable new page</title></head>
              <body style="margin:0;background:#168030;color:white"><h1>New document</h1></body></html>`);
        }, 1_200);
        return;
      }
      response
        .writeHead(200, { "content-type": "text/html; charset=utf-8" })
        .end(`<!doctype html><html><head><title>Stable old page</title></head>
          <body style="margin:0;background:#0e50a0;color:white"><h1>Old document</h1></body></html>`);
    });
    let hostServer: Awaited<ReturnType<typeof startBrowserHostServer>> | undefined;
    let client: HostTestClient | undefined;
    try {
      await new Promise<void>((accept, reject) => {
        appServer.once("error", reject);
        appServer.listen(0, "127.0.0.1", () => accept());
      });
      const address = appServer.address();
      assert(address && typeof address !== "string");
      hostServer = await startBrowserHostServer({
        profilePath,
        chromiumExecutable: chromiumExecutable!,
        runtimeVersion: "test-runtime",
        hostVersion: "0.1.0",
        playwrightVersion: "test-playwright",
        runtimeEpoch: 3,
        headless: true,
        deviceScaleFactor: 2,
        downloadPath: join(profilePath, "downloads"),
        maxActivePages: 2,
        maxTabs: 2,
        bindHost: "127.0.0.1",
        port: 0,
        authToken: token,
      });
      client = new HostTestClient(
        `ws://127.0.0.1:${hostServer.port}/control`,
        token,
      );
      await client.open();
      await client.waitForEvent("ready");
      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });
      await client.call({
        type: "create_page",
        payload: {
          tab_id: "tab-presentation",
          initial_url: `http://127.0.0.1:${address.port}/`,
          viewport: {
            width: 900,
            height: 700,
            surface_width: 900,
            surface_height: 700,
            device_scale_factor_millis: 1_000,
            device_type: "desktop",
          },
          navigation_revision: 0,
          snapshot_revision: 0,
        },
      });
      await client.call({
        type: "start_screencast",
        payload: {
          tab_id: "tab-presentation",
          format: "jpeg",
          quality: 85,
          max_width: 1_800,
          max_height: 1_400,
        },
      });
      const initialFrame = await waitForScreencastFrame(
        client,
        event => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-presentation",
      );
      assert.equal(initialFrame.event.event.type, "screencast_frame");
      await new Promise(resolve => setTimeout(resolve, 200));
      client.takeBufferedScreencastFrameCount();

      const navigation = client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-presentation",
          control: { mode: "agent", lease_id: "lease-presentation", fence: 1 },
          navigation: {
            action: "url",
            url: `http://127.0.0.1:${address.port}/slow`,
          },
        },
      });
      await new Promise(resolve => setTimeout(resolve, 500));
      assert.equal(
        client.takeBufferedScreencastFrameCount(),
        0,
        "navigation must not publish Chromium transition frames",
      );
      const navigationResult = await navigation;
      assert.equal(navigationResult.outcome.status, "succeeded");
      const committedFrame = await waitForScreencastFrame(
        client,
        event => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-presentation"
          && event.event.payload.navigation_revision > 0,
      );
      assert.equal(committedFrame.event.event.type, "screencast_frame");

      await client.call({
        type: "update_control",
        payload: { fence: 1, mode: "agent" },
      });
      const cursor = await waitForAgentCursor(client, "tab-presentation");
      assert.equal(cursor.event.type, "agent_cursor");
      if (cursor.event.type === "agent_cursor") {
        assert.equal(cursor.event.payload.visible, true);
      }

      await new Promise(resolve => setTimeout(resolve, 200));
      client.takeBufferedScreencastFrameCount();
      const reload = client.call({
        type: "navigate",
        payload: {
          tab_id: "tab-presentation",
          control: { mode: "agent", lease_id: "lease-presentation", fence: 1 },
          navigation: { action: "reload" },
        },
      });
      await new Promise(resolve => setTimeout(resolve, 250));
      assert.equal(
        client.takeBufferedScreencastFrameCount(),
        0,
        "reload must keep the last presented frame until the document is stable",
      );
      const reloadResult = await reload;
      assert.equal(reloadResult.outcome.status, "succeeded");
      const reloadedFrame = await waitForScreencastFrame(
        client,
        event => event.event.type === "screencast_frame"
          && event.event.payload.tab_id === "tab-presentation"
          && event.event.payload.navigation_revision > 1,
      );
      assert.equal(reloadedFrame.event.event.type, "screencast_frame");
    } finally {
      client?.close();
      await hostServer?.close();
      await new Promise<void>((accept) => appServer.close(() => accept()));
      await rm(profilePath, { recursive: true, force: true });
    }
  },
);

async function waitForScreencastFrame(
  client: HostTestClient,
  matches: (event: EventEnvelope) => boolean,
): Promise<{ event: EventEnvelope; binary: Buffer }> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const event = await client.waitForEvent("screencast_frame");
    const binary = await client.waitForBinary();
    if (matches(event)) return { event, binary };
  }
  throw new Error("Host did not produce the expected screencast frame");
}

async function waitForAgentCursor(
  client: HostTestClient,
  tabId: string,
  action?: "move" | "click" | "drag" | "type" | "scroll",
): Promise<EventEnvelope> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const event = await client.waitForEvent("agent_cursor");
    if (
      event.event.type === "agent_cursor"
      && event.event.payload.tab_id === tabId
      && (!action || event.event.payload.action === action)
    ) {
      return event;
    }
  }
  throw new Error(`Host did not produce an agent cursor event for ${tabId}`);
}

function snapshotFrom(response: ResponseEnvelope): HostSnapshot {
  assert.equal(
    response.outcome.status,
    "succeeded",
    JSON.stringify(response.outcome),
  );
  if (response.outcome.status !== "succeeded") {
    throw new Error("snapshot command failed");
  }
  assert.equal(response.outcome.payload.type, "snapshot");
  if (response.outcome.payload.type !== "snapshot") {
    throw new Error("snapshot response has the wrong type");
  }
  return response.outcome.payload.payload;
}

class HostTestClient {
  readonly #websocket: WebSocket;
  readonly #responses = new Map<string, (response: ResponseEnvelope) => void>();
  readonly #events: EventEnvelope[] = [];
  readonly #eventWaiters = new Map<
    HostEventType,
    Array<(event: EventEnvelope) => void>
  >();
  readonly #binary: Buffer[] = [];
  readonly #binaryWaiters: Array<(payload: Buffer) => void> = [];
  #sequence = 0;

  constructor(url: string, token: string) {
    this.#websocket = new WebSocket(url, {
      headers: { authorization: `Bearer ${token}` },
    });
    this.#websocket.on("message", (data, isBinary) => {
      if (isBinary) {
        const payload = Buffer.from(data as Buffer);
        const waiter = this.#binaryWaiters.shift();
        if (waiter) waiter(payload);
        else this.#binary.push(payload);
        return;
      }
      const value = JSON.parse(data.toString()) as
        | ResponseEnvelope
        | EventEnvelope;
      if ("request_id" in value) {
        this.#responses.get(value.request_id)?.(value);
        this.#responses.delete(value.request_id);
      } else {
        const waiters = this.#eventWaiters.get(value.event.type);
        const waiter = waiters?.shift();
        if (waiter) waiter(value);
        else this.#events.push(value);
      }
    });
  }

  open(): Promise<void> {
    return new Promise((accept, reject) => {
      if (this.#websocket.readyState === WebSocket.OPEN) {
        accept();
        return;
      }
      this.#websocket.once("open", () => accept());
      this.#websocket.once("error", reject);
    });
  }

  call(command: RequestEnvelope["command"]): Promise<ResponseEnvelope> {
    const requestId = `request-${++this.#sequence}`;
    const commandLabel = command.type === "devtools"
      ? `${command.type}:${command.payload.operation}`
      : command.type;
    const request: RequestEnvelope = {
      request_id: requestId,
      protocol_version: PROTOCOL_VERSION,
      command,
    };
    const responseTimeout = command.type === "devtools"
      && (command.payload.operation === "lighthouse" || command.payload.operation === "heap")
      ? LONG_RUNNING_COMMAND_RESPONSE_TIMEOUT_MILLIS
      : HOST_COMMAND_RESPONSE_TIMEOUT_MILLIS;
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => {
          this.#responses.delete(requestId);
          reject(new Error(`Host response timed out: ${requestId} (${commandLabel})`));
        },
        responseTimeout,
      );
      this.#responses.set(requestId, (response) => {
        clearTimeout(timer);
        accept(response);
      });
      this.#websocket.send(JSON.stringify(request));
    });
  }

  waitForEvent(type: HostEventType): Promise<EventEnvelope> {
    const existingIndex = this.#events.findIndex((event) => event.event.type === type);
    if (existingIndex >= 0) {
      return Promise.resolve(this.#events.splice(existingIndex, 1)[0]);
    }
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`Host event timed out: ${type}`)),
        10_000,
      );
      const waiters = this.#eventWaiters.get(type) ?? [];
      waiters.push((event) => {
        clearTimeout(timer);
        accept(event);
      });
      this.#eventWaiters.set(type, waiters);
    });
  }

  waitForBinary(): Promise<Buffer> {
    const existing = this.#binary.shift();
    if (existing) return Promise.resolve(existing);
    return new Promise((accept, reject) => {
      const timer = setTimeout(
        () => reject(new Error("Host binary payload timed out")),
        10_000,
      );
      this.#binaryWaiters.push((payload) => {
        clearTimeout(timer);
        accept(payload);
      });
    });
  }

  takeBufferedScreencastFrameCount(): number {
    let count = 0;
    for (let index = this.#events.length - 1; index >= 0; index -= 1) {
      if (this.#events[index]?.event.type !== "screencast_frame") continue;
      this.#events.splice(index, 1);
      count += 1;
    }
    for (let index = 0; index < count; index += 1) this.#binary.shift();
    return count;
  }

  close(): void {
    this.#websocket.terminate();
  }
}

type HostEventType = EventEnvelope["event"]["type"];
