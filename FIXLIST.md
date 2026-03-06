# Magi 稳定性验收清单

更新时间：2026-03-06
范围：Plan Ledger 稳定性增益（缓存治理 + 事件轮转 + 回归脚本）

## P0 已完成（代码层）

- [x] 会话级缓存上限治理：引入会话 LRU 访问序与总会话缓存上限，避免长会话内存持续增长。
- [x] 会话内 Plan 缓存上限治理：限制单会话 plan 缓存数量，按访问顺序淘汰旧记录。
- [x] 队列尾清理：会话写队列完成后释放引用，避免 `writeQueues` 残留累积。
- [x] `events` 轮转命名增强：使用 `timestamp-random`，规避同毫秒轮转命名冲突。
- [x] `events` 轮转筛选修复：仅将带时间戳后缀的历史文件纳入清理，避免误把当前活跃 `*.events.jsonl` 当作轮转文件。

## P0 已完成（回归能力）

- [x] 新增 `verify:e2e:plan-ledger-events-rotate`，验证：
- [x] 达阈值后触发轮转。
- [x] 历史轮转文件数量受 `keep` 上限约束。
- [x] 轮转后当前 `events` 文件仍持续可写。

## 回归命令（可直接执行）

```bash /Users/xie/code/magi
npm run -s compile
npm run -s build:extension
npm run -s build:webview
npm run -s verify:e2e:plan-ledger-lifecycle
npm run -s verify:e2e:plan-ledger-session-isolation
npm run -s verify:e2e:plan-ledger-reconcile
npm run -s verify:e2e:plan-ledger-events-rotate
```

## 人工验收建议（发布前）

- [ ] 连续切换 20+ 会话，观察任务面板计划账本加载是否稳定，无跨会话污染。
- [ ] 长会话压测（持续 30~60 分钟）观察内存曲线，确认无明显持续上升趋势。
- [ ] 高并发任务执行期间检查计划账本状态流转：`draft -> awaiting_confirmation -> approved -> executing -> completed/failed` 无逆序或跳变。

