export interface FlowNode {
  id: string;
  x: number;
  y: number;
  data?: Record<string, unknown>;
}

export interface FlowEdge {
  id: string;
  source: string;
  target: string;
  label?: string;
  /** 对应 Link.enabled = "disabled"：虚线 + 变暗渲染，选中态仍然可见。 */
  disabled?: boolean;
}
