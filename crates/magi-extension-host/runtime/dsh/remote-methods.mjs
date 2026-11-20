import { Remote } from '@deepseek-ai/dsh-typert-protocol';

// Declare a prototype method as a Typert remote method without decorator syntax.
export function markRemoteMethod(prototype, method, options) {
  const decorator = options === undefined ? Remote(method) : Remote(options);
  decorator(prototype[method], {
    kind: 'method',
    name: method,
    private: false,
    static: false,
    addInitializer(fn) {
      fn.call(Object.create(prototype));
    },
  });
}
