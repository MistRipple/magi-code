import { chromium } from 'playwright';

const WEB_URL = 'http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/TexHub_TEST&workspaceId=L1VzZXJzL3hpZS9jb2RlL1RleEh1Yl9URVNU';

function log(title, data) {
  console.log(`\n=== ${title} ===`);
  console.log(JSON.stringify(data, null, 2));
}

function ok(cond, msg) {
  console.log(`${cond ? '✅' : '❌'} ${msg}`);
  return cond;
}

async function main() {
  const browser = await chromium.launch({ headless: false, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });

  await page.goto(WEB_URL, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.waitForTimeout(5000);

  // 新建会话
  await page.evaluate(() => {
    document.querySelector('button[title="新建会话"]')?.click();
  });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: '/tmp/magi-e2e/20-thinking-new-session.png' });

  // 发送一个更可能触发 thinking + markdown + 较长流式的请求
  const prompt = '请用中文分四个部分详细解释 TypeScript 泛型：1）基本概念 2）函数泛型 3）接口与类泛型 4）约束与默认类型。每一部分给代码示例，并使用 Markdown 标题、列表和代码块格式输出。';
  await page.evaluate((text) => {
    const textarea = document.querySelector('textarea');
    const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value').set;
    setter.call(textarea, text);
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    document.querySelector('[data-testid="input-send-button"]')?.click();
  }, prompt);

  // 连续采样，验证 thinking、markdown、流式增长
  const samples = [];
  for (let i = 0; i < 12; i++) {
    await page.waitForTimeout(2000);
    const snap = await page.evaluate(() => {
      const main = document.querySelector('main');
      const text = (main?.textContent || '').replace(/\s+/g, ' ').trim();
      const html = main?.innerHTML || '';
      const thinkingNodes = document.querySelectorAll('[class*="thinking"], [class*="Thinking"], details, [class*="thought"]');
      const codeBlocks = document.querySelectorAll('pre code, pre');
      const headings = document.querySelectorAll('h1,h2,h3,h4');
      const stopBtn = Array.from(document.querySelectorAll('button')).find(b => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止')));
      return {
        mainLen: text.length,
        hasThinking: thinkingNodes.length > 0 || text.includes('思考过程'),
        thinkingCount: thinkingNodes.length,
        codeBlockCount: codeBlocks.length,
        headingCount: headings.length,
        hasStopButton: !!stopBtn,
        snippet: text.slice(-220),
        htmlTail: html.slice(-400),
      };
    });
    samples.push(snap);

    // 途中如果 stop 可见，就点击一次验证停止后保留内容
    if (i === 3 && snap.hasStopButton) {
      await page.evaluate(() => {
        const stopBtn = Array.from(document.querySelectorAll('button')).find(b => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止')));
        stopBtn?.click();
      });
      await page.waitForTimeout(3000);
      await page.screenshot({ path: '/tmp/magi-e2e/21-after-stop.png' });
    }
  }

  log('流式采样', samples);

  // 终态检查
  const finalState = await page.evaluate(() => {
    const main = document.querySelector('main');
    const text = (main?.textContent || '').replace(/\s+/g, ' ').trim();
    return {
      sessionId: new URL(location.href).searchParams.get('sessionId'),
      mainLen: text.length,
      hasThinkingLabel: text.includes('思考过程'),
      hasMarkdownHeading: /基本概念|函数泛型|接口与类泛型|约束与默认类型/.test(text),
      hasCodeBlock: document.querySelectorAll('pre, code').length > 0,
      stopGone: !Array.from(document.querySelectorAll('button')).some(b => ((b.textContent || '').trim() === '停止') || ((b.getAttribute('title') || '').includes('停止'))),
      contentPreservedAfterStop: text.length > 150,
      text: text.slice(0, 1200),
    };
  });
  log('终态', finalState);

  // 判定
  let passed = true;
  const lengths = samples.map(s => s.mainLen);
  const hasGrowth = lengths.some((v, idx) => idx > 0 && v > lengths[idx - 1]);
  passed &= ok(hasGrowth, '流式过程中主内容长度有逐步增长');
  passed &= ok(samples.some(s => s.hasThinking), '存在 thinking 独立区域或思考过程标识');
  passed &= ok(samples.some(s => s.codeBlockCount > 0 || s.headingCount > 0), '流式过程中 Markdown 结构已开始渲染');
  passed &= ok(finalState.hasMarkdownHeading, '最终内容包含 Markdown 标题结构');
  passed &= ok(finalState.hasCodeBlock, '最终内容包含代码块');
  passed &= ok(finalState.stopGone, '结束后停止按钮消失');
  passed &= ok(finalState.contentPreservedAfterStop, '停止/结束后已输出内容被保留');

  // 单模式渲染粗检：没有出现明显“双模式切换”文案/区域
  const dualModeEvidence = await page.evaluate(() => {
    const text = (document.querySelector('main')?.textContent || '').replace(/\s+/g, ' ').trim();
    return {
      hasDualModeText: text.includes('模式切换') || text.includes('回退渲染') || text.includes('兼容模式'),
    };
  });
  passed &= ok(!dualModeEvidence.hasDualModeText, '未发现双模式切换/兼容模式痕迹');

  await page.screenshot({ path: '/tmp/magi-e2e/22-thinking-final.png' });

  console.log('\n浏览器错误:', errors.slice(0, 10));
  console.log(`\n最终结果: ${passed ? '✅ PASS' : '❌ FAIL'}`);
  await browser.close();
  process.exit(passed ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

