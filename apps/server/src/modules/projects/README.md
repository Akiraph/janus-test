# 项目

Project 能力负责项目元数据、凭据、Runtime 配置和出站策略。Git 状态与操作会迁到 Source Control，文件字节、修订和工作区副本会迁到 Workspace；新代码不要把这些责任继续塞回 Project。

项目只提供公开查询和命令给工作流使用，不替其他能力直接写表。删除项目时由工作流先清理 Runtime 资源，再清理项目自身状态。
