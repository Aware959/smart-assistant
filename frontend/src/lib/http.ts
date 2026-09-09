import axios from 'axios'

const http = axios.create({
  baseURL: '/',
  timeout: 30_000,
  headers: { 'Content-Type': 'application/json' },
})

http.interceptors.response.use(
  (res) => res,
  (error) => {
    const status = error?.response?.status as number | undefined
    const message =
      error?.response?.data?.message ??
      error?.response?.data?.error ??
      error?.message ??
      '网络请求失败'
    const err = new Error(message) as Error & { status?: number }
    err.status = status
    return Promise.reject(err)
  },
)

export function toErrorMessage(err: unknown): string {
  if (err instanceof Error) return err.message
  return String(err)
}

export { http }