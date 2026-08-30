# Janus

[English](./README.md) | 简体中文

Janus 是一个面向 AI 辅助软件开发的本地优先(local-first)控制平面。单个 Rust
进程在一个 MongoDB 数据库之上持有项目、工作区、会话、轮次(Turn)、终端和持久化的
后台 Operation,并通过带版本的 HTTP + SSE + WebSocket API 对外发布。两个客户端消费
这套 API:`apps/web` 下的 SolidJS Web 应用,以及 `janus-test` —— 一个只使用公开协议
的黑盒 CLI。

## 功能

- **单一所有者的 passkey 认证。** WebAuthn 注册、登录、passkey 管理、恢复码,以及
  恢复凭据的兑换。每个部署只有一个所有者。
- **由 Git 支撑的项目。** 克隆仓库、浏览与编辑文件树,并运行分阶段的 Git 命令
  (status、diff、log、branches、remotes、stage、unstage、commit、fetch、push、
  update);无法快进的 update 会留下明确的冲突记录。
- **会话、轮次与时间线。** 消息路由到已配置的模型供应商,支持故障转移、用量记账、
  附件、中途引导(steering)、取消,以及由模型生成的上下文压缩。
- **终端。** 绑定到项目工作区的 WebSocket shell,通过一次性票据授权,重连后可重放
  回滚缓冲(scrollback)。
- **持久化 Operation。** 长任务作为带步骤、工作项、幂等记录和启动恢复的日志化
  Operation 运行,而不是即发即忘的任务。
- **一条保留的事件流。** 每次状态变更都追加到 `public_events`,可通过 SSE 读取,
  游标不透明、重放有界并带心跳。
- **可选的 fork-sync 自动化。** 一个带签名的 webhook 会把 fork-sync 冲突报告转换成
  项目和 Supervisor 会话,由它们去修复对应的 pull request。

## 环境要求

| 工具 | 版本 | 说明 |
| --- | --- | --- |
| Rust | `1.97.0` | 由 `rust-toolchain.toml` 固定,含 `clippy` + `rustfmt` |
| Bun | `1.3.14` | 由 `apps/web/package.json` 的 `packageManager` 固定 |
| Git | 2.54 或更高 | 源码控制适配器直接调用系统 Git |
| MongoDB | 7.x | 多文档事务所需的单节点副本集;通过 `JANUS_MONGODB_URI` 访问 |

整个 workspace 禁止 `unsafe_code`,并通过根 `Cargo.toml` 中声明的 Clippy lint 拒绝
`dbg!`、`todo!` 和 `unwrap()`。

## 快速开始

```text
cargo xtask setup
cargo xtask dev
```

`setup` 会打印 `rustc`、`cargo`、`bun`、`git` 的版本,并以 `--frozen-lockfile` 安装
Web 依赖。`dev` 同时启动 Vite 和控制平面:API 监听 `http://127.0.0.1:4317`,Web 应用
监听 `http://127.0.0.1:5173`,Vite 把 `/api`(含 WebSocket)和 `/health` 代理到 API。
运行时数据落在 `.janus-dev/`,可用 `JANUS_DATA_ROOT` 改变位置。

`dev` 也需要一个可达的 MongoDB 副本集。默认的 `JANUS_MONGODB_URI`
(`mongodb://localhost:27017/?replicaSet=rs0`)指向一个以 `--replSet rs0` 启动的本地
`mongo:7` 实例;单个非副本集的 `mongod` 会拒绝事务调用。

`cargo xtask dev` 还会导出 `JANUS_PUBLIC_ORIGIN=http://localhost:<port>` 和
`JANUS_WEBAUTHN_RP_ID=localhost`,因为 WebAuthn 不接受 IP 地址作为 relying-party ID。
请通过 `localhost` 而不是 `127.0.0.1` 访问应用。

要并行运行第二套 Web/控制平面,在执行 `cargo xtask dev` 前设置 `JANUS_BIND`、
`JANUS_PUBLIC_ORIGIN`、`JANUS_WEB_PORT`,必要时再加 `JANUS_API_TARGET` —— 例如
`127.0.0.1:4318` 和 `5174`。

### 首次登录

开发模式下 `JANUS_DEV_AUTH` 默认为 `true`,请求无需 passkey 即被授权。该组合仅在监听
地址为回环地址时才允许,在生产模式下会被直接拒绝。

真实部署时,用一个初始化令牌来声明所有权:

```text
cargo run -p janus-server --bin janus-admin -- issue-initialization-token
cargo run -p janus-server --bin janus-admin -- issue-recovery-token
```

两条命令读取与服务端相同的环境变量,向 stdout 打印一个令牌后退出。该令牌由
`POST /api/v1/auth/initialize/options` 和 `/complete` 消费,用于注册所有者的第一个
passkey。

## 仓库结构

```text
apps/server/          部署组装根,公开控制平面
  src/application/    跨能力工作流、后台 worker、恢复逻辑
  src/transport/http/ HTTP、SSE、WebSocket 协议转换
  src/adapters/       系统 Git 与运行时/进程实现
  src/bin/            generate_openapi、janus-admin
apps/web/             SolidJS 客户端(Vite、Bun、Biome、Playwright)
crates/               能力模块,以及 janus-infrastructure
tools/xtask/          setup、dev、check、build、generate 入口
tools/test-cli/       janus-test 黑盒验证 CLI
generated/            由编译后的路由生成的 openapi.json
docs/aegis/           长期有效的设计与规划记录
scripts/              CI 使用的镜像部署脚本
```

