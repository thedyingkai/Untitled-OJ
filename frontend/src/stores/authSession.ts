import type { AuthSnapshot, CurrentUser } from '../types/auth'

const TOKEN_KEY = 'ojos.auth.token'
const USER_KEY = 'ojos.auth.user'

export function getAuthToken(): string {
  return localStorage.getItem(TOKEN_KEY) ?? ''
}

export function getStoredUser(): CurrentUser | null {
  const raw = localStorage.getItem(USER_KEY)
  if (!raw) {
    return null
  }

  try {
    const parsed = JSON.parse(raw) as CurrentUser
    if (!parsed || typeof parsed.user_id !== 'number' || !parsed.username) {
      return null
    }
    return {
      user_id: parsed.user_id,
      username: parsed.username,
      roles: Array.isArray(parsed.roles) ? parsed.roles : [],
      permissions: Array.isArray(parsed.permissions) ? parsed.permissions : [],
    }
  } catch {
    return null
  }
}

export function saveAuthSnapshot(snapshot: AuthSnapshot): void {
  localStorage.setItem(TOKEN_KEY, snapshot.token)

  if (snapshot.user) {
    localStorage.setItem(USER_KEY, JSON.stringify(snapshot.user))
  } else {
    localStorage.removeItem(USER_KEY)
  }
}

export function clearAuthSnapshot(): void {
  localStorage.removeItem(TOKEN_KEY)
  localStorage.removeItem(USER_KEY)
}
