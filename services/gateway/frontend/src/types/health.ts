export type HealthState = 'ok' | 'degraded' | 'error' | 'unknown'

export interface HealthComponent {
  name: string
  status: HealthState
  latency_ms: number
  message?: string
}

export interface AdminHealthResponse {
  status: HealthState
  components: HealthComponent[]
  worker_online_count: number
  queue_pending: number
  internal_auth: string
}
