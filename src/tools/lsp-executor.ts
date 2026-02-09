/**
 * LSP 执行器
 * 提供基于 VSCode Language Server 的代码智能能力
 *
 * 工具: lsp_query
 */

import * as path from 'path';
import * as fs from 'fs';
import * as vscode from 'vscode';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

type LspAction =
  | 'diagnostics'
  | 'definition'
  | 'typeDefinition'
  | 'implementation'
  | 'references'
  | 'hover'
  | 'documentSymbols'
  | 'workspaceSymbols'
  | 'codeAction'
  | 'callHierarchy'
  | 'typeHierarchy'
  | 'signatureHelp'
  | 'rename';

interface LspQueryArgs {
  action: LspAction;
  filePath?: string;
  line?: number;
  character?: number;
  includeDeclaration?: boolean;
  query?: string;
  direction?: 'incoming' | 'outgoing' | 'supertypes' | 'subtypes';
  newName?: string;
}

const SUPPORTED_LANGUAGE_IDS = new Set([
  'typescript',
  'typescriptreact',
  'javascript',
  'javascriptreact',
  'python'
]);

const SUPPORTED_EXTS = new Set([
  '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs', '.py'
]);

/** SymbolKind 数值 → 可读名称 */
const SYMBOL_KIND_NAMES: Record<number, string> = {
  0: 'File', 1: 'Module', 2: 'Namespace', 3: 'Package',
  4: 'Class', 5: 'Method', 6: 'Property', 7: 'Field',
  8: 'Constructor', 9: 'Enum', 10: 'Interface', 11: 'Function',
  12: 'Variable', 13: 'Constant', 14: 'String', 15: 'Number',
  16: 'Boolean', 17: 'Array', 18: 'Object', 19: 'Key',
  20: 'Null', 21: 'EnumMember', 22: 'Struct', 23: 'Event',
  24: 'Operator', 25: 'TypeParameter',
};

