# 运行时

Runtime 能力负责 Runtime、Job、Service、Terminal、日志、端口、访问票据和重启恢复。它只管理外部进程资源，不读取模型凭据、不写 Session 工作区，也不判断 Turn 是否完成。

Project Terminal 连接 Main Workspace；Session 只使用受 Execution 管理的 Job 和 Service。进程、日志和票据的具体实现从 adapters 注入，重启恢复必须是明确的终态或可验证的重新接管。
