export async function serviceIsReady(signal?: AbortSignal): Promise<boolean> {
  const response = await fetch('/api/ready', {
    headers: { Accept: 'application/json' },
    cache: 'no-store',
    signal,
  })

  if (response.status === 503) return false
  if (!response.ok) throw new Error('Service status request failed')
  if (!response.headers.get('content-type')?.startsWith('application/json')) {
    throw new Error('Service status response was not JSON')
  }

  const body: unknown = await response.json()
  return typeof body === 'object' && body !== null && 'status' in body && body.status === 'ready'
}
