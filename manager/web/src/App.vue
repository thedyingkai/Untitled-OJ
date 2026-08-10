<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { RouterLink, RouterView } from "vue-router";
import {
  authError,
  authLabel,
  authMode,
  authRedirecting,
  authenticated,
  beginOidcLogin,
  logoutBrowserSession,
} from "./auth";
import { useOrchestrator } from "./store";

const store = useOrchestrator();

onMounted(async () => {
  store.startPolling();
  await store.refreshCore();
  await store.loadLayout();
});
onUnmounted(() => store.dispose());

async function logout() {
  try {
    await logoutBrowserSession();
  } catch (error) {
    store.toast("err", (error as Error).message);
  }
}

const nav = [
  { to: "/topology", label: "拓扑", icon: "◈" },
  { to: "/market", label: "商店", icon: "▤" },
  { to: "/services", label: "服务", icon: "❖" },
  { to: "/nodes", label: "Nodes", icon: "◎" },
  { to: "/operations", label: "操作", icon: "≡" },
  { to: "/diagnostics", label: "诊断", icon: "⌁" },
];
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">
          <svg viewBox="0 0 32 32" width="26" height="26">
            <rect width="32" height="32" rx="8" fill="#6366f1" />
            <circle cx="10" cy="16" r="3.4" fill="#fff" />
            <circle cx="23" cy="9" r="2.6" fill="#fff" opacity=".85" />
            <circle cx="23" cy="23" r="2.6" fill="#fff" opacity=".85" />
            <path
              d="M12.8 14.6 20.6 10M12.8 17.4 20.6 22"
              stroke="#fff"
              stroke-width="1.6"
              stroke-linecap="round"
            />
          </svg>
        </div>
        <div class="brand-text">
          <div class="brand-name">OJOS</div>
          <div class="brand-sub">Orchestrator</div>
        </div>
      </div>

      <nav class="nav">
        <RouterLink
          v-for="item in nav"
          :key="item.to"
          :to="item.to"
          class="nav-item"
          active-class="active"
        >
          <span class="nav-icon">{{ item.icon }}</span>
          <span>{{ item.label }}</span>
          <span
            v-if="item.to === '/operations' && store.runningOperations.length"
            class="nav-badge"
            >{{ store.runningOperations.length }}</span
          >
        </RouterLink>
      </nav>

      <div class="sidebar-footer">
        <div class="daemon-state">
          <span
            class="dot"
            :class="store.connected ? 'ok' : 'err'"
            :title="store.connected ? 'daemon 已连接' : 'daemon 连接失败'"
          ></span>
          <div class="daemon-meta">
            <div class="daemon-line">
              {{ store.connected ? "daemon 已连接" : "daemon 离线" }}
            </div>
            <div class="daemon-sub" v-if="store.health">
              {{ store.health.store === "persistent" ? "持久存储" : "内存存储" }}
              <template v-if="store.health.warnings?.length">
                · {{ store.health.warnings.length }} 条警告
              </template>
            </div>
          </div>
        </div>

        <!-- HttpOnly 会话指示 -->
        <div class="session-state">
          <span class="chip" :class="authenticated ? 'ok' : ''">{{ authLabel }}</span>
          <button
            v-if="authMode === 'oidc' && authenticated"
            class="btn ghost sm session-logout"
            title="销毁服务端 Web 会话"
            @click="logout"
          >
            退出
          </button>
        </div>
      </div>
    </aside>

    <main class="content">
      <div
        v-if="store.coreStatus === 'error' && !store.authRequired"
        class="connection-error"
        role="alert"
      >
        <span>{{ store.coreError || "编排器数据加载失败" }}</span>
        <button class="btn sm" :disabled="store.loading" @click="store.refreshCore(true)">
          {{ store.loading ? "重试中…" : "重试" }}
        </button>
      </div>
      <div v-else-if="store.authRequired" class="connection-error" role="alert">
        <span>
          {{
            authError ||
            (authRedirecting ? "正在跳转到身份提供方…" : "需要登录后才能访问编排器")
          }}
        </span>
        <button
          v-if="authMode === 'oidc' && !authRedirecting"
          class="btn sm"
          @click="beginOidcLogin"
        >
          登录
        </button>
      </div>
      <RouterView />
    </main>

    <!-- 全局提示 -->
    <div class="toasts">
      <TransitionGroup name="toast">
        <div
          v-for="toast in store.toasts"
          :key="toast.id"
          class="toast"
          :class="toast.kind"
        >
          {{ toast.text }}
        </div>
      </TransitionGroup>
    </div>
  </div>
</template>

<style scoped>
.shell {
  display: flex;
  height: 100%;
}

.sidebar {
  width: 208px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  background: var(--bg-soft);
  border-right: 1px solid var(--border);
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 18px 16px 14px;
}
.logo {
  display: flex;
}
.brand-name {
  font-weight: 700;
  font-size: 15px;
  color: var(--text-strong);
  letter-spacing: 0.02em;
  line-height: 1.1;
}
.brand-sub {
  font-size: 11px;
  color: var(--faint);
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  flex: 1;
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  border-radius: 8px;
  color: var(--muted);
  text-decoration: none;
  font-size: 13.5px;
  font-weight: 500;
  transition: all 0.15s ease;
}
.nav-item:hover {
  color: var(--text);
  background: rgba(148, 163, 184, 0.07);
}
.nav-item.active {
  color: var(--text-strong);
  background: var(--accent-soft);
}
.nav-icon {
  width: 18px;
  text-align: center;
  font-size: 14px;
  opacity: 0.9;
}
.nav-badge {
  margin-left: auto;
  background: var(--accent);
  color: #fff;
  font-size: 10.5px;
  font-weight: 600;
  padding: 1px 7px;
  border-radius: 999px;
}

.sidebar-footer {
  padding: 14px 16px;
  border-top: 1px solid var(--border);
}
.daemon-state {
  display: flex;
  align-items: center;
  gap: 9px;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.dot.ok {
  background: var(--ok);
  box-shadow: 0 0 6px rgba(52, 211, 153, 0.7);
}
.dot.err {
  background: var(--err);
  box-shadow: 0 0 6px rgba(248, 113, 113, 0.7);
}
.daemon-line {
  font-size: 12px;
  font-weight: 500;
}
.daemon-sub {
  font-size: 11px;
  color: var(--faint);
}
.session-state {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-top: 10px;
}
.session-state .chip {
  font-size: 11px;
  padding: 1px 8px;
}
.session-logout {
  padding: 2px 6px;
  font-size: 11px;
}

.content {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.connection-error {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 14px;
  border-bottom: 1px solid rgba(248, 113, 113, 0.45);
  background: rgba(127, 29, 29, 0.22);
  color: #fecaca;
  font-size: 12px;
}

.toasts {
  position: fixed;
  top: 16px;
  right: 16px;
  z-index: 200;
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-width: 380px;
}
.toast {
  padding: 10px 14px;
  border-radius: 10px;
  font-size: 13px;
  background: var(--panel-solid);
  border: 1px solid var(--border-strong);
  box-shadow: var(--shadow);
  word-break: break-all;
}
.toast.ok {
  border-color: rgba(52, 211, 153, 0.4);
}
.toast.err {
  border-color: rgba(248, 113, 113, 0.5);
}
.toast-enter-active,
.toast-leave-active {
  transition: all 0.2s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateX(12px);
}
</style>
