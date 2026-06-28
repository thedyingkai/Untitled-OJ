import type { HealthState } from '../types/health'
import type { JudgeStatus } from '../types/judge'
import type { ModuleStatus } from '../types/module'
import type { TaskStatus, WorkerStatus } from '../types/worker'
import type { ProblemDifficulty, ProblemStatus, ProblemVisibility } from '../types/problem'

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
  description: '前端暂不认识当前状态。',
}

const judgeStatusMeta: Record<JudgeStatus, StatusMeta> = {
  PENDING: {
    label: '等待中',
    type: 'default',
    className: 'status-pending',
    description: '已进入队列，等待 Worker 领取。',
  },
  JUDGING: {
    label: '评测中',
    type: 'info',
    className: 'status-judging',
    description: 'Worker 正在执行本次提交。',
  },
  ACCEPTED: {
    label: '通过',
    type: 'success',
    className: 'status-accepted',
    description: '全部必需测试点通过。',
  },
  WRONG_ANSWER: {
    label: '答案错误',
    type: 'error',
    className: 'status-wrong-answer',
    description: '程序输出与标准答案不一致。',
  },
  COMPILE_ERROR: {
    label: '编译错误',
    type: 'warning',
    className: 'status-compile-error',
    description: '源码未能通过编译。',
  },
  RUNTIME_ERROR: {
    label: '运行错误',
    type: 'error',
    className: 'status-runtime-error',
    description: '程序崩溃或返回非零退出码。',
  },
  TIME_LIMIT_EXCEEDED: {
    label: '超时',
    type: 'warning',
    className: 'status-time-limit',
    description: '程序超过时间限制。',
  },
  MEMORY_LIMIT_EXCEEDED: {
    label: '超内存',
    type: 'warning',
    className: 'status-memory-limit',
    description: '程序超过内存限制。',
  },
  OUTPUT_LIMIT_EXCEEDED: {
    label: '输出超限',
    type: 'warning',
    className: 'status-output-limit',
    description: '程序输出超过限制。',
  },
  SYSTEM_ERROR: {
    label: '系统错误',
    type: 'error',
    className: 'status-system-error',
    description: '评测系统处理本次提交时失败。',
  },
  CANCELLED: {
    label: '已取消',
    type: 'default',
    className: 'status-cancelled',
    description: '本次提交已取消。',
  },
  UNSUPPORTED_LANGUAGE: {
    label: '语言不支持',
    type: 'default',
    className: 'status-unsupported',
    description: '当前评测机不支持所选语言。',
  },
}

const taskStatusMeta: Record<TaskStatus, StatusMeta> = {
  PENDING: {
    label: '等待中',
    type: 'default',
    className: 'status-pending',
    description: '任务正在等待 Worker 租约。',
  },
  RUNNING: {
    label: '运行中',
    type: 'info',
    className: 'status-judging',
    description: '任务已被 Worker 领取。',
  },
  SUCCEEDED: {
    label: '成功',
    type: 'success',
    className: 'status-accepted',
    description: '任务已成功完成。',
  },
  FAILED: {
    label: '失败',
    type: 'error',
    className: 'status-system-error',
    description: '任务失败且不再重试。',
  },
  CANCELLED: {
    label: '已取消',
    type: 'default',
    className: 'status-cancelled',
    description: '任务已取消。',
  },
}

const healthStatusMeta: Record<string, StatusMeta> = {
  ok: {
    label: 'OK',
    type: 'success',
    className: 'status-health-ok',
    description: '组件健康。',
  },
  degraded: {
    label: '降级',
    type: 'warning',
    className: 'status-health-degraded',
    description: '组件可达，但报告为降级状态。',
  },
  down: {
    label: '不可用',
    type: 'error',
    className: 'status-health-down',
    description: '组件不可用。',
  },
  error: {
    label: '异常',
    type: 'error',
    className: 'status-health-down',
    description: '组件返回错误。',
  },
  unknown: {
    label: '未知',
    type: 'default',
    className: 'status-unknown',
    description: '暂时无法判断组件健康状态。',
  },
}

const workerStatusMeta: Record<string, StatusMeta> = {
  ONLINE: {
    label: '在线',
    type: 'success',
    className: 'status-worker-online',
    description: 'Worker 已连接并持续心跳。',
  },
  OFFLINE: {
    label: '离线',
    type: 'default',
    className: 'status-worker-offline',
    description: 'Worker 近期没有心跳。',
  },
  DRAINING: {
    label: '排空中',
    type: 'warning',
    className: 'status-worker-draining',
    description: 'Worker 将停止领取新任务。',
  },
  BUSY: {
    label: '繁忙',
    type: 'info',
    className: 'status-worker-busy',
    description: 'Worker 没有空闲执行槽位。',
  },
  ERROR: {
    label: '异常',
    type: 'error',
    className: 'status-worker-error',
    description: 'Worker 报告异常状态。',
  },
}

