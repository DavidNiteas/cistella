# AGENTS.md — cistella

面向自动化代理与开发者的项目规范。改动构建、打包、测试流程时，必须同步更新本文件。

## 项目速览

- Rust workspace（`crates/cistella-tree-space`、`crates/cistella-workspace`、`crates/openalex-analysis-core`、`crates/openalex-analysis-studio`）+ Tauri 2 GUI（`openalex-analysis-studio`，产品名 `cistella`）。
- 前端在 `crates/openalex-analysis-studio`（Vite + React + pnpm）。
- 详细设计与工单见 `_dev/设计路线.md`、`_dev/开发状态.md`。

## 技术栈约束（构建/打包）

- **本项目不使用 Python。** 构建、打包、冒烟等流程脚本一律用 **Node.js（ESM）+ pnpm scripts** 编写，放在 `crates/openalex-analysis-studio/scripts/`。
- Windows 原生操作用系统内建能力，不引入额外安装依赖：
  - zip 打包 → PowerShell + .NET `System.IO.Compression.ZipFile`（见 `make-portable.js` 与 `package-release.js` 的 `zipDirectory`）
  - 文件哈希 → `certutil -hashfile <file> SHA256`
- 编排脚本只组合既有单功能脚本，不复制其逻辑。

## bin_test 规范（前后端分离手测 bundle）

`bin_test/` 是**自包含的手测目录**，机制基于前后端分离编译实现前端热更新：

- **生成唯一入口**：`pnpm build:bin-test`（`scripts/build-bin-test.js`）。它整体再生成该目录，禁止手工增删其中的 exe。
- **自包含原则（硬性）**：`bin_test/` 运行时**零依赖 `target/`**。可以整体拷到任意位置运行；任何脚本/测试不得引用 `target/` 下的二进制作为 bin_test 机制的输入（`headless-smoke-test.js` 指向 `bin_test/cistella-headless.exe` 是正确示范）。
- **热更新流程**：改前端 → `pnpm build` → 用 `dist/` 覆盖 `bin_test/cistella-frontend/` → 重启 GUI 生效。后端不变则无需重编 exe。资源按请求从磁盘读取（`src-tauri/src/lib.rs` 的 `bin_test_frontend_response`）。
- **GUI exe 必须带 `--features bin-test-frontend` 构建**（加载 exe 旁的 `cistella-frontend/`，走 `cistella-bin-test://` 协议）；`build-release.js` 产出的内嵌版会覆盖 `target/release/cistella.exe`，二者不要混同步。
- **bin-test 构建不得内嵌真实前端**：`tauri::generate_context!()` 默认会把 `frontendDist`（`../dist`）编译进二进制。bin-test 构建必须改用 `tauri.bin-test.conf.json`（`frontendDist` 指向 `bin-test-frontend-stub/`），否则 exe 里那份编译期快照会过时，并可能掩盖热更新（`src-tauri/src/lib.rs` 的 `run()` 开头按 feature 切换 context）。release 构建保持默认内嵌。
- `bin_test/cistella-portable/` 是 portable 模式标记：存在即 portable，全部状态写入该目录内，不碰用户数据。不要删除。

## 构建流水线（两条独立管线，用户按需选择）

两条流水线**互相独立，产物不同**（GUI exe 分别带/不带 `bin-test-frontend` feature），不要串行强制，也不要混同步产物：

**管线 A：手测 / 前端热更新** —— 日常迭代用
- `pnpm build:bin-test` 生成自包含的 `bin_test/`（见下节规范）。
- 前端改动只需 `pnpm build` 后覆盖 `bin_test/cistella-frontend/`，重启 GUI 即生效，无需重编 exe。
- `pnpm test:headless` 对 bin_test 跑冒烟，属此管线。

**管线 B：内嵌正式版 / 发布** —— 最终验证与分发用
- `pnpm build:release`：前端内嵌进二进制的正式版（`tauri build --no-bundle`）+ headless + portable zip（`scripts/make-portable.js`，产物在 `target/release/`）。
- **`pnpm package` 是发布标准入口**（`scripts/package-release.js`），只走本管线，fail-fast：
  1. `build:release` —— 内嵌正式版 + portable zip
  2. collect —— 产物汇总到 `release/<product>-<version>/`（git 忽略），含：
     - `<product>-<version>-portable.zip`
     - `<product>.exe`、`<product>-headless.exe`（内嵌 release 版）
     - `SHA256SUMS.txt`（certutil SHA-256）与 `manifest.json`（版本、构建时间、git commit、逐产物 hash）
- `pnpm package --full` 额外运行 `release-smoke-test.js`（全量 cargo 测试回归，较慢）。

## 常用命令

```bash
# 前端开发（HMR）
pnpm --dir crates/openalex-analysis-studio dev

# 手测 bundle 与冒烟
pnpm --dir crates/openalex-analysis-studio build:bin-test
pnpm --dir crates/openalex-analysis-studio test:headless

# 发布
pnpm --dir crates/openalex-analysis-studio build:release
pnpm --dir crates/openalex-analysis-studio package        # 标准打包流程

# Rust 测试
cargo test -p cistella-core --lib
cargo test -p cistella-desktop --lib
```

## 前端布局约定

- `.page`（`src/style.css`）是所有工作区的**单列纵向滚动**容器（flex column + `overflow-y: auto`），禁止回到固定网格/裁剪式布局。
- 需要并排的区域用 `.page-cols`（auto-fit 子网格，窄屏自动塌缩），如笔记列表+编辑器、设置卡片、Vault 信息区。
- 卡片默认全宽；不要再引入 `span2/span3` 之类的跨列类。
- 功能级样式放各 `features/<x>/Page.module.css`，全局只留布局工具类。

## 其他约定

- `bin_test/`、`release/`、`target/` 均 git 忽略；只有 scripts 进版本库。
- 根 `package.json` 被 git 忽略（历史约定），脚本入口统一挂在 `crates/openalex-analysis-studio/package.json`。
