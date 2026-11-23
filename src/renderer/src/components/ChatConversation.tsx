import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { ArrowDown, CalendarDays, Check, Copy, Mail, Sparkles } from 'lucide-react'
import type { ChatMessage } from '../../../shared/types'
import type { Chat } from '../store'
import { insertTemplate } from '../templates'

function useConversation(sessionId: string | undefined, working: boolean, sentAt: number) {
  const [state, setState] = useState<{ rows: ChatMessage[]; loading: boolean; failed: boolean }>({
    rows: [], loading: Boolean(sessionId), failed: false
  })
  const traffic = useRef({ working, sentAt })
  const wake = useRef<() => void>(() => {})
  useEffect(() => {
    if (!sessionId) return
    let active = true
    let revision: number | undefined
    let reading = false
    let queued = false
    let event: ReturnType<typeof setTimeout> | undefined
    let poll: ReturnType<typeof setTimeout> | undefined

    const read = async (): Promise<void> => {
      if (!active || document.hidden) return
      if (reading) { queued = true; return }
      reading = true
      clearTimeout(poll)
      try {
        const update = await window.api.sessions.messages(sessionId, revision)
        if (active) {
          revision = update.revision
          setState((old) => update.rows === null && !old.loading && !old.failed ? old : {
            rows: update.rows ?? old.rows, loading: false, failed: false
          })
        }
      } catch {
        if (active) setState((old) => old.failed ? old : { ...old, loading: false, failed: true })
      } finally {
        reading = false
        if (active && queued) {
          queued = false
          void read()
        } else if (active && !document.hidden) {
          const fast = traffic.current.working || Date.now() - traffic.current.sentAt < 8000
          poll = setTimeout(() => void read(), fast ? 500 : 10000)
        }
      }
    }

    const off = window.api.sessions.onChanged((ids) => {
      if (event || (!ids.includes(sessionId) && !ids.includes('*'))) return
      event = setTimeout(() => { event = undefined; void read() }, 30)
    })
    const onVisibility = (): void => {
      if (document.hidden) clearTimeout(poll)
      else void read()
    }
    wake.current = () => void read()
    document.addEventListener('visibilitychange', onVisibility)
    void read()
    return () => {
      active = false
      wake.current = () => {}
      off()
      clearTimeout(event)
      clearTimeout(poll)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [sessionId])
  useEffect(() => {
    if (traffic.current.working === working && traffic.current.sentAt === sentAt) return
    traffic.current = { working, sentAt }
    wake.current()
  }, [working, sentAt])
  return state
}

function inline(text: string): ReactNode[] {
  return text.split(/(\*\*[^*\n]+\*\*|`[^`\n]+`)/g).map((part, index) => {
    if (part.startsWith('**') && part.endsWith('**') && part.length > 4) return <strong key={index}>{part.slice(2, -2)}</strong>
    if (part.startsWith('`') && part.endsWith('`') && part.length > 2) return <code key={index}>{part.slice(1, -1)}</code>
    return part
  })
}

function ReplyText({ text }: { text: string }): React.JSX.Element {
  return <div className="conversation-reply">
    {text.split(/^```[^\n]*$/m).map((chunk, index) => {
      const content = chunk.trim()
      if (!content) return null
      if (index % 2) return <details className="conversation-code" key={index}><summary>Xem đoạn mã</summary><pre>{content}</pre></details>
      return <div key={index}>{content.split(/(\n{2,})/).filter((part) => part.trim()).map((part, paragraph) => {
        if (/^#{1,4}\s+[^\n]+$/.test(part)) return <h4 key={paragraph}>{inline(part.replace(/^#{1,4}\s+/, ''))}</h4>
        return <p key={paragraph}>{inline(part)}</p>
      })}</div>
    })}
  </div>
}

function Message({ message }: { message: ChatMessage }): React.JSX.Element {
  const [copied, setCopied] = useState(false)
  const [copyError, setCopyError] = useState(false)
  const mine = message.role === 'user'
  return (
    <article className={mine ? 'conversation-message mine' : 'conversation-message'}>
      <div className="conversation-byline">{mine ? 'Bạn' : <><Sparkles size={13} /> Claude</>}</div>
      {mine ? <p>{message.text}</p> : <ReplyText text={message.text} />}
      {!mine && <button type="button" className="conversation-copy" aria-label={copied ? 'Đã sao chép câu trả lời' : 'Sao chép câu trả lời'} onClick={() => {
        void navigator.clipboard.writeText(message.text).then(() => { setCopied(true); setCopyError(false) }, () => setCopyError(true))
      }}>{copied ? <Check size={13} /> : <Copy size={13} />} {copied ? 'Đã sao chép' : 'Sao chép'}</button>}
      {copyError && <span role="status">Chưa sao chép được câu trả lời.</span>}
    </article>
  )
}

const normalized = (value: string): string => value.trim().replace(/\s+/g, ' ')

export function ChatConversation({ chat, onOpenConsole }: { chat: Chat; onOpenConsole: () => void }): React.JSX.Element {
  const busy = chat.status === 'working' || chat.status === 'starting'
  const { rows, loading, failed } = useConversation(chat.sessionId, busy, chat.sentDraft?.at ?? 0)
  const scroll = useRef<HTMLDivElement>(null)
  const following = useRef(true)
  const [showLatest, setShowLatest] = useState(false)
  const pending = chat.sentDraft
  const pendingText = pending && !rows.some((row) => row.role === 'user' && (row.at === 0 || row.at >= pending.at - 1500) &&
    (normalized(row.text) === normalized(pending.text) || normalized(row.text).startsWith(normalized(pending.text) + ' ')))
  const hasMessages = rows.length > 0 || pendingText
  useLayoutEffect(() => {
    const element = scroll.current
    if (element && following.current) element.scrollTop = element.scrollHeight
  }, [rows, pendingText, busy, chat.needsInput])

  return (
    <div className="conversation-surface">
    <div className="conversation-view" ref={scroll} onScroll={(e) => {
      const element = e.currentTarget
      following.current = element.scrollHeight - element.scrollTop - element.clientHeight < 70
      setShowLatest(!following.current)
    }}>
      {!hasMessages && !loading && !failed && !chat.needsInput && (
        <div className="conversation-welcome">
          <span className="conversation-welcome-icon"><Sparkles size={23} /></span>
          <h2>Hôm nay mình làm gì?</h2>
          <p>Giao việc cho Claude hoặc bắt đầu từ một mẫu.</p>
          <div className="conversation-suggestions">
            <button type="button" disabled={chat.status === 'exited'} onClick={() => insertTemplate('Lập kế hoạch chuẩn bị cho sự kiện [tên sự kiện], kèm người phụ trách và hạn chót.')}><CalendarDays size={15} /> Lập kế hoạch sự kiện</button>
            <button type="button" disabled={chat.status === 'exited'} onClick={() => insertTemplate('Soạn thư mời tham dự [tên sự kiện] với thông tin [ngày, giờ, địa điểm].')}><Mail size={15} /> Soạn thư mời</button>
          </div>
        </div>
      )}
      <div className="conversation-list" role="log" aria-label="Tin nhắn" aria-live="polite">
        {rows.map((message) => <Message key={message.id} message={message} />)}
        {pending && pendingText && <Message key="pending" message={{ id: 'pending', role: 'user', text: pending.text, at: pending.at }} />}
        {busy && <p className="conversation-status"><span />{chat.status === 'starting' ? 'Đang mở cuộc trò chuyện…' : 'Claude đang thực hiện yêu cầu…'}</p>}
      </div>
      {chat.needsInput && (
        <div className="conversation-notice" role="status">
          <span>Claude cần bạn xác nhận để tiếp tục.</span>
          <button type="button" onClick={onOpenConsole}>Xem yêu cầu</button>
        </div>
      )}
      {failed && (
        <div className="conversation-notice" role="status">
          <span>Chưa tải được lịch sử hội thoại.</span>
          <button type="button" onClick={onOpenConsole}>Mở chế độ gốc</button>
        </div>
      )}
    </div>
    {showLatest && <button type="button" className="conversation-latest" onClick={() => {
      if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight
      following.current = true
      setShowLatest(false)
    }}><ArrowDown size={14} /> Mới nhất</button>}
    </div>
  )
}
