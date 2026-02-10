/**
 * VSCode Terminal Executor 测试
 * 用于验证终端功能是否正常工作
 */

import * as vscode from 'vscode';
import { VSCodeTerminalExecutor } from '../tools/vscode-terminal-executor';

export async function testVSCodeTerminal() {
  const output = vscode.window.createOutputChannel('Magi Terminal Test');
  output.show();
  
  output.appendLine('=== VSCode Terminal Executor 测试 ===');
  output.appendLine('');

  const executor = new VSCodeTerminalExecutor();

  try {
    // 测试 1: 简单命令（不显示终端）
    output.appendLine('测试 1: 执行简单命令（后台）');
    const launch1 = await executor.launchProcess({
      command: 'echo "Hello from Magi"',
      wait: true,
      maxWaitSeconds: 5,
      showTerminal: false,
      name: 'orchestrator',
    });
    output.appendLine(`✅ 命令执行成功`);
    output.appendLine(`   状态: ${launch1.status}`);
    output.appendLine(`   退出码: ${launch1.return_code}`);
    output.appendLine(`   输出: ${launch1.output}`);
    output.appendLine('');

    // 测试 2: 显示终端窗口
    output.appendLine('测试 2: 显示终端窗口执行命令');
    const launch2 = await executor.launchProcess({
      command: 'echo "This should appear in a terminal window"',
      wait: true,
      maxWaitSeconds: 5,
      showTerminal: true,
      name: 'worker-claude',
    });
    output.appendLine(`✅ 终端窗口已打开`);
    output.appendLine(`   状态: ${launch2.status}`);
    output.appendLine(`   退出码: ${launch2.return_code}`);
    output.appendLine(`   输出: ${launch2.output}`);
    output.appendLine('');

    // 测试 3: 列出文件
    output.appendLine('测试 3: 列出当前目录文件');
    const launch3 = await executor.launchProcess({
      command: 'ls -la',
      wait: true,
      maxWaitSeconds: 10,
      showTerminal: true,
      name: 'worker-gemini',
    });
    output.appendLine(`✅ 命令执行成功`);
    output.appendLine(`   状态: ${launch3.status}`);
    output.appendLine(`   退出码: ${launch3.return_code}`);
    output.appendLine(`   输出长度: ${launch3.output.length} 字符`);
    output.appendLine('');

    // 测试 4: 获取进程列表
    output.appendLine('测试 4: 检查进程管理');
    const processes = executor.listProcesses();
    output.appendLine(`✅ 当前进程数: ${processes.length}`);
    processes.forEach((proc, index) => {
      output.appendLine(`   进程 ${index + 1}: ${proc.command} (状态: ${proc.state})`);
    });
    output.appendLine('');

    output.appendLine('=== 所有测试完成 ===');
    output.appendLine('✅ VSCode Terminal Executor 工作正常！');

    vscode.window.showInformationMessage('✅ VSCode Terminal 测试完成！查看输出面板了解详情。');

  } catch (error: any) {
    output.appendLine('');
    output.appendLine('❌ 测试失败！');
    output.appendLine(`错误: ${error.message}`);
    output.appendLine(`堆栈: ${error.stack}`);
    
    vscode.window.showErrorMessage(`❌ VSCode Terminal 测试失败: ${error.message}`);
  }
}

/**
 * 注册测试命令
 */
export function registerTerminalTest(context: vscode.ExtensionContext) {
  const disposable = vscode.commands.registerCommand(
    'magi.testTerminal',
    testVSCodeTerminal
  );
  context.subscriptions.push(disposable);
}

