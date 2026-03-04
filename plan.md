# 本地检索与辅助模型增强方案（当前基线）

## 目标
统一检索路线为本地基础设施，不引入向量检索通道；通过“本地索引 + 辅助模型增强”提升可用性与稳定性。

## 当前架构

```text
codebase_retrieval
  ├─ L1: PKB 本地搜索 (InvertedIndex + SymbolIndex + DependencyGraph)
  ├─ L2: grep_search 精确匹配
  └─ L3: lsp_query 符号查询
```

三路并行执行，结果按预算聚合并去重。

## 辅助模型的作用（已实现）

辅助模型不是向量检索，而是用于增强本地检索效果：

1. 查询扩展（`QueryExpander`）
- 离线同义词扩展
- 可选 LLM 扩展与意图分析
- 返回 `weightHints`，影响排序权重

2. 语义重排（`SemanticReranker`）
- 在本地召回结果上进行语义精排
- 不改变索引结构，仅优化结果顺序

3. 知识提取（`ProjectKnowledgeBase`）
- 从会话中提取 ADR/FAQ/Learning
- 作为项目上下文补充，服务后续问答与检索

## 索引与更新链路

1. 启动时初始化 `ProjectKnowledgeBase`
2. `LocalSearchEngine.buildIndex()` 优先恢复持久化快照
3. 快照不可用或变化比例过高时全量重建
4. 文件监听触发 `changed/created/deleted` 增量更新
5. 索引与扩展缓存防抖落盘

## 持久化策略

- 路径：`.magi/cache/search-index.json.gz`
- 包含倒排索引、符号索引、依赖图、文件清单、查询扩展缓存
- 恢复时执行新鲜度校验并增量同步

## 明确约束

1. 不实现向量检索相关能力
2. 不新增向量检索配置项
3. 不在 UI 展示向量检索设置
4. 继续以本地检索基础设施作为主路径

## 维护建议

1. 持续优化 `QueryExpander` 的项目词汇注入与同义词质量
2. 继续收敛 `codebase_retrieval` 输出格式，降低噪声
3. 保持 L1/L2/L3 的并行与预算策略稳定，不引入额外检索层
