import * as os from 'os';
import * as path from 'path';
import { DEFAULT_AGENT_HOST, DEFAULT_AGENT_PORT, getDefaultAgentBaseUrl } from '../shared/agent-shared-config';

export const AGENT_VERSION = '0.1.0';
export const MAGI_HOME_DIR = path.join(os.homedir(), '.magi');
export const AGENT_STATE_DIR = path.join(MAGI_HOME_DIR, 'agent');
export const AGENT_PID_FILE = path.join(AGENT_STATE_DIR, 'agent.pid');
export const AGENT_LOG_FILE = path.join(AGENT_STATE_DIR, 'agent.log');
export const AGENT_RUNTIME_FILE = path.join(AGENT_STATE_DIR, 'runtime.json');
export const AGENT_LAUNCH_LOCK_FILE = path.join(AGENT_STATE_DIR, 'launch.lock');
export const AGENT_WORKSPACES_FILE = path.join(AGENT_STATE_DIR, 'workspaces.json');
export const AGENT_UI_SETTINGS_FILE = path.join(AGENT_STATE_DIR, 'ui-settings.json');
export const AGENT_CLIENTS_DIR = path.join(AGENT_STATE_DIR, 'clients');

export { DEFAULT_AGENT_HOST, DEFAULT_AGENT_PORT, getDefaultAgentBaseUrl };
