import type { MemoContent } from './api'

export type Draft = MemoContent & { version: number | null; requestKey: string | null }

export function draftKey(userId: string, memoId: string): string {
  return `cloud.memo-draft.${userId}.${memoId}`
}

export function readDraft(key: string): Draft | null {
  try {
    const draft: Draft | null = JSON.parse(localStorage.getItem(key) ?? 'null')
    if (!draft || typeof draft.title !== 'string' || typeof draft.body !== 'string'
      || !(draft.version === null || Number.isSafeInteger(draft.version))
      || !(draft.requestKey === null || typeof draft.requestKey === 'string')) return null
    return draft
  } catch {
    return null
  }
}

export function writeDraft(key: string, draft: Draft | null): boolean {
  try {
    if (draft) localStorage.setItem(key, JSON.stringify(draft))
    else localStorage.removeItem(key)
    return true
  } catch {
    return false
  }
}
