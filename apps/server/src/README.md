# 服务端源码

这里的分层是依赖约束，不是按调用次数划分的文件夹。`platform` 提供稳定基座，能力模块拥有业务状态，`application` 负责跨能力工作流，`transport` 面向公开协议，`adapters` 连接外部世界。Workspace 已作为 `janus-workspace` 独立 crate 由 server 组装；其余能力仍在迁移期，新增代码按目标边界写，不要继续把旧模块做成更大的总接口。

能力之间通过命令、查询和不可变快照协作。需要多个能力共同完成的动作由工作流统一编排；不要用全局事件总线、通用 Repository 或 service locator 逃避依赖声明。
