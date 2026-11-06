import type { Context } from '@deepseek-ai/cordis'
import { PiAiAdapter } from '@deepseek-ai/dsh-llm-pi-ai'
import { readFile } from 'node:fs/promises'
import helper from './helper'
import { mystery } from '@deepseek-ai/dsh-mystery'

export const name = 'mini-dsh-plugin'
export const inject = ['llm', 'settings']

export function apply(ctx: Context) {
  ctx.logger.warn('hello')
  ctx.effect(() => () => {})
  ctx.llm.registerAdapter(['mini'], new PiAiAdapter({}))
  ctx.inject(['tools'], (toolsCtx) => { toolsCtx.tools.register({}) })
  ctx.webServer.register('/x', () => {})
  ctx.webServer.registerFallback(() => {})
  void readFile; void helper; void mystery
}