const moduleStatusMeta: Record<string, StatusMeta> = {
  ENABLED: {
    label: '已启用',
    type: 'success',
    className: 'status-module-enabled',
    description: '模块已启用。',
  },
  DISABLED: {
    label: '已禁用',
    type: 'default',
    className: 'status-module-disabled',
    description: '模块已禁用。',
  },
  INSTALLING: {
    label: '安装中',
    type: 'info',
    className: 'status-module-installing',
    description: '模块正在安装。',
  },
  UPGRADING: {
    label: '升级中',
    type: 'info',
    className: 'status-module-upgrading',
    description: '模块正在升级。',
  },
  FAILED_INSTALL: {
    label: '安装失败',
    type: 'error',
    className: 'status-module-failed',
    description: '模块安装失败。',
  },
  FAILED_UPGRADE: {
    label: '升级失败',
    type: 'error',
    className: 'status-module-failed',
    description: '模块升级失败。',
  },
  REMOVED: {
    label: '已移除',
    type: 'default',
    className: 'status-module-removed',
    description: '模块已移除。',
  },
}

const difficultyMeta: Record<ProblemDifficulty | string, StatusMeta> = {
  easy: {
    label: '简单',
    type: 'success',
    className: 'difficulty-easy',
    description: '入门或热身难度。',
  },
  medium: {
    label: '中等',
    type: 'warning',
    className: 'difficulty-medium',
    description: '标准比赛难度。',
  },
  hard: {
    label: '困难',
    type: 'error',
    className: 'difficulty-hard',
    description: '进阶或高要求题目。',
  },
}

const visibilityMeta: Record<ProblemVisibility, StatusMeta> = {
  public: {
    label: '公开',
    type: 'success',
    className: 'visibility-public',
    description: '所有已登录用户可见。',
  },
  private: {
    label: '私有',
    type: 'warning',
    className: 'visibility-private',
    description: '仅授权用户可见。',
  },
  contest_only: {
    label: '比赛内',
    type: 'info',
    className: 'visibility-contest',
    description: '仅在比赛作用域内可用。',
  },
}

const problemStatusMeta: Record<ProblemStatus, StatusMeta> = {
  draft: {
    label: '草稿',
    type: 'default',
    className: 'problem-draft',
    description: '题目仍在准备中。',
  },
  ready: {
    label: '待发布',
    type: 'info',
    className: 'problem-ready',
    description: '题目已可审核或发布。',
  },
  published: {
    label: '已发布',
    type: 'success',
    className: 'problem-published',
    description: '题目已发布。',
  },
  archived: {
    label: '已归档',
    type: 'default',
    className: 'problem-archived',
    description: '题目已归档。',
  },
}

export function getJudgeStatusMeta(status: JudgeStatus | string): StatusMeta {
  return judgeStatusMeta[status as JudgeStatus] ?? fromUnknown(status)
}

export function getTaskStatusMeta(status: TaskStatus | string): StatusMeta {
  return taskStatusMeta[status as TaskStatus] ?? fromUnknown(status)
}

export function getHealthStatusMeta(status: HealthState | 'down' | string): StatusMeta {
  return healthStatusMeta[String(status).toLowerCase()] ?? fromUnknown(status)
}

export function getWorkerStatusMeta(status: WorkerStatus | 'BUSY' | 'ERROR' | string): StatusMeta {
  return workerStatusMeta[String(status).toUpperCase()] ?? fromUnknown(status)
}

export function getModuleStatusMeta(status: ModuleStatus | string): StatusMeta {
  return moduleStatusMeta[String(status).toUpperCase()] ?? fromUnknown(status)
}

export function getDifficultyMeta(difficulty: ProblemDifficulty | string): StatusMeta {
  return difficultyMeta[String(difficulty).toLowerCase()] ?? fromUnknown(difficulty)
}

export function getProblemVisibilityMeta(visibility: ProblemVisibility | string): StatusMeta {
  return visibilityMeta[visibility as ProblemVisibility] ?? fromUnknown(visibility)
}

export function getProblemStatusMeta(status: ProblemStatus | string): StatusMeta {
  return problemStatusMeta[status as ProblemStatus] ?? fromUnknown(status)
}

function fromUnknown(value: string): StatusMeta {
  if (!value) {
    return fallbackMeta
  }
  return {
    label: value,
    type: 'default',
    className: 'status-unknown',
    description: `未识别状态：${value}`,
  }
}
