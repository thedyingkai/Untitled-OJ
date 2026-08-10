<script setup lang="ts">
/**
 * FlowCanvas —— 轻量节点画布引擎（零依赖）。
 * 支持：平移、滚轮缩放（以指针为中心）、节点拖拽、端口连线（带预览）、
 * 边/节点选中、外部拖入（drop）、自适应视图。
 */
import {
  computed,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  watch,
} from "vue";
import type { Directive } from "vue";
import type { FlowEdge, FlowNode } from "../flow-types";

const props = defineProps<{
  nodes: FlowNode[];
  edges: FlowEdge[];
  selectedNode?: string | null;
  selectedEdge?: string | null;
}>();

const emit = defineEmits<{
  "node-move": [id: string, position: { x: number; y: number }];
  "node-click": [id: string];
  "edge-click": [id: string];
  "pane-click": [];
  connect: [source: string, target: string];
  "drop-at": [position: { x: number; y: number }, event: DragEvent];
}>();

const viewport = ref<HTMLDivElement | null>(null);
const view = reactive({ x: 60, y: 40, scale: 1 });

const MIN_SCALE = 0.25;
const MAX_SCALE = 1.9;
const NODE_WIDTH = 176;
const NODE_HEIGHT = 78;

/*
 * 节点实际尺寸测量（用于边的锚点）。
 *
 * 这里刻意不使用 reactive(Map)：函数 ref 会在每次渲染后执行，若无条件写入
 * 一个新的尺寸对象，边路径在渲染时又读取这个 Map，就会形成无限更新循环。
 * ResizeObserver 只在尺寸真实变化时提升 revision，因此一次 resize 最多触发一次
 * 必要的路径重算。
 */
interface NodeSize {
  width: number;
  height: number;
}

const sizes = new Map<string, NodeSize>();
const observedElements = new Map<Element, string>();
const sizeRevision = ref(0);
let resizeObserver: ResizeObserver | null = null;

function updateNodeSize(id: string, element: HTMLElement) {
  const next = {
    width: element.offsetWidth || NODE_WIDTH,
    height: element.offsetHeight || NODE_HEIGHT,
  };
  const previous = sizes.get(id);
  if (previous?.width === next.width && previous.height === next.height) return;
  sizes.set(id, next);
  sizeRevision.value += 1;
}

function ensureResizeObserver() {
  if (resizeObserver || typeof ResizeObserver === "undefined") return;
  resizeObserver = new ResizeObserver((entries) => {
    for (const entry of entries) {
      const id = observedElements.get(entry.target);
      if (id) updateNodeSize(id, entry.target as HTMLElement);
    }
  });
}

function observeNode(id: string, element: HTMLElement) {
  ensureResizeObserver();
  observedElements.set(element, id);
  resizeObserver?.observe(element);
  updateNodeSize(id, element);
}

function unobserveNode(id: string, element: HTMLElement) {
  resizeObserver?.unobserve(element);
  observedElements.delete(element);
  if (sizes.delete(id)) sizeRevision.value += 1;
}

const vNodeMeasure: Directive<HTMLElement, string> = {
  mounted(element, binding) {
    observeNode(binding.value, element);
  },
  updated(element, binding) {
    if (binding.value === binding.oldValue) return;
    if (binding.oldValue) unobserveNode(binding.oldValue, element);
    observeNode(binding.value, element);
  },
  unmounted(element, binding) {
    unobserveNode(binding.value, element);
  },
};

function nodeSize(id: string) {
  // 让边路径和 fitView 只依赖显式的尺寸版本，而不是 Map 写操作本身。
  void sizeRevision.value;
  return sizes.get(id) ?? { width: NODE_WIDTH, height: NODE_HEIGHT };
}

const nodeMap = computed(() => {
  const map = new Map<string, FlowNode>();
  for (const node of props.nodes) map.set(node.id, node);
  return map;
});

/* ---------- 坐标换算 ---------- */

