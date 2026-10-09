# GitHub 新版本发布流程

本文是 Magi 正式稳定版的操作手册和完成门禁。发布人员和自动化代理必须按顺序执行。日常开发及单纯修改文档无需执行整套发布预检。

**主流程：检查工作区 → 准备版本与说明 → 本地完整预检 → 推送 main → 同一提交 CI 全部通过 → 推送版本 Tag → 四平台打包与发布 → 在线验收。** 上一步未通过，不进入下一步。推送 Tag、安装包构建成功或出现 Release 页面，都不能单独作为发布完成依据。

## 1. 发布边界与事实来源

- 发布仓库固定为 MistRipple/magi-code，正式发布提交必须位于 main。
- 正式发行物只有 Electron。magi-daemon 是业务内核，由 Electron Main 托管 sidecar；不再发布 Tauri、CEF 或 Browser Runtime 独立产品。
- Electron 安装包不做 Apple/Windows 代码签名和公证。本地打包关闭 macOS 证书自动发现。旧版 Tauri 迁移桥仍需保留旧更新器使用的 .sig；它与操作系统代码签名是两回事。
- 提交及附注 Tag 的身份使用 **MistRipple &lt;east_xiaodong@126.com&gt;**，不得使用 Codex &lt;codex@openai.com&gt;。
- 已公开交付版本的内容保持不变。产品或安装包需要修正时发布更高版本；尚未成功交付的失败版本按第 8 节恢复。
- 当前工作流会设置 Latest 并覆盖稳定 Feed。虽然存在预发布标签识别表达式，但没有隔离稳定 Feed；**不要直接用本流程发布 alpha、beta、rc、test 版本**。预发布须先实现独立频道并验证。

| 内容 | 事实来源 | 发布要求 |
| --- | --- | --- |
| 产品版本 | 根 Cargo.toml 的 workspace.package.version | 由 [product-version.mjs](../scripts/product-version.mjs) 读取，与 Tag 和说明文件名一致 |
| Rust 锁文件 | 根 Cargo.lock | 工作区包版本随发版更新；验证使用 --locked |
| npm 依赖 | 根 package-lock.json | 使用根目录 npm ci 安装，不无条件升级依赖 |
| npm 包版本 | 根与 Desktop package.json 的 0.0.0 占位版本 | 不改成产品版本；Web 不新增产品版本字段 |
| 安装包版本 | Desktop 构建脚本及 Electron Builder extraMetadata | 从 Cargo 注入，不手动编辑生成物 |
| 发布说明 | .github/releases/vX.Y.Z.md | 非空，描述功能、修复、升级与兼容影响 |
| 本地预检 | [release-preflight.mjs](../scripts/release-preflight.mjs) | 完整命令包含打包和审计 |
| CI 与安全 | [ci.yml](../.github/workflows/ci.yml)、[security.yml](../.github/workflows/security.yml) | 验证最终发布 SHA |
| 发布工作流 | [release.yml](../.github/workflows/release.yml) | 四平台构建、启动、发布及更新源维护 |
| 产物命名 | [electron-builder.yml](../apps/desktop/electron-builder.yml) | 与 Electron 清单及迁移脚本一致 |
| 旧版迁移 | [build-legacy-desktop-bridge.mjs](../scripts/build-legacy-desktop-bridge.mjs) | 四个迁移包、四份 .sig、八个平台入口 |

发布配置或产物规则变化时同步更新本手册。根 [AGENTS.md](../AGENTS.md) 和 [文档入口](README.md) 已指向此文件，不另建第二套发布规则。

## 2. 开始前检查

以下命令在仓库根目录的 **Bash** 中运行；Windows 可使用 Git Bash。按步骤执行，不把整篇文档一次性粘贴运行。任何命令失败即停止，先查明原因。各步骤沿用同一终端中的 RELEASE_* 变量。

~~~bash
set -euo pipefail
RELEASE_REPO=MistRipple/magi-code

git status --short
git branch --show-current
git remote -v
git config user.name
git config user.email
gh auth status
git fetch origin main --tags
gh release list --repo "$RELEASE_REPO" --limit 10
node scripts/product-version.mjs
~~~

