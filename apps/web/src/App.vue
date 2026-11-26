<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import { api, ApiError, type User } from './api'

const route = useRoute()
const router = useRouter()
const user = ref<User>()
const loading = ref(true)
const githubEnabled = ref(false)
const sessionError = ref('')
const signingOut = ref(false)
const callbackMessages = new Map([
  ['denied', '이 GitHub 계정은 아직 사용 승인을 받지 않았습니다.'],
  ['invalid', '로그인 요청이 만료됐거나 취소됐습니다. 다시 시작해 주세요.'],
  ['unavailable', '로그인 서비스에 연결할 수 없습니다. 잠시 후 다시 시도해 주세요.'],
])
const callbackMessage = computed(() => typeof route.query.login === 'string'
  ? callbackMessages.get(route.query.login) : '')
const loginUrl = computed(() => {
  const destination = /^\/memos(?:\/(?:new|[0-9a-f-]{36}))?$/i.test(route.path) ? route.path : '/memos'
  return `/api/auth/github/start?return_to=${encodeURIComponent(destination)}`
})

async function loadSession() {
  loading.value = true
  sessionError.value = ''
  try {
    user.value = await api.me()
    if (route.path === '/') await router.replace('/memos')
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      user.value = undefined
      try {
        githubEnabled.value = (await api.providers()).includes('github')
      } catch {
        sessionError.value = '로그인 옵션을 불러오지 못했습니다.'
      }
    } else {
      sessionError.value = '로그인 상태를 확인할 수 없습니다.'
    }
  } finally {
    loading.value = false
  }
}

async function signOut() {
  sessionError.value = ''
  const before = route.fullPath
  if (before !== '/' && await router.replace('/')) return
  signingOut.value = true
  try {
    await api.logout()
    await loadSession()
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      await loadSession()
    } else {
      await router.replace(before)
      sessionError.value = '로그아웃하지 못했습니다. 다시 시도해 주세요.'
    }
  } finally {
    signingOut.value = false
  }
}

onMounted(async () => {
  await router.isReady()
  await loadSession()
})
</script>

<template>
  <template v-if="user">
    <header class="app-header">
      <span class="brand">Deepria Cloud</span>
      <span class="account">{{ user.display_name }}</span>
      <button type="button" class="quiet" :disabled="signingOut" @click="signOut">로그아웃</button>
    </header>
    <p v-if="sessionError" class="banner" role="alert">{{ sessionError }}</p>
    <RouterView v-slot="{ Component }">
      <component :is="Component" :key="user.id" :user="user" @session-expired="loadSession" />
    </RouterView>
  </template>
  <main v-else class="login">
    <h1>Deepria Cloud</h1>
    <p v-if="loading" role="status">로그인 상태를 확인하고 있습니다.</p>
    <template v-else>
      <p v-if="callbackMessage" role="alert">{{ callbackMessage }}</p>
      <p v-if="sessionError" role="alert">{{ sessionError }}</p>
      <button v-if="sessionError" type="button" @click="loadSession">다시 확인</button>
      <a v-else-if="githubEnabled" :href="loginUrl" class="button">GitHub로 로그인</a>
      <p v-else>이 환경에서 GitHub 로그인을 사용하려면 OAuth 설정이 필요합니다.</p>
    </template>
  </main>
</template>
