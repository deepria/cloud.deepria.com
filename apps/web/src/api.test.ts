import { afterEach, expect, test, vi } from 'vitest'
import { api } from './api'
import { draftKey, readDraft, writeDraft } from './drafts'

afterEach(() => vi.unstubAllGlobals())

test('memo changes send version or a stable create key to the same origin', async () => {
  const fetchMock = vi.fn(async () => Response.json({ id: 'memo', version: 2 }))
  vi.stubGlobal('fetch', fetchMock)

  const content = { title: 'Writing', body: 'Draft' }
  await api.create(content, 'retry-key')
  await api.update('memo', content, 1)

  expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/memos', expect.objectContaining({
    method: 'POST',
    credentials: 'same-origin',
    body: JSON.stringify({ ...content, request_key: 'retry-key' }),
  }))
  expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/memos/memo', expect.objectContaining({
    method: 'PATCH',
    body: JSON.stringify({ ...content, version: 1 }),
  }))
})

test('exposes conflicts and distinguishes gateway HTML from an API response', async () => {
  vi.stubGlobal('fetch', vi.fn()
    .mockResolvedValueOnce(Response.json({ error: { message: 'Changed' } }, { status: 409 }))
    .mockResolvedValueOnce(new Response('<html></html>', { headers: { 'content-type': 'text/html' } })))

  await expect(api.delete('id', 1)).rejects.toMatchObject({ status: 409, message: 'Changed' })
  await expect(api.me()).rejects.toThrow('서비스 응답')
})

test('drafts are scoped to a user and persist the version and create key', () => {
  const item = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    setItem: (key: string, value: string) => item.set(key, value),
    getItem: (key: string) => item.get(key),
    removeItem: (key: string) => item.delete(key),
  })
  const a = draftKey('a', 'new')
  const b = draftKey('b', 'new')
  const draft = { title: 'T', body: 'B', version: null, requestKey: 'same-retry-key' }

  expect(writeDraft(a, draft)).toBe(true)
  expect(readDraft(a)).toEqual(draft)
  expect(readDraft(b)).toBeNull()
  expect(writeDraft(a, null)).toBe(true)
  expect(readDraft(a)).toBeNull()
})
