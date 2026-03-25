import { chromium } from 'playwright';

const WEB_URL = 'http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/TexHub_TEST&workspaceId=L1VzZXJzL3hpZS9jb2RlL1RleEh1Yl9URVNU';

async function main() {
  const browser = await chromium.launch({ headless: false, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  let navigations = 0;
  page.on('framenavigated', (frame) => {
    if (frame === page.mainFrame()) {
      navigations += 1;
      console.log('[navigated]', page.url());
    }
  });

  await page.goto(WEB_URL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(5000);

  await page.evaluate(() => document.querySelector('button[title="新建会话"]')?.click());
  await page.waitForTimeout(2500);

  const before = await page.evaluate(() => ({
    timeOrigin: performance.timeOrigin,
    href: location.href,
    sessionId: new URL(location.href).searchParams.get('sessionId'),
    main: (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 200),
  }));
  console.log('BEFORE', JSON.stringify(before, null, 2));

  const textarea = page.locator('textarea').first();
  await textarea.fill('你好，请简短回答：1加1等于几？');
  await page.waitForTimeout(400);
  await page.locator('[data-testid="input-send-button"]').click();

  await page.waitForTimeout(10000);

  const after = await page.evaluate(() => ({
    timeOrigin: performance.timeOrigin,
    href: location.href,
    sessionId: new URL(location.href).searchParams.get('sessionId'),
    main: (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 800),
    active: Array.from(document.querySelectorAll('.session-item.active')).map((el) => (el.textContent || '').replace(/\s+/g, ' ').trim()),
  }));
  console.log('AFTER', JSON.stringify(after, null, 2));
  console.log('NAVIGATIONS', navigations);

  const reloaded = after.timeOrigin !== before.timeOrigin;
  const passed = !reloaded && after.main.includes('1加1') && after.active.length > 0;
  console.log('RELOADED', reloaded);
  console.log('PASS', passed);

  await page.screenshot({ path: '/tmp/magi-e2e/40-no-reload-after-send.png' });
  await browser.close();
  process.exit(passed ? 0 : 1);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});

