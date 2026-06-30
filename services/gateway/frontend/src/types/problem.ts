export type ProblemVisibility = 'private' | 'public'
export type ProblemStatus = 'draft' | 'ready' | 'published' | 'archived'
export type ProblemDifficulty = 'easy' | 'medium' | 'hard'
export type ProblemType =
  | 'traditional'
  | 'interactive'
  | 'communication'
  | 'output_only'
  | 'heuristic'

export interface ProblemItem {
  id: number
  slug: string
  title: string
  statement: string
  problem_type: ProblemType
  visibility: ProblemVisibility
  status: ProblemStatus
  difficulty: ProblemDifficulty
  tags: string
  time_limit_ms: number
  memory_limit_mb: number
  created_by: number
  created_at: string
  updated_at: string
  manifest_sha256?: string
  source_format?: string
  samples?: ProblemSample[]
}

export interface ProblemSample {
  case_no: number
  input: string
  output: string
}

export interface ProblemListResponse {
  problems: ProblemItem[]
  total: number
}

export interface ProblemDetailResponse {
  problem: ProblemItem
}

export interface ProblemFormInput {
  title: string
  slug?: string
  statement?: string
  time_limit_ms?: number
  memory_limit_mb?: number
  problem_type?: ProblemType
  visibility?: ProblemVisibility
  difficulty?: ProblemDifficulty
  tags?: string
  status?: ProblemStatus
}

export interface TestCaseItem {
  no: number
  input: string
  answer: string
  score: number
  group: number
  sample: boolean
  hidden: boolean
  time_limit_ms: number
  memory_limit_mb: number
}

export interface PackageComponent {
  type: string
  name: string
  config_path: string
}

export interface PackageLanguageLimit {
  language: string
  time_limit_ms: number
  memory_limit_mb: number
}

export interface PackageLimits {
  default_time_limit_ms: number
  default_memory_limit_mb: number
  languages: PackageLanguageLimit[]
}

export interface PackageSummary {
  schema: string
  slug: string
  title: string
  problem_type: string
  visibility: string
  status: string
  source_format: string
  manifest_sha256: string
  total_cases: number
  total_score: number
  sample_count: number
  file_count: number
  size_bytes: number
  limits: PackageLimits
  runner: PackageComponent
  checker: PackageComponent
  scorer: PackageComponent
}

export interface PackageValidationIssue {
  level: 'error' | 'warning'
  code: string
  message: string
  path?: string
  case_no?: number
}

export interface PackageValidationResult {
  valid: boolean
  errors: PackageValidationIssue[]
  warnings: PackageValidationIssue[]
}

export interface ProblemPackageResponse {
  package: PackageSummary
  validation: PackageValidationResult
}

export interface ValidateProblemPackageResponse {
  validation: PackageValidationResult
}

export interface PackageCasesResponse {
  cases: TestCaseItem[]
}
