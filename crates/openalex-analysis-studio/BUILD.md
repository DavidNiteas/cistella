# cistella 构建说明

cistella 的对外产品是桌面应用，生产构建使用 Tauri 构建工具链。仓库根目录的脚本负责前端构建；Tauri 再将前端资源嵌入桌面应用并生成可分发产物。

## 推荐构建

在仓库根目录执行：

```powershell
pnpm build
```

## 桌面工程构建

也可以进入桌面工程目录后直接运行：

```powershell
cd crates/openalex-analysis-studio
pnpm tauri build
```

开发模式可使用：

```powershell
pnpm tauri dev
```

## 验证

```powershell
cargo check -p cistella-desktop
cargo test -p cistella-core
```

`crates/openalex-analysis-studio` 是当前仓库保留的历史路径；桌面应用及其发布产物统一称为 cistella。OpenAlex 只在需要验证其数据源导入适配器时出现，不是构建目标或产品名称。
