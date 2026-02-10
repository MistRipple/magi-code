/**
 * 全方位测试主执行器
 * 
 * 整合所有测试工程师，执行完整的测试套件
 */

import { TestCommandCenter } from './test-command-center';
import { StateManagementEngineer } from './engineers/state-management-engineer';
import { MessageRoutingEngineer } from './engineers/message-routing-engineer';
import { UIInteractionEngineer } from './engineers/ui-interaction-engineer';
import { EndToEndEngineer } from './engineers/end-to-end-engineer';
import { IntentClassificationE2EEngineer } from './engineers/intent-classification-e2e';
import { MessageHubE2EEngineer } from './engineers/message-hub-e2e-engineer';

type TestMode = 'quick' | 'full' | 'unit' | 'e2e';

function resolveTestMode(rawMode: string | undefined): TestMode {
  const mode = (rawMode ?? 'quick').toLowerCase();
  if (mode === 'quick' || mode === 'full' || mode === 'unit' || mode === 'e2e') {
    return mode;
  }
  return 'quick';
}

function registerEngineersByMode(commandCenter: TestCommandCenter, mode: TestMode): void {
  if (mode === 'unit') {
    commandCenter.registerEngineer(new StateManagementEngineer());
    commandCenter.registerEngineer(new MessageRoutingEngineer());
    commandCenter.registerEngineer(new UIInteractionEngineer());
    return;
  }

  commandCenter.registerEngineer(new StateManagementEngineer());
  commandCenter.registerEngineer(new MessageRoutingEngineer());
  commandCenter.registerEngineer(new UIInteractionEngineer());
  commandCenter.registerEngineer(new EndToEndEngineer());

  if (mode === 'quick') {
    return;
  }

  commandCenter.registerEngineer(new IntentClassificationE2EEngineer());
  commandCenter.registerEngineer(new MessageHubE2EEngineer());
}

async function main() {
  const mode = resolveTestMode(process.argv[2]);

  console.log('========================================');
  console.log('Magi 全方位测试系统');
  console.log('========================================');
  console.log('模拟多个代理工程师协作进行全面测试\n');
  console.log(`测试模式: ${mode}\n`);
  
  // 创建测试指挥中心
  const commandCenter = new TestCommandCenter();
  
  // 按模式注册测试工程师
  registerEngineersByMode(commandCenter, mode);
  
  // 执行所有测试
  await commandCenter.runAllTests();
  
  // 分析结果并生成修复建议
  const reports = commandCenter.getReports();
  const criticalIssues = reports.flatMap(r => r.issues).filter(i => i.severity === 'critical');
  const highIssues = reports.flatMap(r => r.issues).filter(i => i.severity === 'high');
  
  if (criticalIssues.length > 0 || highIssues.length > 0) {
    console.log('\n⚠️  发现需要立即处理的问题！');
    console.log('建议优先修复以下问题：\n');
    
    [...criticalIssues, ...highIssues].forEach((issue, idx) => {
      console.log(`${idx + 1}. [${issue.severity.toUpperCase()}] ${issue.category}`);
      console.log(`   问题: ${issue.description}`);
      if (issue.location) {
        console.log(`   位置: ${issue.location}`);
      }
      if (issue.suggestedFix) {
        console.log(`   修复: ${issue.suggestedFix}`);
      }
      console.log();
    });
  } else {
    console.log('\n✅ 所有关键测试通过！系统运行良好。');
  }
}

// 执行测试
if (require.main === module) {
  main().catch(error => {
    console.error('测试执行失败:', error);
    process.exit(1);
  });
}

export { main };
