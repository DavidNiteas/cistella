# OpenAlex Analysis Studio 构建说明

Studio 的生产构建必须使用 Tauri CLI，而不是单独使用 `cargo build`：

```powershell
cd crates/openalex-analysis-studio
pnpm tauri build
```

该命令会按 `src-tauri/tauri.conf.json` 执行 `pnpm build`，并将 `dist/` 前端资源嵌入 Tauri 应用资源中，最终生成桌面应用及安装包。

Release Windows 应用使用 `windows_subsystem = "windows"`，启动时不会弹出控制台终端窗口。开发模式下使用 `pnpm tauri dev` 时，终端仍会保留用于显示 Vite/Rust 日志，这是开发所需行为。
