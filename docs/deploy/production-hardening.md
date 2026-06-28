# 生产加固

> 文档状态：当前实现
> 适用范围：部署 / 运维 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档列出 OJOS 生产部署前必须完成或确认的安全加固项。

## 2. 适用范围

适用于生产部署、预发环境、安全评审和上线前检查。

## 3. 当前实现

当前仓库提供 compose、环境变量模板、Gateway 内部 HMAC、worker token、权限系统和 E2E/人工审计入口。

## 4. 目标设计

生产环境应使用 TLS、内网隔离、secret manager、对象存储、备份策略、日志审计和最小权限 worker runtime。

## 5. 关键流程

上线前依次检查网络边界、secret、compose、数据库、Redis、artifact storage、admin 权限、worker token 和 E2E 验收记录。

## 6. 配置说明

所有 secret 从环境或 secret manager 注入。示例配置只能作为结构参考，不能直接用于生产。

## 7. 安全边界

只公开前端和 Gateway。worker 只出站访问 Gateway。artifact 下载必须鉴权。日志不能包含 secret 和用户源码全文。

## 8. 验收方式

执行人工配置审计和运行时验收，确认普通用户不能访问 admin、worker token 错误不能注册、内部服务不公开、日志不泄露 secret 或本机路径。

## 9. 常见问题

- 生产使用示例 secret：必须重新生成。
- Redis 暴露公网：立即关闭外部访问。
- worker 容器权限过大：按 nsjail/cgroup 最小要求收敛。

## 10. 相关文档

- [安全边界](../security/security-boundary.md)
- [Control Plane 部署](deploy-control-plane.md)
