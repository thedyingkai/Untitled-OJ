<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import Modal from "../components/Modal.vue";
import OperationLogs from "../components/OperationLogs.vue";
import { api } from "../api";
import { useOrchestrator } from "../store";
import type { DeploymentRow, StoreModule } from "../types";

const store = useOrchestrator();

onMounted(async () => {
  if (!store.storeIndex) {
    // App polling may still be loading the dynamic capability matrix. Coalesce
    // with it before deciding whether catalog.search is currently published.
    await store.refreshCore();
    await store.refreshStore();
  }
});

const packageSearch = ref("");
const modules = computed<StoreModule[]>(() => {
  const items = store.storeIndex?.index?.modules ?? [];
  const query = packageSearch.value.trim().toLowerCase();
  if (!query) return items;
  return items.filter((module) =>
    [module.id, module.name, module.description, module.kind, ...module.tags]
      .join(" ")
      .toLowerCase()
      .includes(query),
  );
});
const installedCount = computed(() => store.deployments.length);
const readyNodes = computed(() =>
  store.nodes.filter((node) => node.status.toUpperCase() === "READY"),
);
function deploymentsFor(serviceId: string): DeploymentRow[] {
  return store.deployments.filter(
    (deployment) => deployment.service_id === serviceId,
  );
}

/* ---------- 安装抽屉 ---------- */

const installOpen = ref(false);
const installTarget = ref<StoreModule | null>(null);
const targetNodeId = ref("");
const installing = ref(false);
const validating = ref(false);
const validationResult = ref<{
  valid: boolean;
  catalogId: string;
  keyIds: string[];
  platform: string;
} | null>(null);
const installResult = ref<{ operationId: string | null; ok: boolean } | null>(
  null,
);

function openInstall(module: StoreModule) {
  installTarget.value = module;
  installResult.value = null;
  validationResult.value = null;
  targetNodeId.value = readyNodes.value[0]?.node_id ?? "";
  installOpen.value = true;
}

async function runValidate() {
  if (!store.ensureAction("release.validate")) return;
  const module = installTarget.value;
  if (!module || !targetNodeId.value) {
    store.toast("err", "必须选择受信任 Catalog Release 和 READY Node");
    return;
  }
  validating.value = true;
  validationResult.value = null;
  try {
    const result = await api.storeValidate({
      service_id: module.id,
      version: module.version,
      catalog_source_id: module.source_id,
      channel: module.channel,
      target_node_id: targetNodeId.value,
    });
    validationResult.value = {
      valid: result.valid,
      catalogId: result.catalog_id,
      keyIds: result.verified_key_ids,
      platform: `${result.target_platform.os}/${result.target_platform.arch}`,
    };
    store.toast("ok", "Release 签名、依赖、平台和 Provider 计划校验通过");
  } catch (err) {
    store.toast("err", `Release 校验失败：${(err as Error).message}`);
  } finally {
    validating.value = false;
  }
}

async function runInstall() {
  if (!store.ensureAction("release.install")) return;
  const module = installTarget.value;
  if (!module || !targetNodeId.value) {
    store.toast("err", "必须选择一个 READY Node");
    return;
  }
  installing.value = true;
  installResult.value = null;
  try {
    const result = await api.storeInstall({
      service_id: module.id,
      version: module.version,
      catalog_source_id: module.source_id,
      channel: module.channel,
      target_node_id: targetNodeId.value,
      mode: "MANAGED",
      start: true,
      migration_policy: "APPLY",
    });
    installResult.value = { operationId: result.operation_id, ok: true };
    store.toast("ok", `安装操作已提交：${result.operation_id}`);
    await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
  } catch (err) {
    installResult.value = { operationId: null, ok: false };
    store.toast("err", `安装失败：${(err as Error).message}`);
  } finally {
    installing.value = false;
  }
}

/* ---------- 仅导入 ---------- */

const importOpen = ref(false);
const importTargetKey = ref("");
const importTargetNodeId = ref("");
const importing = ref(false);

