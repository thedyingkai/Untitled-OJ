import type { HealthState } from '../types/health'
import type { JudgeStatus } from '../types/judge'
import type { ProblemDifficulty, ProblemStatus, ProblemVisibility } from '../types/problem'
import type { ServiceStatus } from '../types/service'
import type { TaskStatus, WorkerStatus } from '../types/worker'

export type OjosTagType = 'default' | 'success' | 'warning' | 'error' | 'info'

export interface StatusMeta {
  label: string
  type: OjosTagType
  className: string
  description: string
}

const fallbackMeta: StatusMeta = {
  label: '未知',
  type: 'default',
  className: 'status-unknown',
  description: '前端暂不识别当前状态。',
}

const judgeStatusMeta: Record<string, StatusMeta> = {
  PENDING: meta('等待中', 'default', 'status-pending', '提交已进入队列。'),
  JUDGING: meta('评测中', 'info', 'status-judging', 'Worker 正在执行评测。'),
  ACCEPTED: meta('通过', 'success', 'status-accepted', '全部测试点通过。'),
  WRONG_ANSWER: meta('答案错误', 'error', 'status-wrong-answer', '输出与期望结果不一致。'),
  RUNTIME_ERROR: meta('运行错误', 'error', 'status-runtime-error', '程序运行时异常。'),
  COMPILE_ERROR: meta('编译错误', 'error', 'status-compile-error', '编译未通过。'),
  TIME_LIMIT_EXCEEDED: meta('超时', 'warning', 'status-time-limit', '运行时间超过限制。'),
  MEMORY_LIMIT_EXCEEDED: meta('超内存', 'warning', 'status-memory-limit', '内存使用超过限制。'),
  SYSTEM_ERROR: meta('系统错误', 'error', 'status-system-error', '评测系统异常。'),
}

const taskStatusMeta: Record<string, StatusMeta> = {
  queued: meta('排队中', 'default', 'status-task-queued', '任务等待调度。'),
  running: meta('运行中', 'info', 'status-task-running', '任务正在执行。'),
  finished: meta('已完成', 'success', 'status-task-finished', '任务已完成。'),
  failed: meta('失败', 'error', 'status-task-failed', '任务执行失败。'),
  cancelled: meta('已取消', 'default', 'status-task-cancelled', '任务已取消。'),
}

const healthStatusMeta: Record<string, StatusMeta> = {
  ok: meta('正常', 'success', 'status-health-ok', '服务健康。'),
  healthy: meta('正常', 'success', 'status-health-ok', '服务健康。'),
  warning: meta('警告', 'warning', 'status-health-warning', '服务存在警告。'),
  degraded: meta('降级', 'warning', 'status-health-degraded', '服务降级运行。'),
  error: meta('异常', 'error', 'status-health-error', '服务异常。'),
  down: meta('离线', 'error', 'status-health-down', '服务不可达。'),
  unknown: meta('未知', 'default', 'status-health-unknown', '健康状态未知。'),
}

const workerStatusMeta: Record<string, StatusMeta> = {
  ONLINE: meta('在线', 'success', 'status-worker-online', 'Worker 在线。'),
  OFFLINE: meta('离线', 'default', 'status-worker-offline', 'Worker 离线。'),
  DRAINING: meta('排空中', 'warning', 'status-worker-draining', 'Worker 不再领取新任务。'),
  BUSY: meta('繁忙', 'info', 'status-worker-busy', 'Worker 正在处理任务。'),
  ERROR: meta('异常', 'error', 'status-worker-error', 'Worker 报告异常。'),
}

const serviceStatusMeta: Record<string, StatusMeta> = {
  ENABLED: meta('已启用', 'success', 'status-service-enabled', 'Service 已启用。'),
  DISABLED: meta('已禁用', 'default', 'status-service-disabled', 'Service 已禁用。'),
  INSTALLING: meta('安装中', 'info', 'status-service-installing', 'Service 正在安装。'),
  UPGRADING: meta('升级中', 'info', 'status-service-upgrading', 'Service 正在升级。'),
  FAILED_INSTALL: meta('安装失败', 'error', 'status-service-failed', 'Service 安装失败。'),
  FAILED_UPGRADE: meta('升级失败', 'error', 'status-service-failed', 'Service 升级失败。'),
  REMOVED: meta('已移除', 'default', 'status-service-removed', 'Service 已移除。'),
}

const difficultyMeta: Record<string, StatusMeta> = {
  easy: meta('简单', 'success', 'difficulty-easy', '入门难度。'),
  medium: meta('中等', 'warning', 'difficulty-medium', '标准难度。'),
  hard: meta('困难', 'error', 'difficulty-hard', '高难度题目。'),
}

const visibilityMeta: Record<string, StatusMeta> = {
  public: meta('公开', 'success', 'visibility-public', '所有登录用户可见。'),
  private: meta('私有', 'warning', 'visibility-private', '仅授权用户可见。'),
}

const problemStatusMeta: Record<string, StatusMeta> = {
  draft: meta('草稿', 'default', 'problem-draft', '题目仍在准备中。'),
  ready: meta('待发布', 'info', 'problem-ready', '题目可以审核或发布。'),
  published: meta('已发布', 'success', 'problem-published', '题目已发布。'),
  archived: meta('已归档', 'default', 'problem-archived', '题目已归档。'),
}

export function getJudgeStatusMeta(status: JudgeStatus | string): StatusMeta {
  return judgeStatusMeta[String(status).toUpperCase()] ?? fromUnknown(status)
}

export function getTaskStatusMeta(status: TaskStatus | string): StatusMeta {
  return taskStatusMeta[String(status).toLowerCase()] ?? fromUnknown(status)
}

export function getHealthStatusMeta(status: HealthState | 'down' | string): StatusMeta {
  return healthStatusMeta[String(status).toLowerCase()] ?? fromUnknown(status)
}

export function getWorkerStatusMeta(status: WorkerStatus | 'BUSY' | 'ERROR' | string): StatusMeta {
  return workerStatusMeta[String(status).toUpperCase()] ?? fromUnknown(status)
}

export function getServiceStatusMeta(status: ServiceStatus | string): StatusMeta {
  return serviceStatusMeta[String(status).toUpperCase()] ?? fromUnknown(status)
}

export function getDifficultyMeta(difficulty: ProblemDifficulty | string): StatusMeta {
  return difficultyMeta[String(difficulty).toLowerCase()] ?? fromUnknown(difficulty)
}

export function getProblemVisibilityMeta(visibility: ProblemVisibility | string): StatusMeta {
  return visibilityMeta[String(visibility).toLowerCase()] ?? fromUnknown(visibility)
}

export function getProblemStatusMeta(status: ProblemStatus | string): StatusMeta {
  return problemStatusMeta[String(status).toLowerCase()] ?? fromUnknown(status)
}

function meta(label: string, type: OjosTagType, className: string, description: string): StatusMeta {
  return { label, type, className, description }
}

function fromUnknown(value: string): StatusMeta {
  if (!value) return fallbackMeta
  return {
    label: value,
    type: 'default',
    className: 'status-unknown',
    description: '前端暂不识别当前状态。',
  }
}
