<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { serviceIsReady } from './api'

const status = ref<'loading' | 'ready' | 'unavailable' | 'error'>('loading')
const messages = {
  loading: '연결 상태를 확인하고 있습니다.',
  ready: '서비스에 연결됐습니다.',
  unavailable: '서비스가 준비 중입니다.',
  error: '연결 상태를 확인할 수 없습니다.',
}

async function refresh() {
  status.value = 'loading'
  try {
    status.value = await serviceIsReady(AbortSignal.timeout(5000)) ? 'ready' : 'unavailable'
  } catch {
    status.value = 'error'
  }
}

onMounted(refresh)
</script>

<template>
  <main>
    <h1>Deepria Cloud</h1>
    <p role="status">{{ messages[status] }}</p>
    <button type="button" :disabled="status === 'loading'" @click="refresh">
      다시 확인
    </button>
  </main>
</template>
