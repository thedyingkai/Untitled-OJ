<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{ status: string }>();

const kind = computed(() => {
  const value = props.status?.toUpperCase?.() ?? "";
  if (["SUCCEEDED", "OK", "RUNNING-OK", "HEALTHY"].includes(value)) return "ok";
  if (["FAILED", "ERROR", "UNREACHABLE"].includes(value)) return "err";
  if (
    [
      "RUNNING",
      "PLANNED",
      "AWAITING_CONFIRMATION",
      "DEFERRED",
      "PENDING",
      "INSTALLING",
      "STARTING",
    ].includes(value)
  )
    return "warn";
  return "";
});
</script>

<template>
  <span class="chip" :class="kind">{{ status || "—" }}</span>
</template>
