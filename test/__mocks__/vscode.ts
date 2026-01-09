/**
 * VSCode 模块 Mock
 * 用于在非 VSCode 环境下运行测试
 */

export const languages = {
  getDiagnostics: () => [],
};

export const DiagnosticSeverity = {
  Error: 0,
  Warning: 1,
  Information: 2,
  Hint: 3,
};

export const Uri = {
  file: (path: string) => ({ fsPath: path, path }),
  parse: (str: string) => ({ fsPath: str, path: str }),
};

export const workspace = {
  workspaceFolders: [],
  getConfiguration: () => ({
    get: () => undefined,
    update: () => Promise.resolve(),
  }),
  onDidChangeConfiguration: () => ({ dispose: () => {} }),
};

export const window = {
  showInformationMessage: () => Promise.resolve(),
  showWarningMessage: () => Promise.resolve(),
  showErrorMessage: () => Promise.resolve(),
  createOutputChannel: () => ({
    appendLine: () => {},
    show: () => {},
    dispose: () => {},
  }),
};

export const commands = {
  registerCommand: () => ({ dispose: () => {} }),
  executeCommand: () => Promise.resolve(),
};

export const EventEmitter = class {
  event = () => {};
  fire() {}
  dispose() {}
};

export default {
  languages,
  DiagnosticSeverity,
  Uri,
  workspace,
  window,
  commands,
  EventEmitter,
};

