import { ref } from "vue";
import type { CatalogSourceRow } from "./model";
import { catalogApi } from "./api";
import { isCanonicalEd25519PublicKey, normalizeCatalogSource } from "./model";
import type { ControlPlaneContext } from "../control-plane/context";

export function useCatalogManager(store: ControlPlaneContext) {
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

  async function loadCatalogs() {
    if (!store.ensureAction("catalog.list")) return;
    catalogLoading.value = true;
    try {
      catalogs.value = (await catalogApi.catalogs())
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

  async function registerCatalog() {
    if (!store.ensureAction("catalog.register")) return;
    const publicKey = catalogForm.value.public_key.trim();
    if (publicKey && !isCanonicalEd25519PublicKey(publicKey)) {
      store.toast(
        "err",
        "Ed25519 公钥必须是原始 32 字节公钥的 44 字符 padded base64",
      );
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
      await catalogApi.registerCatalog(source);
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
    if (
      !window.confirm(
        `移除 Catalog ${source.id}？已导入的 Release 元数据不会被删除。`,
      )
    ) {
      return;
    }
    catalogRemoving.value = source.id;
    try {
      await catalogApi.removeCatalog(source.id);
      await store.refreshCore(true);
      await Promise.all([loadCatalogs(), store.refreshStore(true)]);
      store.toast("ok", `Catalog ${source.id} 已移除`);
    } catch (error) {
      store.toast("err", `Catalog 移除失败：${(error as Error).message}`);
    } finally {
      catalogRemoving.value = "";
    }
  }

  return {
    catalogManagerOpen,
    catalogs,
    catalogLoading,
    catalogSaving,
    catalogRemoving,
    catalogForm,
    loadCatalogs,
    openCatalogManager,
    registerCatalog,
    removeCatalog,
  };
}