function toWorld(clientX: number, clientY: number) {
  const rect = viewport.value?.getBoundingClientRect();
  if (!rect) return { x: 0, y: 0 };
  return {
    x: (clientX - rect.left - view.x) / view.scale,
    y: (clientY - rect.top - view.y) / view.scale,
  };
}

/* ---------- 平移 / 缩放 ---------- */

let panning = false;
let panStart = { x: 0, y: 0, vx: 0, vy: 0 };
let panMoved = false;

function onPanePointerDown(event: PointerEvent) {
  if (event.button !== 0) return;
  panning = true;
  panMoved = false;
  panStart = { x: event.clientX, y: event.clientY, vx: view.x, vy: view.y };
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
}

function onPanePointerMove(event: PointerEvent) {
  if (dragState.id) {
    dragNodeMove(event);
    return;
  }
  if (connectState.active) {
    connectMove(event);
    return;
  }
  if (!panning) return;
  const dx = event.clientX - panStart.x;
  const dy = event.clientY - panStart.y;
  if (Math.abs(dx) + Math.abs(dy) > 3) panMoved = true;
  view.x = panStart.vx + dx;
  view.y = panStart.vy + dy;
}

function onPanePointerUp(event: PointerEvent) {
  if (dragState.id) {
    dragNodeEnd();
    return;
  }
  if (connectState.active) {
    connectEnd(event);
    return;
  }
  if (panning && !panMoved) emit("pane-click");
  panning = false;
}

function onWheel(event: WheelEvent) {
  event.preventDefault();
  const rect = viewport.value?.getBoundingClientRect();
  if (!rect) return;
  const pointer = {
    x: event.clientX - rect.left,
    y: event.clientY - rect.top,
  };
  const factor = event.deltaY < 0 ? 1.12 : 1 / 1.12;
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, view.scale * factor));
  const ratio = next / view.scale;
  view.x = pointer.x - (pointer.x - view.x) * ratio;
  view.y = pointer.y - (pointer.y - view.y) * ratio;
  view.scale = next;
}

/* ---------- 节点拖拽 ---------- */

const dragState = reactive({
  id: "" as string,
  offsetX: 0,
  offsetY: 0,
  moved: false,
});

function onNodePointerDown(event: PointerEvent, node: FlowNode) {
  if (event.button !== 0) return;
  event.stopPropagation();
  const world = toWorld(event.clientX, event.clientY);
  dragState.id = node.id;
  dragState.offsetX = world.x - node.x;
  dragState.offsetY = world.y - node.y;
  dragState.moved = false;
  viewport.value?.setPointerCapture(event.pointerId);
}

function dragNodeMove(event: PointerEvent) {
  const node = nodeMap.value.get(dragState.id);
  if (!node) return;
  const world = toWorld(event.clientX, event.clientY);
  const x = world.x - dragState.offsetX;
  const y = world.y - dragState.offsetY;
  if (Math.abs(x - node.x) + Math.abs(y - node.y) > 0.5) dragState.moved = true;
  emit("node-move", node.id, { x, y });
}

function dragNodeEnd() {
  if (dragState.id && !dragState.moved) emit("node-click", dragState.id);
  dragState.id = "";
}

/* ---------- 端口连线 ---------- */

const connectState = reactive({
  active: false,
  source: "",
  cursor: { x: 0, y: 0 },
  hover: "" as string,
});

function onPortPointerDown(event: PointerEvent, nodeId: string) {
  event.stopPropagation();
  event.preventDefault();
  connectState.active = true;
  connectState.source = nodeId;
  connectState.cursor = toWorld(event.clientX, event.clientY);
  connectState.hover = "";
  viewport.value?.setPointerCapture(event.pointerId);
}

function connectMove(event: PointerEvent) {
  connectState.cursor = toWorld(event.clientX, event.clientY);
  connectState.hover = nodeAtPoint(connectState.cursor, connectState.source);
}

function connectEnd(_event: PointerEvent) {
  const target = connectState.hover;
  const source = connectState.source;
  connectState.active = false;
  connectState.source = "";
  connectState.hover = "";
  if (target && source && target !== source) {
    emit("connect", source, target);
  }
}

