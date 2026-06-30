import { apiClient } from './client'
import type {
  PackageCasesResponse,
  ProblemDetailResponse,
  ProblemFormInput,
  ProblemListResponse,
  ProblemPackageResponse,
  ProblemVisibility,
  ValidateProblemPackageResponse,
} from '../types/problem'

export interface ProblemListParams {
  page?: number
  page_size?: number
  keyword?: string
  visibility?: ProblemVisibility | ''
  difficulty?: string
  tags?: string
}

export function listProblems(params: ProblemListParams): Promise<ProblemListResponse> {
  return apiClient.get<ProblemListResponse>('/problem/problems', { params })
}

export function getProblem(id: number): Promise<ProblemDetailResponse> {
  return apiClient.get<ProblemDetailResponse>(`/problem/problems/${id}`)
}

export function createProblem(payload: ProblemFormInput): Promise<{ problem_id: number; slug: string }> {
  return apiClient.post('/problem/problems', payload)
}

export function updateProblem(id: number, payload: ProblemFormInput): Promise<ProblemDetailResponse> {
  return apiClient.put(`/problem/problems/${id}`, payload)
}

export function deleteProblem(id: number): Promise<{ deleted: boolean }> {
  return apiClient.delete(`/problem/problems/${id}`)
}

export function getProblemPackage(id: number): Promise<ProblemPackageResponse> {
  return apiClient.get<ProblemPackageResponse>(`/problem/problems/${id}/package`)
}

export function validateProblemPackage(id: number): Promise<ValidateProblemPackageResponse> {
  return apiClient.post<ValidateProblemPackageResponse>(`/problem/problems/${id}/package/validate`)
}

export function listPackageCases(id: number): Promise<PackageCasesResponse> {
  return apiClient.get<PackageCasesResponse>(`/problem/problems/${id}/package/cases`)
}
