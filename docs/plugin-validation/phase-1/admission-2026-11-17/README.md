# 原包准备与准入证据

范围为 macOS arm64、Node v26.13.1 的原包准备、依赖解析、入口准入和配置源解析。**未运行原包激活、daemon 或 Electron 业务；没有业务通过结果。** 两组项目分别固定 Mgr 0.2.12 和 0.2.13，其余四包及版本相同；这是更新前后准备基线，不是在线更新验收。

## 实际执行

1. 对主清单的 6 个归档运行 `cargo run -p magi-plugin-system --example prepare_native_package --locked -- <archive> <integrity> <name> <version> <empty-stage>`。SHA-512、原包身份、声明结构由 Rust 校验，记录原始清单 SHA-256；不执行原包。
2. 在 `target/dsh-phase-1/projects/mgr-<version>` 创建本目录 `dependencies` 中保存的真实项目清单，使用 `npm install --ignore-scripts --no-audit --no-fund --prefix <project>` 解析依赖。没有使用 `--force` 或 `--legacy-peer-deps`。每组锁文件包含 199 项（含项目根和可选平台项），npm 本机实际安装 190 个包；此数不是业务指标。
3. 将 `runtime/dsh` 的宿主模块与其 `package.json` 放入该项目的 `runtime` 目录，使宿主和原包在同一项目解析 SDK。以 `node --experimental-import-meta-resolve` 调用 `inspectOriginalPackage`，输入 Rust 准备结果的原始清单摘要。两组各五个包的真实入口与 SDK 模块身份均通过；结果在 `results.json`。
4. 将每个原包的全部原始文件与 npm 安装内容逐一比对。两组均未改变任何原包文件，逐包文件数写入结果；没有代码改写、合成清单或包装插件入口。
5. 使用各版本 Mgr 自带的 `cordis.patch.yml` 验证锁定 Include 方言解析。条目与源文本摘要写入结果；没有应用加载图。

`results.json` 中路径相对仓库根。`dependencies/*/package.json` 与锁文件是准备项目的原样副本，恢复运行时应放回上述 `target` 项目路径，将主清单的原始归档放在 `target/dsh-phase-1/originals`，并先重新核对 integrity。未保留凭据或用户配置。

## SDK 解析规则

npm 默认的 peer SemVer 规则排除未显式包含该预发布版本的范围，首次解析因此在 WorkBuddy 的 `dsh-atomic-write` peer 失败。DSH 正式规则使用 `includePrerelease: true`，这些范围接受当前 0.2.2-rc.3 基线。因此准备项目使用本次 SDK 清单的精确版本 `overrides`，统一 Cordis／DSH 模块；原包版本要求再由正式准入检查校验，实际 SDK 地址也必须与宿主一致。其余第三方依赖仍按 npm 正常约束解析，不能全局跳过 peer 校验。

这是可复查的准备过程，**正式安装事务还未接线**。后续须把同一规则纳入唯一安装所有者，完成来源接纳、共享 SDK 的不可变身份、在线安装图替换及失败恢复；不能仅复制准备目录就宣称平台已切换。

## 相关检查

- 共享宿主 Node 回归：15 项通过，包含未执行原包的导入条件、清单变更、共享 SDK 身份、DSH 版本规则、配置表达式保留、非法与过量配置。
- Rust 共享宿主定向回归：4 项通过，包含真实控制连接上的原包准入与源文本解析。
- `cargo test -p magi-plugin-system -p magi-extension-host --all-targets --locked` 全部通过，保留现有包、资源、生命周期和宿主回归。
- `cargo clippy -p magi-extension-host -p magi-plugin-system --all-targets --locked -- -D warnings` 通过。

以上均不替代阶段 1 的原包业务任务。正式 Loader/Include、配置 CAS、运行图收敛、文件变更接纳、`flush`、UI 及旧路径删除仍未验收。
