// Installs window.__ModuleLoader__ before any bundled or external client script runs.

import * as React from 'react';
import * as ReactDOM from 'react-dom';
import * as ReactDOMClient from 'react-dom/client';
import * as ReactJsxRuntime from 'react/jsx-runtime';
import * as Cordis from '@deepseek-ai/cordis';
import * as Cosmokit from '@deepseek-ai/cosmokit';
import Schema from '@deepseek-ai/schemastery';
import * as ClientStore from '@deepseek-ai/dsh-client-store';
import * as ClientUiSlots from '@deepseek-ai/dsh-client-ui-slots';
import * as TypertProtocol from '@deepseek-ai/dsh-typert-protocol';
import * as Zod from 'zod';
import * as Primitives from '@deepseek-ai/dsh-client-ui-primitives';

const BUILTIN_MODULE_TABLE = Object.freeze({
  react: React,
  'react-dom': ReactDOM,
  'react-dom/client': ReactDOMClient,
  'react/jsx-runtime': ReactJsxRuntime,
  'react/jsx-dev-runtime': ReactJsxRuntime,
  '@deepseek-ai/cordis': Cordis,
  '@deepseek-ai/cosmokit': Cosmokit,
  '@deepseek-ai/schemastery': Schema,
  '@deepseek-ai/dsh-client-store': ClientStore,
  '@deepseek-ai/dsh-client-ui-slots': ClientUiSlots,
  '@deepseek-ai/dsh-client-ui-primitives': Primitives,
  '@deepseek-ai/dsh-typert-protocol': TypertProtocol,
  zod: Zod,
});

export const loadedDefinitions = new Map();
const evaluatedExports = new Map();
export const loadListeners = new Set();

export function requireModule(specifier) {
  if (Object.hasOwn(BUILTIN_MODULE_TABLE, specifier)) {
    return BUILTIN_MODULE_TABLE[specifier];
  }
  if (evaluatedExports.has(specifier)) {
    return evaluatedExports.get(specifier);
  }
  const def = loadedDefinitions.get(specifier)
    ?? (specifier.endsWith('/client') ? loadedDefinitions.get(specifier.slice(0, -'/client'.length)) : undefined);
  if (def) {
    const out = def.factory(requireModule);
    evaluatedExports.set(specifier, out);
    return out;
  }
  throw new Error(`[dsh-client-host] Module "${specifier}" is not available in the locked client runtime`);
}

globalThis.__ModuleLoader__ = {
  load(definition) {
    if (!definition || typeof definition.id !== 'string' || typeof definition.factory !== 'function') {
      throw new TypeError('Invalid __ModuleLoader__.load() definition');
    }
    loadedDefinitions.set(definition.id, definition);
    for (const fn of [...loadListeners]) {
      try { fn(definition.id); } catch (err) { console.error('[dsh-client-host] module load listener error:', err); }
    }
  },
  require: requireModule,
};
