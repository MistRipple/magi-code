// 翻译器工厂的统一入口：新增上游生态只需在这里注册一个工厂。
import { TranslatorRegistry } from './core/registry.mjs';
import { createDshTranslator } from './dsh/translator.mjs';
import { createPiTranslator } from './pi/translator.mjs';

export function createRegistry() {
  return new TranslatorRegistry().register('dsh', createDshTranslator).register('pi', createPiTranslator);
}
