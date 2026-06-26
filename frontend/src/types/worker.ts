export type WorkerStatus = 'ONLINE' | 'OFFLINE' | 'DRAINING'
export type TaskStatus = 'PENDING' | 'RUNNING' | 'SUCCEEDED' | 'FAILED' | 'CANCELLED'

export interface WorkerItem {
  worker_id: string
  worker_name: string
  hostname: string
  version: string
  capabilities: string[]
  supported_languages: string[]
  max_concurrency: number
  running_count: number
  last_seen: string
  status: WorkerStatus
}

export interface JudgeTaskItem {
  task_id: string
  submission_id: number
  worker_id?: string
  status: TaskStatus
  lease_expires_at?: string
  heartbeat_at?: string
  attempt: number
}

export interface QueueStatus {
  stream_length: number
  pending_count: number
  consumer_group: string
  last_id?: string
  trim_strategy?: string
  pending_oldest_idle_ms?: number
  scheduled: number
  pending: number
  judging: number
}

export interface QueueStatusResponse extends QueueStatus {}

export interface WorkersResponse {
  workers: WorkerItem[]
}

export interface JudgeTasksResponse {
  tasks: JudgeTaskItem[]
}

export interface AdminActionResponse {
  ok: boolean
}
