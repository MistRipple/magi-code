#!/usr/bin/env node
/**
 * URL 模式配置回归
 *
 * 目标：
 * 1) 旧版 endpointUrl 配置会迁移为 baseUrl + urlMode=full，并回写到配置文件
 * 2) standard/full 两种 URL 模式遵循统一的路径解析规则
 */

const fs = require('fs');
const os = require('os');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function ensureCompiled(file) {
  if (!fs.existsSync(file)) {
    throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run -s compile`);
  }
}

function withTempHome() {
  const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-url-mode-'));
  const previous = {
    HOME: process.env.HOME,
    USERPROFILE: process.env.USERPROFILE,
  };
  process.env.HOME = tempHome;
  process.env.USERPROFILE = tempHome;
  return {
    tempHome,
    restore() {
      process.env.HOME = previous.HOME;
      process.env.USERPROFILE = previous.USERPROFILE;
    },
  };
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

async function main() {
  const configPath = path.join(OUT, 'llm', 'config.js');
  const urlModePath = path.join(OUT, 'llm', 'url-mode.js');
  for (const file of [configPath, urlModePath]) {
    ensureCompiled(file);
  }

  const homeScope = withTempHome();
  try {
    const configDir = path.join(homeScope.tempHome, '.magi');
    fs.mkdirSync(configDir, { recursive: true });
    const llmConfigPath = path.join(configDir, 'llm.json');
    fs.writeFileSync(llmConfigPath, JSON.stringify({
      orchestrator: {
        endpointUrl: 'https://proxy.example.com/v3/chat',
        apiKey: 'orch-key',
        model: 'proxy-model',
        provider: 'openai',
        enabled: true,
      },
      workers: {
        claude: {
          endpointUrl: 'https://anthropic-proxy.example.com/custom/messages',
          apiKey: 'worker-key',
          model: 'claude-custom',
          provider: 'anthropic',
          enabled: true,
        },
      },
      auxiliary: {
        endpointUrl: 'https://proxy.example.com/v9/aux',
        apiKey: 'aux-key',
        model: 'aux-model',
        provider: 'openai',
        enabled: true,
      },
    }, null, 2));

    const { LLMConfigLoader } = require(configPath);
    const {
      resolveModelsBaseUrl,
      resolveSdkBaseUrl,
    } = require(urlModePath);

    const fullConfig = LLMConfigLoader.loadFullConfig();
    const persisted = readJson(llmConfigPath);

    assert(fullConfig.orchestrator.baseUrl === 'https://proxy.example.com/v3/chat', 'orchestrator baseUrl 未迁移为旧 endpointUrl');
    assert(fullConfig.orchestrator.urlMode === 'full', 'orchestrator urlMode 未迁移为 full');
    assert(fullConfig.workers.claude.baseUrl === 'https://anthropic-proxy.example.com/custom/messages', 'worker baseUrl 未迁移为旧 endpointUrl');
    assert(fullConfig.workers.claude.urlMode === 'full', 'worker urlMode 未迁移为 full');
    assert(fullConfig.auxiliary.baseUrl === 'https://proxy.example.com/v9/aux', 'auxiliary baseUrl 未迁移为旧 endpointUrl');
    assert(fullConfig.auxiliary.urlMode === 'full', 'auxiliary urlMode 未迁移为 full');

    assert(!('endpointUrl' in persisted.orchestrator), 'orchestrator 持久化文件仍残留 endpointUrl');
    assert(!('endpointUrl' in persisted.workers.claude), 'worker 持久化文件仍残留 endpointUrl');
    assert(!('endpointUrl' in persisted.auxiliary), 'auxiliary 持久化文件仍残留 endpointUrl');
    assert(persisted.orchestrator.urlMode === 'full', '持久化 orchestrator urlMode 未回写为 full');
    assert(persisted.workers.claude.urlMode === 'full', '持久化 worker urlMode 未回写为 full');
    assert(persisted.auxiliary.urlMode === 'full', '持久化 auxiliary urlMode 未回写为 full');

    assert(
      resolveSdkBaseUrl('openai', 'https://api.openai.com', 'standard') === 'https://api.openai.com/v1',
      'OpenAI standard 模式应自动补 /v1',
    );
    assert(
      resolveSdkBaseUrl('openai', 'https://proxy.example.com/v3/chat', 'full') === 'https://proxy.example.com/v3/chat',
      'OpenAI full 模式应原样透传完整路径',
    );
    assert(
      resolveSdkBaseUrl('anthropic', 'https://api.anthropic.com/v1', 'standard') === 'https://api.anthropic.com',
      'Anthropic standard 模式应剥离多余 /v1',
    );
    assert(
      resolveModelsBaseUrl('anthropic', 'https://api.anthropic.com/v1', 'standard') === 'https://api.anthropic.com/v1',
      'Anthropic models 查询应使用 /v1 路径',
    );
    assert(
      resolveModelsBaseUrl('openai', 'https://proxy.example.com/v3/chat', 'full') === null,
      'Full 模式下不应自动派生模型列表地址',
    );

    console.log('\n=== url mode config regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'legacy_endpoint_url_migrates_to_base_url_with_full_mode',
        'standard_and_full_url_rules_are_applied_consistently',
      ],
    }, null, 2));
  } finally {
    homeScope.restore();
  }
}

main().catch((error) => {
  console.error('url mode config 回归失败:', error?.stack || error);
  process.exit(1);
});
