import { chromium } from 'playwright';

const WEB_URL = 'http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/TexHub_TEST&workspaceId=L1VzZXJzL3hpZS9jb2RlL1RleEh1Yl9URVNU';

function pass(ok, msg) {
  console.log(`${ok ? '✅' : '❌'} ${msg}`);
  return ok;
}

async function main() {
  const browser = await chromium.launch({ headless: false, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  const executeRequests = [];
  const errors = [];

  page.on('request', (req) => {
    if (req.url().includes('/api/task/execute')) {
      executeRequests.push({ url: req.url(), method: req.method(), at: Date.now() });
      console.log('[request]', req.method(), req.url());
    }
  });
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (err) => errors.push(err.message));

  await page.goto(WEB_URL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(5000);

  // 新建会话
  await page.evaluate(() => document.querySelector('button[title="新建会话"]')?.click());
  await page.waitForTimeout(2500);
  await page.screenshot({ path: '/tmp/magi-e2e/31-stream-new-session.png' });

  // 尝试切到“深度”模式，提高出现 thinking 的概率
  await page.evaluate(() => {
    const btns = Array.from(document.querySelectorAll('button'));
    const deep = btns.find((b) => (b.textContent || '').trim() === '深度');
    deep?.click();
  });
  await page.waitForTimeout(800);

  const prompt = '请用中文写一篇较长的 TypeScript 泛型教程，至少包含以下 Markdown 结构：\\n# 标题\\n## 基本概念\\n## 函数泛型\\n## 接口与类泛型\\n## 约束与默认类型\\n每节都给出代码块和解释。为了便于我观察流式过程，请逐步输出，不要一次性极短结束。';

  const textarea = page.locator('textarea').first();
  await textarea.fill(prompt);
  await page.waitForTimeout(500);
  await page.locator('[data-testid="input-send-button"]').click();
  await page.screenshot({ path: '/tmp/magi-e2e/32-stream-sent.png' });

  // 连续采样流式过程
  const samples = [];
  let stopped = false;
  for (let i = 0; i < 18; i++) {
    await page.waitForTimeout(1500);
    const snap = await page.evaluate(() => {
      const main = document.querySelector('main');
      const text = (main?.textContent || '').replace(/\s+/g, ' ').trim();
      const thinkingText = text.includes('思考过程');
      const thinkingNodes = document.querySelectorAll('[class*="thinking"], [class*="Thinking"], details, [class*="thought"]');
      const codeBlocks = document.querySelectorAll('pre, code');
      const headings = document.querySelectorAll('h1,h2,h3,h4');
      const stopBtn = Array.from(document.querySelectorAll('button')).find((b) => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止')));
      return {
        len: text.length,
        hasThinking: thinkingText || thinkingNodes.length > 0,
        thinkingCount: thinkingNodes.length,
        codeBlockCount: codeBlocks.length,
        headingCount: headings.length,
        hasStop: !!stopBtn,
        textTail: text.slice(-220),
      };
    });
    samples.push(snap);

    // 出现 stop 且已有内容增长后，点一次 stop
    if (!stopped && i >= 3 && snap.hasStop && snap.len > 250) {
      await page.evaluate(() => {
        const stopBtn = Array.from(document.querySelectorAll('button')).find((b) => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止')));
        stopBtn?.click();
      });
      stopped = true;
      await page.waitForTimeout(2500);
      await page.screenshot({ path: '/tmp/magi-e2e/33-after-stop-click.png' });
    }
  }

  console.log('\n=== SAMPLES ===');
  console.log(JSON.stringify(samples, null, 2));

  const finalState = await page.evaluate(() => {
    const main = document.querySelector('main');
    const text = (main?.textContent || '').replace(/\s+/g, ' ').trim();
    const stopBtn = Array.from(document.querySelectorAll('button')).find((b) => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止')));
    return {
      sessionId: new URL(location.href).searchParams.get('sessionId'),
      mainLen: text.length,
      hasThinkingText: text.includes('思考过程'),
      codeBlockCount: document.querySelectorAll('pre, code').length,
      headingCount: document.querySelectorAll('h1,h2,h3,h4').length,
      hasBasicConcept: text.includes('基本概念'),
      hasFunctionGeneric: text.includes('函数泛型'),
      hasConstraint: text.includes('约束') || text.includes('默认类型'),
      hasStop: !!stopBtn,
      activeSession: Array.from(document.querySelectorAll('.session-item.active')).map((el) => (el.textContent || '').replace(/\s+/g, ' ').trim()),
      text: text.slice(0, 1400),
    };
  });

  console.log('\n=== FINAL ===');
  console.log(JSON.stringify(finalState, null, 2));
  console.log('\n=== EXECUTE REQUESTS ===');
  console.log(JSON.stringify(executeRequests, null, 2));
  console.log('\n=== ERRORS ===');
  console.log(JSON.stringify(errors.slice(0, 10), null, 2));

  const lengths = samples.map((s) => s.len);
  const hasProgressiveGrowth = lengths.some((v, idx) => idx > 0 && v > lengths[idx - 1]);
  const hasThinking = samples.some((s) => s.hasThinking) || finalState.hasThinkingText;
  const hasStreamingMarkdown = samples.some((s) => s.codeBlockCount > 0 || s.headingCount > 0);
  const stopContentPreserved = stopped ? finalState.mainLen > 220 : true;
  const stopEventuallyGone = stopped ? !finalState.hasStop : true;

  let passed = true;
  passed &= pass(executeRequests.length >= 1, '发送时至少发出一次 /api/task/execute');
  passed &= pass(hasProgressiveGrowth, '流式过程中主内容长度有逐步增长');
  passed &= pass(hasThinking, '存在 thinking 独立展示或思考过程标识');
  passed &= pass(hasStreamingMarkdown, '流式过程中已开始渲染 Markdown 结构');
  passed &= pass(finalState.codeBlockCount > 0 || finalState.headingCount > 0, '终态包含 Markdown 标题或代码块');
  passed &= pass(stopContentPreserved, '停止后已输出内容被保留');
  passed &= pass(stopEventuallyGone, '停止后停止按钮消失');
  passed &= pass(finalState.activeSession.length > 0, '当前会话在 Sidebar 中正确激活');

  await page.screenshot({ path: '/tmp/magi-e2e/34-stream-final.png' });
  await browser.close();
  process.exit(passed ? 0 : 1);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});

