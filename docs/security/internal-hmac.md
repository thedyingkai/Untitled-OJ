# 内部 HMAC 安全说明

> 文档状态：当前实现
> 适用范围：安全 / 后端开发 / 部署
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 Gateway 到内部服务之间的 HMAC 鉴权机制。它用于防止外部客户端伪造 `X-Auth-Verified`、用户 ID、角色或权限头，从而绕过 Gateway。

## 2. 适用范围

适用于维护 `services/gateway`、`services/auth`、`services/problem-api`、`services/judge-api` 和 `services/shared/security/internalauth` 的开发者。

## 3. 当前实现

Gateway 在代理内部请求时签名，内部服务验证签名后才信任转发上下文。签名覆盖 method、path、timestamp、body digest 和 nonce。nonce 用于防重放。

## 4. 目标设计

后续可接入密钥轮换、双 key 灰度和集中 secret manager。轮换期间应允许新旧 key 短暂并行，但不能降低 fail closed 行为。

## 5. 关键流程

客户端请求先进入 Gateway。Gateway 校验 JWT，生成内部用户上下文，计算 HMAC 签名并转发。内部服务验证签名、timestamp 和 nonce，然后执行业务权限判断。

内部请求至少应包含以下语义：

| 项 | 用途 |
| --- | --- |
| method | 防止同一路径的不同 HTTP 方法被重放 |
| path | 绑定目标 API，避免签名被挪用到其他资源 |
| timestamp | 限制签名有效窗口 |
| nonce | 防止有效窗口内重复请求 |
| body digest | 防止请求体被篡改 |
| user context header | 传递 Gateway 已校验的用户身份和权限摘要 |

内部服务只在 HMAC 验证通过后读取用户上下文头。验证失败时应 fail closed，返回 401 或 403，而不是降级为匿名用户。

## 6. 配置说明

内部 HMAC key 通过环境变量或配置注入。健康检查只能显示 key 是否存在或是否可用，不返回明文。

开发环境可以使用本地 `.env.example` 中的变量名准备配置，但真实 key 必须由运行环境注入。多个服务必须使用同一组内部签名配置，否则 Gateway 转发到下游时会全部失败。多机部署时，应保证 Control Plane 内所有内部服务时间同步，否则 timestamp 校验会出现间歇性失败。

健康检查中只允许返回类似 `configured: true`、`status: ok`、`latency_ms`、`error` 这样的摘要，不能返回 key、签名串、nonce 内容或下游服务完整 DSN。

## 7. 安全边界

HMAC 只保护内部服务信任边界，不替代用户权限系统。Worker API 还必须校验 `X-OJOS-Worker-Token` 和 task lease。

## 8. 验收方式

伪造 `X-Auth-Verified`、过期 timestamp、重复 nonce 或错误签名都应失败。普通用户不能直接访问内部服务完成管理操作。

建议执行三类验收：

1. 正常链路：通过 Gateway 登录并访问题目、提交、admin health，确认内部服务能识别用户上下文。
2. 伪造链路：绕过 Gateway 直接向内部服务加 `X-Auth-Verified` 和用户头，预期失败。
3. 重放链路：复用旧 timestamp/nonce 或修改 body 后复用签名，预期失败。

如果当前环境没有直接访问内部服务的网络条件，应把验收步骤记录为“未执行”，不能写成通过。静态验证只能证明代码和配置没有明显危险残留，不能替代网络边界验收。

## 9. 常见问题

- 所有内部请求失败：检查 Gateway 和服务端 key 是否一致。
- 偶发 timestamp 失败：检查机器时间同步。
- 重放误报：检查 nonce 存储和过期时间。

## 10. 相关文档

- [安全边界](security-boundary.md)
- [架构内部 HMAC](../architecture/internal-auth.md)
