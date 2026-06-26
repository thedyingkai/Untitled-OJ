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
  label: 'Unknown',
  type: 'default',
  className: 'status-unknown',
  description: 'The current status is not recognized by the frontend.',
}

const judgeStatusMeta: Record<JudgeStatus, StatusMeta> = {
  PENDING: {
    label: 'Pending',
    type: 'default',
    className: 'status-pending',
    description: 'Queued and waiting for a worker.',
  },
  JUDGING: {
    label: 'Judging',
    type: 'info',
    className: 'status-judging',
    description: 'A worker is executing this submission.',
  },
  ACCEPTED: {
    label: 'Accepted',
    type: 'success',
    className: 'status-accepted',
    description: 'All required cases passed.',
  },
  WRONG_ANSWER: {
    label: 'Wrong Answer',
    type: 'error',
    className: 'status-wrong-answer',
    description: 'The output did not match the expected answer.',
  },
  COMPILE_ERROR: {
    label: 'Compile Error',
    type: 'warning',
    className: 'status-compile-error',
    description: 'The source failed to compile.',
  },
  RUNTIME_ERROR: {
    label: 'Runtime Error',
    type: 'error',
    className: 'status-runtime-error',
    description: 'The program crashed or returned a non-zero exit code.',
  },
  TIME_LIMIT_EXCEEDED: {
    label: 'Time Limit',
    type: 'warning',
    className: 'status-time-limit',
    description: 'The program exceeded the time limit.',
  },
  MEMORY_LIMIT_EXCEEDED: {
    label: 'Memory Limit',
    type: 'warning',
    className: 'status-memory-limit',
    description: 'The program exceeded the memory limit.',
  },
  OUTPUT_LIMIT_EXCEEDED: {
    label: 'Output Limit',
    type: 'warning',
    className: 'status-output-limit',
    description: 'The program produced too much output.',
  },
  SYSTEM_ERROR: {
    label: 'System Error',
    type: 'error',
    className: 'status-system-error',
    description: 'The judge system failed while processing this submission.',
  },
  CANCELLED: {
    label: 'Cancelled',
    type: 'default',
    className: 'status-cancelled',
    description: 'The submission was cancelled.',
  },
  UNSUPPORTED_LANGUAGE: {
    label: 'Unsupported',
    type: 'default',
    className: 'status-unsupported',
    description: 'The selected language is not supported by the judge.',
  },
}

const taskStatusMeta: Record<TaskStatus, StatusMeta> = {
  PENDING: {
    label: 'Pending',
    type: 'default',
    className: 'status-pending',
    description: 'Task is waiting for a worker lease.',
  },
  RUNNING: {
    label: 'Running',
    type: 'info',
    className: 'status-judging',
    description: 'Task is currently leased by a worker.',
  },
  SUCCEEDED: {
    label: 'Succeeded',
    type: 'success',
    className: 'status-accepted',
    description: 'Task completed successfully.',
  },
  FAILED: {
    label: 'Failed',
    type: 'error',
    className: 'status-system-error',
    description: 'Task failed and is not retrying.',
  },
  CANCELLED: {
    label: 'Cancelled',
    type: 'default',
    className: 'status-cancelled',
    description: 'Task was cancelled.',
  },
}

const healthStatusMeta: Record<string, StatusMeta> = {
  ok: {
    label: 'OK',
    type: 'success',
    className: 'status-health-ok',
    description: 'Component is healthy.',
  },
  degraded: {
    label: 'Degraded',
    type: 'warning',
    className: 'status-health-degraded',
    description: 'Component is reachable but reports a degraded state.',
  },
  down: {
    label: 'Down',
    type: 'error',
    className: 'status-health-down',
    description: 'Component is unavailable.',
  },
  error: {
    label: 'Down',
    type: 'error',
    className: 'status-health-down',
    description: 'Component returned an error.',
  },
  unknown: {
    label: 'Unknown',
    type: 'default',
    className: 'status-unknown',
    description: 'Component health could not be determined.',
  },
}

