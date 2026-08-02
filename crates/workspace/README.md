# Workspace

Workspace 负责 Main 和 Session 工作区副本、内容修订、快照、清单、差异以及受控文件变更。它只处理文件内容和修订身份，不拥有 Git 仓库协议、Session 消息或 Turn 执行；Git worktree 仅作为副本生命周期的底层机制。

所有路径必须经过工作区边界校验，文件变更必须推进内容修订并留下原因、操作者和前一修订。Project、Session 和 Execution 通过窄接口使用工作区，不得直接访问工作区表或底层目录。

复制、传播、删除和冲突处理属于工作流编排；本模块只提供可组合的工作区操作。历史迁移仍使用旧 owner 名称，检查器会将其归一到 `workspace`。
