# 服务契约与生成关系

本文描述当前仓库同时存在的服务输入和消费路径，不把过渡设计写成已完成事实。

## 当前输入的所有权

| 文件或目录 | 所有者与用途 | 当前消费路径 |
| --- | --- | --- |
| `services/<id>/ojos.service.yaml` | 开发者维护的 Service Contract v3 源清单：运行时、API/资源依赖、迁移、权限、前端和配置 schema 引用 | `tools/ojos-service` 编译器 |
| 清单引用的 `api/openapi.yaml`、schema、frontend manifest | 开发者维护的 v3 契约输入 | 编译为统一契约、SDK 和构建输入 |
| `services/<id>/gen/` | 编译器所有的产品输出，不手工改业务字段 | Go/Rust/TypeScript SDK、服务端适配、`service.contract.json` 和 `build-input.json` |
| `service.yaml`、`release.yaml` | 现存 Service 身份与 Release v2 源码模板；不是带真实 OCI digest 的正式发布物 | 兼容视图、现有 runtime resource 打包、旧 Catalog 生成入口 |
| 根目录 `openapi.yaml`、`*.api`、GoZero routes/handler | 部分现存服务的发布描述与 GoZero 输入/实现；尚未全部由 v3 输入统一派生 | 对应服务自身的生成入口和路由注册 |
| 经外部 builder 解析的真实 artifact digests 与签名 Catalog | 构建/发布产物，不是开发者可随意填写的默认值 | `ojos service publish`、可信 Catalog 导入和安装 |

v3 编译器不会自动改写现有 GoZero 业务处理器。修改一个现存服务的 API 时，要同时明确它的实现入口与契约入口，不能只修改 `gen/`，也不能默认两份 OpenAPI 已经自动同步。

## v3 产品生成流程

```text
ojos.service.yaml + 引用的 OpenAPI/schema/frontend manifest
    → ojos service build
    → service.contract.json + SDK/适配产物 + build-input.json
    → builder 生成实际 OCI、前端及构建证明
    → 解析真实 digest 后发布签名 Catalog
```

示例使用 `services/contest-service/ojos.service.yaml`。它是 v3 参考服务，不含伪造的运行镜像 digest 或发布签名。编译器 build 输入不代表这些实际产物已经构建、签名或上线。

软件验证及演练在独立验证目录中运行，不属于上述产品生成输入，不得由生成器输出到产品工作树。

## 服务启动与业务代码边界

`contest-service` 是新服务的启动参考。它的进程入口不再同时组装数据库、权限、事件与 HTTP：

| 入口 | 职责 |
| --- | --- |
| `main.go` | 处理产品健康检查命令、加载配置、建立进程信号上下文、选择退出码。 |
| `internal/config` | 将环境与 Agent 物化文件位置转换为配置；托管模式约束在这里明确。 |
| `internal/app` | 接收显式上下文、配置和 logger，组装组件依赖图与适配器；不重新读取进程信号或环境。 |
| `internal/httpapi` | 请求解析、权限调用、业务调用与 HTTP 响应。 |
| `internal/contest` | 比赛规则、仓储接口与当前持久化实现；HTTP 类型不进入规则。 |
| `platform/shared/go/bootstrap` | 稳定的依赖启动顺序、失败回滚、readiness、后台失败传播及逆序关闭。 |

参考服务的启动依赖仍为 logging → database/permissions → events → domain → HTTP；事件依赖 database，domain 同时依赖 database、permissions、events。关闭时保留整体 15 秒期限，不把一次 Start 成功当作持续 ready。进程退出与应用关闭由同一上下文衔接，组件构造不得自行退出进程。

新增服务沿用这些职责约定，不复制一套新的生命周期管理器。已有 6 个 GoZero 服务已分离进程入口、`internal/app` 应用组装与 `internal/svc` 资源构造/清理，启动失败向上返回并释放已取得资源。它们保留 GoZero 的路由、中间件和信号处理，没有全部改接共享 bootstrap；具体入口、失败回滚及仍保留的运行限制见 [Go 服务生命周期](service-lifecycle.md)。

## 当前过渡边界

- `manager/installer` 的 runtime resource 发现仍读取 `service.yaml` / `release.yaml`，不能据此断言它已自动识别全部 v3 参考服务。
- Catalog v2 是签名分发格式；Service Contract v3 是服务描述和编译工作流。二者不是相互替换的同一版本号。
- Release v2 的现有消费和兼容数据不能仅因 v3 输入存在而删除。后续应建立显式转换，再逐步消除重复人工维护。
- 新增服务优先从 v3 参考服务和编译器入口开始。接入现有打包/部署路径时，先确认该路径已支持它，不能以 SDK 编译成功代替部署集成。

后续统一工作按[重构计划](refactoring-plan.md)推进。旧协议字段仍见[Service Contract v2](../orchestrator/service-contract-v2.md)，运行部署规则见[运维手册](../orchestrator/operations-v1.md)。
