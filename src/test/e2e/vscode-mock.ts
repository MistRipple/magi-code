/**
 * VSCode API Mock
 * 用于在 Node.js 环境中运行 E2E 测试
 */

// Mock DiagnosticSeverity
export const DiagnosticSeverity = {
  Error: 0,
  Warning: 1,
  Information: 2,
  Hint: 3
};

// Mock Uri
export class Uri {
  readonly scheme: string;
  readonly authority: string;
  readonly path: string;
  readonly query: string;
  readonly fragment: string;
  readonly fsPath: string;

  private constructor(scheme: string, authority: string, path: string, query: string, fragment: string) {
    this.scheme = scheme;
    this.authority = authority;
    this.path = path;
    this.query = query;
    this.fragment = fragment;
    this.fsPath = path;
  }

  static file(path: string): Uri {
    return new Uri('file', '', path, '', '');
  }

  static parse(value: string): Uri {
    return new Uri('file', '', value, '', '');
  }

  toString(): string {
    return `${this.scheme}://${this.path}`;
  }
}

// Mock languages
export const languages = {
  getDiagnostics: (): Array<[Uri, any[]]> => {
    // 返回空诊断列表
    return [];
  }
};

// Mock window
export const window = {
  showInformationMessage: async (message: string) => {
    console.log(`[INFO] ${message}`);
    return undefined;
  },
  showWarningMessage: async (message: string) => {
    console.log(`[WARN] ${message}`);
    return undefined;
  },
  showErrorMessage: async (message: string) => {
    console.log(`[ERROR] ${message}`);
    return undefined;
  },
  createOutputChannel: (name: string) => ({
    appendLine: (value: string) => console.log(`[${name}] ${value}`),
    append: (value: string) => process.stdout.write(value),
    clear: () => {},
    show: () => {},
    hide: () => {},
    dispose: () => {}
  }),
  withProgress: async <T>(options: any, task: (progress: any) => Promise<T>): Promise<T> => {
    return task({
      report: (value: { message?: string; increment?: number }) => {
        if (value.message) console.log(`[Progress] ${value.message}`);
      }
    });
  }
};

// Mock workspace
export const workspace = {
  workspaceFolders: undefined as any,
  getConfiguration: (section?: string) => ({
    get: <T>(key: string, defaultValue?: T): T | undefined => defaultValue,
    has: (key: string) => false,
    update: async () => {}
  }),
  onDidChangeConfiguration: () => ({ dispose: () => {} }),
  fs: {
    readFile: async (uri: Uri) => Buffer.from(''),
    writeFile: async (uri: Uri, content: Uint8Array) => {},
    stat: async (uri: Uri) => ({ type: 1, ctime: 0, mtime: 0, size: 0 }),
    readDirectory: async (uri: Uri) => []
  }
};

// Mock commands
export const commands = {
  registerCommand: (command: string, callback: (...args: any[]) => any) => ({
    dispose: () => {}
  }),
  executeCommand: async <T>(command: string, ...args: any[]): Promise<T | undefined> => {
    return undefined;
  }
};

// Mock ProgressLocation
export const ProgressLocation = {
  Notification: 15,
  SourceControl: 1,
  Window: 10
};

// Mock EventEmitter
export class EventEmitter<T> {
  private listeners: Array<(e: T) => any> = [];
  
  event = (listener: (e: T) => any) => {
    this.listeners.push(listener);
    return { dispose: () => {
      const idx = this.listeners.indexOf(listener);
      if (idx >= 0) this.listeners.splice(idx, 1);
    }};
  };
  
  fire(data: T): void {
    this.listeners.forEach(l => l(data));
  }
  
  dispose(): void {
    this.listeners = [];
  }
}

// 默认导出
export default {
  DiagnosticSeverity,
  Uri,
  languages,
  window,
  workspace,
  commands,
  ProgressLocation,
  EventEmitter
};

