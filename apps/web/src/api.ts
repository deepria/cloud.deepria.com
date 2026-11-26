export class ApiError extends Error {
  constructor(readonly status: number, message: string) {
    super(message)
  }
}

export type User = { id: string; display_name: string }
export type MemoSummary = { id: string; title: string; version: number; updated_at: string }
export type Memo = MemoSummary & { body: string; created_at: string }
export type MemoPage = { items: MemoSummary[]; next_cursor: string | null }
export type MemoContent = { title: string; body: string }

async function request<T>(path: string, method = 'GET', body?: object): Promise<T> {
  const response = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: {
      Accept: 'application/json',
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body && JSON.stringify(body),
    cache: 'no-store',
    signal: AbortSignal.timeout(8000),
  })

  if (response.status === 204) return undefined as T
  const json = response.headers.get('content-type')?.startsWith('application/json')
    ? await response.json()
    : null
  if (!response.ok) throw new ApiError(response.status, json?.error?.message ?? '요청에 실패했습니다.')
  if (json === null) throw new Error('서비스 응답을 확인할 수 없습니다.')
  return json as T
}

export const api = {
  me: () => request<User>('/api/me'),
  providers: () => request<string[]>('/api/auth/providers'),
  logout: () => request<void>('/api/auth/logout', 'POST'),
  list: (cursor: string | null = null) => request<MemoPage>(
    cursor ? `/api/memos?cursor=${encodeURIComponent(cursor)}` : '/api/memos',
  ),
  read: (id: string) => request<Memo>(`/api/memos/${encodeURIComponent(id)}`),
  create: (content: MemoContent, requestKey: string) => request<Memo>(
    '/api/memos', 'POST', { ...content, request_key: requestKey },
  ),
  update: (id: string, content: MemoContent, version: number) => request<Memo>(
    `/api/memos/${encodeURIComponent(id)}`, 'PATCH', { ...content, version },
  ),
  delete: (id: string, version: number) => request<void>(
    `/api/memos/${encodeURIComponent(id)}`, 'DELETE', { version },
  ),
}
