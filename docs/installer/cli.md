# CLI

正式 CLI 围绕 Service-first 对象提供命令：

- `ojosctl service validate/package/verify/install-plan/install/enable/disable`
- `ojosctl set expand`
- `ojosctl endpoint validate/plan-register`
- `ojosctl link plan-create`
- `ojosctl topology snapshot`
- `ojosctl runtime services`
- `ojosctl device ...`

旧 Module-first CLI 已删除。用户输入旧命令时只能得到删除提示，不能进入成功执行路径。
