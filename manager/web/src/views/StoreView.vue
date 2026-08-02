<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import Modal from "../components/Modal.vue";
import OperationLogs from "../components/OperationLogs.vue";
import { api, findOperationId } from "../api";
import { useOrchestrator } from "../store";
import type { GithubRelease, StoreModule } from "../types";

const store = useOrchestrator();

onMounted(() => {
  if (!store.storeIndex) store.refreshStore();
});

const modules = computed<StoreModule[]>(
  () => store.storeIndex?.index?.modules ?? [],
);
const installed = computed(() => store.storeIndex?.installed ?? {});
const installedCount = computed(() => Object.keys(installed.value).length);

/* ---------- 安装抽屉 ---------- */

const installOpen = ref(false);
const installTarget = ref<StoreModule | null>(null);
const manualMode = ref(false);

const repoInput = ref("");
const releases = ref<GithubRelease[]>([]);
const releasesLoading = ref(false);
const selectedAssetUrl = ref("");
const directUrl = ref("");
const checksumInput = ref("");
const executeDriver = ref(false);
const externalRunning = ref(false);
const hostIp = ref("127.0.0.1");

const installing = ref(false);
const installResult = ref<{ operationId: string | null; ok: boolean } | null>(
  null,
);

function openInstall(module: StoreModule | null) {
  installTarget.value = module;
  manualMode.value = module === null;
  repoInput.value = module?.repo ?? "";
  directUrl.value = module?.repo ? "" : (module?.source_url ?? "");
  checksumInput.value = module?.checksum ?? "";
  releases.value = [];
  selectedAssetUrl.value = "";
  installResult.value = null;
  executeDriver.value = false;
  externalRunning.value = false;
  hostIp.value = "127.0.0.1";
  installOpen.value = true;
  if (module?.repo) loadReleases(module.repo);
}

async function loadReleases(repo: string) {
  if (!repo.includes("/")) {
    store.toast("err", "仓库格式应为 owner/name");
    return;
  }
  releasesLoading.value = true;
  releases.value = [];
  selectedAssetUrl.value = "";
  try {
    releases.value = await api.githubReleases(repo.trim());
    if (!releases.value.length) {
      store.toast("info", "该仓库暂无 Release");
    } else {
      const firstAsset = releases.value[0]?.assets?.[0];
      if (firstAsset) selectedAssetUrl.value = firstAsset.browser_download_url;
    }
  } catch (err) {
    store.toast("err", `获取 Release 失败：${(err as Error).message}`);
  } finally {
    releasesLoading.value = false;
  }
}

const effectiveSourceUrl = computed(
  () => selectedAssetUrl.value || directUrl.value.trim(),
);
const checksumRequired = computed(
  () => store.storeStatus?.require_release_checksum ?? false,
);

function chooseDriverExecution() {
  if (executeDriver.value) externalRunning.value = false;
}

function chooseExternalRunning() {
  if (externalRunning.value) executeDriver.value = false;
}

async function runInstall() {
  const sourceUrl = effectiveSourceUrl.value;
  if (!sourceUrl) {
    store.toast("err", "请先选择 Release 资产或填写包地址");
    return;
  }
  if (checksumRequired.value && !checksumInput.value.trim()) {
    store.toast("err", "当前 daemon 强制校验 release 包，请填写 sha256 校验和");
    return;
  }
  if (executeDriver.value && externalRunning.value) {
    store.toast("err", "运行时驱动与“外部服务已在运行”不能同时启用");
    return;
  }
  installing.value = true;
  installResult.value = null;
  try {
    const payload: Record<string, unknown> = {
      source_url: sourceUrl,
      execute_service_driver: executeDriver.value,
      external_service_running: externalRunning.value,
      host_ip: hostIp.value.trim() || "127.0.0.1",
    };
    if (checksumInput.value.trim()) payload.checksum = checksumInput.value.trim();
    const result = await api.storeInstall(payload);
    const operationId = findOperationId(result);
    installResult.value = { operationId, ok: true };
    store.toast("ok", "安装动作已执行");
    store.refreshCore();
    store.refreshStore(true);
  } catch (err) {
    installResult.value = { operationId: null, ok: false };
    store.toast("err", `安装失败：${(err as Error).message}`);
  } finally {
    installing.value = false;
  }
}

/* ---------- 卸载 ---------- */

const uninstalling = ref("");
const uninstallDriverEnabled = ref(false);

