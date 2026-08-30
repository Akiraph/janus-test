# UI 事件驱动化重构规划(三个病根)

> 状态:方向对齐文档,未开始实施。代码现状基于 2026-08-30 分支 `fix/audit-round-1`。

## 背景

审计发现 UI 实时更新存在三个相互关联的病根,根源相同:**客户端没有自己的投影能力,服务端替它投影并推送完整快照**。

### 病根一:UI 更新走"全量投影重推",不是"增量事件应用"

`apps/server/src/application/state_worker.rs` 是投影引擎:每个事件落地后,对每个受影响的资源重新读取完整投影再广播。

- `push_timeline`(`state_worker.rs:294`)每次 `timeline(session_id, None, None, 100)` 把整页 100 条重新查出来、序列化、推给所有客户端;客户端 `setQueryData` 整体替换数组。
- 代价:一条 `timeline.item_created` 事件放大成"整页时间线的查询 + 序列化 + 广播 + 全列表重渲染"。
- `SessionConversation` 的注释承认:`collapse state is keyed by itemId… so it survives <For> re-mounts`——行会整页重挂载,UI 本地状态(折叠/滚动)丢失,靠 `rowOpenState` 补丁救回。

### 病根二:流式文本被逼出走旁路,客户端攒了三套状态系统

全量投影装不下 token 级 delta(总不能每 token 重查整页),于是 stream-text 绕开事件系统直连广播:

- `state_worker.rs:11-12` 明确不投影 `model.stream_delta`;`ModelStreamDelta => {}` 是空分支。
- 服务端 `StateBroadcaster::push_stream_text`(`crates/infrastructure/src/state_broadcaster.rs:240`)推累积全文,`cursor: None`;前端用 `window.dispatchEvent(CustomEvent("janus:stream-text"))` 接收(`apps/web/src/lib/modelStream.ts:191`)。
- 客户端三套并行状态:TanStack Query 缓存、`modelStream` 全局 signal、组件局部 signal。`provisionalText/provisionalReasoning/…` 五个 provisional props 是两条通道的对账逻辑。
- 补偿性启发式:`MAX_RETAINED_OUTPUTS = 32`、缓存 token 归一化、`SUMMARY_START` 正则猜 reasoning 换行;`useEventStream` 里手维护的 `SESSION_SCOPED_QUERY_KEYS`("invalidated to force a refetch")是第二层补偿——重连时 cursor 跳事件,干脆失效重查。

### 病根三:领域模型——事件贫血、面向 UI、聚合边界切错

- `ToolCallChanged` / `RoundChanged` 事件不带 `session_id`,投影引擎 `resolve_session_id`(`state_worker.rs:595`)反查 DB 才能投影——事件不可独立投影。
- `TimelineItem` 是 `sourceKind` 多态的杂货抽屉,thought/tool/message/attachment 全拍扁成一行;前端 `sessionTimeline.ts` + `compression.ts` 把结构还原回来——服务端存投影、客户端做投影,同一逻辑写两遍。
- 23 个 `EventType`(`crates/infrastructure/src/events.rs:27`)把领域事实(`TurnCreated`)、UI 传输(`ModelStreamDelta`)、噪音(`SystemStarted`)混在一个枚举里。
- Turn 执行横跨 sessions/execution/models/workspace 四 crate,事件日志兼做领域审计 + UI 喂料,一仆二主。

### 零碎问题

- `crates/README.md` 还在说 SQLite / `0001_initial.sql`,实际 MongoDB(**已在本分支修复**)。
- turn 帧 id 是 `"sessionid_turnid"` 拼接串,前端 `indexOf("_")` 拆(`useEventStream.ts:83`)——字符串复合键。
- 投影引擎除了事件唤醒还挂 250ms 轮询 ticker(`state_worker.rs:49`)——对通知机制不信任的补丁。
- 广播容量 4096(`state_broadcaster.rs:95`):重帧 → 慢消费者 `Lagged` → 断开 → 重连 → snapshot → 更大负载,潜在抖动循环。

### 值得保留

