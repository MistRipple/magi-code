import * as fs from 'fs';
import * as path from 'path';
import { AGENT_UI_SETTINGS_FILE } from '../config';

type UriLike = {
  fsPath: string;
  toString(): string;
};

type PositionLike = {
  line: number;
  character: number;
};

type RangeLike = {
  start: PositionLike;
  end: PositionLike;
};

type TextDocumentLike = {
  uri: UriLike;
  isDirty: boolean;
  getText(): string;
  positionAt(offset: number): PositionLike;
  save(): Promise<boolean>;
};

type WorkspaceFolderLike = {
  name: string;
  uri: UriLike;
};

type WorkspaceEditOperation =
  | { type: 'createFile'; uri: UriLike }
  | { type: 'replace'; uri: UriLike; range: RangeLike; text: string }
  | { type: 'insert'; uri: UriLike; position: PositionLike; text: string };

class UriImpl implements UriLike {
  constructor(public readonly fsPath: string) {}

  toString(): string {
    return `file://${this.fsPath}`;
  }
}

class Position implements PositionLike {
  constructor(
    public readonly line: number,
    public readonly character: number,
  ) {}
}

class Range implements RangeLike {
  constructor(
    public readonly start: PositionLike,
    public readonly end: PositionLike,
  ) {}
}

class WorkspaceEdit {
  readonly operations: WorkspaceEditOperation[] = [];

  createFile(uri: UriLike): void {
    this.operations.push({ type: 'createFile', uri });
  }

  replace(uri: UriLike, range: RangeLike, text: string): void {
    this.operations.push({ type: 'replace', uri, range, text });
  }

  insert(uri: UriLike, position: PositionLike, text: string): void {
    this.operations.push({ type: 'insert', uri, position, text });
  }
}

const textDocuments: TextDocumentLike[] = [];

function getWorkspaceRoot(): string {
  return process.cwd();
}

function buildWorkspaceFolders(): WorkspaceFolderLike[] {
  const root = getWorkspaceRoot();
  return [{ name: path.basename(root), uri: new UriImpl(root) }];
}

function clampOffset(content: string, offset: number): number {
  if (!Number.isFinite(offset)) {
    return 0;
  }
  return Math.max(0, Math.min(content.length, offset));
}

function offsetToPosition(content: string, offset: number): PositionLike {
  const normalizedOffset = clampOffset(content, offset);
  const head = content.slice(0, normalizedOffset);
  const lines = head.split('\n');
  const line = Math.max(0, lines.length - 1);
  const character = lines[lines.length - 1]?.length ?? 0;
  return new Position(line, character);
}

function positionToOffset(content: string, position: PositionLike): number {
  const lines = content.split('\n');
  let offset = 0;
  const targetLine = Math.max(0, Math.min(position.line, Math.max(0, lines.length - 1)));
  for (let i = 0; i < targetLine; i += 1) {
    offset += lines[i].length + 1;
  }
  const currentLine = lines[targetLine] ?? '';
  offset += Math.max(0, Math.min(position.character, currentLine.length));
  return offset;
}

function createTextDocument(filePath: string): TextDocumentLike {
  const normalizedPath = path.resolve(filePath);
  let content = fs.existsSync(normalizedPath) ? fs.readFileSync(normalizedPath, 'utf8') : '';

  return {
    uri: new UriImpl(normalizedPath),
    isDirty: false,
    getText(): string {
      return content;
    },
    positionAt(offset: number): PositionLike {
      return offsetToPosition(content, offset);
    },
    async save(): Promise<boolean> {
      await fs.promises.mkdir(path.dirname(normalizedPath), { recursive: true });
      await fs.promises.writeFile(normalizedPath, content, 'utf8');
      return true;
    },
  };
}

async function openTextDocument(uri: UriLike): Promise<TextDocumentLike> {
  const existing = textDocuments.find((doc) => doc.uri.fsPath === uri.fsPath);
  if (existing) {
    return existing;
  }
  const doc = createTextDocument(uri.fsPath);
  textDocuments.push(doc);
  return doc;
}

async function applyEdit(edit: WorkspaceEdit): Promise<boolean> {
  for (const operation of edit.operations) {
    const filePath = operation.uri.fsPath;
    if (operation.type === 'createFile') {
      await fs.promises.mkdir(path.dirname(filePath), { recursive: true });
      if (!fs.existsSync(filePath)) {
        await fs.promises.writeFile(filePath, '', 'utf8');
      }
      continue;
    }

    let content = fs.existsSync(filePath) ? await fs.promises.readFile(filePath, 'utf8') : '';

    if (operation.type === 'replace') {
      const start = positionToOffset(content, operation.range.start);
      const end = positionToOffset(content, operation.range.end);
      content = `${content.slice(0, start)}${operation.text}${content.slice(end)}`;
    } else if (operation.type === 'insert') {
      const offset = positionToOffset(content, operation.position);
      content = `${content.slice(0, offset)}${operation.text}${content.slice(offset)}`;
    }

    await fs.promises.mkdir(path.dirname(filePath), { recursive: true });
    await fs.promises.writeFile(filePath, content, 'utf8');
  }

  return true;
}

function loadAgentUiSettings(): { deepTask?: boolean } {
  try {
    if (!fs.existsSync(AGENT_UI_SETTINGS_FILE)) {
      return {};
    }
    return JSON.parse(fs.readFileSync(AGENT_UI_SETTINGS_FILE, 'utf8')) as { deepTask?: boolean };
  } catch {
    return {};
  }
}

export const Uri = {
  file(filePath: string): UriLike {
    return new UriImpl(path.resolve(filePath));
  },
};

export const workspace = {
  textDocuments,
  workspaceFolders: buildWorkspaceFolders(),
  getConfiguration(_section?: string) {
    return {
      get<T>(key: string, fallback?: T): T {
        if (key === 'deepTask') {
          const value = loadAgentUiSettings().deepTask;
          return (typeof value === 'boolean' ? value : fallback) as T;
        }
        return fallback as T;
      },
    };
  },
  async openTextDocument(uri: UriLike): Promise<TextDocumentLike> {
    return openTextDocument(uri);
  },
  async applyEdit(edit: WorkspaceEdit): Promise<boolean> {
    return applyEdit(edit);
  },
};

export const languages = {
  getDiagnostics(_uri?: UriLike): [] | Array<[UriLike, []]> {
    if (_uri) {
      return [];
    }
    return [];
  },
};

export const commands = {
  async executeCommand<T>(_command: string, ..._args: unknown[]): Promise<T | undefined> {
    return undefined;
  },
};

export const DiagnosticSeverity = {
  Error: 0,
  Warning: 1,
  Information: 2,
  Hint: 3,
} as const;

export type ExtensionContext = {
  globalState?: {
    get<T>(key: string): T | undefined;
    update(key: string, value: unknown): Promise<void>;
  };
};

export {
  Position,
  Range,
  WorkspaceEdit,
};

export default {
  Uri,
  workspace,
  languages,
  commands,
  DiagnosticSeverity,
  Position,
  Range,
  WorkspaceEdit,
};
