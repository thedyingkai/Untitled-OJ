import { apiClient } from './client'
import type {
  CreateSubmissionRequest,
  CreateSubmissionResponse,
  JudgeLanguagesResponse,
  JudgeStatus,
  SubmissionCasesResponse,
  SubmissionDebugLogsResponse,
  SubmissionDetailResponse,
  SubmissionListResponse,
} from '../types/judge'
import type {
  AdminActionResponse,
  JudgeTasksResponse,
  QueueStatusResponse,
  WorkersResponse,
} from '../types/worker'

export interface SubmissionListParams {
  page?: number
  page_size?: number
  status?: JudgeStatus | ''
  problem_id?: number
  user_id?: number
  language?: string
  created_from?: string
  created_to?: string
}

export function createSubmission(
  payload: CreateSubmissionRequest,
): Promise<CreateSubmissionResponse> {
  return apiClient.post('/judge/submissions', payload)
}

export function listSubmissions(params: SubmissionListParams): Promise<SubmissionListResponse> {
  return apiClient.get('/judge/submissions', { params })
}

export function listJudgeLanguages(): Promise<JudgeLanguagesResponse> {
  return apiClient.get('/judge/languages')
}

export function getSubmission(id: number): Promise<SubmissionDetailResponse> {
  return apiClient.get(`/judge/submissions/${id}`)
}

export function getSubmissionCases(id: number): Promise<SubmissionCasesResponse> {
  return apiClient.get(`/judge/submissions/${id}/cases`)
}

export function getSubmissionDebugLogs(
  id: number,
  params: { case_no?: number; max_bytes?: number },
): Promise<SubmissionDebugLogsResponse> {
  return apiClient.get(`/judge/submissions/${id}/debug-logs`, { params })
}

export function getAdminQueue(): Promise<QueueStatusResponse> {
  return apiClient.get('/judge/admin/queue')
}

export function getAdminWorkers(): Promise<WorkersResponse> {
  return apiClient.get('/judge/admin/workers')
}

export function getAdminTasks(): Promise<JudgeTasksResponse> {
  return apiClient.get('/judge/admin/tasks')
}

export function drainWorker(workerId: string): Promise<AdminActionResponse> {
  return apiClient.post(`/judge/admin/workers/${encodeURIComponent(workerId)}/drain`)
}

export function requeueSubmission(submissionId: number): Promise<AdminActionResponse> {
  return apiClient.post(`/judge/admin/submissions/${submissionId}/requeue`)
}