function nodeAtPoint(point: { x: number; y: number }, excludeId: string) {
  for (const node of props.nodes) {
    if (node.id === excludeId) continue;
    const size = nodeSize(node.id);
    const pad = 14;
    if (
      point.x >= node.x - pad &&
      point.x <= node.x + size.width + pad &&
      point.y >= node.y - pad &&
      point.y <= node.y + size.height + pad
    ) {
      return node.id;
    }
  }
  return "";
}

/* ---------- 边路径 ---------- */

function anchorSource(node: FlowNode) {
  const size = nodeSize(node.id);
  return { x: node.x + size.width, y: node.y + size.height / 2 };
}
function anchorTarget(node: FlowNode) {
  const size = nodeSize(node.id);
  return { x: node.x, y: node.y + size.height / 2 };
}

function edgePath(edge: FlowEdge): string {
  const source = nodeMap.value.get(edge.source);
  const target = nodeMap.value.get(edge.target);
  if (!source || !target) return "";
  const from = anchorSource(source);
  const to = anchorTarget(target);
  return bezier(from, to);
}

function bezier(
  from: { x: number; y: number },
  to: { x: number; y: number },
): string {
  const dx = Math.max(46, Math.abs(to.x - from.x) * 0.45);
  return `M ${from.x} ${from.y} C ${from.x + dx} ${from.y}, ${to.x - dx} ${to.y}, ${to.x} ${to.y}`;
}

function edgeLabelPos(edge: FlowEdge) {
  const source = nodeMap.value.get(edge.source);
  const target = nodeMap.value.get(edge.target);
  if (!source || !target) return { x: 0, y: 0 };
  const from = anchorSource(source);
  const to = anchorTarget(target);
  return { x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 - 7 };
}

const previewPath = computed(() => {
  if (!connectState.active) return "";
  const source = nodeMap.value.get(connectState.source);
  if (!source) return "";
  return bezier(anchorSource(source), connectState.cursor);
});

/* ---------- 视图控制 ---------- */

function fitView() {
  if (!props.nodes.length || !viewport.value) return;
  const rect = viewport.value.getBoundingClientRect();
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of props.nodes) {
    const size = nodeSize(node.id);
    minX = Math.min(minX, node.x);
    minY = Math.min(minY, node.y);
    maxX = Math.max(maxX, node.x + size.width);
    maxY = Math.max(maxY, node.y + size.height);
  }
  const pad = 70;
  const width = maxX - minX + pad * 2;
  const height = maxY - minY + pad * 2;
  const scale = Math.min(
    MAX_SCALE,
    Math.max(MIN_SCALE, Math.min(rect.width / width, rect.height / height)),
  );
  view.scale = Math.min(scale, 1.15);
  view.x = (rect.width - (maxX - minX) * view.scale) / 2 - minX * view.scale;
  view.y = (rect.height - (maxY - minY) * view.scale) / 2 - minY * view.scale;
}

function zoom(direction: 1 | -1) {
  const rect = viewport.value?.getBoundingClientRect();
  if (!rect) return;
  const center = { x: rect.width / 2, y: rect.height / 2 };
  const factor = direction > 0 ? 1.2 : 1 / 1.2;
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, view.scale * factor));
  const ratio = next / view.scale;
  view.x = center.x - (center.x - view.x) * ratio;
  view.y = center.y - (center.y - view.y) * ratio;
  view.scale = next;
}

defineExpose({ fitView });

/* ---------- 外部拖入 ---------- */

function onDrop(event: DragEvent) {
  emit("drop-at", toWorld(event.clientX, event.clientY), event);
}

/* ---------- 初始化 ---------- */

let fitted = false;
let fitTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleFitView() {
  if (fitTimer) clearTimeout(fitTimer);
  fitTimer = setTimeout(() => {
    fitTimer = null;
    fitView();
  }, 50);
}

