<script lang="ts">
  import { onMount } from 'svelte';
  import { Terminal, type ITheme } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import '@xterm/xterm/css/xterm.css';
  import { i18n } from '../../stores/i18n.svelte';
  import { terminalChannelUrl } from '../../web/agent-api';

  interface Props {
    terminalTabId: string;
    workspaceId?: string;
    workspacePath?: string;
    sessionId: string;
  }

  let { terminalTabId, workspaceId, workspacePath = '', sessionId }: Props = $props();
  let terminalHost: HTMLDivElement;
  let accessibleStatus = $state(i18n.t('terminalPanel.status.connecting'));

  function cssValue(style: CSSStyleDeclaration, name: string, fallback: string): string {
    return style.getPropertyValue(name).trim() || fallback;
  }

  function createTerminalTheme(style: CSSStyleDeclaration): ITheme {
    return {
      background: 'rgba(0, 0, 0, 0)',
      foreground: cssValue(style, '--foreground', '#d4d4d4'),
      cursor: cssValue(style, '--foreground', '#d4d4d4'),
      cursorAccent: cssValue(style, '--background', '#1e1e1e'),
      selectionBackground: cssValue(style, '--surface-selected', 'rgba(59, 130, 246, 0.28)'),
      black: cssValue(style, '--foreground-muted', '#808080'),
      red: cssValue(style, '--error', '#ef4444'),
      green: cssValue(style, '--success', '#10b981'),
      yellow: cssValue(style, '--warning', '#f59e0b'),
      blue: cssValue(style, '--info', '#3b82f6'),
      magenta: cssValue(style, '--primary', '#8b5cf6'),
      cyan: cssValue(style, '--color-codex', '#10a37f'),
      white: cssValue(style, '--foreground', '#d4d4d4'),
      brightBlack: cssValue(style, '--foreground-muted', '#808080'),
      brightRed: cssValue(style, '--error', '#ef4444'),
      brightGreen: cssValue(style, '--success', '#10b981'),
      brightYellow: cssValue(style, '--warning', '#f59e0b'),
      brightBlue: cssValue(style, '--info', '#3b82f6'),
      brightMagenta: cssValue(style, '--primary', '#8b5cf6'),
      brightCyan: cssValue(style, '--color-codex', '#10a37f'),
      brightWhite: cssValue(style, '--foreground', '#ffffff'),
    };
  }

  function terminalErrorMessage(value: unknown): string {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return '';
    const message = (value as Record<string, unknown>).message;
    return typeof message === 'string' ? message.trim() : '';
  }

  onMount(() => {
    const computed = getComputedStyle(terminalHost);
    const terminal = new Terminal({
      allowTransparency: true,
      cursorBlink: true,
      cursorStyle: 'block',
      fontFamily: cssValue(computed, '--font-mono', "'SFMono-Regular', Consolas, monospace"),
      fontSize: 13,
      lineHeight: 1.25,
      macOptionIsMeta: true,
      scrollback: 10_000,
      tabStopWidth: 4,
      theme: createTerminalTheme(computed),
    });
    const fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    terminal.open(terminalHost);

    const encoder = new TextEncoder();
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let reconnectAttempt = 0;
    let connectionGeneration = 0;
    let disposed = false;
    let terminalEnded = false;
    let resizeFrame: number | null = null;

    function sendResize(): void {
      if (!socket || socket.readyState !== WebSocket.OPEN || terminal.cols < 2 || terminal.rows < 2) {
        return;
      }
      socket.send(JSON.stringify({
        type: 'resize',
        cols: terminal.cols,
        rows: terminal.rows,
      }));
    }

    function fit(): void {
      if (disposed || terminalHost.clientWidth === 0 || terminalHost.clientHeight === 0) return;
      try {
        fitAddon.fit();
        sendResize();
      } catch (error) {
        console.warn('[TerminalTab] 调整终端尺寸失败:', error);
      }
    }

    function scheduleFit(): void {
      if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
      resizeFrame = requestAnimationFrame(() => {
        resizeFrame = null;
        fit();
      });
    }

    function syncAppearance(): void {
      const next = getComputedStyle(terminalHost);
      terminal.options.theme = createTerminalTheme(next);
      terminal.options.fontFamily = cssValue(
        next,
        '--font-mono',
        "'SFMono-Regular', Consolas, monospace",
      );
      scheduleFit();
    }

    function scheduleReconnect(generation: number): void {
      if (
        disposed
        || terminalEnded
        || generation !== connectionGeneration
        || reconnectTimer !== null
      ) {
        return;
      }
      const delay = Math.min(10_000, 500 * 2 ** Math.min(reconnectAttempt, 5));
      reconnectAttempt += 1;
      accessibleStatus = i18n.t('terminalPanel.status.reconnecting');
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    }

    function writeTerminalNotice(message: string): void {
      terminal.writeln(`\r\n\x1b[33m${message}\x1b[0m`);
    }

    function connect(): void {
      if (disposed || terminalEnded) return;
      const generation = ++connectionGeneration;
      accessibleStatus = i18n.t('terminalPanel.status.connecting');
      const next = new WebSocket(terminalChannelUrl({
        terminalTabId,
        workspaceId,
        workspacePath,
        sessionId,
        cols: terminal.cols,
        rows: terminal.rows,
      }));
      next.binaryType = 'arraybuffer';
      socket = next;
      next.onopen = () => {
        if (disposed || socket !== next || generation !== connectionGeneration) return;
        reconnectAttempt = 0;
        terminal.reset();
        accessibleStatus = i18n.t('terminalPanel.status.connected');
        sendResize();
        terminal.focus();
      };
      next.onmessage = (event) => {
        if (disposed || socket !== next || generation !== connectionGeneration) return;
        if (event.data instanceof ArrayBuffer) {
          terminal.write(new Uint8Array(event.data));
          return;
        }
        if (typeof event.data !== 'string') return;
        try {
          const message = JSON.parse(event.data) as {
            type?: string;
            code?: string;
            message?: string;
            exitCode?: number;
            signal?: string | null;
          };
          if (message.type === 'ready') {
            accessibleStatus = i18n.t('terminalPanel.status.connected');
            return;
          }
          if (message.type === 'exit') {
            terminalEnded = true;
            accessibleStatus = i18n.t('terminalPanel.status.exited');
            writeTerminalNotice(i18n.t('terminalPanel.exit', { code: message.exitCode ?? 0 }));
            return;
          }
          if (message.type === 'closed') {
            terminalEnded = true;
            accessibleStatus = i18n.t('terminalPanel.status.closed');
            return;
          }
          if (message.type === 'error') {
            const detail = terminalErrorMessage(message) || i18n.t('terminalPanel.error.connection');
            writeTerminalNotice(detail);
            if (message.code === 'terminal_runtime_failed') terminalEnded = true;
          }
        } catch (error) {
          console.warn('[TerminalTab] 解析终端消息失败:', error);
        }
      };
      next.onerror = () => {
        if (socket === next) accessibleStatus = i18n.t('terminalPanel.status.reconnecting');
      };
      next.onclose = () => {
        if (socket === next) socket = null;
        scheduleReconnect(generation);
      };
    }

    const dataSubscription = terminal.onData((data) => {
      if (socket?.readyState === WebSocket.OPEN) socket.send(encoder.encode(data));
    });
    const binarySubscription = terminal.onBinary((data) => {
      if (socket?.readyState !== WebSocket.OPEN) return;
      socket.send(Uint8Array.from(data, (character) => character.charCodeAt(0)));
    });
    const resizeSubscription = terminal.onResize(sendResize);
    const resizeObserver = new ResizeObserver(scheduleFit);
    const appearanceObserver = new MutationObserver(syncAppearance);
    resizeObserver.observe(terminalHost);
    appearanceObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-theme'],
    });
    appearanceObserver.observe(document.body, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-theme'],
    });
    scheduleFit();
    connect();

    return () => {
      disposed = true;
      connectionGeneration += 1;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
      resizeObserver.disconnect();
      appearanceObserver.disconnect();
      dataSubscription.dispose();
      binarySubscription.dispose();
      resizeSubscription.dispose();
      socket?.close();
      terminal.dispose();
    };
  });
</script>

<div class="terminal-pane">
  <div
    class="terminal-host"
    bind:this={terminalHost}
    role="application"
    aria-label={i18n.t('terminalPanel.title')}
  ></div>
  <span class="terminal-status" aria-live="polite">{accessibleStatus}</span>
</div>

<style>
  .terminal-pane {
    position: relative;
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: color-mix(in srgb, var(--background) 76%, transparent);
  }

  .terminal-host {
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    padding: 10px 8px 8px;
    box-sizing: border-box;
    overflow: hidden;
  }

  .terminal-status {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  :global(.terminal-host .xterm),
  :global(.terminal-host .xterm-screen),
  :global(.terminal-host .xterm-viewport) {
    height: 100%;
  }

  :global(.terminal-host .xterm-viewport) {
    background-color: transparent !important;
  }

  :global(.terminal-host .xterm .xterm-helper-textarea:focus) {
    outline: none;
  }
</style>