- durable Operations(work items、幂等、启动恢复)。
- `public_events` 保留日志 + cursor 概念——**修复 UI 问题需要的原料(可重放的有序事件日志)已经在库里,客户端只是没被允许消费事件本身**。
- capability crate 边界、exhaustive match、clippy 门禁、黑盒 test CLI、OpenAPI 生成类型。

## 总判断

修复方向:把 `public_events` 日志 + cursor 从"服务端投影器的输入"升级为**客户端可直接消费的一等公民**。基础设施已齐(可重放、可续传:SSE `/api/v1/events` 的 `id:` 即事件 cursor,EventSource 用 Last-Event-ID 恢复),缺的是三件事:事件自描述性、客户端增量应用、stream-text 归位。

## 阶段划分

每个阶段独立 PR、可独立验证。CI(Quality + Docker)是唯一仲裁者,不本地构建。

### 阶段 0 —— 事件自描述(病根三的最小切口)

**目标**:消灭 `resolve_session_id` 反查;每个事件能独立投影。

- 给所有事件补 `session_id`/`turn_id` 关联键,重点 `ToolCallChanged`、`RoundChanged`(当前只带 turn_id,投影器反查 DB)。
- 给 23 个 `EventType` 加归属注释:domain fact / transport(stream)/ noise。
- **验证**:state_worker 删掉 `resolve_session_id`;现有测试不回归。

**为什么独立**:纯服务端改动,不碰前端,CI 可完整仲裁;为阶段 1 铺路。

### 阶段 1 —— 客户端增量应用(病根一)

**目标**:`timeline.item_created` 不再放大成整页重查 + 全列表重渲染。

- 服务端:SSE 增加 `event: domain` 帧(原始事件,复用 `after(cursor)` 与 `id:`);`event: state` 降级为低频资源的快照兜底。
- 客户端:useEventStream 缓冲事件 + 持久 cursor;新增"事件 → 投影"的增量应用器,Timeline 局部增改,不再整页 `setQueryData`。
- **好处**:折叠/滚动状态不再被整页替换冲掉,`rowOpenState` 补丁可删;广播负载骤降,4096 抖动循环自然缓解。
- **验证**:黑盒 test CLI 断言 SSE 收到 `domain` 帧且含 `session_id`;前端 Timeline 不再整页重挂载(行为回归)。

### 阶段 2 —— turn output 持久流,stream-text 归位(病根二)

**目标**:token 增量进 durable 通道,三套状态收敛成一套。

- Turn 的 token delta 落为 append-only 可重放流,`ModelStreamDelta` 从空分支变成真实事件。
- 前端 `modelStream` 从 DOM CustomEvent 改为订阅同一事件通道;删 `provisional*` 对账、`MAX_RETAINED_OUTPUTS`、`SUMMARY_START`、`SESSION_SCOPED_QUERY_KEYS` 失效重查。
- **验证**:重连后 cursor 续传,无 token 丢失;删除启发式后黑盒行为等价。

### 阶段 3 —— 领域事件模型重切(病根三根治)

**目标**:客户端投影层成为唯一投影者,`TimelineItem` 多态、事件混类、复合键一并收敛。

- `sourceKind` 多态扁平行 → 客户端按领域事件还原,删 `sessionTimeline.ts` / `compression.ts` 双份投影逻辑。
- `"sessionid_turnid"` 复合键 → 结构化 `{sessionId, turnId}`,删前端 `indexOf("_")`。
- Turn 跨 crate 的事件集成层显式化,但保留——它是 durable Operations 之外第二个正确的组件。

**此阶段最大,拆多个子 PR。**

## 边界与保留

- 不碰:capability crate 边界、exhaustive match、clippy 门禁、黑盒 test CLI、OpenAPI 生成类型;durable Operations、`public_events` + cursor 原样保留。
- 250ms ticker:阶段 1 后投影引擎改纯事件唤醒,只在启动恢复期保留。
- 广播容量 4096 断连机制保留为兜底(阶段 1 后负载骤降,不再触发)。

## 验证方式

- 每阶段独立 PR;CI(Quality + Docker)唯一仲裁者;黑盒 test CLI 做行为回归;不本地构建。
- 阶段 0/1 先合入,再推进阶段 2/3;阶段 3 拆多个子 PR。
