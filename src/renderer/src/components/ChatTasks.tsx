import { useEffect, useState } from 'react'
import { ChevronRight, CircleCheck, CircleX, Loader } from 'lucide-react'
import type { ChatTask } from '../../../shared/types'

/** A chat's background tasks, reloaded as the transcript changes and every few seconds while one runs. */
export function useChatTasks(sessionId: string | undefined): ChatTask[] {
  const [tasks, setTasks] = useState<ChatTask[]>([])
  useEffect(() => {
    setTasks([])
    if (!sessionId || typeof window.api.sessions.tasks !== 'function') return
    let alive = true
    let timer: ReturnType<typeof setTimeout> | undefined
    let poll: ReturnType<typeof setInterval> | undefined
    const load = (): void => {
      window.api.sessions
        .tasks(sessionId)
        .then((list) => {
          if (!alive) return
          // same list (a poll while nothing changed): no re-render
          setTasks((prev) => (JSON.stringify(prev) === JSON.stringify(list) ? prev : list))
          // running ones write output without touching the transcript: keep looking
          clearInterval(poll)
          if (list.some((t) => t.status === 'running')) poll = setInterval(load, 3000)
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
      clearInterval(poll)
      off()
    }
  }, [sessionId])
  return tasks
}

function dur(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

const took = (t: ChatTask): string => (t.endedAt ? dur(t.endedAt - t.startedAt) : `running ${dur(Date.now() - t.startedAt)}`)

export function ChatTasks({ tasks }: { tasks: ChatTask[] }): React.JSX.Element {
  const [open, setOpen] = useState<string | null>(null)
  // "running 4s" is computed at render: tick while something runs, or it stays frozen
  const [, tick] = useState(0)
  const anyRunning = tasks.some((t) => t.status === 'running')
  useEffect(() => {
    if (!anyRunning) return
    const i = setInterval(() => tick((n) => n + 1), 1000)
    return () => clearInterval(i)
  }, [anyRunning])
  if (!tasks.length) return <div className="chat-tasks"><div className="chat-images-empty">No background tasks in this chat yet. When Claude runs something in the background (a build, a dev server…), it shows up here.</div></div>
  return (
    <div className="chat-tasks">
      {tasks.map((t) => {
        const ok = t.status === 'completed'
        const running = t.status === 'running'
        const expanded = open === t.id || (open === null && running)
        return (
          <div key={t.id} className={`task-card s-${running ? 'running' : ok ? 'ok' : 'fail'}`}>
            <button className="task-head" onClick={() => setOpen(expanded ? '' : t.id)}>
              <ChevronRight size={13} className={expanded ? 'task-chev open' : 'task-chev'} />
              {running ? <Loader size={14} className="task-spin" /> : ok ? <CircleCheck size={14} /> : <CircleX size={14} />}
              <span className="task-title">{t.description || t.command}</span>
              <span className="task-time">{took(t)}</span>
            </button>
            {expanded && (
              <div className="task-body">
                {t.description && <code className="task-cmd">{t.command}</code>}
                <pre className="task-out">{t.output?.trim() || (running ? 'No output yet…' : 'No output.')}</pre>
                <div className="task-meta">
                  {t.id} · {t.status}
                </div>
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}
