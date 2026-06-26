export type Nullable<T> = T | null

export interface ApiEnvelope<T = unknown> {
  code?: number
  msg?: string
  data?: T
  request_id?: string
}

export interface ApiRequestError {
  status: number
  code?: number
  message: string
  requestId?: string
  details?: unknown
  fieldErrors?: Record<string, string[]>
}

export interface PageQuery {
  page?: number
  page_size?: number
}

export interface PageResult<T> {
  items: T[]
  total: number
  page: number
  page_size: number
}

export interface SelectOption<T = string> {
  label: string
  value: T
  disabled?: boolean
}

export type SortOrder = 'ascend' | 'descend' | false
