export interface CapabilityEntrypoint {
  capability: 'workspace' | 'fs' | 'terminal' | 'git' | 'lsp' | 'diagnostics';
  file: string;
  responsibility: string;
  migrationPriority: 'high' | 'medium' | 'low';
}

export const CAPABILITY_ENTRYPOINTS: CapabilityEntrypoint[] = [
  {
    capability: 'fs',
    file: 'src/tools/file-executor.ts',
    responsibility: '文件读取、创建、编辑、插入、删除等工程写操作入口',
    migrationPriority: 'high',
  },
  {
    capability: 'lsp',
    file: 'src/tools/lsp-executor.ts',
    responsibility: '代码智能查询与语言服务宿主调用入口',
    migrationPriority: 'high',
  },
  {
    capability: 'diagnostics',
    file: 'src/host/runtime-host.ts',
    responsibility: '诊断能力宿主契约与运行时注入入口',
    migrationPriority: 'high',
  },
  {
    capability: 'workspace',
    file: 'src/host/runtime-host.ts',
    responsibility: '当前工作区、会话恢复与宿主能力聚合入口',
    migrationPriority: 'high',
  },
  {
    capability: 'terminal',
    file: 'src/tools/tool-manager.ts',
    responsibility: 'Shell、终端进程管理与执行权限治理入口',
    migrationPriority: 'high',
  },
  {
    capability: 'git',
    file: 'src/host/runtime-host.ts',
    responsibility: 'Git 工作区隔离与 worktree 生命周期宿主入口',
    migrationPriority: 'high',
  },
];
