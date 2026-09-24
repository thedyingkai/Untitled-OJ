# Web 客户端的功能边界

Web 是 `/api/v1` 客户端。页面可以组织交互，但权限、部署状态和异步操作结果仍以控制面为准；客户端校验不能替代服务端的签名、ETag、计划摘要和冲突检查。

## 代码入口

```text
manager/web/src/
  views/                         页面布局、路由入口与交互组装
  components/                    跨页面的展示组件
  features/
    control-plane/
      state.ts                   Pinia 状态与一致刷新
      runtime.ts                 每个 Store 实例的请求、定时器、取消与代次
      projection.ts              显式服务端事实 → 展示行的纯转换
      context.ts                 表单可用的能力、反馈和刷新命令
    store/
      api.ts / normalizers.ts    Release 接口及响应适配
      useReleaseInstall.ts       安装输入、只读校验、确认和提交
      useReleaseImport.ts        仅导入 Release
      useReleaseLifecycle.ts     升级、回滚、卸载与元数据删除的交互
      composition-form.ts        签名 CompositionPlan 的动态输入规则
    catalog/                     来源注册、信任输入和列表管理
    nodes/ deployments/ topology/ operations/ diagnostics/
                                 对应功能的 API、模型转换与辅助逻辑
  shared/api/                    身份等待、CSRF、超时、取消、信封和分页
  types.ts                       当前控制面客户端共享契约
```

功能模块直接引用自己的 API client，跨功能调用显式指向相应模块。`shared/api` 不依赖功能模块；响应适配也不读取 Pinia。根目录 `api.ts`、`store.ts`、`composition-form.ts`、`deployment-errors.ts` 只保留旧源码路径的兼容导出，正式调用方不再通过它们取得实现。新代码不要增加兼容入口的消费者。

`StoreView.vue` 负责页面组装与模板，不再持有所有安装、导入、部署变更和 Catalog 表单实现。表单接收窄的 `ControlPlaneContext` 和所需只读选择列表，不自行查找全局 Store。

## 状态与请求所有权

- Pinia 保存可展示的数据与加载状态；浏览器资源保存在以 Store 为键的实例运行状态中，不进入序列化数据。
- 同一 Store 的普通轮询请求合并；强制刷新递增代次并取消旧请求。不同 Store 不共用刷新 Promise、AbortController、轮询或 toast 定时器。
- `dispose()` 只关闭本实例资源。请求即使忽略取消、最终仍返回，也要先核对所有权，再写入状态。
- `projection.ts` 只从 Node、Deployment 和 Topology 参数生成展示行，不请求网络、不更改输入，也不通过同名服务猜测 Deployment 身份。
- 画布布局仍按 Topology 保存，不进入业务 revision。旧布局响应在赋值前被丢弃。

## 安装校验与确认

安装流程保留“选择 → 校验 → 确认当前输入 → 提交”的边界。输入指纹包含 Release、Catalog 来源、channel、Node、Topology/ETag、Binding 和 pipeline 参数。请求返回及确认摘要生成后都核对代次与输入；切换 Release、编辑输入或开始新校验会使旧结果失效。

首次取得 CompositionPlan 是计划发现，不能立即安装。输入所依赖的 plan digest 或 release graph digest 改变时，也必须重新校验。模糊 provider 不被自动选中；含 API requirement 的安装仍要求明确 Topology、applied revision 和服务端 prospective diff。

删除 Release 元数据、卸载 Deployment、仅导入和安装继续使用不同命令。`202` 与操作编号只表示已接受；最终结果来自 Operation 与服务端投影。

## 修改与验收

修改接口响应时先定位对应 `api.ts` 和 normalizer；修改安装流程时先定位表单及 Composition 规则；修改多表展示关系时先定位 projection；修改轮询和取消时先定位 state/runtime。不要重新把这些职责聚合回页面或根目录兼容文件。

所有软件验证在仓库外，对当前源码副本执行。类型检查与打包、纯函数/组件/竞态回归是不同证据；本地响应替身不等同于已部署控制面、OIDC、Agent 或真实容器链路的验收。
