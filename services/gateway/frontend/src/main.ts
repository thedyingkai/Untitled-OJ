import { createPinia } from 'pinia'
import { createApp } from 'vue'

import './style.css'
import App from './App.vue'
import { setUnauthorizedHandler } from './api/client'
import { router } from './router'
import { startUserFrontendHost } from './ojos-frontend/shell-host'
import { useAuthStore } from './stores/auth'

const app = createApp(App)
const pinia = createPinia()

app.use(pinia)
app.use(router)

setUnauthorizedHandler(() => {
  const auth = useAuthStore()
  auth.clearAuth()

  const current = router.currentRoute.value
  if (current.name !== 'login') {
    void router.push({
      name: 'login',
      query: { redirect: current.fullPath },
    })
  }
})

app.mount('#app')

const frontendHost = startUserFrontendHost(router, pinia)
window.addEventListener('beforeunload', () => void frontendHost.dispose(), { once: true })

if (import.meta.hot) {
  import.meta.hot.dispose(() => void frontendHost.dispose())
}
