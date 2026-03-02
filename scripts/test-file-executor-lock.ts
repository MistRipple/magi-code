import { FileExecutor } from '../src/tools/file-executor';
import { WorkspaceRoots } from '../src/workspace/workspace-roots';
import * as path from 'path';
import * as fs from 'fs/promises';
import * as vscode from 'vscode'; // Mock this

// 由于 FileExecutor 严重依赖 vscode API（如 vscode.workspace.openTextDocument，vscode.WorkspaceEdit），
// 在独立脚本中运行需要 Mock vscode
const mockVsCode = {
  Uri: {
    file: (p: string) => ({ fsPath: p })
  },
  workspace: {
    textDocuments: [],
    openTextDocument: async (uri: any) => {
      try {
        const content = await fs.readFile(uri.fsPath, 'utf-8');
        return {
          getText: () => content,
          save: async () => {},
          lineCount: content.split('\n').length,
          lineAt: (line: number) => ({
            range: { start: { line, character: 0 }, end: { line, character: content.split('\n')[line]?.length || 0 } }
          })
        };
      } catch {
        return {
          getText: () => '',
          save: async () => {},
          lineCount: 1,
          lineAt: (line: number) => ({
            range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }
          })
        };
      }
    },
    applyEdit: async (edit: any) => {
      // 简化模拟，假设直接应用成功，但我们需要真正写入文件以测试并发读取最新内容
      for (const [uri, replaceContent] of edit._replacements.entries()) {
         await fs.writeFile(uri.fsPath, replaceContent, 'utf-8');
      }
      return true;
    }
  },
  WorkspaceEdit: class {
    _replacements = new Map();
    replace(uri: any, range: any, newContent: string) {
       this._replacements.set(uri, newContent);
    }
    createFile() {}
    insert() {}
  },
  Range: class {
    constructor(start: any, end: any) {}
  },
  Position: class {
     constructor(line: number, character: number) {}
  }
};

// 注入 mock
(global as any).vscode = mockVsCode;

// 如果 FileExecutor 里是通过 import * as vscode 引入的，独立运行 TS 可能会有问题。
// 我们可以写一个稍微高级一点的 mock 或者干脆写个测试文件通过 vscode 插件宿主环境运行。