watch(
  () => props.nodes.length,
  (count) => {
    if (!fitted && count > 0) {
      fitted = true;
      scheduleFitView();
    }
  },
);

function onResize() {
  /* 视口变化时无需强制重排，仅保留钩子 */
}

onMounted(() => {
  window.addEventListener("resize", onResize);
  if (props.nodes.length) {
    fitted = true;
    scheduleFitView();
  }
});
onBeforeUnmount(() => {
  window.removeEventListener("resize", onResize);
  if (fitTimer) clearTimeout(fitTimer);
  resizeObserver?.disconnect();
  resizeObserver = null;
  observedElements.clear();
  sizes.clear();
});
</script>

<template>
  <div
    ref="viewport"
    class="flow-viewport"
    data-testid="flow-viewport"
    @pointerdown="onPanePointerDown"
    @pointermove="onPanePointerMove"
    @pointerup="onPanePointerUp"
    @pointercancel="onPanePointerUp"
    @wheel="onWheel"
    @drop.prevent="onDrop"
    @dragover.prevent
  >
    <!-- 网格背景（跟随平移缩放） -->
    <div
      class="flow-grid"
      :style="{
        backgroundSize: `${22 * view.scale}px ${22 * view.scale}px`,
        backgroundPosition: `${view.x}px ${view.y}px`,
      }"
    ></div>

    <!-- 世界坐标层 -->
    <div
      class="flow-world"
      :style="{
        transform: `translate(${view.x}px, ${view.y}px) scale(${view.scale})`,
      }"
    >
      <!-- 边 -->
      <svg class="flow-edges">
        <defs>
          <marker
            id="flow-arrow"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 1 L 9 5 L 0 9 z" fill="#64748b" />
          </marker>
          <marker
            id="flow-arrow-active"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 1 L 9 5 L 0 9 z" fill="#818cf8" />
          </marker>
          <marker
            id="flow-arrow-disabled"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 1 L 9 5 L 0 9 z" fill="#334155" />
          </marker>
        </defs>
        <g v-for="edge in edges" :key="edge.id" :data-edge-id="edge.id">
          <!-- 命中区域 -->
          <path
            :d="edgePath(edge)"
            class="edge-hit"
            @pointerdown.stop
            @click.stop="emit('edge-click', edge.id)"
          />
          <path
            :d="edgePath(edge)"
            class="edge-line"
            :class="{
              selected: selectedEdge === edge.id,
              disabled: edge.disabled,
            }"
            :marker-end="
              selectedEdge === edge.id
                ? 'url(#flow-arrow-active)'
                : edge.disabled
                  ? 'url(#flow-arrow-disabled)'
                  : 'url(#flow-arrow)'
            "
          />
          <text
            v-if="edge.label"
            class="edge-label"
            :class="{ disabled: edge.disabled }"
            :x="edgeLabelPos(edge).x"
            :y="edgeLabelPos(edge).y"
            text-anchor="middle"
          >
            {{ edge.label }}
          </text>
        </g>
        <!-- 连线预览 -->
        <path v-if="previewPath" :d="previewPath" class="edge-preview" />
      </svg>

      <!-- 节点 -->
      <div
        v-for="node in nodes"
        :key="node.id"
        class="flow-node"
        :data-node-id="node.id"
        :class="{
          'connect-hover': connectState.hover === node.id,
          dragging: dragState.id === node.id,
        }"
        :style="{ left: `${node.x}px`, top: `${node.y}px` }"
        v-node-measure="node.id"
        @pointerdown="onNodePointerDown($event, node)"
      >
        <slot name="node" :node="node" :selected="selectedNode === node.id" />
        <!-- 目标端口（左） -->
        <span class="port port-in" title="连线目标"></span>
        <!-- 源端口（右，可拖出连线） -->
        <span
          class="port port-out"
          title="拖拽创建 Link"
          @pointerdown="onPortPointerDown($event, node.id)"
        ></span>
      </div>
    </div>

    <!-- 缩放控制 -->
    <div class="flow-controls">
      <button title="放大" @click="zoom(1)">＋</button>
      <button title="缩小" @click="zoom(-1)">－</button>
      <button title="适应视图" @click="fitView">⤢</button>
    </div>

    <div class="flow-zoom-indicator mono">{{ Math.round(view.scale * 100) }}%</div>
  </div>
