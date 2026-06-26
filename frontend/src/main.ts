import { createPinia } from 'pinia'
import { createApp } from 'vue'

import './style.css'
import App from './App.vue'
import { setUnauthorizedHandler } from './api/client'
import { router } from './router'
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
