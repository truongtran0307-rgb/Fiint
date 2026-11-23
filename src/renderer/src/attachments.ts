import { create } from 'zustand'

/**
 * Files waiting to go out with a chat's next message. They sit in a tray above the chat bar (thumbnails
 * for pictures, small cards for other files) and nothing is typed into Claude's prompt until you press
 * Enter — so removing one from the tray really removes it.
 */
export interface Attachment {
  key: string
  path: string
  name: string
  /** bytes; -1 unknown / folder */
  size: number
  dir: boolean
  image: boolean
  /** picture to show: data: URL (from disk) or blob: URL (pasted) */
  preview?: string
}

export const useAttachments = create<{ byChat: Record<string, Attachment[]> }>(() => ({ byChat: {} }))

const IMAGE_RE = /\.(png|jpe?g|gif|webp|bmp|svg|ico|avif)$/i
const baseName = (p: string): string => p.split(/[\\/]/).filter(Boolean).pop() ?? p
let seq = 0

export function attachmentsOf(chatId: string): Attachment[] {
  return useAttachments.getState().byChat[chatId] ?? []
}

function add(chatId: string, items: Attachment[]): void {
  useAttachments.setState((s) => {
    const cur = s.byChat[chatId] ?? []
    const have = new Set(cur.map((a) => a.path.toLowerCase()))
    const fresh = items.filter((a) => !have.has(a.path.toLowerCase()) && have.add(a.path.toLowerCase()))
    return fresh.length ? { byChat: { ...s.byChat, [chatId]: [...cur, ...fresh] } } : s
  })
}

const release = (a: Attachment): void => {
  if (a.preview?.startsWith('blob:')) URL.revokeObjectURL(a.preview)
}

export function removeAttachment(chatId: string, key: string): void {
  useAttachments.setState((s) => {
    const cur = s.byChat[chatId] ?? []
    cur.filter((a) => a.key === key).forEach(release)
    return { byChat: { ...s.byChat, [chatId]: cur.filter((a) => a.key !== key) } }
  })
}

export function clearAttachments(chatId: string): void {
  useAttachments.setState((s) => {
    if (!s.byChat[chatId]) return s
    s.byChat[chatId].forEach(release)
    const byChat = { ...s.byChat }
    delete byChat[chatId]
    return { byChat }
  })
}

/** Empty the tray and hand back what was in it (to be sent). Previews stay alive for a moment so nothing flickers. */
export function takeAttachments(chatId: string): Attachment[] {
  const items = attachmentsOf(chatId)
  if (!items.length) return items
  useAttachments.setState((s) => {
    const byChat = { ...s.byChat }
    delete byChat[chatId]
    return { byChat }
  })
  setTimeout(() => items.forEach(release), 2000)
  return items
}

/** Files already on disk (picked, dropped, copied in Explorer). */
export async function attachPaths(chatId: string, paths: string[]): Promise<void> {
  if (!paths.length) return
  const infos =
    typeof window.api.app.fileInfo === 'function'
      ? await window.api.app.fileInfo(paths).catch(() => null)
      : null
  add(
    chatId,
    paths.map((path, i) => {
      const info = infos?.[i]
      return {
        key: `a${++seq}`,
        path,
        name: baseName(path),
        size: info?.size ?? -1,
        dir: !!info?.dir,
        image: IMAGE_RE.test(path),
        preview: info?.preview
      }
    })
  )
}

/** A picture with no file behind it (clipboard): park it in temp so Claude can read it by path. */
export async function attachBlob(chatId: string, blob: Blob): Promise<void> {
  const ext = (blob.type.split('/')[1] || 'png').replace('jpeg', 'jpg')
  const path = await window.api.app.saveImage(new Uint8Array(await blob.arrayBuffer()), ext)
  add(chatId, [
    {
      key: `a${++seq}`,
      path,
      name: `Pasted image.${ext}`,
      size: blob.size,
      dir: false,
      image: true,
      preview: URL.createObjectURL(blob)
    }
  ])
}

/** Open the OS file picker for a chat. */
export async function pickAttachments(chatId: string, cwd?: string): Promise<void> {
  // (missing until the app restarts after an update)
  if (typeof window.api.app.pickFiles !== 'function') return
  const paths = await window.api.app.pickFiles(cwd).catch(() => [] as string[])
  await attachPaths(chatId, paths)
}

export function formatSize(n: number): string {
  if (n < 0) return ''
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
