# Web Shell Service

Web Shell 是 Root 侧可热插拔 Service。菜单、路由、权限和后端 Endpoint 来自 Root Runtime Manager 生成的 UI registry。

Web Shell 只通过 Gateway 访问后端 Service，不直接访问 Non-root backend endpoint，不执行危险 apply。