function moduleKey(module: StoreModule): string {
  return `${module.source_id}\u0000${module.id}\u0000${module.version}`;
}

const importTarget = computed(
  () => modules.value.find((module) => moduleKey(module) === importTargetKey.value) ?? null,
);

function openImport(module?: StoreModule) {
  const target = module ?? modules.value[0];
  importTargetKey.value = target ? moduleKey(target) : "";
  importTargetNodeId.value = readyNodes.value[0]?.node_id ?? "";
  importOpen.value = true;
}

async function runImport() {
  if (!store.ensureAction("release.import")) return;
  const module = importTarget.value;
  if (!module || !importTargetNodeId.value) {
    store.toast("err", "必须从受信任 Catalog 选择 Release 和目标平台 Node");
    return;
  }
  importing.value = true;
  try {
    await api.storeImport({
      service_id: module.id,
      version: module.version,
      catalog_source_id: module.source_id,
      channel: module.channel,
      target_node_id: importTargetNodeId.value,
    });
    store.toast("ok", "Release 已导入；没有创建 Deployment 或运行时任务");
    importOpen.value = false;
    await store.refreshStore(true);
  } catch (err) {
    store.toast("err", `导入失败：${(err as Error).message}`);
  } finally {
    importing.value = false;
  }
}

/* ---------- 卸载 ---------- */

const uninstalling = ref("");
const replacing = ref("");
const deletingRelease = ref("");

async function replaceRelease(
  deployment: DeploymentRow,
  action: "upgrade" | "rollback",
) {
  const capability = action === "upgrade" ? "release.upgrade" : "release.rollback";
  if (!store.ensureAction(capability)) return;
  const label = action === "upgrade" ? "升级到最新兼容版本" : "回滚到最近一次已证明版本";
  if (!window.confirm(`${label}：${deployment.deployment_id}？`)) return;
  replacing.value = `${action}:${deployment.deployment_id}`;
  try {
    const result = action === "upgrade"
      ? await api.storeUpgrade({ deployment_id: deployment.deployment_id })
      : await api.storeRollback({ deployment_id: deployment.deployment_id });
    store.toast("ok", `${label}操作已提交：${result.operation_id}`);
    await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
  } catch (err) {
    store.toast("err", `${label}失败：${(err as Error).message}`);
  } finally {
    replacing.value = "";
  }
}

async function uninstall(deployment: DeploymentRow) {
  if (!store.ensureAction("deployment.uninstall")) return;
  if (
    !window.confirm(
      `卸载 ${deployment.deployment_id}？Release 元数据会保留。`,
    )
  )
    return;
  uninstalling.value = deployment.deployment_id;
  try {
    const result = await api.deploymentAction(deployment.deployment_id, "uninstall");
    store.toast("ok", `卸载操作已提交：${result.operation_id}`);
    await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
  } catch (err) {
    store.toast("err", `卸载失败：${(err as Error).message}`);
  } finally {
    uninstalling.value = "";
  }
}

async function deleteImportedRelease(module: StoreModule) {
  if (!store.ensureAction("release.delete")) return;
  if (!window.confirm(`删除未被 Deployment 使用的 Release ${module.id}@${module.version}？`)) {
    return;
  }
  deletingRelease.value = `${module.id}@${module.version}`;
  try {
    await api.deleteRelease(module.id, module.version);
    store.toast("ok", `已删除 Release ${module.id}@${module.version}`);
    await store.refreshStore(true);
  } catch (err) {
    store.toast("err", `删除 Release 失败：${(err as Error).message}`);
  } finally {
    deletingRelease.value = "";
  }
}

interface CatalogSourceRow {
  id: string;
  url: string;
  required_key_id: string;
  auth_secret_ref: string;
  enabled: boolean;
}

