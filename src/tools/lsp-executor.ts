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
  | 'references'
  | 'hover'
  | 'documentSymbols'
  | 'workspaceSymbols';

interface LspQueryArgs {
  action: LspAction;
  filePath?: string;
  line?: number;
  character?: number;
  includeDeclaration?: boolean;
  query?: string;
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
- references: find reference locations at position
- hover: fetch hover info at position
- documentSymbols: list symbols in a file
- workspaceSymbols: search symbols in workspace

Notes:
- line/character are 0-based.
- Supported languages: TS/JS and Python.
- filePath can be absolute or workspace-relative.
`,
      input_schema: {
        type: 'object',
        properties: {
          action: {
            type: 'string',
            enum: ['diagnostics', 'definition', 'references', 'hover', 'documentSymbols', 'workspaceSymbols'],
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
            description: 'Query string for workspaceSymbols'
          }
        },
        required: ['action']
      },
      metadata: {
        source: 'builtin',
        category: 'code-intel',
        tags: ['lsp', 'diagnostics', 'symbols', 'definition', 'references', 'hover']
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
        case 'references':
          return await this.handleReferences(toolCall, args);
        case 'hover':
          return await this.handleHover(toolCall, args);
        case 'documentSymbols':
          return await this.handleDocumentSymbols(toolCall, args);
        case 'workspaceSymbols':
          return await this.handleWorkspaceSymbols(toolCall, args);
        default:
          return this.error(toolCall, `Unsupported action: ${action}`);
      }
    } catch (error: any) {
      logger.error('LSP tool failed', { error: error?.message }, LogCategory.TOOLS);
      return this.error(toolCall, error?.message || 'LSP tool failed');
    }
  }

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
    const position = this.getPosition(args);
    if (!args.filePath || !position) {
      return this.error(toolCall, 'filePath, line, character are required for definition');
    }

    const uri = await this.resolveAndOpen(args.filePath);
    if (!uri) {
      return this.error(toolCall, 'File not found or unsupported language');
    }

    const result = await vscode.commands.executeCommand<any>('vscode.executeDefinitionProvider', uri, position);
    const locations = this.serializeLocations(result);
    return this.ok(toolCall, { uri: uri.toString(), locations });
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

  private serializeRange(range: vscode.Range): { start: any; end: any } {
    return {
      start: { line: range.start.line, character: range.start.character },
      end: { line: range.end.line, character: range.end.character }
    };
  }

  private serializeDiagnostic(diag: vscode.Diagnostic): any {
    return {
      message: diag.message,
      severity: diag.severity,
      source: diag.source,
      code: diag.code,
      range: this.serializeRange(diag.range)
    };
  }

  private serializeLocations(result: any): any[] {
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

  private serializeSymbols(result: any): any[] {
    if (!result) return [];
    if (Array.isArray(result)) {
      return result.map((symbol: any) => this.serializeSymbol(symbol));
    }
    return [this.serializeSymbol(result)];
  }

  private serializeSymbol(symbol: any): any {
    if (!symbol) return null;
    const kind = typeof symbol.kind === 'number' ? symbol.kind : undefined;
    const kindName = kind !== undefined && (vscode.SymbolKind as any)[kind]
      ? (vscode.SymbolKind as any)[kind]
      : undefined;

    if (symbol.location) {
      return {
        name: symbol.name,
        kind,
        kindName,
        location: {
          uri: symbol.location.uri?.toString(),
          range: symbol.location.range ? this.serializeRange(symbol.location.range) : undefined
        }
      };
    }

    return {
      name: symbol.name,
      kind,
      kindName,
      range: symbol.range ? this.serializeRange(symbol.range) : undefined,
      selectionRange: symbol.selectionRange ? this.serializeRange(symbol.selectionRange) : undefined,
      children: Array.isArray(symbol.children)
        ? symbol.children.map((child: any) => this.serializeSymbol(child)).filter(Boolean)
        : []
    };
  }

  private serializeHover(hover: any): any {
    return {
      contents: hover?.contents,
      range: hover?.range ? this.serializeRange(hover.range) : undefined
    };
  }

  private ok(toolCall: ToolCall, payload: any): ToolResult {
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
