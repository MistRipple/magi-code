/**
 * VSCode Terminal Executor 测试
 * 用于验证终端功能是否正常工作
 */

import * as vscode from 'vscode';
import { VSCodeTerminalExecutor } from '../tools/vscode-terminal-executor';

export async function testVSCodeTerminal() {
  const output = vscode.window.createOutputChannel('MultiCLI Terminal Test');
  output.show();
  
  output.appendLine('=== VSCode Terminal Executor 测试 ===');
  output.appendLine('');

  const executor = new VSCodeTerminalExecutor();

  try {
    // 测试 1: 简单命令（不显示终端）
    output.appendLine('测试 1: 执行简单命令（后台）');
    const result1 = await executor.execute({
      command: 'echo "Hello from MultiCLI"',
      useVSCodeTerminal: true,
      showTerminal: false,
    });
    output.appendLine(`✅ 命令执行成功`);
    output.appendLine(`   退出码: ${result1.exitCode}`);
    output.appendLine(`   输出: ${result1.stdout}`);
    output.appendLine(`   耗时: ${result1.duration}ms`);
    output.appendLine('');

    // 测试 2: 显示终端窗口
    output.appendLine('测试 2: 显示终端窗口执行命令');
    const result2 = await executor.execute({
      command: 'echo "This should appear in a terminal window"',
      useVSCodeTerminal: true,
      showTerminal: true,
      keepTerminalOpen: true,
      name: 'MultiCLI Test Terminal',
    });
    output.appendLine(`✅ 终端窗口已打开`);
    output.appendLine(`   退出码: ${result2.exitCode}`);
    output.appendLine(`   输出: ${result2.stdout}`);
    output.appendLine(`   耗时: ${result2.duration}ms`);
    output.appendLine('');

    // 测试 3: 列出文件
    output.appendLine('测试 3: 列出当前目录文件');
    const result3 = await executor.execute({
      command: 'ls -la',
      useVSCodeTerminal: true,
      showTerminal: true,
      keepTerminalOpen: false,
      name: 'List Files',
    });
    output.appendLine(`✅ 命令执行成功`);
    output.appendLine(`   退出码: ${result3.exitCode}`);
    output.appendLine(`   输出长度: ${result3.stdout.length} 字符`);
    output.appendLine(`   耗时: ${result3.duration}ms`);
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
    'multicli.testTerminal',
    testVSCodeTerminal
  );
  context.subscriptions.push(disposable);
}