const catalogManagerOpen = ref(false);
const catalogs = ref<CatalogSourceRow[]>([]);
const catalogLoading = ref(false);
const catalogSaving = ref(false);
const catalogRemoving = ref("");
const catalogForm = ref({
  id: "",
  url: "",
  required_key_id: "",
  auth_secret_ref: "",
  public_key: "",
});

function normalizeCatalogSource(value: Record<string, unknown>): CatalogSourceRow | null {
  const id = typeof value.id === "string" ? value.id.trim() : "";
  const url = typeof value.url === "string" ? value.url.trim() : "";
  const requiredKeyId =
    typeof value.required_key_id === "string" ? value.required_key_id.trim() : "";
  if (!id || !url || !requiredKeyId) return null;
  return {
    id,
    url,
    required_key_id: requiredKeyId,
    auth_secret_ref:
      typeof value.auth_secret_ref === "string" ? value.auth_secret_ref : "",
    enabled: value.enabled !== false,
  };
}

async function loadCatalogs() {
  if (!store.ensureAction("catalog.list")) return;
  catalogLoading.value = true;
  try {
    catalogs.value = (await api.catalogs())
      .map(normalizeCatalogSource)
      .filter((source): source is CatalogSourceRow => source !== null);
  } catch (error) {
    store.toast("err", `Catalog 列表加载失败：${(error as Error).message}`);
  } finally {
    catalogLoading.value = false;
  }
}

async function openCatalogManager() {
  if (!store.ensureAction("catalog.list")) return;
  catalogManagerOpen.value = true;
  await loadCatalogs();
}

function isCanonicalEd25519PublicKey(value: string): boolean {
  if (!/^[A-Za-z0-9+/]{43}=$/.test(value)) return false;
  try {
    const raw = window.atob(value);
    return raw.length === 32 && window.btoa(raw) === value;
  } catch {
    return false;
  }
}

async function registerCatalog() {
  if (!store.ensureAction("catalog.register")) return;
  const publicKey = catalogForm.value.public_key.trim();
  if (publicKey && !isCanonicalEd25519PublicKey(publicKey)) {
    store.toast("err", "Ed25519 公钥必须是原始 32 字节公钥的 44 字符 padded base64");
    return;
  }
  const source = {
    id: catalogForm.value.id.trim(),
    url: catalogForm.value.url.trim(),
    required_key_id: catalogForm.value.required_key_id.trim(),
    ...(catalogForm.value.auth_secret_ref.trim()
      ? { auth_secret_ref: catalogForm.value.auth_secret_ref.trim() }
      : {}),
    ...(publicKey ? { public_key: publicKey } : {}),
  };
  if (!source.id || !source.url || !source.required_key_id) {
    store.toast("err", "Catalog ID、URL 和可信签名 key ID 均为必填项");
    return;
  }
  catalogSaving.value = true;
  try {
    await api.registerCatalog(source);
    catalogForm.value = {
      id: "",
      url: "",
      required_key_id: "",
      auth_secret_ref: "",
      public_key: "",
    };
    await store.refreshCore(true);
    await Promise.all([loadCatalogs(), store.refreshStore(true)]);
    store.toast("ok", `Catalog ${source.id} 已注册并完成服务端校验`);
  } catch (error) {
    store.toast("err", `Catalog 注册失败：${(error as Error).message}`);
  } finally {
    catalogSaving.value = false;
  }
}

