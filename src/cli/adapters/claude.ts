import { spawn } from 'child_process';
import { PersistentSessionAdapter, PersistentAdapterConfig } from './persistent-session-adapter';

export class ClaudeAdapter extends PersistentSessionAdapter {
  static async checkInstalled(): Promise<boolean> {
    return new Promise((resolve) => {
      const proc = spawn('claude', ['--version'], { stdio: ['ignore', 'pipe', 'pipe'] });
      proc.on('error', () => resolve(false));
      proc.on('close', (code) => resolve(code === 0));
      setTimeout(() => { proc.kill(); resolve(false); }, 3000);
    });
  }

  constructor(config: PersistentAdapterConfig) {
    super('claude', config);
  }
}
