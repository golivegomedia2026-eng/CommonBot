# CommonBot

通用智能体平台：新建智能体并定义职责、为智能体建知识库（可结合联网回答）、通过对话生成新的智能体。主机为 NVIDIA DGX Spark，Rust 实现。

当前阶段：三项基本要求（新建智能体并定义任务、知识库加联网回答、通过对话生成可独立使用的智能体）都已实现，并能打成解压即用的离线包和带窗口的桌面版安装包。手机客户端和语音还没有做（手机用浏览器访问）。

- 使用说明：[简体中文](web/src/guide/zh-CN.md) · [繁體中文](web/src/guide/zh-TW.md) · [English](web/src/guide/en.md) · [日本語](web/src/guide/ja.md)（应用里的「使用说明」页是同一份）
- 发行与升级：[docs/RELEASE.md](docs/RELEASE.md)
- 方案：[docs/v1/PLAN.md](docs/v1/PLAN.md)
- 决策：[docs/v1/DECISIONS.md](docs/v1/DECISIONS.md)
- 进度：[docs/v1/PROGRESS.md](docs/v1/PROGRESS.md)
- 效果图（演示数据，浏览器直接打开）：[docs/v1/mockup/index.html](docs/v1/mockup/index.html)

## 现在能做什么

- 离线包：解压即用，含界面、PDF 解析库、使用说明；Linux 可装成开机自启的服务；局域网访问在界面里开关
- 升级检查：读取签名的版本策略，有新版本时提示，低于最低版本时必须升级（需要先配置签名密钥，见发行说明）
- 建造师：和它对话，它把访谈整理成智能体的定义（手动改过的字段不会被覆盖），出题试答并评判，确认后发布
- 发布：每个智能体可以有独立链接（访客不需要访问码，只能用到这一个智能体）和 API 密钥（兼容 OpenAI 的接口，流式和非流式都支持）；可随时取消发布或更换密钥
- 新建智能体：写清负责什么、不负责什么、开场白和示例问题，可固定使用某个模型；系统提示词由这些内容生成并可预览
- 每个智能体有自己的对话记录
- 知识库：多级文件夹；上传 PDF、Word、PowerPoint、Excel、文本、Markdown、网页、图片；扫描件由看图模型识别；入库在后台排队，可暂停、重试
- 依据知识库回答：关键词和语义两路检索，回答里标注出处编号，点开可看原文片段
- 联网：自建搜索（SearXNG）或搜索接口；每个智能体可设「不联网 / 知识库优先、不足时联网 / 每次都联网」，对话里可临时开关；可进一步读取网页正文；网页内容只当资料，不当指令
- 对话里上传文件：只在这个对话里使用，不进知识库；出处按「知识库 / 本次上传 / 网页」分开标注
- 云端后备：本机模型连不上时，允许的智能体改用云端模型，并在回答上标明
- 知识范围可以选到文件夹：智能体有默认范围，每次对话可临时改，也可以在文件夹页面直接提问
- 连接本机的推理服务（默认 Ollama，也可以是 vLLM、llama.cpp 等任何 OpenAI 兼容接口），检测连通并列出模型
- 流式对话，可中途停止，已生成的部分会保留；对话保存在本机，重启后还在
- 添加云端模型作为后备；密钥只存在本机，不会回传给界面
- 简体中文、繁體中文、English、日本語；八套色境
- 默认只有本机能用；开放局域网后，其他设备凭访问码进入

## 从源码运行（Spark 与 Mac 相同）

需要 Rust、Node 22 和 pnpm，以及一个已装好模型的推理服务（例如 Ollama）。

```bash
scripts/fetch-vendor.sh
```

```bash
pnpm --dir web install --frozen-lockfile
```

```bash
pnpm --dir web build
```

```bash
cargo build --release -p commonbot-server
```

```bash
./target/release/commonbot
```

然后打开 <http://localhost:8600>。要让其他设备访问，在「系统与外观」里打开开关。

打成离线包（产物在 `dist/`）：

```bash
scripts/package.sh
```

没有模型服务时，可以用仓库里的假模型看界面（它只会复述你的话）：

```bash
cargo run -p commonbot-mock-llm
```

## 检查

浏览器回归（用假模型把主要流程在真实浏览器里走一遍）：

```bash
scripts/e2e/run.sh
```

单元与接口测试：

```bash
pnpm --dir web test
```

```bash
cargo test --workspace
```

## 实际界面截图

| 智能体对话：依据知识库回答，带出处 | 知识库：文件夹、文档、入库状态 |
| --- | --- |
| ![出处](docs/screenshots/p2-chat-sources.png) | ![知识库](docs/screenshots/p2-kb.png) |

| 提问时选知识范围 | 检索测试 |
| --- | --- |
| ![范围](docs/screenshots/p2-chat-scope.png) | ![检索](docs/screenshots/p2-kb-search.png) |

| 联网回答 | 对话里上传文件 |
| --- | --- |
| ![联网](docs/screenshots/p3-chat-web.png) | ![文件](docs/screenshots/p3-chat-files.png) |

| 建造师：访谈、草稿、试答 | 发布后的链接与密钥 |
| --- | --- |
| ![建造师](docs/screenshots/p4-builder.png) | ![发布](docs/screenshots/p4-builder-published.png) |

| 桌面版窗口 |
| --- |
| ![桌面版](docs/screenshots/p5-desktop.png) |

| 必须升级时的提示 | 有新版本时的提示 |
| --- | --- |
| ![必须升级](docs/screenshots/p6-upgrade-required.png) | ![可选升级](docs/screenshots/p6-upgrade-optional.png) |

| 访客的独立对话页（手机） | 云端后备 |
| --- | --- |
| ![访客](docs/screenshots/p4-public-phone.png) | ![云端](docs/screenshots/p3-chat-cloud.png) |

| 智能体列表 | 配置抽屉 | 手机 |
| --- | --- | --- |
| ![列表](docs/screenshots/p1-agents.png) | ![配置](docs/screenshots/p2-agent-folders.png) | ![手机](docs/screenshots/p2-chat-phone.png) |

## 效果图截图

| 智能体对话（宣纸） | 知识库文件夹 |
| --- | --- |
| ![对话](docs/screenshots/chat-light.png) | ![知识库](docs/screenshots/kb-light.png) |

| 提问时选知识范围 | 配置抽屉 |
| --- | --- |
| ![范围](docs/screenshots/chat-scope-light.png) | ![配置](docs/screenshots/chat-config-light.png) |

| 建造师 | 深色（黛夜·青灯） | 手机端 |
| --- | --- | --- |
| ![建造师](docs/screenshots/builder-light.png) | ![深色](docs/screenshots/chat-dark.png) | ![手机](docs/screenshots/chat-phone.png) |

M&J Group · 473276@qq.com