</template>

<style scoped>
.flow-viewport {
  position: absolute;
  inset: 0;
  overflow: hidden;
  cursor: grab;
  touch-action: none;
  user-select: none;
}
.flow-viewport:active {
  cursor: grabbing;
}

.flow-grid {
  position: absolute;
  inset: 0;
  background-image: radial-gradient(circle, #1e293b 1.2px, transparent 1.2px);
  pointer-events: none;
}

.flow-world {
  position: absolute;
  top: 0;
  left: 0;
  transform-origin: 0 0;
}

.flow-edges {
  position: absolute;
  top: 0;
  left: 0;
  width: 1px;
  height: 1px;
  overflow: visible;
  pointer-events: none;
}
.edge-hit {
  fill: none;
  stroke: transparent;
  stroke-width: 14;
  pointer-events: stroke;
  cursor: pointer;
}
.edge-line {
  fill: none;
  stroke: #475569;
  stroke-width: 1.7;
  pointer-events: none;
  transition: stroke 0.12s ease;
}
/* 已停用的 Link：虚线 + 更淡的线色 */
.edge-line.disabled {
  stroke: #334155;
  stroke-width: 1.5;
  stroke-dasharray: 6 5;
}
.edge-line.selected {
  stroke: var(--accent-2);
  stroke-width: 2.4;
}
/* 选中的停用边保留虚线，但颜色回到强调色以保证可见 */
.edge-line.disabled.selected {
  stroke: var(--accent-2);
  stroke-width: 2.2;
  stroke-dasharray: 6 5;
}
.edge-label {
  fill: #7285a0;
  font-size: 10px;
  font-family: var(--mono);
  pointer-events: none;
  paint-order: stroke;
  stroke: #0a0e16;
  stroke-width: 3px;
}
.edge-label.disabled {
  fill: #4c5a72;
}
.edge-preview {
  fill: none;
  stroke: var(--accent-2);
  stroke-width: 2;
  stroke-dasharray: 7 4;
  pointer-events: none;
}

.flow-node {
  position: absolute;
  cursor: grab;
}
.flow-node.dragging {
  cursor: grabbing;
  z-index: 10;
}
.flow-node.connect-hover :deep(.node) {
  border-color: var(--ok);
  box-shadow: 0 0 0 3px rgba(52, 211, 153, 0.25);
}

.port {
  position: absolute;
  top: 50%;
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: var(--accent);
  border: 2px solid var(--bg);
  transform: translateY(-50%);
  z-index: 5;
}
.port-in {
  left: -6px;
}
.port-out {
  right: -6px;
  cursor: crosshair;
  transition: transform 0.12s ease, box-shadow 0.12s ease;
}
.port-out:hover {
  transform: translateY(-50%) scale(1.35);
  box-shadow: 0 0 0 4px rgba(99, 102, 241, 0.25);
}

.flow-controls {
  position: absolute;
  left: 14px;
  bottom: 14px;
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
  background: var(--panel-solid);
}
.flow-controls button {
  width: 30px;
  height: 28px;
  border: none;
  border-bottom: 1px solid var(--border);
  background: transparent;
  color: var(--muted);
  font-size: 14px;
  cursor: pointer;
}
.flow-controls button:last-child {
  border-bottom: none;
}
.flow-controls button:hover {
  background: #1a2438;
  color: var(--text);
}

.flow-zoom-indicator {
  position: absolute;
  right: 14px;
  bottom: 14px;
  font-size: 11px;
  color: var(--faint);
  background: rgba(13, 18, 32, 0.8);
  padding: 3px 8px;
  border-radius: 6px;
  border: 1px solid var(--border);
}
</style>
