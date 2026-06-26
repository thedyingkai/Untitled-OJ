# OJOS

> 文档状态：当前实现
> 适用范围：项目入口 / 开发 / 部署 / 文档导航
> 最后更新：2026-06-26

OJOS 是一个分布式在线评测系统，当前处于 Core Judge Platform 工程化阶段。仓库中已经包含 Gateway、Auth、Problem API、Judge API、Vue 前端、PostgreSQL、Redis signal stream、Worker Link 以及 Rust judge-worker。

完整文档入口：

- [文档首页](docs/README.md)
- [端到端工程验收](docs/e2e/e2e-engineering-acceptance.md)
- [Control Plane 部署](docs/deploy/deploy-control-plane.md)
- [模块拓扑设计](docs/architecture/module-topology.md)

当前可在本机执行的静态验收：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

Docker 镜像构建、Linux cgroup v2、nsjail 和多 worker 运行验收需要具备对应能力的环境。没有实际执行时，不允许把这些验收记录为通过。
