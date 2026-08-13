import {
  defineComponent,
  h,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
  type PropType,
} from "vue";
import {
  useRoute,
  type RouteLocationNormalizedLoaded,
  type Router,
} from "vue-router";
import type {
  FrontendDynamicRouteAdapterV1,
  FrontendRouteViewV1,
} from "./contribution-host";
import type { FrontendRouteV1 } from "./loader";

interface OwnedRouteV1 {
  readonly moduleId: string;
  readonly name: string;
}

export function createVueRouteAdapter(
  router: Router,
  parentName?: string,
): FrontendDynamicRouteAdapterV1 {
  const owned = new Map<string, OwnedRouteV1[]>();
  let sequence = 0;
  return Object.freeze({
    validate(moduleId: string, routes: readonly FrontendRouteV1[]) {
      const paths = new Set<string>();
      for (const route of routes) {
        if (paths.has(route.path)) throw new Error(`frontend route ${route.path} is duplicated`);
        paths.add(route.path);
        const owners = owned.get(route.path) ?? [];
        const conflictingOwner = owners.find((owner) => owner.moduleId !== moduleId);
        if (conflictingOwner !== undefined) {
          throw new Error(`frontend route ${route.path} is owned by ${conflictingOwner.moduleId}`);
        }
        const ownedNames = new Set(owners.map((owner) => owner.name));
        const staticCollision = router
          .getRoutes()
          .some(
            (record) =>
              record.path === route.path &&
              (typeof record.name !== "string" || !ownedNames.has(record.name)),
          );
        if (staticCollision) throw new Error(`frontend route ${route.path} conflicts with the Shell`);
      }
    },
    register(moduleId: string, route: FrontendRouteV1, view: FrontendRouteViewV1) {
      sequence += 1;
      const name = `ojos.frontend:${moduleId}:${route.id}:${sequence}`;
      const record = {
        path: route.path,
        name,
        component: ExtensionRouteView,
        props: Object.freeze({ view }),
        meta: Object.freeze({
          title: route.title,
          requiresAuth: true,
          ...(route.permission === undefined ? {} : { permissions: [route.permission] }),
        }),
      };
      const remove = parentName === undefined
        ? router.addRoute(record)
        : router.addRoute(parentName, record);
      const owner = Object.freeze({ moduleId, name });
      const owners = owned.get(route.path) ?? [];
      owners.push(owner);
      owned.set(route.path, owners);
      let disposed = false;
      return {
        dispose() {
          if (disposed) return;
          disposed = true;
          remove();
          const current = owned.get(route.path);
          if (current === undefined) return;
          const index = current.indexOf(owner);
          if (index >= 0) current.splice(index, 1);
          if (current.length === 0) owned.delete(route.path);
        },
      };
    },
  });
}

const ExtensionRouteView = defineComponent({
  name: "OjosFrontendExtensionRoute",
  props: {
    view: {
      type: Object as PropType<FrontendRouteViewV1>,
      required: true,
    },
  },
  setup(props) {
    const element = ref<HTMLElement>();
    const route = useRoute();
    const error = ref("");
    let queue = Promise.resolve();
    let mounted = false;

    const renderSurface = (): void => {
      const target = element.value;
      if (!mounted || target === undefined) return;
      const context = safeRouteContext(route);
      queue = queue
        .catch(() => undefined)
        .then(() => props.view.mount(target, context))
        .then(
          () => {
            error.value = "";
          },
          (cause) => {
            error.value = cause instanceof Error ? cause.message : String(cause);
          },
        );
    };

    onMounted(() => {
      mounted = true;
      renderSurface();
    });
    watch(() => route.fullPath, renderSurface);
    onBeforeUnmount(() => {
      mounted = false;
      if (element.value !== undefined) props.view.unmount(element.value);
    });
    return () =>
      h("section", { class: "ojos-frontend-extension-route" }, [
        error.value === ""
          ? null
          : h("div", { role: "alert", class: "ojos-frontend-extension-error" }, error.value),
        h("div", { ref: element, class: "ojos-frontend-extension-surface" }),
      ]);
  },
});

function safeRouteContext(route: RouteLocationNormalizedLoaded): Readonly<Record<string, unknown>> {
  return Object.freeze({
    path: route.path,
    name: typeof route.name === "string" ? route.name : "",
    params: copyRouteValues(route.params),
    query: copyRouteValues(route.query),
  });
}

function copyRouteValues(
  values: Readonly<Record<string, unknown>>,
): Readonly<Record<string, string | readonly string[]>> {
  const result: Record<string, string | readonly string[]> = {};
  for (const [key, value] of Object.entries(values)) {
    if (typeof value === "string") result[key] = value;
    else if (Array.isArray(value)) {
      result[key] = Object.freeze(value.filter((item): item is string => typeof item === "string"));
    }
  }
  return Object.freeze(result);
}
