<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { RouterLink, onBeforeRouteLeave, onBeforeRouteUpdate, useRoute, useRouter } from 'vue-router'
import { api, ApiError, type Memo, type MemoContent, type MemoSummary, type User } from './api'
import { draftKey, readDraft, writeDraft } from './drafts'

const { user } = defineProps<{ user: User }>()
const emit = defineEmits<{ sessionExpired: [] }>()
const route = useRoute()
const router = useRouter()
const selected = computed(() => typeof route.params.id === 'string' ? route.params.id : '')
const items = ref<MemoSummary[]>([])
const nextCursor = ref<string | null>(null)
const listLoading = ref(false)
const listError = ref('')
const editorLoading = ref(false)
const editorError = ref('')
const editor = ref<Editor | null>(null)
const date = new Intl.DateTimeFormat('ko-KR', { dateStyle: 'short' })
let timer: ReturnType<typeof setTimeout> | undefined
let listSequence = 0
let editorSequence = 0

type Editor = MemoContent & {
  id: string
  version: number | null
  requestKey: string | null
  dirty: boolean
  draftStored: boolean
  saving: boolean
  message: string
  conflict: Memo | null
}

function errorText(error: unknown, fallback: string) {
  if (error instanceof ApiError && error.status === 401) emit('sessionExpired')
  return error instanceof ApiError ? error.message : fallback
}

async function loadList(append = false) {
  const sequence = ++listSequence
  listLoading.value = true
  listError.value = ''
  try {
    const page = await api.list(append ? nextCursor.value : null)
    if (sequence !== listSequence) return
    const memos = append ? [...items.value, ...page.items] : page.items
    items.value = [...new Map(memos.map(item => [item.id, item])).values()]
    nextCursor.value = page.next_cursor
  } catch (error) {
    if (sequence === listSequence) listError.value = errorText(error, '메모 목록을 불러오지 못했습니다.')
  } finally {
    if (sequence === listSequence) listLoading.value = false
  }
}

function storedKey(id: string) {
  return draftKey(user.id, id)
}

async function open(id: string) {
  clearTimeout(timer)
  const sequence = ++editorSequence
  editor.value = null
  editorError.value = ''
  editorLoading.value = !!id && id !== 'new'
  if (!id) return

  const draft = readDraft(storedKey(id))
  try {
    const memo = id === 'new' ? null : await api.read(id)
    if (sequence !== editorSequence) return
    const draftDiffers = !!draft && (draft.title !== (memo?.title ?? '') || draft.body !== (memo?.body ?? ''))
    editor.value = {
      id,
      title: draft?.title ?? memo?.title ?? '',
      body: draft?.body ?? memo?.body ?? '',
      version: draft?.version ?? memo?.version ?? null,
      requestKey: id === 'new' ? (draft?.requestKey ?? crypto.randomUUID()) : null,
      dirty: draftDiffers || (id === 'new' && !!draft),
      draftStored: !!draft,
      saving: false,
      message: draftDiffers ? '이 브라우저의 미저장 초안을 불러왔습니다.' : '',
      conflict: draftDiffers && memo && draft.version !== memo.version ? memo : null,
    }
    if (draft && memo && !draftDiffers) {
      editor.value.version = memo.version
      writeDraft(storedKey(id), null)
    }
  } catch (error) {
    if (sequence === editorSequence) editorError.value = errorText(error, '메모를 불러오지 못했습니다.')
  } finally {
    if (sequence === editorSequence) editorLoading.value = false
  }
}

function persist(current: Editor) {
  current.draftStored = writeDraft(storedKey(current.id), {
    title: current.title,
    body: current.body,
    version: current.version,
    requestKey: current.requestKey,
  })
}

function edit(field: keyof MemoContent, event: Event) {
  const current = editor.value
  if (!current) return
  current[field] = (event.target as HTMLInputElement | HTMLTextAreaElement).value
  current.dirty = true
  persist(current)
  current.message = current.draftStored ? '저장 대기 중' : '브라우저에 초안을 보관할 수 없습니다.'
  schedule(current)
}

function schedule(current: Editor) {
  clearTimeout(timer)
  if (current.id !== 'new' && !current.saving && !current.conflict) {
    timer = setTimeout(() => void save(current), 800)
  }
}

async function latest(current: Editor) {
  try {
    current.conflict = await api.read(current.id)
    current.message = '서버에 바뀐 내용이 있습니다. 아래에서 두 버전을 비교해 주세요.'
  } catch (error) {
    current.message = errorText(error, '서버 버전을 불러오지 못했습니다. 다시 시도해 주세요.')
  }
}

async function save(current: Editor) {
  clearTimeout(timer)
  if (current.saving || !current.dirty || current.conflict) return

  current.saving = true
  current.message = '저장 중'
  const content = { title: current.title, body: current.body }

  try {
    const isNew = current.id === 'new'
    const memo = isNew
      ? await api.create(content, current.requestKey!)
      : await api.update(current.id, content, current.version!)
    if (isNew) {
      if ((memo.title !== content.title || memo.body !== content.body)
        && !writeDraft(storedKey(memo.id), { ...content, version: null, requestKey: null })) {
        current.message = '앞서 저장된 메모가 있습니다. 새 초안을 보관할 수 없어 현재 화면에 유지합니다.'
        return
      }
      writeDraft(storedKey('new'), null)
      current.dirty = false
      current.saving = false
      await router.replace(`/memos/${memo.id}`)
    } else {
      current.version = memo.version
      current.dirty = current.title !== content.title || current.body !== content.body
      if (current.dirty) persist(current)
      else writeDraft(storedKey(current.id), null)
      current.message = current.dirty ? '추가 변경사항 저장 대기 중' : '저장했습니다.'
    }
    void loadList()
  } catch (error) {
    if (error instanceof ApiError && error.status === 409 && current.id !== 'new') {
      await latest(current)
    } else {
      current.message = errorText(error, '저장에 실패했습니다. 다시 시도해 주세요.')
    }
    return
  } finally {
    current.saving = false
  }
  if (current.dirty) schedule(current)
}

