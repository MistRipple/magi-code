import { LocalAgentService } from './service/local-agent-service';
import { DEFAULT_AGENT_PORT } from './config';

const DEFAULT_AGENT_LISTEN_HOST = '0.0.0.0';

function parseArg(name: string): string | undefined {
  const entry = process.argv.find((value) => value.startsWith(`--${name}=`));
  if (!entry) {
    return undefined;
  }
  return entry.slice(name.length + 3);
}

async function main(): Promise<void> {
  const portRaw = parseArg('port');
  const port = portRaw ? Number(portRaw) : DEFAULT_AGENT_PORT;
  const service = new LocalAgentService(
    Number.isFinite(port) ? port : DEFAULT_AGENT_PORT,
    DEFAULT_AGENT_LISTEN_HOST,
  );

  const workspaces = process.env.MAGI_AGENT_WORKSPACES
    ? JSON.parse(process.env.MAGI_AGENT_WORKSPACES) as Array<{ rootPath: string; name?: string }>
    : [];

  for (const workspace of workspaces) {
    if (workspace?.rootPath) {
      service.registerWorkspace(workspace.rootPath, workspace.name);
    }
  }

  let shutdownPromise: Promise<void> | null = null;
  const shutdown = (signal: 'SIGINT' | 'SIGTERM') => {
    if (shutdownPromise) {
      return;
    }
    shutdownPromise = service.stop()
      .catch((error) => {
        console.error(`[magi-agent] 停止失败 (${signal}):`, error);
      })
      .finally(() => {
        process.exit(0);
      });
  };

  process.once('SIGINT', () => {
    shutdown('SIGINT');
  });
  process.once('SIGTERM', () => {
    shutdown('SIGTERM');
  });

  await service.start();
}

void main().catch((error) => {
  console.error('[magi-agent] 启动失败:', error);
  process.exit(1);
});
