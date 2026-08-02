# 会话

Session 能力负责会话、消息、用户可见时间线、检查点、上传和附件。它不拥有 Round、Tool Call、模型 Attempt、进程或工作区字节；Execution 的状态通过公开命令写入，时间线只是展示投影。

新 Session 从 Main 当前 Git 管理内容创建，Session 工作区对用户只读。跨能力的创建、删除和 Runtime 清理由工作流编排。
