// 只用于原包明确接纳 null 的条件订阅。返回不可用，不注册一个永不触发的
// “成功”监听；必需订阅、未知事件仍由正式准入及运行期事件检查拒绝。
import { EventsService } from '@deepseek-ai/cordis';

export function installNullableEvents(ctx, names = [], diagnostic = () => {}) {
  const unavailable = new Set(names);
  for (const method of ['on', 'once']) {
    ctx[method] = function (name, ...args) {
      if (unavailable.has(name)) {
        diagnostic(`DSH 事件 ${name} 不可用，沿原包声明返回 null`);
        return null;
      }
      // ctx.on 是 EventService 的 mixin；使用原型方法，保留当前调用方的
      // traceable service receiver，不能捕获绑定到 root 的方法。
      return EventsService.prototype[method].call(this, name, ...args);
    };
  }
}
