# CmdHub: 智能 Agent 命令行工具枢纽

> [English](./README.md) | 简体中文

> 一个去中心化、意图驱动的命令行工具（CLI）全局注册表与离线检索基础设施，专为 AI Agents 和现代开发者构建。

## 什么是 CmdHub？

CmdHub 提供了标准化、机器可读的 **ACI (Agent-Computer Interface，智能体计算机接口)** 命令行合约。这使得 AI Agents 能够无幻觉、毫秒级延迟且安全受控地发现并执行终端命令行工具。

### 架构

```
cmdhub-oss/                  # 本仓库 (开源客户端)
├── cmdhub-cli/              # cmdh — 本地 CLI 客户端 (Rust)
├── cmdhub-mcp/              # MCP 服务端，用于 IDE 或 Agent 的工具链集成
├── cmdhub-shared/           # 共享数据类型与 ACI Schema 定义
├── cmdhub-skills/           # 本地动态插件/技能系统
├── AGENTS.md                # AI 编码智能体指令规范
└── schemas/                 # ACI JSON Schema 定义规范
```

### 核心特性

- **离线混合搜索**：FTS5 全文搜索 + 基于 `sqlite-vec` 的本地 ONNX 向量检索，利用 RRF (Reciprocal Rank Fusion) 双重重排，检索延迟 `< 1ms`。
- **MCP 服务端**：原生支持 Model Context Protocol 协议，可与 Claude Code、Cursor、Cline 等 Agent 客户端一键打通。
- **安全执行屏障**：为危险命令（如破坏性删除或提权）提供自动的风险等级感知与交互式安全拦截机制。
- **XDG 规范兼容**：严格遵循 XDG Base Directory 规范（配置文件默认在 `~/.config/cmdhub/`，数据默认在 `~/.local/share/cmdhub/`）。
- **Ed25519 签名数据库**：所有分发的本地 SQLite 数据包都带有数字签名验证，防范劫持。

## 快速入门

### 1. 从 Source 安装
```bash
cargo install cmdhub-cli --bin cmdh
cargo install cmdhub-mcp
```

### 2. 初始化与同步数据
```bash
# 初始化默认配置文件 (支持幂等性)
cmdh init

# 从 CDN 更新本地数据库
cmdh update
```

### 3. 命令行检索
```bash
# 智能模糊检索 (输出三种预设模式：--full, --usage-only, --minimal)
cmdh search "extract tar excluding node_modules" --usage-only
```

### 4. 启动 MCP 服务
```bash
# 在 Stdio 传输通道中启动守护进程
cmdhub-mcp
```

## 文档

完整的系统架构设计、API 规范和数据格式详情请参考 `cmdhub-docs` 目录：
- [产品需求规格说明书 (PRD)](./cmdhub-docs/01-prd.md)
- [系统架构与设计](./cmdhub-docs/02-architecture-design.md)
- [CLI 命令行工具规格说明](./cmdhub-docs/04-cli-design-spec.md)
- [ACI 合约 Schema 定义](./cmdhub-docs/09-aci-schema-definition.md)
- [MCP 服务端集成协议](./cmdhub-docs/11-mcp-server-protocol.md)

## 本地开发

请查看 [AGENTS.md](./AGENTS.md) 了解本项目要求的开发规范和 CI 要求。

```bash
# 代码格式化检查
cargo fmt --all -- --check

# 代码静态检查
cargo clippy --all-targets --all-features -- -D warnings

# 执行全部测试集
cargo test --all-features --workspace

# 提交前本地预检
pre-commit run --all-files
```

## 许可证

本项目基于 MIT 许可证开源 — 详情请参见 [LICENSE](./LICENSE)。
