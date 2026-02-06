# Docs 目录说明

## 目标

统一文档命名与分层，降低查找成本，避免根目录文档继续增长。

## 目录结构

```text
docs/
├── README.md
├── architecture/
├── workflow/
├── context/
├── runtime/
└── archive/
```

## 各目录职责

- `docs/architecture/`
  - 系统边界、子系统划分、消息响应设计等稳定架构文档。
- `docs/workflow/`
  - 工作流与 UX 配套规范、流程改造方案。
- `docs/context/`
  - 上下文与记忆系统设计、压缩格式规范。
- `docs/runtime/`
  - 运行时升级基线、验收标准、演进路线。
- `docs/archive/`
  - 历史方案、外部实验和不参与主链路的归档资料。

## 命名规范

- 使用 kebab-case。
- 文件名表达“主题 + 文档类型”，避免前缀冗余。
  - 示例：`unified-memory-plan.md`、`compression-format-reference.md`。
- 同一主题优先放入对应目录，不再在 `docs/` 根层平铺。

## 新增文档约束

- 新文档必须归入上述分层目录。
- 仅 `docs/README.md` 允许作为顶层总览。
- 需要跨文档引用时，优先使用仓库绝对路径（如 `docs/workflow/workflow-design.md`）。
