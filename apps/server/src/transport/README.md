# 公开传输层

这里是 HTTP、SSE 和 WebSocket 的公共边界。它负责认证上下文、请求验证、DTO、OpenAPI、游标和错误映射，再调用工作流或能力的公开接口。

handler 不直接操作数据库、Unit of Work、EventStore 或内部 projection，也不在这里实现业务重试、资源清理和 Turn 调度。公共类型由 Rust 生成 OpenAPI，再生成前端类型；内部结构变化不能未经设计泄漏到 API。
