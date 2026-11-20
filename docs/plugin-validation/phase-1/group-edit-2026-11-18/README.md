# 整组来源候选与组合顺序

本批实现公开 Group 编辑所需的来源后端：完整成员数组作为一个候选进入已有来源日志与配置 CAS。尚未提供公开 `ctx.loader` 或 Group 对象，Mgr 核心业务与阶段 1 未通过。原包与运行身份、源码／依赖摘要及专项输出见 [result.json](result.json)。

## 根因与结果

原内部来源编辑只覆盖单行，不能保证完整组的增删、更新和排序一次接纳。实查还发现锁定 Include 的 `routeJournal` 不保存同父组内的位置变化，位置也没有计算补丁插入的行。现在位置针对完整组合列表，单行位置和整组顺序使用同一布局实现；需要跨 append 层时显式移动来源行，携带有效配置，保持稳定 ID，并保留最多可保留的原行所有者。上游 helper 与插件原包未改。

`reconcile` 在内存中准备所有变化，不写文件或运行条目。过期 `expectedEntries`、重复 ID、组外身份冲突、非法字段或未准入模块使整个候选失败。原有字段来源归属继续生效；明确移除的成员不再保留其专属补丁。组配置由父覆盖层拥有时，完整数组回到该覆盖层。保存正文重新编译后的有效组必须等于提案，然后才沿原 `pendingEdit`、文件保存、CAS 和串行应用入口推进，没有另一个组配置管理器或逐行提交路径。

运行回归中，一次动作同时改基础文件和补丁文件，保留成员读取到已提交的修订 2 后使用新配置，新成员启动、旧成员回收也都读取修订 2；包含未准入模块的整批则不改文件、调用绑定或执行日志。相同组重复提交不推进修订或重跑业务。

## 验证

- Node 39 项回归与 Include 生成物核对通过，含三个来源所有者的全部 24 种顺序、字段所属覆盖层、父配置覆盖、从被删除祖先移出成员、匿名身份、冲突、最终来源重编译及诊断保留。
- `cargo test -p magi-plugin-system -p magi-extension-host --all-targets --locked --quiet`：175 项通过，5 个需准备项目的原包专项默认忽略；加载运行回归 20 项。两 crate 的 clippy `-D warnings`、相关 Rust 格式和差异空白检查通过。
- 单独显式运行四个固定 Mgr 原包专项：原图应用、文件通知启停、单行来源编辑及新增整组来源替换。默认忽略的静态准备专项本轮未重跑。

固定 `dsh-plugin-mgr@0.2.13` 使用独立项目 `target/dsh-phase-1/projects/mgr-group-edit-2026-11-18`。执行前验证 tarball 与全部 9 个原包文件的安装字节，运行源码与本批最终源码一致，依赖锁未变。整组专项经过修订 1、2、3、4，依次为等待、撤除、以新激活身份恢复等待、清空；同时验证无变更不应用、旧实例不能读取新实例、原包清单和 bundle 字节不变。恢复后的实际依赖仍是 `loader`、`webServer`，业务为 `not-run`。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-group-edit-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host original_mgr_ \
  --locked -- --ignored --nocapture
```

最初逐行重建布局的测量暴露了额外开销，已改为整组索引与排序。最终 [benchmark.json](benchmark.json) 记录本机 4000 条全部停用的声明，分布在一个基础数组和一个补丁插入数组：无变更约 81 ms、反序约 66 ms。它只测纯配置准备，没有执行插件、Provider 或 UI，不是插件支持率或业务性能指标，也不是跨平台预算承诺。

## 未完成范围

来源候选和内部图执行不等于公开 Loader／Entry／Group 对象。所需选项形态、宿主内存根树动作、Include `flush`、动态来源、安装事务和发行 daemon 协调器仍需完成；内存根动作不能通过重定向到 Include 子树冒充实现。真实 Electron、Mgr 页面及安装／升级／卸载业务、完整应用重启、Windows/Linux 尚未验收，不能将本批结果计入原包核心业务通过率。
