<script setup lang="ts">
/**
 * 控制面令牌门禁：daemon 配置了 ORCHESTRATOR_INTERNAL_TOKEN 时，任何 API 返回
 * 401 都会把 store.authRequired 置真，本组件覆盖全屏索取令牌并重试。
 */
import { nextTick, ref, watch } from "vue";
import { useOrchestrator } from "../store";

const store = useOrchestrator();

const token = ref("");
const saving = ref(false);
const tokenInput = ref<HTMLInputElement | null>(null);

watch(
  () => store.authRequired,
  async (required) => {
    if (!required) return;
    await nextTick();
    tokenInput.value?.focus();
  },
  { immediate: true },
);

async function save() {
  const value = token.value.trim();
  if (!value || saving.value) return;
  saving.value = true;
  try {
    await store.setToken(value);
    if (!store.authRequired) token.value = "";
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Teleport to="body">
    <Transition name="gate">
      <div v-if="store.authRequired" class="gate-overlay">
        <div class="gate-card fade-in">
          <div class="gate-head">
            <span class="gate-icon">🔒</span>
            <div>
              <h3>此编排器启用了控制面令牌</h3>
              <p class="gate-sub">
                daemon 配置了
                <code class="mono">ORCHESTRATOR_INTERNAL_TOKEN</code>，除
                <code class="mono">GET /health</code>
                外的所有接口都需要携带令牌。
              </p>
            </div>
          </div>

          <form class="gate-body" @submit.prevent="save">
            <div class="field">
              <label for="orchestrator-token">访问令牌</label>
              <input
                id="orchestrator-token"
                ref="tokenInput"
                class="input mono"
                type="password"
                autocomplete="off"
                spellcheck="false"
                placeholder="粘贴 ORCHESTRATOR_INTERNAL_TOKEN"
                v-model="token"
              />
              <span class="hint">
                令牌仅保存在本浏览器（localStorage），不会写回 daemon。
              </span>
            </div>
            <div class="gate-actions">
              <button
                class="btn primary"
                type="submit"
                :disabled="saving || !token.trim()"
              >
                {{ saving ? "验证中…" : "保存并重试" }}
              </button>
            </div>
          </form>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.gate-overlay {
  position: fixed;
  inset: 0;
  z-index: 300;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(5, 8, 14, 0.82);
  backdrop-filter: blur(4px);
}
.gate-card {
  width: 440px;
  max-width: 100%;
  background: var(--panel);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow);
  padding: 20px 22px 18px;
}
.gate-head {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  margin-bottom: 16px;
}
.gate-icon {
  font-size: 20px;
  line-height: 1.2;
}
.gate-head h3 {
  font-size: 14.5px;
}
.gate-sub {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--muted);
  line-height: 1.6;
}
.gate-sub code {
  color: var(--accent-2);
  word-break: break-all;
}
.gate-body {
  margin: 0;
}
.gate-body .field {
  margin-bottom: 12px;
}
.gate-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.gate-enter-active,
.gate-leave-active {
  transition: opacity 0.15s ease;
}
.gate-enter-from,
.gate-leave-to {
  opacity: 0;
}
</style>