## 架构

### 能力模块

`crates/` 下的每个模块都在 `module.toml` 里声明自己的契约:公开根、它拥有的集合、它
发布的事件,以及它可以依赖的模块。`cargo xtask check architecture` 会强制执行该文件。

| 模块 | Crate | 拥有的集合 | 发布的事件 | 可依赖 |
| --- | --- | --- | --- | --- |
| identity | `janus-identity` | `owners`, `initialization_tokens`, `passkeys`, `ceremonies`, `login_sessions`, `recovery_batches`, `recovery_codes`, `recovery_states` | — | — |
| models | `janus-models` | `model_providers`, `models`, `model_failover`, `model_attempts`, `model_usage_ledger`, `automation_settings` | `model_config.changed` | — |
| runtime | `janus-runtime` | `runtimes`, `log_streams`, `async_tasks`, `terminals`, `runtime_access_tickets` | `runtime.changed`, `async_task.changed`, `terminal.changed` | — |
| workspace | `janus-workspace` | `workspace_copies`, `content_revisions`, `workspace_snapshots`, `workspace_mutation_intents` | — | — |
| notifications | `janus-notifications` | `notification_channels` | `notification_channel.changed` | — |
| source-control | `janus-source-control` | `project_git_state`, `git_update_conflicts`, `git_update_conflict_paths` | `git.state_changed`, `git.update_conflict_changed` | workspace |
| projects | `janus-projects` | `projects`, `github_credentials`, `memories` | `project.changed`, `project.main_revision_changed` | runtime, workspace |
| sessions | `janus-sessions` | `sessions`, `turns`, `messages`, `timeline_items`, `checkpoints`, `uploads`, `attachments`, `message_attachments` | `session.changed`, `session.deleted`, `turn.created`, `turn.status_changed`, `timeline.item_created`, `timeline.item_updated`, `checkpoint.created` | workspace |
| execution | `janus-execution` | `rounds`, `tool_calls`, `plan_versions`, `compact_summaries`, `context_versions` | `round.changed`, `tool_call.created`, `tool_call.changed`, `context.changed` | models, projects, runtime, sessions, workspace |

`janus-infrastructure` 位于所有模块之下,只包含通用的技术构件:ID 与关联 ID、时钟、
MongoDB 连接与事务辅助、公开事件日志、Operation 日志、工作项、幂等记录、Blob 存储、
加密的密钥,以及可移植的进程辅助。它不含任何工作种类,也不含服务端工作流。它的集合,
加上 Operation 和 Blob 相关的集合,归属 `platform` 这个 owner:`public_events`、
`projection_cursor`、`operations`、`operation_steps`、`work_items`、
`idempotency_records`、`command_idempotency_records`、`blob_objects`、
`blob_references`、`blob_cleanup_intents`。

### 服务端分层

- `src/application/` 是跨能力工作的唯一组装边界:事务顺序、执行调度、后台 worker、
  启动恢复和资源清理。它不拥有任何业务表。
- `src/transport/http/` 把公开协议转换成能力调用或应用调用。处理器不写能力表、不重试
  业务工作,也不调度轮次。
- `src/adapters/` 实现部署相关的细节 —— 系统 Git、进程、终端 —— 从不决定 Session、
  Turn 或 Project 的结果。
- `AppState` 连接组装根的资源,并向传输层和系统测试暴露窄口径的能力查询 getter。新的
  工作流属于 `Application`,而不是 `AppState`。

### `cargo xtask check architecture` 强制的规则

- 每个模块都要暴露 `interface.rs` 或 `interface/mod.rs`,跨模块引用必须走这条接口路径。
- 模块依赖必须在 `module.toml` 中声明,并保持无环。
- 一个集合只有一个 owner,一个事件名只有一个发布者。
- 允许跨模块集合读取,不允许跨模块写入。生产代码只能写自己模块拥有的集合。
- 集合归属在 `crates/infrastructure/src/schema.rs` 中声明,并与每个 `module.toml`
  对照检查:每个集合要么带索引要么无索引,有且仅有一个声明的 owner,并且生产代码必须
  用内联字符串字面量调用 `.collection("...")` —— 绝不允许绑定的句柄。
- 禁止 `apps/server/src/ports/` 和 `crates/ports/`;同样禁止在
  `tools/test-cli/Cargo.toml` 中依赖 `janus-server`。

### 启动与关闭契约

下面的顺序是部署契约,不是后台杂务。

1. 从环境解析配置;非法输入直接终止进程。
2. `AppState::initialize` 打开 MongoDB 数据库(创建 schema 目录声明的集合与索引),构建
   基础设施与能力接口,重新挂接孤立的 main 工作树,然后恢复被中断的工作区变更和执行状态。
3. 恢复完成前,`/health/ready` 返回 503。
4. 清除上一次运行遗留的 Blob 入站残留。
5. 把每个仍处于 `running` 的 Operation 标记为 `needs_attention`,附
   `OPERATION_INTERRUPTED`,让客户端重试而不是猜测结果。
