export type JudgeStatus =
  | 'PENDING'
  | 'JUDGING'
  | 'ACCEPTED'
  | 'WRONG_ANSWER'
  | 'COMPILE_ERROR'
  | 'RUNTIME_ERROR'
  | 'TIME_LIMIT_EXCEEDED'
  | 'MEMORY_LIMIT_EXCEEDED'
  | 'OUTPUT_LIMIT_EXCEEDED'
  | 'SYSTEM_ERROR'
  | 'CANCELLED'
  | 'UNSUPPORTED_LANGUAGE'

export interface CreateSubmissionRequest {
  problem_id: number
  language: string
  code: string
}

export interface CreateSubmissionResponse {
  submission_id: number
  status: JudgeStatus
}

export interface SubmissionItem {
  id: number
  problem_id: number
  user_id: number
  language: string
  status: JudgeStatus
  score: number
  time_ms: number
  memory_kb: number
  message?: string
  code_sha256?: string
  created_at?: string
  updated_at?: string
  judged_at?: string
  cancelled_at?: string
  cancel_reason?: string
}

export interface SubmissionListResponse {
  submissions: SubmissionItem[]
  total: number
}

export type SubmissionDetailResponse = SubmissionItem

export interface SubmissionCaseItem {
  case_no: number
  status: JudgeStatus
  score: number
  time_ms: number
  memory_kb: number
  message?: string
}

export interface SubmissionCasesResponse {
  cases: SubmissionCaseItem[]
}

export interface SubmissionDebugLogsResponse {
  case_no: number
  stdout: string
  stderr: string
  checker_log: string
  truncated: boolean
  max_bytes: number
}

export interface JudgeLanguage {
  id: string
  display_name: string
  version?: string
  enabled: boolean
}

export interface JudgeLanguagesResponse {
  languages: JudgeLanguage[]
}
