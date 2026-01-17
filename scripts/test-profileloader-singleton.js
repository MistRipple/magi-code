#!/usr/bin/env node
/**
 * 测试 ProfileLoader 单例保护机制
 *
 * 验证项目：
 * 1. 创建第一个实例时不应该有警告
 * 2. 创建第二个实例时应该有警告
 * 3. 警告信息应该包含实例 ID 和堆栈跟踪
 */

const { ProfileLoader } = require('../out/orchestrator/profile/profile-loader');

console.log('\n🧪 ProfileLoader 单例保护机制测试\n');

let passed = 0;
let failed = 0;
let warnings = [];

// 拦截 console.warn
const originalWarn = console.warn;
console.warn = (...args) => {
  warnings.push(args.join(' '));
  originalWarn(...args);
};

console.log('======================================================================');
console.log('  测试 1: 创建第一个实例（不应该有警告）');
console.log('======================================================================\n');

warnings = [];
const loader1 = new ProfileLoader();

if (warnings.length === 0) {
  console.log('✅ 第一个实例创建成功，没有警告');
  passed++;
} else {
  console.log('❌ 第一个实例不应该有警告');
  console.log('   警告内容:', warnings);
  failed++;
}

console.log('\n======================================================================');
console.log('  测试 2: 创建第二个实例（应该有警告）');
console.log('======================================================================\n');

warnings = [];
const loader2 = new ProfileLoader();

if (warnings.length > 0) {
  console.log('✅ 第二个实例触发了警告');
  passed++;
} else {
  console.log('❌ 第二个实例应该触发警告');
  failed++;
}

console.log('\n======================================================================');
console.log('  测试 3: 验证警告内容');
console.log('======================================================================\n');

const warningText = warnings.join('\n');

const checks = [
  { name: '包含"多个 ProfileLoader 实例"', pattern: /多个 ProfileLoader 实例/ },
  { name: '包含实例 ID', pattern: /实例 ID: \d+/ },
  { name: '包含活跃实例数', pattern: /活跃实例数: \d+/ },
  { name: '包含总创建次数', pattern: /总创建次数: \d+/ },
  { name: '包含建议信息', pattern: /建议：/ },
  { name: '包含堆栈跟踪', pattern: /创建位置堆栈/ },
];

for (const check of checks) {
  if (check.pattern.test(warningText)) {
    console.log(`✅ ${check.name}`);
    passed++;
  } else {
    console.log(`❌ ${check.name}`);
    failed++;
  }
}

console.log('\n======================================================================');
console.log('  测试 4: 创建第三个实例（验证计数器）');
console.log('======================================================================\n');

warnings = [];
const loader3 = new ProfileLoader();

if (warnings.length > 0 && /实例 ID: 3/.test(warnings.join('\n'))) {
  console.log('✅ 第三个实例正确标记为 ID 3');
  passed++;
} else {
  console.log('❌ 实例 ID 计数不正确');
  failed++;
}

// 恢复 console.warn
console.warn = originalWarn;

console.log('\n======================================================================');
console.log('  测试结果汇总');
console.log('======================================================================\n');

console.log(`✅ 通过: ${passed}`);
console.log(`❌ 失败: ${failed}`);
console.log(`📊 成功率: ${((passed / (passed + failed)) * 100).toFixed(1)}%\n`);

if (failed === 0) {
  console.log('🎉 所有测试通过！单例保护机制正常工作。\n');
  process.exit(0);
} else {
  console.log('⚠️  部分测试失败，请检查实现。\n');
  process.exit(1);
}
