# 数据库迁移

这里维护当前部署的唯一 SQLx migration 序列。已应用的 SQL 文件只能新增，不能改写；文件头的 owner 声明必须和表所有权检查一致。迁移由 server 组合根提供给 `janus-infrastructure` 执行，业务规则仍属于对应能力模块。