确认 origin 指向指定仓库，GitHub 账户有推送、运行 Actions 和发布 Release 的权限。逐项识别已有改动，只纳入本次发布的明确内容；不要使用 git add .、reset --hard、clean 或强制覆盖处理其他人的改动。无法得到明确发布快照时，在独立工作副本整理待发布提交，完成门禁后再推进 main。

从干净的 main 开始时可执行 git pull --ff-only origin main。分支分叉、冲突或远端推进时先整合，再验证；不强推 main。对比最近成功发布 Tag 与待发布提交，确定发布说明范围，不照抄历史说明。

身份不正确时，仅设置本仓库配置：

~~~bash
git config user.name MistRipple
git config user.email east_xiaodong@126.com
~~~

### 环境、磁盘与凭据

- Node.js 建议与 CI 对齐，使用 22.x；根 package.json 声明最低版本为 >=22。
- Rust 以 rust-toolchain.toml 为准，目前为 1.97.0，安装 clippy、rustfmt；预检脚本与工作流必须对齐。
- 安装 cargo-audit，当前安全工作流固定为 0.22.2。另需 Git、GitHub CLI、jq、curl 及当前平台原生构建工具；macOS 需要 Xcode Command Line Tools。
- 检查磁盘空间。Rust debug/release 缓存、Electron 解包目录和安装包会同时占用空间。只清理已确认可重建、没有进程使用的生成缓存，不删除状态目录、工作区数据或未提交文件。
- 本地完整预检构建当前操作系统与架构的安装包；跨平台发行包由 GitHub 构建，不用本机交叉打包替代。

~~~bash
node --version
npm --version
rustup show active-toolchain
cargo audit --version
df -h .
npm ci
~~~

缺少工具时按当前仓库版本安装，例如：

~~~bash
rustup toolchain install 1.97.0 --profile minimal --component clippy --component rustfmt
cargo +1.97.0 install cargo-audit --version 0.22.2 --locked
~~~

发布仓库必须配置 TAURI_SIGNING_PRIVATE_KEY；私钥有密码时还需正确设置 TAURI_SIGNING_PRIVATE_KEY_PASSWORD。可用 gh secret list --repo "$RELEASE_REPO" 核对名称，不读取、打印或保存私钥内容。工作流需要 contents: write、actions: read、attestations: write、id-token: write 权限。

## 3. 准备版本、锁文件和发布说明

1. 新版本通常递增补丁号；兼容性变化按产品策略选择更高版本。恢复失败版本时先阅读第 8 节。
2. 修改根 Cargo.toml 的 workspace.package.version，不批量替换历史文档或全部版本字符串。
3. 刷新 Cargo 工作区包版本，检查锁文件差异。不要为了发版无条件执行 cargo update 或 npm update；依赖升级应是明确、可审查的变更。
4. 新建 .github/releases/vX.Y.Z.md，写明新增、修复、平台支持和升级影响。涉及旧持久化格式、数据迁移或不可逆变更时，明确备份与兼容边界。

