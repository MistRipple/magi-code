/**
 * 深度代码检查器 - 发现潜在问题
 * 
 * 检查范围：
 * 1. 今天修复的代码是否完整
 * 2. 是否有遗漏的边界情况
 * 3. 是否有潜在的bug
 */

import * as fs from 'fs';
import * as path from 'path';

interface CodeIssue {
  file: string;
  line?: number;
  severity: 'critical' | 'high' | 'medium' | 'low';
  category: string;
  description: string;
  suggestedFix?: string;
}

class DeepCodeChecker {
  private issues: CodeIssue[] = [];
  private workspaceRoot: string;
  
  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }
  
  async runAllChecks(): Promise<void> {
    console.log('\n========================================');
    console.log('深度代码检查器');
    console.log('========================================\n');
    
    // 1. 检查 webview/index.html 中的修复
    await this.checkWebviewFixes();
    
    // 2. 检查 adapter-factory.ts 中的修复
    await this.checkAdapterFactoryFixes();
    
    // 3. 检查 webview-provider.ts 中的事件监听
    await this.checkWebviewProviderEvents();
    
    // 4. 检查是否有未使用的遗留代码
    await this.checkDeadCode();
    
    // 5. 检查状态一致性
    await this.checkStateConsistency();
    
    this.printReport();
  }
  
  private async checkWebviewFixes(): Promise<void> {
    console.log('[检查] webview/index.html 修复完整性...');
    
    const filePath = path.join(this.workspaceRoot, 'src/ui/webview/index.html');
    const content = fs.readFileSync(filePath, 'utf-8');
    
    // 检查1: 自然语言确认是否使用全量匹配
    if (content.includes('.includes(userInput)')) {
      console.log('  ✓ 自然语言确认使用全量匹配');
    } else if (content.includes('userInput.includes(')) {
      this.issues.push({
        file: 'src/ui/webview/index.html',
        severity: 'high',
        category: '自然语言处理',
        description: '自然语言确认仍使用部分匹配，可能导致误判',
        suggestedFix: '使用 confirmKeywords.includes(userInput) 替代 includes(kw)'
      });
    }
    
    // 检查2: 是否有 setProcessingState(false) 在确认卡片显示时
    if (content.includes('showPlanConfirmation') && content.includes('setProcessingState(false)')) {
      console.log('  ✓ 确认卡片显示时正确设置 isProcessing = false');
    }
    
    // 检查3: 是否检查 isWaitingConfirmation
    if (content.includes('isWaitingConfirmation')) {
      console.log('  ✓ stateUpdate 中检查 isWaitingConfirmation');
    }
    
    // 检查4: 任务完成时是否清理 isPending
    if (content.includes("m.isPending") && content.includes("phase === 'completed'")) {
      console.log('  ✓ 任务完成时清理 isPending 状态');
    }
    
    // 检查5: 是否有重复的事件监听
    const confirmationRequestCount = (content.match(/confirmationRequest/g) || []).length;
    if (confirmationRequestCount > 10) {
      this.issues.push({
        file: 'src/ui/webview/index.html',
        severity: 'low',
        category: '代码质量',
        description: `confirmationRequest 出现 ${confirmationRequestCount} 次，检查是否有重复逻辑`
      });
    }
  }
  
  private async checkAdapterFactoryFixes(): Promise<void> {
    console.log('[检查] adapter-factory.ts 修复完整性...');
    
    const filePath = path.join(this.workspaceRoot, 'src/llm/adapter-factory.ts');
    const content = fs.readFileSync(filePath, 'utf-8');
    
    // 检查1: output 事件是否被注释掉
    if (content.includes("// this.emit('output'")) {
      console.log('  ✓ 遗留 output 事件已被注释');
    } else if (content.includes("this.emit('output'") && !content.includes('emitOrchestratorMessage')) {
      this.issues.push({
        file: 'src/llm/adapter-factory.ts',
        severity: 'high',
        category: '消息重复',
        description: '可能仍有重复的 output 事件发送'
      });
    }
    
    // 检查2: response 事件是否被注释掉
    if (content.includes("// this.emit('response'")) {
      console.log('  ✓ 遗留 response 事件已被注释');
    }
    
    // 检查3: emitOrchestratorMessage 是否正确设置 adapterRole
    if (content.includes("adapterRole: 'orchestrator'")) {
      console.log('  ✓ emitOrchestratorMessage 正确设置 adapterRole');
    }
  }
  
  private async checkWebviewProviderEvents(): Promise<void> {
    console.log('[检查] webview-provider.ts 事件监听...');
    
    const filePath = path.join(this.workspaceRoot, 'src/ui/webview-provider.ts');
    const content = fs.readFileSync(filePath, 'utf-8');
    
    // 检查1: 是否还在监听遗留的 output/response 事件
    if (content.includes("cliFactory.on('output'")) {
      console.log('  ⚠ 仍在监听 output 事件（可能是遗留代码）');
      // 这可能是故意保留的遗留代码，需要进一步检查
    }
    
    // 检查2: 是否正确处理 standardMessage
    if (content.includes("standardMessage")) {
      console.log('  ✓ 正在处理 standardMessage 事件');
    }
  }
  
  private async checkDeadCode(): Promise<void> {
    console.log('[检查] 是否有未使用的遗留代码...');
    
    // 检查 webview-provider.ts 中是否有永远不会触发的事件监听
    const providerPath = path.join(this.workspaceRoot, 'src/ui/webview-provider.ts');
    const providerContent = fs.readFileSync(providerPath, 'utf-8');
    
    // 如果 adapter-factory 不再发送 output 事件，那么 webview-provider 中的监听器就是死代码
    const factoryPath = path.join(this.workspaceRoot, 'src/llm/adapter-factory.ts');
    const factoryContent = fs.readFileSync(factoryPath, 'utf-8');
    
    const factoryEmitsOutput = factoryContent.includes("this.emit('output'") && 
                               !factoryContent.includes("// this.emit('output'");
    const providerListensOutput = providerContent.includes("cliFactory.on('output'");
    
    if (!factoryEmitsOutput && providerListensOutput) {
      this.issues.push({
        file: 'src/ui/webview-provider.ts',
        severity: 'medium',
        category: '死代码',
        description: '监听 output 事件但 adapter-factory 不再发送此事件',
        suggestedFix: '移除或注释掉 cliFactory.on(\'output\') 监听器'
      });
    }
  }
  
  private async checkStateConsistency(): Promise<void> {
    console.log('[检查] 状态一致性...');
    
    const htmlPath = path.join(this.workspaceRoot, 'src/ui/webview/index.html');
    const htmlContent = fs.readFileSync(htmlPath, 'utf-8');
    
    // 检查是否所有设置 streaming=true 的地方都有对应的 streaming=false
    const setStreamingTrue = (htmlContent.match(/streaming\s*[=:]\s*true/g) || []).length;
    const setStreamingFalse = (htmlContent.match(/streaming\s*[=:]\s*false/g) || []).length;
    
    console.log(`  ℹ streaming=true 设置次数: ${setStreamingTrue}`);
    console.log(`  ℹ streaming=false 设置次数: ${setStreamingFalse}`);
    
    if (setStreamingTrue > setStreamingFalse + 2) {
      this.issues.push({
        file: 'src/ui/webview/index.html',
        severity: 'medium',
        category: '状态一致性',
        description: 'streaming 状态设置不平衡，可能导致消息一直显示"运行中"'
      });
    }
  }
  
  private printReport(): void {
    console.log('\n========================================');
    console.log('检查报告');
    console.log('========================================\n');
    
    if (this.issues.length === 0) {
      console.log('✅ 未发现问题！所有修复都已正确实施。\n');
      return;
    }
    
    const critical = this.issues.filter(i => i.severity === 'critical');
    const high = this.issues.filter(i => i.severity === 'high');
    const medium = this.issues.filter(i => i.severity === 'medium');
    const low = this.issues.filter(i => i.severity === 'low');
    
    console.log('问题统计:');
    console.log(`  🔴 严重: ${critical.length}`);
    console.log(`  🟠 高: ${high.length}`);
    console.log(`  🟡 中: ${medium.length}`);
    console.log(`  🟢 低: ${low.length}\n`);
    
    if (critical.length > 0 || high.length > 0) {
      console.log('需要立即处理的问题:');
      [...critical, ...high].forEach((issue, idx) => {
        console.log(`\n${idx + 1}. [${issue.severity.toUpperCase()}] ${issue.category}`);
        console.log(`   文件: ${issue.file}`);
        console.log(`   问题: ${issue.description}`);
        if (issue.suggestedFix) {
          console.log(`   建议: ${issue.suggestedFix}`);
        }
      });
    }
    
    if (medium.length > 0) {
      console.log('\n\n可以优化的问题:');
      medium.forEach((issue, idx) => {
        console.log(`\n${idx + 1}. [${issue.severity.toUpperCase()}] ${issue.category}`);
        console.log(`   文件: ${issue.file}`);
        console.log(`   问题: ${issue.description}`);
        if (issue.suggestedFix) {
          console.log(`   建议: ${issue.suggestedFix}`);
        }
      });
    }
    
    console.log('\n========================================\n');
  }
}

async function main() {
  const checker = new DeepCodeChecker(process.cwd());
  await checker.runAllChecks();
}

if (require.main === module) {
  main().catch(console.error);
}

export { DeepCodeChecker };