6. 标记恢复完成,使 `/health/ready` 返回 200,然后追加带 crate 版本的
   `SystemStarted` 事件。
7. 启动 operation、自动压缩、异步任务投递、通知和状态 worker。
8. 绑定监听器并开始服务。
9. 收到 `Ctrl-C` 或 `SIGTERM` 时,先停止接受连接,再在 10 秒内停止存活的运行时,
   避免本地进程组泄漏。

## 持久化

状态放在 `JANUS_MONGODB_URI` 指向的副本集上的一个 MongoDB 数据库里(默认库名 `janus`),
工作区副本和 Blob 存储与它并列放在 `JANUS_DATA_ROOT` 下。MongoDB 没有 SQL 迁移;schema
是一个 Rust 目录 `crates/infrastructure/src/schema.rs`,声明每个集合及其 owner 模块:

- `COLLECTIONS` 以 `(name, owner)` 对列出全部 54 个集合。
- `INDEXLESS_COLLECTIONS` 列出不带索引、在打开时显式创建的集合(例如事件游标计数器单例
  `event_seq`,以及 `owners`)。
- `index_specs()` 把每个带索引的集合映射到它的 `IndexModel`;SQLite 里的复合主键表变成
  `_pk` 唯一索引,status `IN (...)` 部分过滤在 MongoDB 5–7 上展开为 `$or`/`$eq`。

打开全新数据库是一次逐集合的 `create_indexes` 操作(幂等),外加显式创建无索引集合;
`Database::open` 还会播种 `event_seq` 计数器。`SCHEMA_VERSION` 保持为 4 —— 最后一个
SQL 迁移号 —— 所以 `/api/v1/system/info` 不显示回归。不存在数据迁移:已有的 SQLite
存储不会被导入,部署从全新 MongoDB 开始。

## 公开 API

客户端需要的一切都在 `/api/v1` 之下,再加上两个健康探针;除 Web 客户端回退路由之外,
每个已注册的路由都记录在 `generated/openapi.json`(title `janus-server`,version
`0.1.0`)中。

传输约定:

