import { chromium } from 'playwright';

const WEB_URL = 'http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/TexHub_TEST&workspaceId=L1VzZXJzL3hpZS9jb2RlL1RleEh1Yl9URVNU';

function ok(v, msg) {
  console.log(`${v ? '✅' : '❌'} ${msg}`);
  return v;
}

async function main() {
  const browser = await chromium.launch({ headless: false, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  await page.goto(WEB_URL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(5000);

  await page.evaluate(() => document.querySelector('button[title="新建会话"]')?.click());
  await page.waitForTimeout(2500);

  const textarea = page.locator('textarea').first();
  await textarea.fill('请用 Markdown 输出一段稍长说明：先用二级标题“基本概念”，再用列表列出 3 点，然后给一个 TypeScript 代码块，最后再写一段总结。');
  await page.waitForTimeout(400);
  await page.locator('[data-testid="input-send-button"]').click();

  const samples = [];
  for (let i = 0; i < 10; i++) {
    await page.waitForTimeout(1500);
    const snap = await page.evaluate(() => {
      const text = (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim();
      return {
        len: text.length,
        headingCount: document.querySelectorAll('h1,h2,h3,h4').length,
        codeCount: document.querySelectorAll('pre, code').length,
        hasStop: Array.from(document.querySelectorAll('button')).some((b) => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止'))),
        tail: text.slice(-180),
      };
    });
    samples.push(snap);
  }

  const finalState = await page.evaluate(() => {
    const text = (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim();
    return {
      len: text.length,
      headingCount: document.querySelectorAll('h1,h2,h3,h4').length,
      codeCount: document.querySelectorAll('pre, code').length,
      hasBasicConcept: text.includes('基本概念'),
      hasSummary: text.includes('总结'),
      text: text.slice(0, 1000),
    };
  });

  console.log('\nSAMPLES');
  console.log(JSON.stringify(samples, null, 2));
  console.log('\nFINAL');
  console.log(JSON.stringify(finalState, null, 2));

  const hasGrowth = samples.some((s, i) => i > 0 && s.len > samples[i - 1].len);
  const hasStreamingMarkdown = samples.some((s) => s.headingCount > 0 || s.codeCount > 0);

  let passed = true;
  passed &= ok(hasGrowth, '文本存在逐步增长，符合流式渲染');
  passed &= ok(hasStreamingMarkdown, '流式过程中已出现 Markdown 结构');
  passed &= ok(finalState.headingCount > 0 || finalState.codeCount > 0, '最终保留 Markdown 结构');
  passed &= ok(finalState.hasBasicConcept, '最终包含目标标题内容');

  await page.screenshot({ path: '/tmp/magi-e2e/41-markdown-stream.png' });
  await browser.close();
  process.exit(passed ? 0 : 1);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});

