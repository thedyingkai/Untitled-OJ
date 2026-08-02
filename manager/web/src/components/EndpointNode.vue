<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{
  data: {
    serviceId: string;
    kind: string;
    protocol: string;
    health: string;
    host: string;
    port: string;
  };
  selected?: boolean;
}>();

const healthKind = computed(() => {
  const health = (props.data.health || "").toLowerCase();
  if (["ok", "healthy", "running"].includes(health)) return "ok";
  if (["failed", "error", "unreachable"].includes(health)) return "err";
  if (["deferred", "installing", "starting", "unknown-with-warn"].includes(health))
    return "warn";
  return "unknown";
});

const kindIcon = computed(() => {
  switch (props.data.kind) {
    case "gateway":
      return "⇄";
    case "database":
      return "◫";
    case "cache":
      return "⚡";
    case "storage":
      return "▣";
    case "backend-worker":
      return "⚙";
    case "frontend":
      return "▢";
    case "external":
      return "☁";
    default:
      return "❖";
  }
});
</script>

<template>
  <div class="node" :class="[{ selected }, healthKind]">
    <div class="node-head">
      <span class="kind-icon">{{ kindIcon }}</span>
      <span class="service-name">{{ data.serviceId }}</span>
      <span class="health-dot" :class="healthKind"></span>
    </div>
    <div class="node-sub mono">{{ data.host }}:{{ data.port }}</div>
    <div class="node-tags">
      <span class="tag">{{ data.protocol }}</span>
      <span class="tag" v-if="data.health">{{ data.health }}</span>
    </div>
  </div>
</template>

<style scoped>
.node {
  width: 176px;
  background: linear-gradient(180deg, #182136, #131b2c);
  border: 1px solid var(--border-strong);
  border-radius: 12px;
  padding: 10px 13px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.35);
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}
.node.selected {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px rgba(99, 102, 241, 0.25), 0 4px 16px rgba(0, 0, 0, 0.4);
}
.node.err {
  border-color: rgba(248, 113, 113, 0.5);
}

.node-head {
  display: flex;
  align-items: center;
  gap: 7px;
}
.kind-icon {
  font-size: 13px;
  color: var(--accent-2);
}
.service-name {
  font-weight: 600;
  font-size: 13px;
  color: var(--text-strong);
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.health-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.health-dot.ok {
  background: var(--ok);
  box-shadow: 0 0 6px rgba(52, 211, 153, 0.8);
}
.health-dot.err {
  background: var(--err);
}
.health-dot.warn {
  background: var(--warn);
}
.health-dot.unknown {
  background: #475569;
}

.node-sub {
  margin-top: 4px;
  font-size: 11px;
  color: var(--muted);
}
.node-tags {
  display: flex;
  gap: 5px;
  margin-top: 7px;
}
.tag {
  font-size: 10px;
  padding: 1px 7px;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.1);
  color: var(--muted);
  border: 1px solid rgba(148, 163, 184, 0.12);
}
</style>
