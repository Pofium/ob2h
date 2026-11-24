# ob2h

[English](./README.md)

为 ZCode 会话提供长期记忆、知识图谱以及当前代码库的 AST 图谱。插件会启动
[ob2h](https://github.com/Pofium/ob2h)（OmnesBot to Harness）——一个用 Rust 编写的本地 MCP
服务器，并教会智能体何时回忆、何时保存，以及如何通过它读懂代码库。ob2h 学到的一切都保存在
用户本机的一个文件夹里。

## 插件提供什么

| 组件 | 名称 | 用途 |
| --- | --- | --- |
| MCP 服务器 | `ob2h` | `ob2h serve` 的 57 个工具：记忆、知识图谱、AST 代码图谱、经验与槽位、会话与做梦（dreaming）、开发循环、密钥保险库、日常维护。 |
| 技能 | `ob2h` | 何时回忆与保存、如何使用代码图谱工具、哪些调用需要确认，以及这些工具的常见陷阱。当用户询问记忆、过往决策、项目架构或改动影响范围时加载。 |
| 技能 | `ob2h-setup` | 检查、安装或更新 ob2h 可执行文件，选择数据目录，并验证 MCP 服务器能够启动。安装与更新只在用户同意后执行。 |

## 要求

- ob2h 2.0.1 或更高版本位于 PATH 中，或在设置项 **ob2h executable** 中填写其绝对路径。已提供
  Windows x86-64、Linux x86-64（glibc 与静态 musl）和 macOS（Apple Silicon 与 Intel）的发布包。
  若未安装，`ob2h-setup` 技能会安装它。
- 一个数据目录：由用户在设置项 **Data directory** 中填写的绝对文件夹路径。此项为必填，因为
  ob2h 没有默认的存储位置。

## 安装

- **ZCode**：添加市场 `Pofium/ob2h`（插件市场 → 添加），然后在 **个人 → ob2h-plugins** 下安装
  **OB2H**。
- **Claude Code** 2.1.275 及更高版本：`/plugin install ob2h --marketplace Pofium/ob2h`。

## 快速开始

1. 启用插件。
2. 打开插件设置，填写 **Data directory**，例如 `~/.ob2h`。若要继续使用已有 ob2h 安装的记忆，
   请填写它的 `data` 文件夹。
3. 如果尚未安装 ob2h，在会话中提问：`Set up ob2h.`。智能体会运行 `ob2h-setup` 技能，并在征得
   同意后安装 ob2h。
4. 新建一个会话（MCP 服务器随会话启动），然后提问：
   `What do you remember about me and my projects?`。智能体会调用 `memory_context`，并依据存储
   的内容作答。对于全新的存储，答案是尚未保存任何内容。
5. 告诉它一件需要记住的事：`Remember that I prefer tabs and Rust edition 2021.`。智能体会调用
   `memory_save`。此时再问第 4 步的问题，就会返回这条事实。

## 设置项

| 设置项 | 默认值 | 含义 |
| --- | --- | --- |
| `ob2h executable`（`binary`） | `ob2h` | ob2h 可执行文件的命令名或绝对路径。默认值使用 PATH 中的副本。 |
| `Data directory`（`data_dir`） | 无，必填 | 存放记忆数据库、记忆文件、日志和备份的文件夹的绝对路径（`OB2H_DATA_DIR`）。服务器首次启动时创建，卸载插件时不会删除。 |

两个设置项都会传给 `.mcp.json` 中声明的 MCP 服务器。插件没有钩子（hooks）。

## 安全与副作用

- **运行的命令。** 插件只启动一个进程 `<ob2h executable> serve`，不运行其他任何命令。
  `ob2h-setup` 技能只在用户同意后才下载并解压发布包。对于会改动其他工具的命令
  （`ob2h agent install`、`ob2h doctor --fix`、`ob2h update apply`），它只说明而不执行。
- **网络访问。** 插件本身不发起任何连接。ob2h 会进行以下访问：
  - **模型 API。** 知识抽取、图谱推理、会话摘要和做梦（dreaming）会把文本发送到
    OpenAI 兼容的 API。ob2h 从 `OB2H_LLM_*` 环境变量读取接入点。未设置密钥时，它会读取所连接
    宿主（ZCode、Codex、Hermes）的模型配置并使用该模型；否则回退到本地模型（Qwen3-0.6B，
    约 1.4 GB），该模型在首次使用时从 Hugging Face 下载。设置 `OB2H_LLM_HARNESS_DETECT=0`
    可停止复用宿主模型。
  - **向量嵌入。** 本地嵌入模型
    （`sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`）在首次使用时从
    Hugging Face 下载并缓存。`OB2H_EMBED_PROVIDER=api` 可切换到嵌入 API。
  - **更新检查。** 服务器启动时，ob2h 每 24 小时至多向 `api.github.com` 询问一次是否有更新的
    发布版本。它只反馈结果。`OB2H_UPDATE_ENABLED=0` 可禁用该检查。
  - **计算机之间同步。** 仅当用户在 `<data directory>/sync/peers.json` 中配置了节点时生效：
    ob2h 通过 SSH 与这些计算机交换记忆。
  - `OB2H_OFFLINE=1` 会禁止模型下载和更新检查。
- **写入的文件。** 插件不写入任何文件。ob2h 只在 **Data directory** 内写入：SQLite 数据库、
  `workspace/`（记忆文件，带有自己的 git 仓库）、`backups/`、`logs/`、`run/` 和 `tmp/`。
  下载的模型保存在用户的 Hugging Face 缓存中。会话启动时，ob2h 会把当前打开的项目作为一条
  包含项目路径的记录加入数据库。它不会向项目中写入任何内容。
- **安全的默认值。** 密钥保险库保持锁定，直到用户在本地窗口或终端输入 PIN。PIN 绝不会经过
  MCP 或对话。密钥不会进入记忆、图谱、做梦和同步包。除非 `apply` 为 true，`knowledge_reconcile`
  只做预览。备份会轮换（完整备份 3 份，快速备份 14 份）。
- **不可信输入。** 记忆、图谱节点、经验、会话日志和抽取出的文档都是可能来自第三方的文本。
  `ob2h` 技能会告知智能体不要执行其中出现的指令。
- **数据。** 所有数据都保存在本机的 **Data directory** 中。只有上列 API 调用会把数据带出本机。

## 故障排查

| 症状 | 处理 |
| --- | --- |
| 会话中没有出现 ob2h 工具 | 未找到可执行文件，或 **Data directory** 为空。检查这两个设置项以及宿主设置中 MCP 服务器的状态，让智能体运行 `ob2h-setup` 技能，然后新建会话。 |
| 搜索返回 `warming_up_embeddings` | 嵌入模型正在加载；首次使用时会先下载。稍等几秒后重试。 |
| `project_scan` 回答扫描仍在进行 | 大型仓库的正常现象。轮询 `project_scan_status`。 |
| 代码图谱搜索没有结果 | 显式传入项目 `id`。在 `project_scan_status` 中查看 `last_scanned_at`，若项目从未扫描则运行 `project_scan`。 |
| `viewer_open` 回答 `gui_unavailable` | 该构建不含 GUI，或机器没有图形环境。在桌面会话中运行 `ob2h viewer`，或使用 `ob2h stats`。 |
| `secrets_get` 回答保险库已锁定 | 由用户在本地窗口输入 PIN 解锁，或在终端运行 `ob2h secrets unlock`。智能体绝不询问 PIN。 |
| CLI 命令看不到插件的记忆 | 运行 `ob2h` 命令时，把 `OB2H_DATA_DIR` 设为 **Data directory**。 |

## 链接

- 文档：<https://github.com/Pofium/ob2h#readme>
- 源代码：<https://github.com/Pofium/ob2h>
- 发布版本：<https://github.com/Pofium/ob2h/releases>
- 问题反馈：<https://github.com/Pofium/ob2h/issues>

## 许可证

本插件的文件依据 MIT 许可证授权（[LICENSE](./LICENSE)）。ob2h 本身依据 MIT 许可证分发，见
<https://github.com/Pofium/ob2h/blob/main/LICENSE>。插件不含任何第三方代码或资源。ob2h 按需
下载的模型由其作者依据相应 Hugging Face 模型页面所示的许可证分发。
