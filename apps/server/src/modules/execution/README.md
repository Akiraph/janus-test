# Execution

Execution 负责 Turn、Round、Tool Call、Ask、上下文、工具注册和完成判断。Session 只保存会话与时间线，Model、Runtime、Workspace 和 Source Control 通过窄接口参与执行。

新增执行逻辑应先判断它属于能力接口还是跨模块流程。能力接口放在这里，跨模块事务和调度放在 `application`；HTTP、定时任务和 Runtime 回调只调用应用层入口，不复制调度逻辑。

表名和事件类型是存储契约，改目录或 Rust 类型时不要顺手修改它们。工具执行必须经过本模块的上下文和路径边界，不能绕过接口直接写其他模块的表。