~~~bash
cargo metadata --format-version 1 --no-deps > /dev/null
RELEASE_VERSION="$(node scripts/product-version.mjs)"
RELEASE_TAG="v$RELEASE_VERSION"
[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
test -s ".github/releases/$RELEASE_TAG.md"
git diff -- Cargo.toml Cargo.lock package-lock.json
git diff --check
git tag --list "$RELEASE_TAG"
git ls-remote origin "refs/tags/$RELEASE_TAG" "refs/tags/$RELEASE_TAG^{}"
~~~

确认没有意外依赖升级，继承 workspace 版本的 crate 已反映到 Cargo.lock。新版本 Tag 应不存在；已存在时不能直接覆盖，按失败恢复或更高版本发布处理。

## 4. 执行完整本地预检

~~~bash
export CSC_IDENTITY_AUTO_DISCOVERY=false
npm run release:preflight:full -- --tag "$RELEASE_TAG"
~~~

需要日志时，在 Bash 的 pipefail 生效后使用命令重定向或 tee，避免日志管道掩盖失败。必须同时确认退出码为 0 和日志包含“发布前置校验全部通过”。

完整预检覆盖：

| 阶段 | 验证内容 |
| --- | --- |
| 版本入口 | 产品版本与 Tag 一致、说明文件可读；操作人员另确认非空 |
| 工程检查 | crate 架构文档、Desktop/Worker TypeScript、Web Svelte |
| JS 测试与构建 | Desktop、Worker、Web 测试，Browser 核心边界，Web/Worker 生产构建 |
| 发行边界 | Browser 工具目录、网络策略、App Server 生成协议、Browser 合同、Electron 唯一发行边界 |
| Rust | daemon/bridge debug 构建、真实 loopback preflight、全 workspace Clippy（警告视为错误）、全 workspace 测试 |
| 本机安装包 | 构建 Web、Worker、Rust sidecar、Electron，生成安装包并执行解包结构自检 |
| 安全审计 | npm audit --omit=dev --workspaces 与 cargo audit |

普通 release:preflight 不含打包和审计，不能代替完整命令。desktop:package -- --dir 只生成目录包，也不能代替安装包。产物默认位于 target/electron-dist/。

失败时保留首个错误与阶段，修复后先做相关回归，再重新运行完整预检。不得删除断言、跳过审计、增加未说明的漏洞忽略项或只反复重跑到偶然成功。通过后若修改源码、版本、依赖、生成文件或构建/发布配置，需验证最终快照；仅补充说明文档时运行对应文档检查即可。

审计通过不代表没有任何告警。记录未维护依赖等允许告警，区分生产依赖审计与 npm ci 输出的全依赖统计，不把后者直接说成生产漏洞数量。

## 5. 提交到 main，等待同一提交 CI

审查预检后的工作区与待发布文件，显式暂存发布涉及的路径。版本、锁文件、说明和发布修复必须进入待验收提交；其他人的改动不能顺带提交。

~~~bash
# 按实际情况逐个加入路径，以下仅列常见版本文件。
git add Cargo.toml Cargo.lock ".github/releases/$RELEASE_TAG.md"
git diff --cached --check
git diff --cached --stat
git diff --cached
# 发布快照已提交时，不创建无意义的空提交。
git commit -m "发布 Magi $RELEASE_TAG"
test "$(git branch --show-current)" = main
git push origin main
RELEASE_SHA="$(git rev-parse HEAD)"
test "$(git ls-remote origin refs/heads/main | cut -f1)" = "$RELEASE_SHA"
~~~

查询该 SHA 的最新 main push CI，不能使用旧提交或其他分支的成功记录：

~~~bash
gh run list --repo "$RELEASE_REPO" --workflow ci.yml \
  --branch main --event push --commit "$RELEASE_SHA" \
  --json databaseId,headSha,status,conclusion,url
~~~

将结果中的实际运行 ID 赋给 CI_RUN_ID，再执行下列命令。不要照抄历史发布的运行 ID。

~~~bash
gh run watch "$CI_RUN_ID" --repo "$RELEASE_REPO" --interval 60 --exit-status
gh run view "$CI_RUN_ID" --repo "$RELEASE_REPO" \
  --json headSha,status,conclusion,jobs,url
gh run list --repo "$RELEASE_REPO" --commit "$RELEASE_SHA" \
  --json databaseId,workflowName,status,conclusion,url
~~~

必须满足 headSha 等于 RELEASE_SHA、status 为 completed、conclusion 为 success，三个作业全部成功：Linux 检查与测试、Windows 桌面与进程策略、macOS 桌面与更新策略。

security.yml 有路径过滤，不是每次提交都运行；若触发，同一提交上的审计也必须成功，未触发时仍须有本地完整审计证据。取消、失败、运行中均不算通过。修复产生新 SHA 时更新 RELEASE_SHA，重新走预检、推送与 CI；不先打 Tag 让发布流程补门禁。

## 6. 创建 Tag，推进正式发布

推送前再次确认：版本和说明一致；无未纳入的发布改动；远端 main 仍是已验收 SHA；最新 CI 已通过；目标版本未被他人发布。远端 main 已推进时，先整合并验收新的发布提交。

~~~bash
test "$(node scripts/product-version.mjs)" = "$RELEASE_VERSION"
test -s ".github/releases/$RELEASE_TAG.md"
test "$(git rev-parse HEAD)" = "$RELEASE_SHA"
test "$(git ls-remote origin refs/heads/main | cut -f1)" = "$RELEASE_SHA"
test -z "$(git ls-remote origin "refs/tags/$RELEASE_TAG")"
git tag -a "$RELEASE_TAG" "$RELEASE_SHA" -m "Magi $RELEASE_TAG"
git push origin "refs/tags/$RELEASE_TAG"
~~~

本地同名 Tag 已存在时先核对原因与指向，不直接覆盖。只推送这一枚 Tag，不用 git push --tags 推送无关版本。

~~~bash
gh run list --repo "$RELEASE_REPO" --workflow release.yml \
  --commit "$RELEASE_SHA" --json databaseId,headBranch,headSha,status,conclusion,url
~~~

确认 headBranch 是目标 Tag、headSha 是已验收 SHA，将实际运行 ID 赋给 RELEASE_RUN_ID：

~~~bash
gh run watch "$RELEASE_RUN_ID" --repo "$RELEASE_REPO" --interval 60 --exit-status
gh run view "$RELEASE_RUN_ID" --repo "$RELEASE_REPO" \
  --json headSha,status,conclusion,jobs,url
~~~

工作流依次完成：

1. 检查同一 SHA 的 CI 成功，校验版本、非空说明和发行边界。
2. 并行构建 macOS arm64、macOS x64、Linux x64、Windows x64；真实启动解包程序，访问 /health 与 /web.html，再上传平台附件。
3. 汇总产物，合并两个 macOS 更新清单并校验 SHA-512；生成旧版迁移包、签名和 latest.json。
4. 生成 SHA256SUMS、GitHub 构建来源证明，创建正式 Release 并设为 Latest。
5. 更新 magi-desktop-stable/latest.json，验证 GitHub Latest 和三个 Electron 清单。

四个平台必须全部成功。runner 排队、Intel 构建、签名工具编译慢不等于失败；观察状态，不重复触发。手动 workflow_dispatch 选择 main 只构建，发布作业会跳过；正式发布或恢复应使用目标 Tag。当前工作流没有发布互斥，执行者应保证同一时间只推进一个稳定版发布。

## 7. 发布后在线验收

### 7.1 必须交付的附件

将 X.Y.Z 替换为实际版本。v3.0.52 有 33 个附件，仅作历史参考；不能只凭数量判定完整性。

| 类型 | 必须存在的附件 |
| --- | --- |
| macOS Apple Silicon | Magi-X.Y.Z-mac-arm64.dmg、.zip 及对应 .blockmap |
| macOS Intel | Magi-X.Y.Z-mac-x64.dmg、.zip 及对应 .blockmap |
| Windows x64 | Magi-X.Y.Z-win-x64.exe 及 .blockmap |
| Linux x64 | Magi-X.Y.Z-linux-x86_64.AppImage、Magi-X.Y.Z-linux-amd64.deb |
| Electron 更新 | latest-mac.yml、latest.yml、latest-linux.yml |
| 每平台能力与依赖清单 | Magi-X.Y.Z-&lt;platform&gt;-browser-capability-manifest.json、Magi-X.Y.Z-&lt;platform&gt;.cdx.json；platform 为 macos-arm64、macos-x64、linux-x64、windows-x64 |
| macOS 旧版迁移 | Magi_X.Y.Z_darwin-aarch64-electron.app.tar.gz、Magi_X.Y.Z_darwin-x86_64-electron.app.tar.gz 及各自 .sig |
| Linux/Windows 旧版迁移 | Magi_X.Y.Z_linux-x86_64-electron.AppImage、Magi_X.Y.Z_windows-x86_64-electron.exe 及各自 .sig |
| 完整性及旧更新源 | SHA256SUMS、版本 Release 的 latest.json、稳定 Feed 中内容相同的 latest.json |

### 7.2 Release、Tag 和下载可达性

~~~bash
gh release view "$RELEASE_TAG" --repo "$RELEASE_REPO" \
  --json tagName,isDraft,isPrerelease,url,assets
gh api "repos/$RELEASE_REPO/releases/latest" \
  --jq '{tag_name,draft,prerelease,html_url,assets:[.assets[]|{name,size,browser_download_url}]}'
git ls-remote origin "refs/tags/$RELEASE_TAG" "refs/tags/$RELEASE_TAG^{}"
~~~

Latest 必须是本版本，不是草稿或预发布。附注 Tag 的 ^{} 解引用 SHA 必须为 RELEASE_SHA。magi-desktop-stable 只承载旧更新 Feed，不能成为产品 Latest。按上表核对名称、非零大小及公开下载链接。

~~~bash
gh api "repos/$RELEASE_REPO/releases/tags/$RELEASE_TAG" \
  --jq '.assets[].browser_download_url' > /tmp/magi-release-asset-urls.txt
while IFS= read -r asset_url; do
  curl --fail --silent --show-error --location --head \
    --connect-timeout 15 --max-time 90 "$asset_url" > /dev/null
done < /tmp/magi-release-asset-urls.txt
~~~

HEAD 只证明可达，不证明内容完整。下载到新的独立目录核验文件哈希；不要混入其他版本的附件：

~~~bash
RELEASE_VERIFY_DIR="$(mktemp -d /tmp/magi-release-verify.XXXXXX)"
gh release download "$RELEASE_TAG" --repo "$RELEASE_REPO" --dir "$RELEASE_VERIFY_DIR"
# macOS；Linux 可在同一目录使用 sha256sum -c SHA256SUMS。
(cd "$RELEASE_VERIFY_DIR" && shasum -a 256 -c SHA256SUMS)
~~~

### 7.3 Electron 更新清单与稳定 Feed

三个 YML 的 version 必须等于产品版本，files 中的 URL 指向本版本附件，SHA-512 与下载文件一致；latest-mac.yml 必须同时包含 arm64 与 x64。下载后可从仓库根目录调用已有验证函数：

~~~bash
export RELEASE_VERIFY_DIR RELEASE_VERSION
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { load } from 'js-yaml';
import { verifyUpdateMetadata } from './apps/desktop/scripts/merge-update-metadata.mjs';
for (const name of ['latest-mac.yml', 'latest.yml', 'latest-linux.yml']) {
  const path = join(process.env.RELEASE_VERIFY_DIR, name);
  const metadata = load(await readFile(path, 'utf8'));
  assert.equal(metadata.version, process.env.RELEASE_VERSION);
  assert.ok(metadata.files?.length > 0);
  if (name === 'latest-mac.yml') {
    for (const arch of ['arm64', 'x64']) {
      assert.ok(metadata.files.some(file => file.url.includes('-mac-' + arch + '.')));
    }
  }
  await verifyUpdateMetadata(path, process.env.RELEASE_VERIFY_DIR);
}
console.log('Electron 清单版本、双架构与哈希校验通过');
JS
~~~

稳定 Feed 固定为 [magi-desktop-stable/latest.json](https://github.com/MistRipple/magi-code/releases/download/magi-desktop-stable/latest.json)。核对其 version、说明和发布时间，与版本 Release 的 latest.json 内容比较：

~~~bash
curl --fail --silent --show-error --location --max-time 90 \
  "https://github.com/$RELEASE_REPO/releases/download/magi-desktop-stable/latest.json" \
  -o "$RELEASE_VERIFY_DIR/stable-latest.json"
cmp "$RELEASE_VERIFY_DIR/latest.json" "$RELEASE_VERIFY_DIR/stable-latest.json"
~~~

旧更新清单必须包含下列八个键；每项 URL 指向当前 Tag 的实际迁移包，signature 非空并与相应 .sig 文本一致，不能引用旧版本或错误的普通安装包。

| 平台 | 旧更新入口 |
| --- | --- |
| macOS arm64 | darwin-aarch64-app、darwin-aarch64 |
| macOS x64 | darwin-x86_64-app、darwin-x86_64 |
| Linux x64 | linux-x86_64-appimage、linux-x86_64 |
| Windows x64 | windows-x86_64-nsis、windows-x86_64 |

可在完成上述下载后执行八入口与签名引用核对：

~~~bash
export RELEASE_REPO RELEASE_TAG RELEASE_VERSION RELEASE_VERIFY_DIR
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
const root = process.env.RELEASE_VERIFY_DIR;
const manifest = JSON.parse(await readFile(join(root, 'latest.json'), 'utf8'));
assert.equal(manifest.version, process.env.RELEASE_VERSION);
assert.ok(manifest.notes?.trim());
assert.ok(Number.isFinite(Date.parse(manifest.pub_date)));
const expected = [
  'darwin-aarch64-app', 'darwin-aarch64', 'darwin-x86_64-app', 'darwin-x86_64',
  'linux-x86_64-appimage', 'linux-x86_64', 'windows-x86_64-nsis', 'windows-x86_64',
];
assert.deepEqual(Object.keys(manifest.platforms).sort(), expected.sort());
const prefix = 'https://github.com/' + process.env.RELEASE_REPO
  + '/releases/download/' + process.env.RELEASE_TAG + '/';
const packages = new Set();
for (const key of expected) {
  const entry = manifest.platforms[key];
  assert.ok(entry.url.startsWith(prefix));
  const name = decodeURIComponent(entry.url.slice(prefix.length));
  assert.ok(name && !name.includes('/') && !name.includes('..'));
  assert.ok((await stat(join(root, name))).size > 0);
  assert.ok(entry.signature?.trim());
  assert.equal((await readFile(join(root, name + '.sig'), 'utf8')).trim(), entry.signature);
  packages.add(name);
}
assert.equal(packages.size, 4);
console.log('旧版更新清单八入口、四个迁移包与签名引用校验通过');
JS
~~~

签名文本比较只证明引用一致，不能宣称完成了客户端公钥密码学验证。核实四个平台的工作流真实启动步骤成功；涉及安装器或更新逻辑变更时，还应在对应系统验证真实安装与旧版本升级，记录平台覆盖和未覆盖项。

## 8. 失败恢复与重新发布

### 同一提交的临时故障

先读取失败作业日志，确认是网络、runner 或服务暂态问题，源码、配置和 Tag 无需改变。原 run 结束后仅重跑失败作业：

~~~bash
gh run rerun "$RELEASE_RUN_ID" --repo "$RELEASE_REPO" --failed
gh run watch "$RELEASE_RUN_ID" --repo "$RELEASE_REPO" --interval 60 --exit-status
~~~

CI 临时失败同理使用 CI_RUN_ID。修改源码后重跑旧 run 仍构建旧 SHA，不能验证修复。四平台 artifact 过期或缺失时，需要在已验收 Tag 上触发完整发布，不能拼接其他版本或提交的产物：

~~~bash
gh workflow run release.yml --repo "$RELEASE_REPO" --ref "$RELEASE_TAG"
~~~

随后重新查询 run ID、核对 SHA。不要同时 rerun 和 dispatch，避免并发覆盖。

### 需要修改源码、依赖或工作流

| 当前状态 | 恢复方式 |
| --- | --- |
| 尚无公开 Release | 修复 → 最终快照完整预检 → 推送 main → 新 SHA 全部 CI 通过 → 安全移动失败 Tag → 新发布流程 → 在线验收 |
| 已公开，但代码或安装包有问题 | 发布更高版本，不移动已交付 Tag，不用不同内容覆盖同版本包 |
| Release 已创建，仅上传或 Feed 收尾失败 | 仍标记未完成，保持原 Tag/SHA，从原 run 恢复失败作业，重新验收全部附件与更新源 |

移动尚未交付的 Tag 前，确认旧 run 已结束、无并行发布、无公开同版本 Release。记录远端 Tag 对象 SHA，使用精确 lease：

~~~bash
# RELEASE_SHA 已更新为通过第 4、5 节的新 main 提交。
FAILED_TAG_OBJECT="$(git ls-remote origin "refs/tags/$RELEASE_TAG" | cut -f1)"
test -n "$FAILED_TAG_OBJECT"
test "$(git ls-remote origin refs/heads/main | cut -f1)" = "$RELEASE_SHA"
git tag -fa "$RELEASE_TAG" "$RELEASE_SHA" -m "Magi $RELEASE_TAG"
git push "--force-with-lease=refs/tags/$RELEASE_TAG:$FAILED_TAG_OBJECT" \
  origin "refs/tags/$RELEASE_TAG"
~~~

lease 被拒绝时重新读取远端并调查，不改用无条件 --force。不删除历史 Release/Tag 来隐藏失败。删除 Release 也不会回滚已安装客户端；优先通过更高版本修复。稳定 Feed 回退需要另行评估客户端版本比较和数据兼容，不能当作普通重发操作。

### 已遇到的故障与排查重点

| 现象 | 原因或排查方向 | 正确处理 |
| --- | --- | --- |
| Windows 生成协议/目录提示过期，macOS 正常 | CRLF 与生成器 LF 逐字比较不一致 | 保持根 .gitattributes 的 text=auto eol=lf，用 git check-attr text eol -- 文件路径核对；Windows CI 运行 release:guard，不关闭生成物检查 |
| 恢复测试提示状态根重合 | workspace 位于 daemon 状态根之下 | 测试使用独立临时目录，不放宽生产持久化隔离 |
| 后台恢复偶现 ready 而非 consumed | 接纳后后台消费尚未结束 | 等待可观察状态并设截止时间，保留断言，不用任意固定 sleep 掩盖竞态 |
| Worker 超时测试提前退出或不稳定 | 模拟 IPC 生命周期不完整 | 检查 mock 事件循环存活与资源清理，不跳过超时断言 |
| Linux apt 长时间卡住 | runner 镜像源连接停滞 | 保留官方镜像配置、连接超时与有限重试，不只延长整个作业超时 |
| 本地磁盘不足 | Rust 缓存、解包目录、安装包并存 | 检查占用，停止相关构建后清理可重建缓存，再完整预检 |
| macOS 排队或 Intel 构建慢 | runner 容量或冷缓存 | 观察实际步骤并等待，不重复触发或提前宣称成功 |
| 迁移签名失败 | Secret、密码或产物名错误 | 核对名称与签名工具，不输出私钥，不省略 .sig |
| Latest 正确但旧客户端不更新 | 稳定 Feed 未更新或入口仍指向旧包 | 检查固定 Feed、版本、八入口和四份签名，恢复收尾步骤 |

## 9. 完成清单与交付记录

全部完成后才能报告发布成功：

- [ ] 产品版本、说明、Tag 与最终发布提交一致。
- [ ] 本地完整预检通过，保留日志与允许告警说明。
- [ ] 发布提交已到 main，同一 SHA 的必要 CI 全部成功。
- [ ] Tag 指向该 SHA，发布工作流整体成功。
- [ ] 四平台安装包构建与真实启动验收成功。
- [ ] Latest 指向本版本，附件逐项完整且公开可访问。
- [ ] SHA256SUMS 和 Electron 清单哈希一致，macOS 双架构完整。
- [ ] 旧迁移包、四份 .sig、八入口及稳定 Feed 完整且一致。
- [ ] 构建来源证明已生成，说明包含实际升级影响。
- [ ] 发布产生的改动已检查，已有无关改动保持原状，不错误声称工作区干净。

最终记录至少包含：版本及 Release 链接、提交 SHA、CI 与发布 run 链接、本地预检结果、四平台结果、附件与更新源验收结果、必要告警及未覆盖项。未完成时明确阻塞阶段与证据，不能把“已推 Tag，等待构建”作为完成结论。
