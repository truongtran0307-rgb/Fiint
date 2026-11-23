import { useEffect, useState } from 'react'
import { FileText } from 'lucide-react'
import type { ChatFile } from '../../../shared/types'
import { useStore } from '../store'
import { openFile } from '../actions'
import { FileIcon } from './icons'

/** The files Claude created, edited or read in a chat; reloaded as the transcript grows. */
export function useChatFiles(sessionId: string | undefined): ChatFile[] {
  const [files, setFiles] = useState<ChatFile[]>([])
  useEffect(() => {
    setFiles([])
    if (!sessionId || typeof window.api.sessions.files !== 'function') return
    let alive = true
    let timer: ReturnType<typeof setTimeout> | undefined
    const load = (): void => {
      window.api.sessions
        .files(sessionId)
        .then((list) => {
          if (alive) setFiles((prev) => (JSON.stringify(prev) === JSON.stringify(list) ? prev : list))
        })
        .catch(() => {})
    }
    load()
    const off = window.api.sessions.onChanged((ids) => {
      if (!ids.includes(sessionId) && !ids.includes('*')) return
      clearTimeout(timer)
      timer = setTimeout(load, 700)
    })
    return () => {
      alive = false
      clearTimeout(timer)
      off()
    }
  }, [sessionId])
  return files
}

const norm = (p: string): string => p.replace(/\//g, '\\').toLowerCase()

function ago(at: number): string {
  const s = Math.max(0, Math.round((Date.now() - at) / 1000))
  if (s < 60) return 'now'
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return `${Math.floor(s / 86400)}d`
}

/** The Files tab of a chat: what Claude changed, then what it only read. Click one to open it. */
export function ChatFiles({ files, cwd }: { files: ChatFile[]; cwd: string }): React.JSX.Element {
  const activeTabId = useStore((s) => s.activeTabId)
  const changed = files.filter((f) => f.action !== 'read')
  const read = files.filter((f) => f.action === 'read')
  if (!files.length)
    return (
      <div className="chat-files">
        <div className="chat-images-empty">
          <FileText size={22} />
          <p>Chưa có tệp nào. Tệp Claude tạo, sửa hoặc đọc trong chat này sẽ hiện ở đây.</p>
        </div>
      </div>
    )

  const root = norm(cwd).replace(/\\$/, '') + '\\'
  const row = (f: ChatFile): React.JSX.Element => {
    const sep = Math.max(f.path.lastIndexOf('\\'), f.path.lastIndexOf('/'))
    const name = f.path.slice(sep + 1)
    const dir = f.path.slice(0, Math.max(0, sep))
    // inside the chat's folder: show the folder relative to it
    const shownDir = norm(dir + '\\').startsWith(root) ? dir.slice(root.length) : dir
    const badge = f.action === 'created' ? 'A' : f.action === 'edited' ? 'M' : ''
    const what = f.action === 'created' ? 'Đã tạo' : f.action === 'edited' ? `Đã sửa ${f.edits}×` : 'Đã đọc'
    return (
      <div
        key={f.path}
        className={`change-row${activeTabId === 'f:' + f.path.toLowerCase() ? ' active' : ''}${f.exists ? '' : ' gone'}`}
        title={`${what} — ${f.path}${f.exists ? '' : ' (không còn trên máy)'}`}
        onClick={() => f.exists && openFile(f.path)}
      >
        <FileIcon name={name} />
        <span className={`change-name${f.exists ? '' : ' deleted'}`}>{name}</span>
        <span className="change-dir">{shownDir}</span>
        <span className="change-stats">
          {f.edits > 1 && <span className="file-edits">{f.edits}×</span>}
          <span className="file-ago">{ago(f.at)}</span>
        </span>
        {badge && <span className={`change-badge git-${badge}`}>{badge}</span>}
      </div>
    )
  }

  return (
    <div className="chat-files">
      {changed.length > 0 && <div className="chat-files-head">Đã thay đổi · {changed.length}</div>}
      {changed.map(row)}
      {read.length > 0 && <div className="chat-files-head">Đã đọc · {read.length}</div>}
      {read.map(row)}
    </div>
  )
}