- 成功响应体包装为 `{ "data": ... }`。
- 错误是 `application/problem+json`,带 `type`、`title`、`status`、`code`、`detail`
  和 `request_id`。对于根本没有进入处理器的请求同样如此 —— 见[错误码](#错误码)。
- 有副作用且不能安全重复的命令要求客户端生成 `Idempotency-Key`;修改单个资源的请求
  在 `If-Match` 中携带资源版本。
- 每个响应都带 `X-Request-Id`;`/api/v1/bootstrap` 和 `/api/v1/system/info` 还会在
  `X-Janus-Event-Cursor` 中返回当前事件游标。
- 认证是登录会话 cookie 加 `x-csrf-token`;新生成的恢复码只在
  `x-janus-recovery-codes` 中返回一次。

### 平台

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET | `/health/live` | 存活状态与版本 |
| GET | `/health/ready` | 就绪状态;启动恢复完成前返回 503 |
| GET | `/api/v1/bootstrap` | 客户端初始快照,带事件游标 |
| GET | `/api/v1/system/info` | schema 版本与保留的事件边界 |
| GET | `/api/v1/events` | SSE 流;不透明游标、有界重放、15 秒心跳 |
| GET | `/api/v1/operations/{id}` | 持久化 Operation 投影 |

### 认证与所有者

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| POST | `/api/v1/auth/initialize/options`, `/complete` | 消费初始化令牌,注册第一个 passkey |
| POST | `/api/v1/auth/passkey/options`, `/complete` | passkey 登录 |
| POST | `/api/v1/auth/logout` | 结束登录会话 |
| POST | `/api/v1/auth/recovery/exchange` | 用恢复码换取恢复凭据 |
| POST | `/api/v1/auth/recovery/passkey/options`, `/complete` | 在恢复凭据下注册 passkey |
| GET | `/api/v1/me` | 当前所有者 |
| GET/POST/PATCH/DELETE | `/api/v1/me/passkeys...` | 列出、添加、重命名、吊销 passkey |
| POST | `/api/v1/me/recovery-codes/regenerate` | 签发新的恢复码批次 |

### 项目、文件与 Git

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET/POST | `/api/v1/projects` | 列出、创建(克隆)项目 |
| GET/PATCH/DELETE | `/api/v1/projects/{id}` | 项目投影、更新、删除 |
| POST | `/api/v1/projects/{id}/retry` | 重试失败的项目初始化 |
| GET | `/api/v1/projects/{id}/files/tree`, `/meta`, `/content` | 浏览 Main 工作区 |
| PUT | `/api/v1/projects/{id}/files/text` | 保存文件文本 |
| POST | `/api/v1/projects/{id}/files/move` | 移动或重命名 |
| DELETE | `/api/v1/projects/{id}/files` | 删除路径 |
| GET | `/api/v1/projects/{id}/git/status`, `/diff`, `/log`, `/branches`, `/remotes` | Git 投影 |
| POST | `/api/v1/projects/{id}/git/commands/{stage,unstage,commit,fetch,push,update}` | Git 命令 |
| GET | `/api/v1/projects/{id}/git/update-conflicts`, `/{conflict_id}` | 非快进 update 产生的冲突 |
| POST | `/api/v1/projects/{id}/git/update-conflicts/{conflict_id}/resolve` | 解决一个冲突 |
| GET/POST/PATCH/DELETE | `/api/v1/github-credentials...` | 已存凭据,另有 `/probe` |

### 会话与执行

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET/POST | `/api/v1/projects/{project_id}/sessions` | 列出、创建会话 |
| GET/DELETE | `/api/v1/sessions/{id}` | 会话投影、删除 |
| POST | `/api/v1/sessions/{id}/messages` | 发送消息并开始一个 Turn |
| GET | `/api/v1/sessions/{id}/timeline` | 分页时间线,游标不透明 |
| GET | `/api/v1/sessions/{id}/queued-turns` | 待处理的 Turn 队列 |
| GET/POST | `/api/v1/sessions/{id}/turns/{turn_id}`, `/cancel` | 查看或取消一个 Turn |
| POST | `/api/v1/sessions/{id}/steer` | 引导正在进行的交互式 Turn |
| GET/POST | `/api/v1/sessions/{id}/context`, `/context/compact` | 上下文窗口与压缩 |
| POST/DELETE | `/api/v1/sessions/{id}/attachments...` | 上传与移除附件 |

### 模型、终端、任务、通知

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET/POST/PATCH/DELETE | `/api/v1/model-providers...` | 供应商凭据与模型,另有 `/probe` |
| GET/POST | `/api/v1/terminals` | 列出、创建终端 |
| POST | `/api/v1/terminals/{id}/tickets` | 一次性、绑定来源的访问票据 |
| GET | `/api/v1/terminals/{id}/connect` | WebSocket 升级;先重放再实时 I/O,帧类型为 `input`/`resize`/`signal`/`close` |
| GET | `/api/v1/terminals/{id}/scrollback` | 游标之后的回滚缓冲字节 |
| POST | `/api/v1/terminals/{id}/resize`, `/signal`, `/close` | 终端控制 |
| GET | `/api/v1/async-tasks`, `/{id}/log` | 后台任务列表与日志 |
| POST | `/api/v1/async-tasks/{id}/cancel` | 取消后台任务 |
| GET/POST/PATCH/DELETE | `/api/v1/notification-channels...` | 通知通道,另有 `/test` |
| GET | `/api/v1/automations` | 自动化运行及其会话 |
| GET/PATCH | `/api/v1/automation/settings` | 自动化使用的供应商、模型与推理强度 |
| GET | `/api/v1/automation/webhook/config` | webhook 入口是否启用 |
| POST | `/api/v1/automation/webhook` | 带签名的 fork-sync 入口(默认关闭) |

设置 `JANUS_WEB_DIST` 后,未匹配的路径回退到构建好的 Web 客户端,于是同一个 origin
同时提供 API、健康探针和 SPA。

## 错误码

`code` 是一次失败中稳定的部分,也是客户端应该用来分支判断的字段;`status` 和 `title`
由它经 `apps/server/src/transport/http/problem.rs` 中的共享映射推导而来。`detail` 面向
人阅读,并且在 `INTERNAL_ERROR` 时会被清洗 —— 所以被分类过的失败保留自己的原因,未被
分类的失败保持不透明。

| 分组 | 错误码 |
| --- | --- |
| 通用 | `RESOURCE_NOT_FOUND`(404)、`RESOURCE_VERSION_MISMATCH`(412)、`PRECONDITION_REQUIRED`(428)、`IDEMPOTENCY_KEY_REUSED`(409)、`OPERATION_IN_PROGRESS`(409)、`VALIDATION_FAILED`(422)、`INTERNAL_ERROR`(500) |
| 会话与轮次 | `SESSION_NOT_FOUND`(404)、`ACTIVE_TURN_EXISTS`、`SESSION_DELETING`、`TURN_NOT_INTERACTIVE`、`TURN_TERMINAL`(409)、`TIMELINE_CURSOR_INVALID`(422) |
| 模型与供应商 | `PROVIDER_AUTH_FAILED`、`PROVIDER_STREAM_FAILED`(502)、`MODEL_NOT_CONFIGURED`、`MODEL_CONFIGURATION_FAULT`(422)、`MODEL_CONTEXT_EXCEEDED`、`MODEL_CAPABILITY_MISMATCH`(409)、`MODEL_UNAVAILABLE`(503)、`RATE_LIMITED`(429) |
| 工具与媒体 | `TOOL_NOT_ALLOWED`、`TOOL_PATH_INVALID`、`IMAGE_TOO_LARGE`、`UNSUPPORTED_IMAGE`(422) |
| 运行时与终端 | `RESOURCE_BUSY`、`TERMINAL_NOT_WRITABLE`(409)、`RUNTIME_UNAVAILABLE`、`ASYNC_TASK_LOST`(503)、`TERMINAL_TICKET_INVALID`(401)、`TERMINAL_SCROLLBACK_EXPIRED`(410) |
| 框架层拒绝 | `METHOD_NOT_ALLOWED`(405)、`PAYLOAD_TOO_LARGE`(413)、`UNSUPPORTED_MEDIA_TYPE`(415)、`REQUEST_REJECTED`(其他任意 4xx) |

在进入任何处理器之前就被拒绝的请求 —— 无法解析的请求体、缺失的查询参数、方法不匹配、
超大负载 —— 原本只会返回框架自带的纯文本拒绝信息,完全没有 code。
`client_error_envelope` 会把每一个非 Problem 的 4xx 重建成同样的信封:保留原始状态码,
保留框架的文本作为 `detail`(因为它指明了出错的字段),并在 405 时重新写回路由器计算出
的 `Allow` 头。400 和 422 变成 `VALIDATION_FAILED`,404 变成 `RESOURCE_NOT_FOUND`,
所以这些路径无论是否由处理器产生,看起来都是一致的。

Git 命令和由 Git 支撑的 Operation 携带来自 `GitError::code` 的自有错误码,除共享映射
另有规定外一律映射为 409:`GIT_AUTH_FAILED`、`GIT_REMOTE_UNAVAILABLE`、
`GIT_REMOTE_NOT_FOUND`、`GIT_REPOSITORY_NOT_FOUND`、`GIT_REF_NOT_FOUND`、
`GIT_NOTHING_TO_COMMIT`、`GIT_IDENTITY_UNSET`、`GIT_REPOSITORY_LOCKED`、
`GIT_NON_FAST_FORWARD`、`GIT_DIVERGED`、`GIT_INDEX_NOT_EMPTY`、
`GIT_CHECKOUT_CONFLICT`,以及 `GIT_UPDATE_CONFLICT` —— 后者还会写入
`/git/update-conflicts` 下暴露的冲突记录。分类时同时读取 stdout 和 stderr,因为
`git commit` 把最常见的失败(没有已暂存的内容)报告在 stdout 上。

持久化任务在重启后报告 `OPERATION_INTERRUPTED`,自动化另有
`PROJECT_CLONE_FAILED`、`AUTOMATION_TIMED_OUT`、`OPERATION_LEASE_STALE` 和
`FORK_SYNC_PARTIAL_FAILURE`,这样「克隆没有完成」就能与「远端拒绝了 pull request」
区分开。

工具调用在时间线里失败,而不是通过 HTTP 失败,它们的结果携带自己的错误码 ——
流式到达的调用被截断时是 `TOOL_ARGUMENTS_INVALID`(此时工具不会被执行),此外还有
`TOOL_EXECUTION_FAILED`、`TOOL_SKIPPED_AFTER_BLOCK`,以及来自
`crates/execution/src/tools/` 的 `TOOL_EDIT_*` 和 `TOOL_ATTACHMENT_*` 系列。

## 生成的契约

```text
Rust 路由 + utoipa 注解
  -> cargo run -p janus-server --bin generate_openapi
  -> generated/openapi.json
  -> openapi-typescript
  -> apps/web/src/generated/api.ts
```

`cargo xtask generate` 会跑完整条链路。两个生成文件都已提交,且绝不能手改:先改 Rust
路由或 DTO,再重新生成,然后审查 diff。Docker 的 web 构建阶段依赖已提交的
`src/generated/api.ts`,这样客户端产物无需 Rust 工具链即可构建。

## 配置

服务端在启动时从环境读取配置,遇到非法输入拒绝启动。大部分配置面在
`apps/server/src/config.rs` 中解析与校验。

| 变量 | 默认值 | 用途 |
| --- | --- | --- |
| `JANUS_BIND` | `127.0.0.1:4317` | 监听的 socket 地址 |
| `JANUS_MODE` | `development` | `development` 或 `production` |
| `JANUS_DEV_AUTH` | 开发模式 `true`,生产模式 `false` | 绕过认证;仅限回环地址,生产模式下禁止 |
| `JANUS_PUBLIC_ORIGIN` | `http://<bind>` | 绝对的 http(s) origin,不含 path、query、fragment;生产模式必须是 https |
| `JANUS_WEBAUTHN_RP_ID` | 公开 origin 的 host,否则 `localhost` | WebAuthn relying-party ID |
| `JANUS_WEBAUTHN_RP_NAME` | `Janus` | relying-party 显示名 |
| `JANUS_DATA_ROOT` | `.janus-dev` | 数据库、工作区与 blob;会解析为绝对路径 |
| `JANUS_WEB_DIST` | 未设置 | 构建好的 Web 客户端目录,用于同源提供 |
| `JANUS_MASTER_KEY` | 未设置 | base64url(无填充)密钥,解码后必须正好 32 字节;用于加密存储的密钥。生产必需;开发模式会生成并复用 `<data root>/development-master.key` |
| `JANUS_AUTOMATION_WEBHOOK_ENABLED` | `false` | 启用 `/api/v1/automation/webhook` |
| `JANUS_AUTOMATION_WEBHOOK_SECRET` | 未设置 | webhook 启用时必需 |
| `JANUS_AUTOMATION_GITHUB_TOKEN` | 未设置 | 用于私有仓库克隆和 `gh` push 的 classic PAT |
| `RUST_LOG` | `janus=info` | tracing 过滤器;日志以 JSON 输出 |

被拒绝的组合包括:生产模式下启用开发认证、在非回环绑定上启用开发认证、生产模式下使用
非 https 的公开 origin,以及启用了 webhook 却没有 secret。SSE 心跳固定为 15 秒。
`JANUS_MASTER_KEY` 由 `crates/infrastructure/src/secrets.rs` 读取而不是由 `Config`
读取,所以缺少它的生产进程会在初始化阶段失败。

在服务端进程之外读取的变量:`JANUS_WEB_PORT`(默认 `5173`)和 `JANUS_API_TARGET`
(默认 `http://127.0.0.1:4317`)配置 Vite 开发服务器及其代理,`JANUS_BASE_URL`
(默认 `http://127.0.0.1:4317`)把 `janus-test` 指向运行中的服务,`JANUS_WEB_URL`
(默认 `http://127.0.0.1:5173`)是 Playwright 的 base URL。

## Web 客户端

`apps/web` 是一个 SolidJS 应用,用 Vite 构建、TypeScript 做类型检查、Biome 做 lint 与
格式化、Playwright 做测试。它使用 `@solidjs/router`、`@tanstack/solid-query`,以及
`@xterm/xterm` 实现终端。

```text
src/app/          外壳、路由、布局
src/lib/          传输层(api.ts)、查询、事件流、模型流、
                  WebAuthn、视口以及共享工具
src/components/   不含业务词汇的可复用视觉组件
src/features/     auth、automation、execution、file-editor、models、
                  notifications、projects、security、session、source-control、
                  system、terminal
src/generated/    api.ts,由 generated/openapi.json 生成
```

分层规则:`features/` 拥有业务布局与交互,按能力分组;`components/` 只放真正被复用的
视觉组件;`lib/` 承载传输、游标、错误和生成类型这些基础。组件自己不组装 HTTP 请求,
不存在只做转发的 `pages` 层,领域状态机也不在客户端重新实现。

| 脚本 | 命令 |
| --- | --- |
| `dev` | `vite --host 127.0.0.1` |
| `build` | `vite build` |
| `preview` | `vite preview --host 127.0.0.1 --port 4173` |
| `typecheck` | 对应用和 E2E 工程运行 `tsc --noEmit` |
| `lint` / `format` | `biome check` / `biome check --write` |
| `generate:types` | `openapi-typescript ../../generated/openapi.json -o src/generated/api.ts` |
| `test:e2e` | `playwright test` |
| `test:e2e:live` | 先构建 `janus-server` + `janus-test`,再运行 `live-execution.spec.ts` |

## 无障碍

客户端的目标是既能用指针操作,也能只用键盘和屏幕阅读器使用。下面这些约定是契约:
新增 UI 应当遵循它们,而不是把它们所替换掉的旧写法再带回来。

**焦点始终可见。** 每个可交互表面 —— 输入框、select 触发器、两个 textarea、树行、
按钮、链接 —— 在 `:focus-visible` 时都带真正的 `outline`,颜色为 `var(--text)`。
`--shadow-focus` 仍然存在,但它是高程(elevation)而不是焦点指示器:它 8–18% 的 alpha
远低于 WCAG 1.4.11 要求的 3:1,而 `--accent-strong` 相对 `--surface` 实测约为 1.6:1,
两者都不能单独承担焦点指示。输入框使用 `outline-offset: 1px`;textarea 和树行使用负
offset,让焦点环落在无边框表面内部或滚动容器内部。客户端里只剩两处 `outline: none`,
每一处紧随其后都配了 `:focus-visible` 焦点环。

**对话框锁定并归还焦点。** `components/ui/Dialog.tsx` 在挂载时把焦点移入对话框,在
清理时归还给打开它的元素,Tab 与 Shift-Tab 在其中循环,Escape 关闭,并用
`createUniqueId` 接好 `aria-labelledby`/`aria-describedby`。`description` 是必填
prop,所以任何对话框都不可能在不说明后果的情况下上线。破坏性和不可逆的操作 ——
删除供应商或通知通道、重新生成恢复码、push 到远端 —— 都走一个说明「会发生什么」以及
「Janus 能否撤销」的对话框,而不是原生 `confirm()`。

**复合控件有明确的键盘模型。** 文件浏览器是一个 `role="tree"`,带漫游式
tabindex:ArrowDown 和 ArrowUp 在展平后的*可见*行列表上移动,ArrowRight 展开或进入
下级,ArrowLeft 折叠或回到父节点,Home 与 End 跳到首尾,Enter 与 Space 激活。深度由
每行的 `aria-level` 承载;折叠当前持有焦点的分支时会回落到第一行,而不是让整棵树失去
tab 停靠点。会话标签条和工作区文档标签是
`role="tablist"`/`tab`/`tabpanel`,同样使用漫游式 tabindex。每个可折叠触发器都声明
`aria-expanded` 和 `aria-controls`。

**状态会被朗读,且从不只靠颜色。** 错误是 `role="alert"`,进度是 `role="status"`,
通知容器是 `aria-live="polite"`。ahead/behind 计数、排队消息和异步任务行都在图形旁边
提供文字等价物。装饰性图标和 spinner 标为 `aria-hidden="true"`,这样屏幕阅读器读到的
是一次「Saving」而不是「Saving image」。流式转录区刻意保持 `aria-live="off"`:逐个
增量朗读会在一个轮次内淹没屏幕阅读器。

**进行中和失败状态是可读的。** 长耗时的行会显示正在执行哪个操作,而不是只看起来被
禁用;请求进行期间触发器被禁用;加载失败提供 Retry;未保存的缓冲区支持 Ctrl+S /
Cmd+S 保存并在 `beforeunload` 时告警;而输给乐观并发的保存
(`RESOURCE_VERSION_MISMATCH`)会提示重新打开文件并重做这次编辑,而不是报一个笼统的
失败。

`cargo xtask check` 对此的覆盖仅限于 Biome 的无障碍规则、`tsc` 和 `vite build` 能触及
的范围;Playwright 套件不在质量门内,也没有针对焦点行为的组件测试。焦点是否真的落在
预期位置、屏幕阅读器是否真的读出我们以为的内容,靠人工评审与手动验证。另外注意
`styles/tokens.css` 只定义了浅色调色板 —— 没有深色主题需要做对比度检查。

## 验证

`cargo xtask check` 是唯一的质量门,按以下顺序执行:架构检查、
`cargo check --workspace --all-targets --keep-going`(先于代码生成的类型/借用检查门,
`--keep-going` 让 rustc 在一次运行中报告相互独立 crate 的错误)、OpenAPI + 客户端类型
生成、`cargo fmt --check`、`cargo clippy --workspace --all-targets --keep-going -- -D warnings`、
`cargo test --workspace --no-fail-fast`,然后是 Web 的 `typecheck`、`lint` 和 `build`。`cargo xtask`
是 `.cargo/config.toml` 中声明的 `cargo run --package xtask --` 别名。

| 目的 | 命令 |
| --- | --- |
| 校验工具链、安装 Web 依赖 | `cargo xtask setup` |
| 同时运行服务端与 Web | `cargo xtask dev` |
| 只检查架构边界 | `cargo xtask check architecture` |
| 重新生成 OpenAPI 与客户端类型 | `cargo xtask generate` |
| 完整质量门 | `cargo xtask check` |
| 发布构建(workspace + Web 产物) | `cargo xtask build` |
| 单个 crate 的测试 | `cargo test -p <crate>` |
| 浏览器端到端测试 | `bun run --cwd apps/web test:e2e` |
| 空白字符检查 | `git diff --check` |

### 黑盒 CLI

`janus-test` 只通过公开的 HTTP、SSE 和 WebSocket 表面来验证运行中的服务。它从不打开
MongoDB 数据存储,也绝不能依赖 `janus-server`;架构检查会强制这一点。用 `--base-url` 或
`JANUS_BASE_URL` 指定目标。

```text
cargo run -p janus-test -- health
cargo run -p janus-test -- request GET /api/v1/system/info
cargo run -p janus-test -- events follow --count 1
```

| 子命令 | 主要参数 |
| --- | --- |
| `health` | — |
| `request <METHOD> <PATH>` | `--json <file>`、`-H "Name: value"` |
| `events follow` | `--after <cursor>`、`--count <n>` |
| `events range` | `--after`、`--until`、`--limit`(默认 256) |
| `projects list \| create \| get \| git-status` | `--name`、`--url`、`--branch`、`--idempotency-key` |
| `sessions list \| create \| get \| delete \| post-message \| timeline \| get-turn \| steer \| cancel` | `--expected-version`、`--idempotency-key`、`--before/--after/--limit`、`--reason` |
| `terminal create \| list \| ticket \| scrollback \| resize \| signal \| close` | `--after`、`--limit`、`cols rows`、`ctrl_c \| terminate` |
| `operations get \| wait` | `--timeout-seconds`(120)、`--poll-millis`(250) |

普通运行使用确定性的测试供应商。真实供应商、凭据、流式、重试、故障转移、延迟和成本
属于一个独立的 smoke 流程;外部令牌绝不能泄漏进普通测试、日志或提交。

## 持续集成

| 工作流 | 触发 | 任务 |
| --- | --- | --- |
| `.github/workflows/quality.yml` | 推送到 `main`、每个 pull request | 固定 Rust `1.97.0` + Bun `1.3.14`,启动 `mongo:7` 副本集 service 容器,`bun install --frozen-lockfile`,为需要提交的测试设置 git 身份,然后执行 `cargo xtask check` |
| `.github/workflows/ci.yml` | pull request、推送到 `main`/`master`/`dev`、手动 | PR 上构建 Docker 镜像做验证;由允许的 actor 推送时,发布 `linux/amd64` 标签到 GHCR 并运行部署脚本 |

发布的标签是 `ghcr.io/<owner>/<repo>` 下的 `<ref>-amd64` 和 `<short-sha>-amd64`,另有
用于 buildx 注册表缓存的 `:cache` 标签。随后 `scripts/deploy_image.js` 通过 SSH 把某个
标签拉到目标主机,并重建 `CONTAINER_NAMES` 中列出的每个容器,沿用它此前的
`docker inspect` 配置。它读取 `SERVER_ADDRESS`、`USERNAME`、`PORT`、`PRIVATE_KEY`、
`CONTAINER_NAMES` 和 `ADMIN_PASSWORD` 这几个 secret,并在自己的日志中脱敏密钥材料。
提交信息里包含 `deps):` 的提交会被跳过。

GitHub Actions 是正确性的权威:用 `gh run list` 和 `gh run view --log-failed` 判断,
而不是把本地通过当成构建为绿。

## 容器部署

`Dockerfile` 产出一个把前端和后端作为单进程运行的镜像:第一阶段用 Bun 构建 Web 产物,
第二阶段用 `rust:1.97.0-bookworm` 构建 `janus-server`,Debian slim 运行时保留 `git`
(供源码控制适配器使用)和 `tini`(回收派生出的会话与终端进程)。

```text
docker build -t janus:local .
docker run --rm -p 4317:4317 -v janus-data:/data \
  -e JANUS_MONGODB_URI=mongodb://host.docker.internal:27017/?replicaSet=rs0 \
  janus:local
```

镜像默认值是 `JANUS_BIND=0.0.0.0:4317`、`JANUS_DEV_AUTH=false`、
`JANUS_DATA_ROOT=/data` 和 `JANUS_WEB_DIST=/app/web`;`/data` 是一个卷,进程以非特权的
`janus` 用户运行,4317 端口同时提供 API、健康探针和 Web 客户端。需要一个可达的 MongoDB
副本集——用 `JANUS_MONGODB_URI` 指向它(`host.docker.internal` 可达宿主机上运行的副本集)。
真实部署还需设置 `JANUS_MODE=production`,以及与公开主机名匹配的 https
`JANUS_PUBLIC_ORIGIN`。

`janus-admin` 与 `janus-server` 一起打进镜像,因此管理令牌直接用部署好的镜像签发,不
需要代码检出。它会独占打开数据根,所以要在服务端容器停止时,以一次性容器的形式针对同
一个卷运行,并给它和服务端相同的环境 —— `Config::from_env` 会校验整份配置,所以生产环
境下仍需 `JANUS_MASTER_KEY` 和 https 的 `JANUS_PUBLIC_ORIGIN`。

```text
docker run --rm -v janus-data:/data \
  -e JANUS_MODE=production \
  -e JANUS_PUBLIC_ORIGIN=https://janus.example.com \
  -e JANUS_MASTER_KEY="$JANUS_MASTER_KEY" \
  janus:local janus-admin issue-initialization-token
```

## Fork-sync 自动化

webhook 入口默认关闭。用 `JANUS_AUTOMATION_WEBHOOK_ENABLED=true` 加一个非空的
`JANUS_AUTOMATION_WEBHOOK_SECRET` 启用它;缺少 secret 时启动会失败。

`POST /api/v1/automation/webhook` 要求 `Content-Type: application/json`,并在
`X-Janus-Webhook-Secret` 或 `Authorization: Bearer ...` 之一中携带 secret,比较为常量
时间。请求体是固定的 `fork_sync_conflict` 契约:`event`、`timestamp`、`summary`、可选的
`github_credential_id`,以及非空的 `conflicts` 数组,数组项带 `fullName`、`htmlUrl`、
`prUrl`、`defaultBranch`、`parentDefaultBranch`,可选 `parentFullName` 和 `message`。
仓库和 pull request 的 URL 会被规范化到 `github.com`,其他一律以 `422` 拒绝。

合法请求以 `202` 接受,并返回一个 Operation 投影。该 Operation 克隆每个仓库,创建项目和
Supervisor 会话,并在同一个租约下串行处理报告中的各个仓库,子工作项使用确定性的幂等
键,因此被回收的工作项在重启后可以安全续跑。任何邮件正文或令牌都不会作为可执行的提示
输入被持久化。

对私有仓库和基于 `gh` 的 push,把 `JANUS_AUTOMATION_GITHUB_TOKEN` 设为一个 GitHub
classic PAT。它只会作为加密的项目凭据存储,并以 `GH_TOKEN`/`GITHUB_TOKEN` 暴露给受管的
轮次,注入的任务在 push 前会先运行 `gh auth setup-git`。对公开仓库可以不设置它,或者在
载荷中提供一个已存在的项目凭据 id。自动化会话使用的供应商、模型和推理强度是所有者设置,
位于 `/api/v1/automation/settings`,推理强度默认 `high`。

## 文档

各边界的首要参考是目录内的 README:`crates/README.md` 说明模块归属,
`apps/server/README.md` 和各 `src/*/README.md` 说明服务端分层,`apps/web/src/*/README.md`
说明客户端分层。长期有效的设计与规划决策放在 `docs/aegis/`,由 `docs/aegis/INDEX.md`
索引,基线在 `baseline/`,设计在 `specs/`。

## 约定

- 改源头再重新生成:绝不手改 `generated/openapi.json` 或
  `apps/web/src/generated/api.ts`。
- 能力逻辑留在拥有它的模块的 `interface.rs` 之后,跨能力行为放进 `Application`,而不是
  放进处理器或 `AppState`。
- 宁可写三行重复代码,也不要未经验证的 repository、service locator 或全局事件总线;
  不要用一揽子属性去消除 lint 或边界检查的失败。
- 提交信息遵循 Conventional Commits,并说明这次改动为什么要做。

## 许可

AGPL-3.0-or-later,见 `LICENSE`。
