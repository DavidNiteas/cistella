# cistella

cistella 是一个本地化个人文献保管库与阅读、管理、分析工作台。

它把个人文献库作为第一性对象：

- **库即一切**：文献、阅读记录、标注与分析上下文围绕本地库组织。
- **复制即走**：库目录是可携带、可备份、可迁移的资产单元。
- **打开即用**：以桌面应用为唯一对外入口，打开即可继续工作。
- **本地优先**：数据默认保存在用户自己的设备与库目录中。
- **免安装可携带**：发布目标是可复制、可直接使用的桌面应用。

## 工作区

- **Vault**：创建、打开、连接和管理本地文献库。
- **Reading**：围绕个人文献开展阅读、标注、标签与笔记工作。
- **Source**：在当前库上进行数据源导入、检索、统计与分析。
- **Settings**：管理语言、便携式偏好和应用行为。

OpenAlex 是 cistella 支持的重要外部数据源之一。cistella 提供针对 OpenAlex 数据的导入适配，将其转换为自己的本地库数据；产品模型、库格式和工作台并不依赖 OpenAlex，也不以 OpenAlex 为产品中心。

## 开发治理

- 设计路线：`_dev/设计路线.md`
- 开发状态：`_dev/开发状态.md`
- 工单目录：`_dev/工单-*/`

## 构建与验证

在仓库根目录执行：

```powershell
pnpm build
cargo check -p cistella-desktop
cargo test -p cistella-core
```

桌面应用的详细构建说明见 [`crates/openalex-analysis-studio/BUILD.md`](crates/openalex-analysis-studio/BUILD.md)。目录名暂保留为仓库历史路径，不代表对外产品名称。
