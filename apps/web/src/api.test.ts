import { afterEach, expect, test, vi } from 'vitest'
import { serviceIsReady } from './api'

afterEach(() => vi.unstubAllGlobals())

test('reports ready only for the API readiness response', async () => {
  const fetchMock = vi.fn().mockResolvedValue(Response.json({ status: 'ready' }))
  vi.stubGlobal('fetch', fetchMock)

  await expect(serviceIsReady()).resolves.toBe(true)
  expect(fetchMock).toHaveBeenCalledWith('/api/ready', expect.objectContaining({ cache: 'no-store' }))
})

test('distinguishes a readiness failure from an unexpected SPA fallback', async () => {
  vi.stubGlobal('fetch', vi.fn()
    .mockResolvedValueOnce(Response.json({}, { status: 503 }))
    .mockResolvedValueOnce(new Response('<html></html>', { headers: { 'content-type': 'text/html' } })))

  await expect(serviceIsReady()).resolves.toBe(false)
  await expect(serviceIsReady()).rejects.toThrow('not JSON')
})