async function uninstall(moduleId: string) {
  if (!uninstallDriverEnabled.value) {
    store.toast("err", "请先授权执行卸载运行时驱动");
    return;
  }
  if (
    !window.confirm(
      `卸载 ${moduleId}？将执行停止/删除运行时并移除 Service 及其 Release 记录（端点与 Link 需先清理）。`,
    )
  )
    return;
  uninstalling.value = moduleId;
  try {
    await api.dispatchAction("service.delete", {
      service_id: moduleId,
      confirm: "true",
      execute_service_driver: "true",
    });
    store.toast("ok", `${moduleId} 已卸载`);
    await Promise.all([store.refreshCore(), store.refreshStore(true)]);
  } catch (err) {
    store.toast("err", `卸载失败：${(err as Error).message}`);
  } finally {
    uninstalling.value = "";
    uninstallDriverEnabled.value = false;
  }
}

const kindLabels: Record<string, string> = {
  gateway: "网关",
  "backend-api": "后端 API",
  "backend-worker": "工作进程",
  database: "数据库",
  cache: "缓存",
  storage: "存储",
  frontend: "前端",
  external: "外部",
  agent: "代理",
};
</script>

<template>
  <PageHeader title="插件商店" subtitle="从索引仓库或任意 GitHub Release 安装模块">
    <button class="btn sm" @click="openInstall(null)">手动安装</button>
    <button class="btn sm" @click="store.refreshStore(true)">刷新索引</button>
  </PageHeader>

  <div class="store-body">
    <!-- 状态条 -->
    <div class="status-bar" v-if="store.storeStatus">
      <span class="chip accent mono">{{ store.storeStatus.index_url }}</span>
      <span
        class="chip"
        :class="store.storeStatus.package_load_enabled ? 'ok' : 'warn'"
        :title="
          store.storeStatus.package_load_enabled
            ? ''
            : '未启用时，导入和安装请求会被拒绝。启动 daemon 前设置 ORCHESTRATOR_RELEASE_PACKAGE_LOAD=1'
        "
      >
        包加载 {{ store.storeStatus.package_load_enabled ? "已启用" : "未启用" }}
      </span>
      <span class="chip" :class="store.storeStatus.github_token_configured ? 'ok' : ''">
        GitHub Token {{ store.storeStatus.github_token_configured ? "已配置" : "未配置" }}
      </span>
      <span
        class="chip"
        :class="store.storeStatus.require_release_checksum ? 'ok' : 'warn'"
      >
        校验和
        {{ store.storeStatus.require_release_checksum ? "强制" : "可选" }}
      </span>
    </div>

    <div v-if="installedCount" class="card uninstall-authorization">
      <label class="check">
        <input
          v-model="uninstallDriverEnabled"
          type="checkbox"
          :disabled="!!uninstalling"
        />
        授权执行卸载运行时驱动
      </label>
      <span class="hint">
        卸载会实际执行停止/删除命令。每次尝试后都会自动撤销授权。
      </span>
    </div>

    <!-- 模块卡片 -->
    <div class="grid" v-if="modules.length">
      <div v-for="module in modules" :key="module.id" class="card module-card fade-in">
        <div class="module-head">
          <div>
            <div class="module-name">{{ module.name }}</div>
            <div class="module-id mono">{{ module.id }}</div>
          </div>
          <span v-if="installed[module.id]" class="chip ok">
            已部署 {{ installed[module.id].deployments.length }} 个 · v{{
              installed[module.id].version
            }}
          </span>
        </div>
        <p class="module-desc">{{ module.description }}</p>
        <div class="module-tags">
          <span class="chip">{{ kindLabels[module.kind] ?? module.kind }}</span>
          <span v-for="tag in module.tags" :key="tag" class="chip">{{ tag }}</span>
        </div>
        <div class="module-actions">
          <button class="btn primary sm" @click="openInstall(module)">
            {{ installed[module.id] ? "重新安装 / 更新" : "安装" }}
          </button>
          <button
            v-if="installed[module.id]"
            class="btn danger sm"
            :disabled="!!uninstalling || !uninstallDriverEnabled"
            :title="
              uninstallDriverEnabled ? '' : '请先授权执行卸载运行时驱动'
            "
            @click="uninstall(module.id)"
          >
            {{ uninstalling === module.id ? "卸载中…" : "卸载" }}
          </button>
          <span class="module-source mono muted">
            {{ module.repo || module.source_url }}
          </span>
        </div>
      </div>
    </div>

    <div v-else class="empty">
      <span class="icon">▤</span>
      <span>
        索引为空或加载失败。<br />
        设置环境变量 <code class="mono">OJOS_STORE_INDEX_URL</code>
        指向索引 JSON（GitHub raw 地址或仓库内相对路径），或使用「手动安装」。
      </span>
    </div>
  </div>

  <!-- 安装抽屉 -->
  <Modal
    :open="installOpen"
    :title="installTarget ? `安装 ${installTarget.name}` : '手动安装模块'"
    width="560px"
    @close="installOpen = false"
  >
    <!-- GitHub Release 流程 -->
    <div class="field">
      <label>GitHub 仓库（owner/name）</label>
      <div class="row">
        <input
          class="input"
          v-model="repoInput"
          placeholder="例如 ojos-modules/judge-api"
          @keyup.enter="loadReleases(repoInput)"
        />
        <button
          class="btn"
          :disabled="releasesLoading || !repoInput.trim()"
          @click="loadReleases(repoInput)"
        >
          {{ releasesLoading ? "获取中…" : "获取 Releases" }}
        </button>
      </div>
    </div>

    <div class="field" v-if="releases.length">
      <label>Release 资产（release 包 zip / release.yaml）</label>
      <select class="select" v-model="selectedAssetUrl">
        <template v-for="release in releases" :key="release.tag_name">
          <option
            v-for="asset in release.assets"
            :key="asset.browser_download_url"
            :value="asset.browser_download_url"
          >
            {{ release.tag_name }} · {{ asset.name }}
            ({{ Math.round(asset.size / 1024) }} KB)
          </option>
        </template>
      </select>
    </div>

    <div class="divider"><span>或直接指定包地址</span></div>

    <div class="field">
      <label>release 包地址</label>
      <input
        class="input"
        v-model="directUrl"
        placeholder="https://…/module.zip 或仓库内相对路径 services/judge-api"
      />
      <span class="hint">
        支持 zip / tar.gz / release.yaml 直链，以及 daemon 仓库根下的相对路径。
      </span>
    </div>

    <div class="field">
      <label>
        校验和（{{ checksumRequired ? "必填" : "可选" }}，sha256:…）
      </label>
      <input class="input" v-model="checksumInput" placeholder="sha256:…" />
    </div>

    <div class="row options">
      <label class="check">
        <input
          type="checkbox"
          v-model="executeDriver"
          :disabled="externalRunning || installing"
          @change="chooseDriverExecution"
        />
        安装后执行运行时驱动（真正启动）
      </label>
      <label class="check">
        <input
          type="checkbox"
          v-model="externalRunning"
          :disabled="executeDriver || installing"
          @change="chooseExternalRunning"
        />
        外部服务已在运行
      </label>
    </div>
    <span class="hint">
      “外部服务已在运行”只登记现有服务并检查端点，不会启动进程；它与运行时驱动互斥。
    </span>
    <div class="field">
      <label>目标主机 IP</label>
      <input class="input" v-model="hostIp" placeholder="127.0.0.1" />
    </div>

    <div v-if="installResult" class="install-result">
      <div class="chip" :class="installResult.ok ? 'ok' : 'err'">
        {{ installResult.ok ? "动作已提交" : "安装失败" }}
      </div>
      <template v-if="installResult.operationId">
        <p class="muted" style="margin: 10px 0 6px">
          操作 <span class="mono">{{ installResult.operationId }}</span> 日志：
        </p>
        <OperationLogs :operation-id="installResult.operationId" live />
      </template>
    </div>

    <template #footer>
      <button class="btn" @click="installOpen = false">关闭</button>
      <button
        class="btn primary"
        :disabled="
          installing ||
          !effectiveSourceUrl ||
          (checksumRequired && !checksumInput.trim())
        "
        @click="runInstall"
      >
        {{ installing ? "安装中…" : "导入并安装" }}
      </button>
    </template>
  </Modal>