async function removeCatalog(source: CatalogSourceRow) {
  if (!store.ensureAction("catalog.remove")) return;
  if (!window.confirm(`移除 Catalog ${source.id}？已导入的 Release 元数据不会被删除。`)) {
    return;
  }
  catalogRemoving.value = source.id;
  try {
    await api.removeCatalog(source.id);
    await store.refreshCore(true);
    await Promise.all([loadCatalogs(), store.refreshStore(true)]);
    store.toast("ok", `Catalog ${source.id} 已移除`);
  } catch (error) {
    store.toast("err", `Catalog 移除失败：${(error as Error).message}`);
  } finally {
    catalogRemoving.value = "";
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
  <PageHeader title="Store" subtitle="从受信任 Catalog v2 选择精确版本与 OCI digest">
    <input
      v-model="packageSearch"
      class="input"
      data-action="catalog.search"
      :disabled="!store.supportsAction('catalog.search')"
      placeholder="搜索 Release、类型或标签"
      style="width: 220px"
    />
    <button
      class="btn sm"
      data-action="catalog.list"
      :disabled="!store.supportsAction('catalog.list')"
      @click="openCatalogManager"
    >
      管理 Catalog
    </button>
    <button
      class="btn sm"
      :disabled="!store.supportsAction('release.import')"
      @click="openImport()"
    >
      仅导入 Release
    </button>
    <button
      class="btn sm"
      :disabled="!store.supportsAction('catalog.search')"
      @click="store.refreshStore(true)"
    >
      刷新索引
    </button>
  </PageHeader>

  <div class="store-body">
    <div v-if="store.storeLoadStatus === 'loading'" class="card load-state">
      正在加载商店目录与运行状态…
    </div>
    <div
      v-else-if="store.storeLoadStatus === 'error'"
      class="card load-state error-state"
      role="alert"
    >
      <span>{{ store.storeError }}</span>
      <button class="btn sm" @click="store.refreshStore(true)">重试</button>
    </div>
    <div class="status-bar">
      <span class="chip accent">Catalog v2 · 签名验证</span>
      <span class="chip">{{ modules.length }} 个可安装版本</span>
      <span class="chip" :class="readyNodes.length ? 'ok' : 'warn'">
        {{ readyNodes.length }} 个 READY Node
      </span>
      <span class="chip">{{ installedCount }} 个 Deployment</span>
    </div>

    <!-- 模块卡片 -->
    <div class="grid" v-if="modules.length">
      <div
        v-for="module in modules"
        :key="moduleKey(module)"
        class="card module-card fade-in"
        :data-testid="`store-package-${module.id}`"
      >
        <div class="module-head">
          <div>
            <div class="module-name">{{ module.name }}</div>
            <div class="module-id mono">{{ module.id }}</div>
          </div>
          <span v-if="deploymentsFor(module.id).length" class="chip ok">
            已部署 {{ deploymentsFor(module.id).length }} 个
          </span>
        </div>
        <p class="module-desc">{{ module.description }}</p>
        <div class="module-tags">
          <span class="chip">{{ kindLabels[module.kind] ?? module.kind }}</span>
          <span v-for="tag in module.tags" :key="tag" class="chip">{{ tag }}</span>
          <span class="chip mono">v{{ module.version }}</span>
          <span class="chip">{{ module.channel }}</span>
        </div>
        <div class="module-actions">
          <button
            class="btn primary sm"
            :disabled="!store.supportsAction('release.install')"
            @click="openInstall(module)"
          >
            {{ deploymentsFor(module.id).length ? "安装另一实例" : "安装" }}
          </button>
          <button
            class="btn sm"
            :disabled="!store.supportsAction('release.import')"
            @click="openImport(module)"
          >
            仅导入
          </button>
          <button
            class="btn danger sm"
            :disabled="!!deletingRelease || !store.supportsAction('release.delete')"
            @click="deleteImportedRelease(module)"
          >
            {{ deletingRelease === `${module.id}@${module.version}` ? "删除中…" : "删除 Release" }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="deployment.deployment_id"
            class="btn sm"
            :disabled="!!replacing || !store.supportsAction('release.upgrade')"
            @click="replaceRelease(deployment, 'upgrade')"
          >
            {{ replacing === `upgrade:${deployment.deployment_id}` ? "提交中…" : `升级 ${deployment.node_id}` }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="`rollback:${deployment.deployment_id}`"
            class="btn sm"
            :disabled="!!replacing || !store.supportsAction('release.rollback')"
            @click="replaceRelease(deployment, 'rollback')"
          >
            {{ replacing === `rollback:${deployment.deployment_id}` ? "提交中…" : `回滚 ${deployment.node_id}` }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="`uninstall:${deployment.deployment_id}`"
            class="btn danger sm"
            :disabled="!!uninstalling || !store.supportsAction('deployment.uninstall')"
            @click="uninstall(deployment)"
          >
            {{ uninstalling === deployment.deployment_id ? "提交中…" : `卸载 ${deployment.node_id}` }}
          </button>
          <span class="module-source mono muted">{{ module.oci_image }}</span>
        </div>
      </div>
    </div>

    <div v-else class="empty">
      <span class="icon">▤</span>
      <span>
        没有可用的受信任 Catalog v2 package。<br />
        未发布 <code class="mono">release.install</code> 时安装入口会保持禁用。
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
    <div v-if="installTarget" class="card package-summary">
      <div><strong>{{ installTarget.id }}@{{ installTarget.version }}</strong></div>
      <div class="mono muted">{{ installTarget.oci_image }}</div>
      <div class="mono muted">metadata {{ installTarget.checksum }}</div>
      <div class="muted">Managed 安装会默认启动，并在健康门禁通过后才提升投影。</div>
    </div>
    <div class="field">
      <label>目标 Node ID</label>
      <select class="select" v-model="targetNodeId">
        <option value="" disabled>选择 READY Node</option>
        <option v-for="node in readyNodes" :key="node.node_id" :value="node.node_id">
          {{ node.node_id }} · {{ node.host_ip || "loopback" }}
        </option>
      </select>
      <span class="hint">
        Node 必须明确选择；Web 不再按 IP 猜测目标节点。
      </span>
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

    <div v-if="validationResult" class="install-result">
      <div class="chip" :class="validationResult.valid ? 'ok' : 'err'">
        {{ validationResult.valid ? "Release 校验通过" : "Release 校验失败" }}
      </div>
      <p class="muted" style="margin: 10px 0 0">
        Catalog <span class="mono">{{ validationResult.catalogId }}</span>
        · {{ validationResult.platform }}
        · key <span class="mono">{{ validationResult.keyIds.join(", ") }}</span>
      </p>
    </div>

    <template #footer>
      <button class="btn" @click="installOpen = false">关闭</button>
      <button
        class="btn"
        :disabled="
          validating ||
          !store.supportsAction('release.validate') ||
          !installTarget ||
          !targetNodeId
        "
        @click="runValidate"
      >
        {{ validating ? "校验中…" : "先校验 Release" }}
      </button>
      <button
        class="btn primary"
        :disabled="
          installing ||
          !store.supportsAction('release.install') ||
          !installTarget ||
          !targetNodeId
        "
        @click="runInstall"
      >
        {{ installing ? "提交中…" : "安装、启动并验证健康" }}
      </button>
    </template>
  </Modal>

  <Modal :open="importOpen" title="仅导入 Release" width="560px" @close="importOpen = false">
    <div class="field">
      <label>受信任 Catalog Release</label>
      <select class="select" v-model="importTargetKey">
        <option value="" disabled>选择已验证签名的 Release</option>
        <option v-for="module in modules" :key="moduleKey(module)" :value="moduleKey(module)">
          {{ module.id }}@{{ module.version }} · {{ module.channel }} · {{ module.source_id }}
        </option>
      </select>
    </div>
    <div class="field">
      <label>目标平台 Node</label>
      <select class="select" v-model="importTargetNodeId">
        <option value="" disabled>选择 READY Node 以确定 OS/架构</option>
        <option v-for="node in readyNodes" :key="node.node_id" :value="node.node_id">
          {{ node.node_id }} · {{ node.host_ip || "loopback" }}
        </option>
      </select>
    </div>
    <p class="hint">服务端会重新验证 Catalog 签名、metadata SHA-256 与平台；仅导入不会创建 Operation、Job、Deployment 或容器。</p>
    <template #footer>
      <button class="btn" @click="importOpen = false">取消</button>
      <button
        class="btn primary"
        :disabled="
          importing ||
          !store.supportsAction('release.import') ||
          !importTarget ||
          !importTargetNodeId
        "
        @click="runImport"
      >
        {{ importing ? "导入中…" : "仅导入" }}
      </button>
    </template>
  </Modal>

  <Modal
    :open="catalogManagerOpen"
    title="受信任 Catalog 来源"
    width="760px"
    @close="catalogManagerOpen = false"
  >
    <form
      v-if="store.supportsAction('catalog.register')"
      class="catalog-form"
      data-action="catalog.register"
      @submit.prevent="registerCatalog"
    >
      <div class="field">
        <label>Catalog ID</label>
        <input v-model="catalogForm.id" class="input" required placeholder="production" />
      </div>
      <div class="field catalog-url-field">
        <label>Catalog v2 URL</label>
        <input
          v-model="catalogForm.url"
          class="input"
          type="text"
          required
          placeholder="https://catalog.example/catalog-v2.json 或仓库内相对路径"
        />
      </div>
      <div class="field">
        <label>可信 Ed25519 key ID</label>
        <input
          v-model="catalogForm.required_key_id"
          class="input"
          required
          placeholder="release-key-2026"
        />
      </div>
      <div class="field">
        <label>认证 Secret 引用（可选）</label>
        <input
          v-model="catalogForm.auth_secret_ref"
          class="input"
          placeholder="env:OJOS_CATALOG_TOKEN"
        />
      </div>
      <div class="field catalog-public-key-field">
        <label>Ed25519 公钥（32 字节 padded base64，首次信任时必填）</label>
        <input
          v-model="catalogForm.public_key"
          class="input"
          autocomplete="off"
          spellcheck="false"
          minlength="44"
          maxlength="44"
          pattern="[A-Za-z0-9+/]{43}="
          placeholder="44 字符 padded base64 Ed25519 公钥"
        />
      </div>
      <button class="btn primary" type="submit" :disabled="catalogSaving">
        {{ catalogSaving ? "注册中…" : "注册并验证" }}
      </button>
    </form>

    <div v-if="catalogLoading" class="empty">正在加载 Catalog 来源…</div>
    <div v-else-if="catalogs.length" class="catalog-list">
      <div v-for="source in catalogs" :key="source.id" class="card catalog-row">
        <div>
          <div><strong>{{ source.id }}</strong> <span class="chip" :class="source.enabled ? 'ok' : 'warn'">{{ source.enabled ? "已启用" : "已停用" }}</span></div>
          <div class="mono muted">{{ source.url }}</div>
          <div class="muted">签名 key：<span class="mono">{{ source.required_key_id }}</span></div>
        </div>
        <button
          v-if="store.supportsAction('catalog.remove')"
          class="btn danger sm"
          data-action="catalog.remove"
          :disabled="!!catalogRemoving"
          @click="removeCatalog(source)"
        >
          {{ catalogRemoving === source.id ? "移除中…" : "移除" }}
        </button>
      </div>
    </div>
    <div v-else class="empty">尚未注册 Catalog 来源。</div>

    <template #footer>
      <button class="btn" @click="catalogManagerOpen = false">关闭</button>
      <button class="btn" :disabled="catalogLoading" @click="loadCatalogs">刷新</button>
    </template>
  </Modal>
</template>

<style scoped>
.store-body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
}
.load-state {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 14px;
  color: var(--muted);
}
.error-state {
  border-color: rgba(248, 113, 113, 0.45);
  color: var(--err);
}

.catalog-form {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 12px;
  align-items: end;
  margin-bottom: 18px;
}
.catalog-form .field {
  margin: 0;
}
.catalog-url-field {
  grid-column: span 2;
}
.catalog-public-key-field {
  grid-column: span 2;
}
.catalog-form > button {
  justify-self: start;
}
.catalog-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.catalog-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 12px;
}
.catalog-row > div {
  min-width: 0;
}
.catalog-row .mono {
  overflow-wrap: anywhere;
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