const workerStatusMeta: Record<string, StatusMeta> = {
  ONLINE: {
    label: 'Online',
    type: 'success',
    className: 'status-worker-online',
    description: 'Worker is connected and heartbeating.',
  },
  OFFLINE: {
    label: 'Offline',
    type: 'default',
    className: 'status-worker-offline',
    description: 'Worker has not heartbeated recently.',
  },
  DRAINING: {
    label: 'Draining',
    type: 'warning',
    className: 'status-worker-draining',
    description: 'Worker will stop taking new tasks.',
  },
  BUSY: {
    label: 'Busy',
    type: 'info',
    className: 'status-worker-busy',
    description: 'Worker has no free execution slots.',
  },
  ERROR: {
    label: 'Error',
    type: 'error',
    className: 'status-worker-error',
    description: 'Worker reported an error state.',
  },
}

const moduleStatusMeta: Record<string, StatusMeta> = {
  ENABLED: {
    label: 'Enabled',
    type: 'success',
    className: 'status-module-enabled',
    description: 'Module is enabled.',
  },
  DISABLED: {
    label: 'Disabled',
    type: 'default',
    className: 'status-module-disabled',
    description: 'Module is disabled.',
  },
  INSTALLING: {
    label: 'Installing',
    type: 'info',
    className: 'status-module-installing',
    description: 'Module installation is in progress.',
  },
  UPGRADING: {
    label: 'Upgrading',
    type: 'info',
    className: 'status-module-upgrading',
    description: 'Module upgrade is in progress.',
  },
  FAILED_INSTALL: {
    label: 'Install Failed',
    type: 'error',
    className: 'status-module-failed',
    description: 'Module installation failed.',
  },
  FAILED_UPGRADE: {
    label: 'Upgrade Failed',
    type: 'error',
    className: 'status-module-failed',
    description: 'Module upgrade failed.',
  },
  REMOVED: {
    label: 'Removed',
    type: 'default',
    className: 'status-module-removed',
    description: 'Module is removed.',
  },
}

const difficultyMeta: Record<ProblemDifficulty | string, StatusMeta> = {
  easy: {
    label: 'Easy',
    type: 'success',
    className: 'difficulty-easy',
    description: 'Introductory or warm-up difficulty.',
  },
  medium: {
    label: 'Medium',
    type: 'warning',
    className: 'difficulty-medium',
    description: 'Standard contest difficulty.',
  },
  hard: {
    label: 'Hard',
    type: 'error',
    className: 'difficulty-hard',
    description: 'Advanced or demanding problem.',
  },
}

const visibilityMeta: Record<ProblemVisibility, StatusMeta> = {
  public: {
    label: 'Public',
    type: 'success',
    className: 'visibility-public',
    description: 'Visible to all authenticated users.',
  },
  private: {
    label: 'Private',
    type: 'warning',
    className: 'visibility-private',
    description: 'Restricted to authorized users.',
  },
  contest_only: {
    label: 'Contest',
    type: 'info',
    className: 'visibility-contest',
    description: 'Available through contest scope.',
  },
}

const problemStatusMeta: Record<ProblemStatus, StatusMeta> = {
  draft: {
    label: 'Draft',
    type: 'default',
    className: 'problem-draft',
    description: 'Problem is still being prepared.',
  },
  ready: {
    label: 'Ready',
    type: 'info',
    className: 'problem-ready',
    description: 'Problem is ready for review or publication.',
  },
  published: {
    label: 'Published',
    type: 'success',
    className: 'problem-published',
    description: 'Problem is published.',
  },
  archived: {
    label: 'Archived',
    type: 'default',
    className: 'problem-archived',
    description: 'Problem is archived.',
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
    description: `Unrecognized status: ${value}`,
  }
}