function resolveConflict(current: Editor, keepDraft: boolean) {
  const server = current.conflict
  if (!server) return
  current.version = server.version
  current.conflict = null
  if (keepDraft) {
    current.dirty = true
    persist(current)
    void save(current)
  } else {
    current.title = server.title
    current.body = server.body
    current.dirty = false
    current.message = '서버 버전을 불러왔습니다.'
    writeDraft(storedKey(current.id), null)
  }
}

async function remove(current: Editor) {
  if (current.version === null || !window.confirm('이 메모를 삭제할까요?')) return
  clearTimeout(timer)
  current.saving = true
  try {
    await api.delete(current.id, current.version)
    writeDraft(storedKey(current.id), null)
    current.dirty = false
    current.saving = false
    await router.replace('/memos')
    void loadList()
  } catch (error) {
    if (error instanceof ApiError && error.status === 409) await latest(current)
    else current.message = errorText(error, '삭제에 실패했습니다.')
  } finally {
    current.saving = false
  }
}

function canLeave() {
  const current = editor.value
  if (current?.saving) {
    current.message = '저장이 끝날 때까지 기다려 주세요.'
    return false
  }
  return !current?.dirty || current.draftStored || window.confirm('보관하지 못한 초안을 버리고 이동할까요?')
}

function beforeUnload(event: BeforeUnloadEvent) {
  const current = editor.value
  if (current?.saving || (current?.dirty && !current.draftStored)) event.preventDefault()
}

onBeforeRouteLeave(canLeave)
onBeforeRouteUpdate(canLeave)
watch(selected, open, { immediate: true })

onMounted(() => {
  window.addEventListener('beforeunload', beforeUnload)
  void loadList()
})
onBeforeUnmount(() => {
  clearTimeout(timer)
  ++listSequence
  ++editorSequence
  window.removeEventListener('beforeunload', beforeUnload)
})
</script>

<template>
  <main class="memo-layout" :class="{ 'memo-selected': selected }">
    <aside class="memo-list" aria-label="메모 목록">
      <div class="toolbar">
        <h1>메모</h1>
        <RouterLink to="/memos/new" class="button">새 메모</RouterLink>
      </div>
      <p v-if="listError" role="alert">{{ listError }} <button type="button" class="quiet" @click="loadList()">다시 시도</button></p>
      <p v-else-if="listLoading && !items.length" role="status">메모를 불러오고 있습니다.</p>
      <p v-else-if="!items.length" class="muted">아직 작성한 메모가 없습니다.</p>
      <ul v-if="items.length">
        <li v-for="item in items" :key="item.id">
          <RouterLink :to="`/memos/${item.id}`">
            <span>{{ item.title || '제목 없음' }}</span>
            <time :datetime="item.updated_at">{{ date.format(new Date(item.updated_at)) }}</time>
          </RouterLink>
        </li>
      </ul>
      <button v-if="nextCursor" type="button" class="quiet" :disabled="listLoading" @click="loadList(true)">더 보기</button>
    </aside>

    <section class="memo-editor" aria-label="메모 편집">
      <RouterLink v-if="selected" to="/memos" class="back">← 메모 목록</RouterLink>
      <p v-if="editorLoading" role="status">메모를 불러오고 있습니다.</p>
      <p v-else-if="editorError" role="alert">
        {{ editorError }} <button type="button" class="quiet" @click="open(selected)">다시 시도</button>
      </p>
      <form v-else-if="editor" @submit.prevent="save(editor)">
        <label for="memo-title">제목</label>
        <input id="memo-title" :value="editor.title" maxlength="200" :disabled="editor.saving && (editor.id === 'new' || !!editor.conflict)" @input="edit('title', $event)">
        <label for="memo-body">내용</label>
        <textarea id="memo-body" :value="editor.body" maxlength="100000" :disabled="editor.saving && (editor.id === 'new' || !!editor.conflict)" @input="edit('body', $event)" />
        <div class="toolbar editor-actions">
          <button type="submit" :disabled="editor.saving || !editor.dirty || !!editor.conflict">저장</button>
          <button v-if="editor.id !== 'new'" type="button" class="quiet danger" :disabled="editor.saving || !!editor.conflict" @click="remove(editor)">삭제</button>
          <span class="muted" role="status">{{ editor.message }}</span>
        </div>
        <div v-if="editor.conflict" class="conflict">
          <h2>서버에 저장된 버전</h2>
          <strong>{{ editor.conflict.title || '제목 없음' }}</strong>
          <pre>{{ editor.conflict.body }}</pre>
          <div class="toolbar">
            <button type="button" class="quiet" @click="resolveConflict(editor, false)">서버 버전 사용</button>
            <button type="button" @click="resolveConflict(editor, true)">내 초안으로 저장</button>
          </div>
        </div>
      </form>
      <p v-else class="muted">메모를 선택하거나 새 메모를 작성하세요.</p>
    </section>
  </main>
</template>
