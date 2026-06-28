# 路径泄露防护

> 文档状态：当前实现
> 适用范围：安全 / API 开发 / 前端开发
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明如何防止服务器内部路径通过 Public API、前端页面或 worker 协议泄露。

## 2. 适用范围

适用于 Problem API、Judge API、Worker API、前端提交详情、题目包页面和 admin debug API。

## 3. 当前实现

worker artifact 通过 URL、digest 和 size metadata 传输，不传 Control Plane 本地路径。路径泄露防护必须通过 DTO 审查、前端页面审查和 E2E `path_leaks=0` 共同确认。

## 4. 目标设计

所有新 API 都应先设计响应 DTO，只暴露业务摘要和受控日志内容。内部 repository 字段不能直接作为 public response。

## 5. 关键流程

内部 storage 读取文件后，API 转换层只返回逻辑摘要。管理员调试日志由后端读取文件并截断为文本，不返回文件路径。

路径信息应按三类处理：

| 类别 | 示例 | 可见范围 | 处理方式 |
| --- | --- | --- | --- |
| 服务端内部路径 | artifact root、worker work dir | 服务进程内部 | 不进入 public DTO |
| artifact 元数据 | digest、size、content type | worker/API 受控返回 | 用于校验传输，不表示本地路径 |
| 调试文本 | stdout/stderr/checker log 截断内容 | 有权限用户或管理员 | 后端读取后截断返回 |

Public API 和前端只应处理业务状态、分数、耗时、内存、错误摘要和受控日志文本。即使管理员页面需要排查，也应通过专门 API 获取截断内容，而不是展示文件路径。

## 6. 配置说明

路径相关配置只在服务端使用，例如 artifact root 和 worker work dir。前端不需要也不应知道这些路径。

配置文件可以声明本地开发 storage root，但文档必须说明这是 Control Plane 内部存储，不是 remote worker 挂载方案。worker node 的 work dir 是本机临时目录，生命周期由 worker 管理，不能与 Control Plane storage 混用。

## 7. 安全边界

Public API 不返回源码路径、结果路径、题目包目录、stdout/stderr/checker log 路径。Worker API 不返回主机绝对路径。

## 8. 验收方式

执行人工检查和 E2E：

1. 打开题目详情和题目包页面，确认只显示题面、样例、限制和校验摘要。
2. 打开提交详情，确认只显示状态、分数、耗时、内存、case 摘要和截断日志。
3. 以普通用户访问他人 private 题目或提交，确认返回 403/404。
4. 检查前端源码没有直接引用内部路径字段。
5. 执行 Docker API E2E，确认 summary 中 `path_leaks=0`。

只要内部路径字段进入 public schema、前端展示或 worker task artifact 之外的响应，就必须修复。

## 9. 常见问题

- DTO 直接复用 DB model：容易泄露内部字段。
- 前端显示路径：说明 API 转换层错误。
- worker 需要挂载 storage：说明 Worker Link 设计被绕过。

## 10. 相关文档

- [Storage 与 Artifact 模型](../architecture/storage-artifact-model.md)
- [Worker Link 协议](../architecture/worker-link-protocol.md)