export class LspExecutor implements ToolExecutor {
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'lsp_query',
      description: `Query VSCode LSP-backed code intelligence for TS/JS and Python.

Actions:
- diagnostics: list diagnostics for a file or workspace
- definition: find definition locations at position
- typeDefinition: find type definition (e.g., interface/type behind a variable)
- implementation: find implementations of interface/abstract class at position
- references: find reference locations at position
- hover: fetch type info and documentation at position
- documentSymbols: list symbols in a file (with signatures)
- workspaceSymbols: search symbols in workspace
- callHierarchy: trace incoming/outgoing call chains from a function`,
      input_schema: {
        type: 'object',
        properties: {
          action: {
            type: 'string',
            enum: [
              'diagnostics', 'definition', 'typeDefinition', 'implementation',
              'references', 'hover', 'documentSymbols', 'workspaceSymbols',
              'callHierarchy'
            ],
            description: 'LSP action to execute'
          },
          filePath: {
            type: 'string',
            description: 'Target file path (absolute or workspace-relative)'
          },
          line: {
            type: 'number',
            description: '0-based line number for position-based actions'
          },
          character: {
            type: 'number',
            description: '0-based character number for position-based actions'
          },
          includeDeclaration: {
            type: 'boolean',
            description: 'Whether to include declaration in references (default true)'
          },
          query: {
            type: 'string',
            description: 'Symbol name to search for (only for workspaceSymbols action, e.g. "FileExecutor")'
          },
          direction: {
            type: 'string',
            enum: ['incoming', 'outgoing'],
            description: 'Direction for callHierarchy (incoming/outgoing)'
          }
        },
        required: ['action']
      },
      metadata: {
        source: 'builtin',
        category: 'code-intel',
        tags: ['lsp', 'diagnostics', 'symbols', 'definition', 'references', 'hover',
               'typeDefinition', 'implementation', 'callHierarchy']
      }
    };
  }

  async getTools(): Promise<ExtendedToolDefinition[]> {
    return [this.getToolDefinition()];
  }

  async isAvailable(toolName: string): Promise<boolean> {
    return toolName === 'lsp_query';
  }

  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as LspQueryArgs;
    const action = args?.action;
    if (!action) {
      return this.error(toolCall, 'Missing action');
    }

    try {
      switch (action) {
        case 'diagnostics':
          return await this.handleDiagnostics(toolCall, args);
        case 'definition':
          return await this.handleDefinition(toolCall, args);
        case 'typeDefinition':
          return await this.handleTypeDefinition(toolCall, args);
        case 'implementation':
          return await this.handleImplementation(toolCall, args);
        case 'references':
          return await this.handleReferences(toolCall, args);
        case 'hover':
          return await this.handleHover(toolCall, args);
        case 'documentSymbols':
          return await this.handleDocumentSymbols(toolCall, args);
        case 'workspaceSymbols':
          return await this.handleWorkspaceSymbols(toolCall, args);
        case 'codeAction':
          return await this.handleCodeAction(toolCall, args);
        case 'callHierarchy':
          return await this.handleCallHierarchy(toolCall, args);
        case 'typeHierarchy':
          return await this.handleTypeHierarchy(toolCall, args);
        case 'signatureHelp':
          return await this.handleSignatureHelp(toolCall, args);
        case 'rename':
          return await this.handleRename(toolCall, args);
        default:
          return this.error(toolCall, `Unsupported action: ${action}`);
      }
    } catch (error: any) {
      logger.error('LSP tool failed', error, LogCategory.TOOLS);
      return this.error(toolCall, error?.message || 'LSP tool failed');
    }
  }

  // ============================================================================
  // Action 处理器
  // ============================================================================

  private async handleDiagnostics(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    if (args.filePath) {
      const uri = await this.resolveAndOpen(args.filePath);
      if (!uri) {
        return this.error(toolCall, 'File not found or unsupported language');
      }
      const diagnostics = vscode.languages.getDiagnostics(uri).map((diag) => this.serializeDiagnostic(diag));
      return this.ok(toolCall, { uri: uri.toString(), diagnostics });
    }

    const all = vscode.languages.getDiagnostics();
    const entries = all.map(([uri, diagnostics]) => ({
      uri: uri.toString(),
      diagnostics: diagnostics.map((diag: vscode.Diagnostic) => this.serializeDiagnostic(diag))
    }));
    return this.ok(toolCall, { entries });
  }

  private async handleDefinition(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    return this.handlePositionCommand(toolCall, args, 'vscode.executeDefinitionProvider', 'definition');
  }

  private async handleTypeDefinition(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    return this.handlePositionCommand(toolCall, args, 'vscode.executeTypeDefinitionProvider', 'typeDefinition');
  }

  private async handleImplementation(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    return this.handlePositionCommand(toolCall, args, 'vscode.executeImplementationProvider', 'implementation');
  }

  private async handleReferences(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for references');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const includeDeclaration = args.includeDeclaration !== false;
    const result = await vscode.commands.executeCommand<any>(
      'vscode.executeReferenceProvider',
      uri,
      position,
      { includeDeclaration }
    );
    const locations = this.serializeLocations(result);
    return this.ok(toolCall, { uri: uri.toString(), locations });
  }

  private async handleHover(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for hover');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const result = await vscode.commands.executeCommand<any>('vscode.executeHoverProvider', uri, position);
    const hovers = Array.isArray(result) ? result.map((hover) => this.serializeHover(hover)) : [];
    return this.ok(toolCall, { uri: uri.toString(), hovers });
  }

  private async handleDocumentSymbols(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    if (!args.filePath) {
      return this.error(toolCall, 'filePath is required for documentSymbols');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const result = await vscode.commands.executeCommand<any>('vscode.executeDocumentSymbolProvider', uri);
    const symbols = this.serializeSymbols(result);
    return this.ok(toolCall, { uri: uri.toString(), symbols });
  }

  private async handleWorkspaceSymbols(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const query = args.query || '';
    const result = await vscode.commands.executeCommand<any>('vscode.executeWorkspaceSymbolProvider', query);
    const symbols = this.serializeSymbols(result);
    return this.ok(toolCall, { query, symbols });
  }

  private async handleCodeAction(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for codeAction');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const range = new vscode.Range(position, position);
    const result = await vscode.commands.executeCommand<any>('vscode.executeCodeActionProvider', uri, range);
    const actions = Array.isArray(result)
      ? result.map((action: any) => ({
          title: action.title,
          kind: action.kind?.value,
          isPreferred: action.isPreferred || false,
        }))
      : [];
    return this.ok(toolCall, { uri: uri.toString(), actions });
  }

  private async handleCallHierarchy(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for callHierarchy');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const items = await vscode.commands.executeCommand<vscode.CallHierarchyItem[]>(
      'vscode.prepareCallHierarchy', uri, position
    );
    if (!items || items.length === 0) {
      return this.ok(toolCall, { uri: uri.toString(), calls: [], note: 'No call hierarchy item found at position' });
    }

    const item = items[0];
    const direction = args.direction === 'outgoing' ? 'outgoing' : 'incoming';
    const command = direction === 'outgoing'
      ? 'vscode.provideOutgoingCalls'
      : 'vscode.provideIncomingCalls';

    const calls = await vscode.commands.executeCommand<any[]>(command, item);
    const serialized = Array.isArray(calls)
      ? calls.map((call: any) => {
          const target = direction === 'outgoing' ? call.to : call.from;
          return {
            name: target?.name,
            kind: target?.kind !== undefined ? SYMBOL_KIND_NAMES[target.kind] : undefined,
            uri: target?.uri?.toString(),
            range: target?.range ? this.serializeRange(target.range) : undefined,
            fromRanges: Array.isArray(call.fromRanges)
              ? call.fromRanges.map((r: vscode.Range) => this.serializeRange(r))
              : [],
          };
        })
      : [];

    return this.ok(toolCall, {
      uri: uri.toString(),
      item: { name: item.name, kind: SYMBOL_KIND_NAMES[item.kind] },
      direction,
      calls: serialized,
    });
  }

  private async handleTypeHierarchy(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for typeHierarchy');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const items = await vscode.commands.executeCommand<vscode.TypeHierarchyItem[]>(
      'vscode.prepareTypeHierarchy', uri, position
    );
    if (!items || items.length === 0) {
      return this.ok(toolCall, { uri: uri.toString(), types: [], note: 'No type hierarchy item found at position' });
    }

    const item = items[0];
    const direction = args.direction === 'subtypes' ? 'subtypes' : 'supertypes';
    const command = direction === 'subtypes'
      ? 'vscode.provideSubtypes'
      : 'vscode.provideSupertypes';

    const types = await vscode.commands.executeCommand<vscode.TypeHierarchyItem[]>(command, item);
    const serialized = Array.isArray(types)
      ? types.map((t) => ({
          name: t.name,
          kind: t.kind !== undefined ? SYMBOL_KIND_NAMES[t.kind] : undefined,
          uri: t.uri?.toString(),
          range: t.range ? this.serializeRange(t.range) : undefined,
          detail: t.detail || undefined,
        }))
      : [];

    return this.ok(toolCall, {
      uri: uri.toString(),
      item: { name: item.name, kind: SYMBOL_KIND_NAMES[item.kind], detail: item.detail || undefined },
      direction,
      types: serialized,
    });
  }

  private async handleSignatureHelp(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for signatureHelp');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const result = await vscode.commands.executeCommand<vscode.SignatureHelp>(
      'vscode.executeSignatureHelpProvider', uri, position
    );
    if (!result || !result.signatures || result.signatures.length === 0) {
      return this.ok(toolCall, { uri: uri.toString(), signatures: [], note: 'No signature help available at position' });
    }

    const signatures = result.signatures.map((sig) => ({
      label: sig.label,
      documentation: this.extractHoverText(sig.documentation),
      parameters: Array.isArray(sig.parameters)
        ? sig.parameters.map((param) => ({
            label: param.label,
            documentation: this.extractHoverText(param.documentation),
          }))
        : [],
    }));

    return this.ok(toolCall, {
      uri: uri.toString(),
      activeSignature: result.activeSignature,
      activeParameter: result.activeParameter,
      signatures,
    });
  }

  private async handleRename(toolCall: ToolCall, args: LspQueryArgs): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for rename');
    }
    if (!args.newName || args.newName.trim().length === 0) {
      return this.error(toolCall, 'newName is required for rename');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    let edit: vscode.WorkspaceEdit | undefined;
    try {
      edit = await vscode.commands.executeCommand<vscode.WorkspaceEdit>(
        'vscode.executeDocumentRenameProvider', uri, position, args.newName.trim()
      );
    } catch (renameError: any) {
      return this.ok(toolCall, { uri: uri.toString(), applied: false, note: renameError?.message || 'Rename not available at this position' });
    }
    if (!edit) {
      return this.ok(toolCall, { uri: uri.toString(), applied: false, note: 'Rename not available at this position' });
    }

    // 序列化 WorkspaceEdit 为可读预览（不自动应用）
    const changes: Record<string, Array<{ range: Record<string, unknown>; newText: string }>> = {};
    for (const [fileUri, textEdits] of edit.entries()) {
      const filePath = fileUri.fsPath;
      const relativePath = this.workspaceRoot
        ? path.relative(this.workspaceRoot, filePath) || filePath
        : filePath;
      changes[relativePath] = textEdits.map((te) => ({
        range: this.serializeRange(te.range),
        newText: te.newText,
      }));
    }

    const fileCount = Object.keys(changes).length;
    const editCount = Object.values(changes).reduce((sum, edits) => sum + edits.length, 0);

    if (editCount === 0) {
      return this.ok(toolCall, {
        uri: uri.toString(),
        newName: args.newName.trim(),
        applied: false,
        note: 'No edits produced by rename. The symbol may already have this name or no references exist.',
      });
    }

    return this.ok(toolCall, {
      uri: uri.toString(),
      newName: args.newName.trim(),
      applied: false,
      preview: { fileCount, editCount, changes },
      note: 'Rename preview generated. Use text_editor(str_replace) to apply changes.',
    });
  }

  // ============================================================================
  // 通用位置命令处理（definition / typeDefinition / implementation 共用）
  // ============================================================================

  private async handlePositionCommand(
    toolCall: ToolCall,
    args: LspQueryArgs,
    command: string,
    actionName: string
  ): Promise<ToolResult> {
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, `filePath, line, character are required for ${actionName}`);
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const result = await vscode.commands.executeCommand<any>(command, uri, position);
    const locations = this.serializeLocations(result);
    return this.ok(toolCall, { uri: uri.toString(), locations });
  }

  // ============================================================================
  // 文件解析
  // ============================================================================

  private async resolveAndOpen(filePath: string): Promise<vscode.Uri | null> {
    const resolved = this.resolvePath(filePath);
    if (!resolved) return null;
    if (!this.isSupportedFile(resolved)) return null;

    const uri = vscode.Uri.file(resolved);
    try {
      const doc = await vscode.workspace.openTextDocument(uri);
      if (!SUPPORTED_LANGUAGE_IDS.has(doc.languageId)) {
        return null;
      }
    } catch (error: any) {
      logger.warn('LSP open document failed', { error: error?.message, filePath: resolved }, LogCategory.TOOLS);
      return null;
    }
    return uri;
  }

  private resolvePath(filePath: string): string | null {
    const normalized = filePath.trim();
    if (!normalized) return null;
    const resolved = path.isAbsolute(normalized)
      ? normalized
      : path.join(this.workspaceRoot, normalized);
    if (!fs.existsSync(resolved)) {
      return null;
    }
    return resolved;
  }

  private isSupportedFile(filePath: string): boolean {
    return SUPPORTED_EXTS.has(path.extname(filePath));
  }

  private getPosition(args: LspQueryArgs): vscode.Position | null {
    if (typeof args.line !== 'number' || typeof args.character !== 'number') {
      return null;
    }
    if (args.line < 0 || args.character < 0) {
      return null;
    }
    return new vscode.Position(args.line, args.character);
  }

  // ============================================================================
  // 序列化
  // ============================================================================

  private serializeRange(range: vscode.Range): { start: { line: number; character: number }; end: { line: number; character: number } } {
    return {
      start: { line: range.start.line, character: range.start.character },
      end: { line: range.end.line, character: range.end.character }
    };
  }

  private serializeDiagnostic(diag: vscode.Diagnostic): Record<string, unknown> {
    return {
      message: diag.message,
      severity: diag.severity,
      source: diag.source,
      code: diag.code,
      range: this.serializeRange(diag.range)
    };
  }

  private serializeLocations(result: any): Array<Record<string, unknown>> {
    if (!result) return [];
    const items = Array.isArray(result) ? result : [result];
    return items.map((item: any) => {
      if (item.targetUri) {
        return {
          uri: item.targetUri.toString(),
          range: this.serializeRange(item.targetRange),
          selectionRange: item.targetSelectionRange ? this.serializeRange(item.targetSelectionRange) : undefined
        };
      }
      return {
        uri: item.uri?.toString(),
        range: item.range ? this.serializeRange(item.range) : undefined
      };
    });
  }

  private serializeSymbols(result: any): Array<Record<string, unknown>> {
    if (!result) return [];
    if (Array.isArray(result)) {
      return result.map((symbol: any) => this.serializeSymbol(symbol)).filter((s): s is Record<string, unknown> => s !== null);
    }
    const single = this.serializeSymbol(result);
    return single ? [single] : [];
  }

  private serializeSymbol(symbol: any): Record<string, unknown> | null {
    if (!symbol) return null;
    const kind = typeof symbol.kind === 'number' ? symbol.kind : undefined;
    const kindName = kind !== undefined ? SYMBOL_KIND_NAMES[kind] : undefined;

    // WorkspaceSymbol（有 location 属性）
    if (symbol.location) {
      return {
        name: symbol.name,
        detail: symbol.detail || undefined,
        kind: kindName,
        location: {
          uri: symbol.location.uri?.toString(),
          range: symbol.location.range ? this.serializeRange(symbol.location.range) : undefined
        }
      };
    }

    // DocumentSymbol（有 range + children）
    return {
      name: symbol.name,
      detail: symbol.detail || undefined,
      kind: kindName,
      range: symbol.range ? this.serializeRange(symbol.range) : undefined,
      selectionRange: symbol.selectionRange ? this.serializeRange(symbol.selectionRange) : undefined,
      children: Array.isArray(symbol.children)
        ? symbol.children.map((child: any) => this.serializeSymbol(child)).filter(Boolean)
        : []
    };
  }

  private serializeHover(hover: any): Record<string, unknown> {
    return {
      contents: this.extractHoverText(hover?.contents),
      range: hover?.range ? this.serializeRange(hover.range) : undefined
    };
  }

  /** 将 hover contents 中的多种格式统一提取为纯文本 */
  private extractHoverText(contents: any): string {
    if (!contents) return '';
    // 纯字符串
    if (typeof contents === 'string') return contents;
    // {language, value} 格式（必须在 MarkdownString 之前检查，否则被 .value 拦截）
    if (typeof contents.language === 'string' && typeof contents.value === 'string') {
      return `\`\`\`${contents.language}\n${contents.value}\n\`\`\``;
    }
    // MarkdownString（仅有 .value 无 .language）
    if (typeof contents.value === 'string') return contents.value;
    // 数组格式
    if (Array.isArray(contents)) {
      return contents.map((c: any) => this.extractHoverText(c)).filter(Boolean).join('\n\n');
    }
    return String(contents);
  }

  // ============================================================================
  // 响应构建
  // ============================================================================

  private ok(toolCall: ToolCall, payload: Record<string, unknown>): ToolResult {
    return {
      toolCallId: toolCall.id,
      content: JSON.stringify(payload, null, 2)
    };
  }

  private error(toolCall: ToolCall, message: string): ToolResult {
    return {
      toolCallId: toolCall.id,
      content: message,
      isError: true
    };
  }
}
