import { ref } from "vue";

/**
 * 控制面令牌。daemon 侧配置 `ORCHESTRATOR_INTERNAL_TOKEN` 后，除 `GET /health`
 * 与静态资源外的所有 API（含只读 GET）都必须带上该请求头，否则返回 401，
 * 见 services/orchestrator/backend/src/auth.rs。
 */
export const ORCHESTRATOR_TOKEN_HEADER = "x-ojos-orchestrator-token";

/** 令牌只写浏览器本地存储，不进构建产物、不随布局同步到 daemon。 */
export const TOKEN_STORAGE_KEY = "ojos.orchestrator.token";

function readStorage(): string {
  try {
    return window.localStorage.getItem(TOKEN_STORAGE_KEY)?.trim() ?? "";
  } catch {
    /* 隐私模式 / 禁用存储时按“未配置令牌”处理 */
    return "";
  }
}

/** 当前令牌的响应式镜像：请求头注入与侧边栏指示共用同一份。 */
const currentToken = ref<string>(readStorage());

/**
 * 全局鉴权状态：任意请求收到 401 后置真，由 TokenGate 覆盖全屏索取令牌。
 * 放在这里而不是 pinia store，是为了避免 api.ts ↔ store.ts 的循环依赖
 * （store.ts 依赖 api.ts，api.ts 只依赖本模块）。
 */
export const authRequired = ref(false);

/** 请求头用的令牌；为空表示未配置，request() 不会发这个头。 */
export function orchestratorToken(): string {
  return currentToken.value;
}

/** 供 store getter 使用，读取 ref 以保持响应式。 */
export function hasOrchestratorToken(): boolean {
  return currentToken.value.length > 0;
}

export function saveOrchestratorToken(token: string): void {
  const value = token.trim();
  currentToken.value = value;
  try {
    if (value) {
      window.localStorage.setItem(TOKEN_STORAGE_KEY, value);
    } else {
      window.localStorage.removeItem(TOKEN_STORAGE_KEY);
    }
  } catch {
    /* 存不下就只在本次会话内生效 */
  }
}

export function clearOrchestratorToken(): void {
  saveOrchestratorToken("");
}

export function markAuthRequired(): void {
  authRequired.value = true;
}

export function resolveAuthRequired(): void {
  authRequired.value = false;
}
