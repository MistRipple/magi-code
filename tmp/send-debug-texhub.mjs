import { chromium } from 'playwright';

const url = 'http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/TexHub_TEST&workspaceId=L1VzZXJzL3hpZS9jb2RlL1RleEh1Yl9URVNU';

async function main() {
  const browser = await chromium.launch({ headless: false, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });

  const requests = [];
  page.on('request', (req) => {
    if (req.url().includes('/api/task/execute')) {
      requests.push({ method: req.method(), url: req.url(), time: Date.now() });
      console.log('[request]', req.method(), req.url());
    }
  });
  page.on('console', (msg) => console.log('[console]', msg.type(), msg.text()));
  page.on('pageerror', (err) => console.log('[pageerror]', err.message));

  await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(5000);

  await page.evaluate(() => {
    document.querySelector('button[title="新建会话"]')?.click();
  });
  await page.waitForTimeout(2500);

  const before = await page.evaluate(() => ({
    sessionId: new URL(location.href).searchParams.get('sessionId'),
    hasTextarea: !!document.querySelector('textarea'),
    textareaValue: document.querySelector('textarea')?.value || '',
    hasSendBtn: !!document.querySelector('[data-testid="input-send-button"]'),
    sendDisabled: document.querySelector('[data-testid="input-send-button"]')?.hasAttribute('disabled') || false,
    main: (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 200),
  }));
  console.log('BEFORE', JSON.stringify(before, null, 2));

  const textarea = page.locator('textarea').first();
  await textarea.fill('你好，请简短回答：1加1等于几？');
  await page.waitForTimeout(500);

  const afterFill = await page.evaluate(() => ({
    textareaValue: document.querySelector('textarea')?.value || '',
    hasSendBtn: !!document.querySelector('[data-testid="input-send-button"]'),
    sendDisabled: document.querySelector('[data-testid="input-send-button"]')?.hasAttribute('disabled') || false,
    sendText: document.querySelector('[data-testid="input-send-button"]')?.textContent || '',
  }));
  console.log('AFTER_FILL', JSON.stringify(afterFill, null, 2));

  if (afterFill.hasSendBtn) {
    await page.locator('[data-testid="input-send-button"]').click();
    console.log('CLICKED_SEND');
  } else {
    console.log('NO_SEND_BUTTON');
  }

  await page.waitForTimeout(8000);
  const afterSend = await page.evaluate(() => ({
    sessionId: new URL(location.href).searchParams.get('sessionId'),
    main: (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 800),
    active: Array.from(document.querySelectorAll('.session-item.active')).map((el) => (el.textContent || '').replace(/\s+/g, ' ').trim()),
    textareaValue: document.querySelector('textarea')?.value || '',
    hasStop: Array.from(document.querySelectorAll('button')).some((b) => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止'))),
  }));
  console.log('AFTER_SEND', JSON.stringify(afterSend, null, 2));
  console.log('EXECUTE_REQUEST_COUNT', requests.length);

  await page.screenshot({ path: '/tmp/magi-e2e/30-send-debug.png' });
  await browser.close();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});