</template>

<style scoped>
.store-body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
}

.status-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 16px;
}

.uninstall-authorization {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
  padding: 10px 14px;
  border-color: rgba(245, 158, 11, 0.35);
}
.uninstall-authorization .hint {
  margin-left: auto;
  text-align: right;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 14px;
}

.module-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition: border-color 0.15s ease, transform 0.15s ease;
}
.module-card:hover {
  border-color: var(--border-strong);
  transform: translateY(-1px);
}
.module-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
}
.module-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-strong);
}
.module-id {
  font-size: 11px;
  color: var(--faint);
}
.module-desc {
  margin: 0;
  font-size: 12.5px;
  color: var(--muted);
  min-height: 36px;
}
.module-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.module-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 2px;
}
.module-source {
  font-size: 10.5px;
  margin-left: auto;
  max-width: 46%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row {
  display: flex;
  gap: 8px;
}
.row .input {
  flex: 1;
}
.options {
  margin-bottom: 14px;
  flex-wrap: wrap;
  gap: 14px;
}
.check {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12.5px;
  color: var(--muted);
  cursor: pointer;
}
.check input {
  accent-color: var(--accent);
}

.divider {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 6px 0 14px;
  color: var(--faint);
  font-size: 11.5px;
}
.divider::before,
.divider::after {
  content: "";
  flex: 1;
  height: 1px;
  background: var(--border);
}

.install-result {
  margin-top: 6px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}

code {
  background: rgba(148, 163, 184, 0.12);
  padding: 1px 6px;
  border-radius: 4px;
}
</style>
