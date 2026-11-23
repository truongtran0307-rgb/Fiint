import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { CalendarDays, Check, Copy, Download, ExternalLink, MapPin, MessageSquarePlus, Pencil, Plus, RotateCcw, Smartphone, Trash2, Users } from 'lucide-react'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { newChat, openFolder } from '../actions'
import { useStore } from '../store'
import './events.css'

interface Guest {
  name: string
  email: string
  submittedAt: number
}

interface EventDraft {
  title: string
  date: string
  venue: string
}

interface EventRecord extends EventDraft {
  id: string
  completedAt: number | null
  guests: Guest[]
}

const emptyDraft: EventDraft = { title: '', date: '', venue: '' }
const dateFormat = new Intl.DateTimeFormat('vi-VN', { dateStyle: 'long', timeZone: 'UTC' })
const timeFormat = new Intl.DateTimeFormat('vi-VN', { dateStyle: 'short', timeStyle: 'short' })

function dateLabel(date: string): string {
  return date ? dateFormat.format(new Date(date + 'T00:00:00Z')) : ''
}

const searchKey = (text: string): string => text.trim().toLowerCase().normalize('NFD').replace(/\p{M}/gu, '').replaceAll('đ', 'd')

export function EventsPane({ standalone = false }: { standalone?: boolean }): React.JSX.Element {
  const [events, setEvents] = useState<EventRecord[]>([])
  const [loading, setLoading] = useState(true)
  const [filter, setFilter] = useState<'active' | 'done'>('active')
  const [selected, setSelected] = useState('')
  const [creating, setCreating] = useState(false)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [eventQuery, setEventQuery] = useState('')
  const [guestQuery, setGuestQuery] = useState('')
  const [deleteId, setDeleteId] = useState<string | null>(null)
  const [draft, setDraft] = useState(emptyDraft)
  const [name, setName] = useState('')
  const [email, setEmail] = useState('')
  const [preview, setPreview] = useState<{ id: string; url: string }>()
  const [busy, setBusy] = useState(false)
  const [feedback, setFeedback] = useState({ text: '', error: false })

  useEffect(() => {
    let active = true
    let request = 0
    const refresh = async (): Promise<void> => {
      const id = ++request
      try {
        const result = await invoke<EventRecord[]>('event_list')
        if (active && id === request) setEvents(result)
      } catch (error) {
        if (active && id === request) setFeedback({ text: String(error), error: true })
      } finally {
        if (active && id === request) setLoading(false)
      }
    }
    const subscription = listen('events:changed', () => void refresh())
    void subscription.then(refresh, refresh)
    return () => {
      active = false
      void subscription.then((unlisten) => unlisten(), () => {})
    }
  }, [])

  const eventKey = searchKey(eventQuery)
  const visible = events.filter((item) => Boolean(item.completedAt) === (filter === 'done') &&
    (!eventKey || searchKey(item.title + ' ' + item.venue).includes(eventKey)))
  const event = visible.find((item) => item.id === selected) ?? visible[0]
  const guestKey = searchKey(guestQuery)
  const guests = event?.guests.filter((guest) => !guestKey || searchKey(guest.name + ' ' + guest.email).includes(guestKey)) ?? []
  const toDelete = events.find((item) => item.id === deleteId)
  const previewUrl = preview?.id === event?.id ? preview?.url : undefined

  async function act(action: () => Promise<void>): Promise<void> {
    setBusy(true)
    setFeedback({ text: '', error: false })
    try {
      await action()
    } catch (error) {
      setFeedback({ text: String(error), error: true })
    } finally {
      setBusy(false)
    }
  }

  function startForm(record?: EventRecord, duplicate = false): void {
    setEditingId(record && !duplicate ? record.id : null)
    setDraft(record ? {
      title: duplicate ? `Bản sao ${record.title}`.slice(0, 120) : record.title,
      date: duplicate ? '' : record.date,
      venue: record.venue
    } : emptyDraft)
    setCreating(true)
  }

  function saveEvent(e: FormEvent): void {
    e.preventDefault()
    void act(async () => {
      const record = editingId
        ? await invoke<EventRecord>('event_update', { id: editingId, draft })
        : await invoke<EventRecord>('event_create', { draft })
      setEvents((current) => editingId ? current.map((item) => item.id === record.id ? record : item) : [record, ...current.filter((item) => item.id !== record.id)])
      setFilter(record.completedAt ? 'done' : 'active')
      setEventQuery('')
      setGuestQuery('')
      setSelected(record.id)
      setDraft(emptyDraft)
      setCreating(false)
      setEditingId(null)
      setName('')
      setEmail('')
      if (editingId) setFeedback({ text: 'Đã lưu thông tin sự kiện.', error: false })
    })
  }

  function select(id: string): void {
    setSelected(id)
    setName('')
    setEmail('')
    setGuestQuery('')
    setFeedback({ text: '', error: false })
  }

  async function setCompleted(record: EventRecord): Promise<void> {
    const completed = !record.completedAt
    const updated = await invoke<EventRecord>('event_set_completed', { id: record.id, completed })
    setEvents((current) => current.map((item) => item.id === record.id ? updated : item))
    if (completed && preview?.id === record.id) setPreview(undefined)
    select(record.id)
    setFilter(completed ? 'done' : 'active')
    setFeedback({ text: completed ? 'Đã chuyển sự kiện sang Đã xong.' : 'Đã mở lại sự kiện.', error: false })
  }

  function deleteEvent(record: EventRecord): void {
    setDeleteId(null)
    void act(async () => {
      await invoke('event_delete', { id: record.id })
      setEvents((current) => current.filter((item) => item.id !== record.id))
      if (preview?.id === record.id) setPreview(undefined)
      select('')
      setFeedback({ text: 'Đã xóa sự kiện.', error: false })
    })
  }

  function register(e: FormEvent): void {
    e.preventDefault()
    if (!event) return
    void act(async () => {
      await invoke('event_register', { id: event.id, registration: { name, email } })
      setName('')
      setEmail('')
      setFeedback({ text: 'Đã ghi đăng ký thử vào danh sách khách.', error: false })
    })
  }

  async function openBrief(record: EventRecord): Promise<void> {
    if (!useStore.getState().claude?.found) throw new Error('Chưa tìm thấy Claude Code trên máy này.')
    const cwd = await window.api.app.pickFolder()
    if (!cwd) return
    if (useStore.getState().root !== cwd) await openFolder(cwd, { autoChat: false })
    newChat({
      cwd,
      agent: 'claude',
      title: `Trang đăng ký · ${record.title}`.slice(0, 60),
      pinnedTitle: true,
      draft: `Tạo hoặc cập nhật trang đăng ký cho sự kiện ${JSON.stringify(record.title)}. Ngày: ${dateLabel(record.date) || 'chưa chốt'}. Địa điểm: ${record.venue || 'chưa chốt'}. Làm giao diện tiếng Việt dễ dùng trên điện thoại, có họ tên, email và xác nhận sau khi gửi. Cho tôi cách xem bản thử và kiểm tra một đăng ký bằng dữ liệu giả. Hãy làm trong thư mục đã chọn và hỏi tôi trước khi xuất bản.`,
    })
  }

  return (
    <section className="ev-workspace" aria-label="Sự kiện">
      <div className="ev-shell">
      <header className="ev-top">
        <div className="ev-title"><span className="ev-title-icon"><CalendarDays size={24} /></span><h1>Sự kiện</h1></div>
        <div className="ev-actions">
          {standalone && <button type="button" onClick={() => useStore.setState({ centerView: 'code' })}>Về trang đầu</button>}
          <button type="button" className="ev-primary" disabled={busy} onClick={() => startForm()}><Plus size={16} /> Tạo sự kiện</button>
        </div>
      </header>

      {feedback.text && <p className="ev-feedback" role={feedback.error ? 'alert' : 'status'}>{feedback.text}</p>}
      {loading && <p role="status">Đang tải sự kiện…</p>}

      {creating && (
        <form className="ev-card ev-form" onSubmit={saveEvent}>
          <h2>{editingId ? 'Sửa sự kiện' : 'Sự kiện mới'}</h2>
          <label>Tên sự kiện<input required maxLength={120} value={draft.title} onChange={(e) => setDraft({ ...draft, title: e.target.value })} /></label>
          <label>Ngày tổ chức<input type="date" value={draft.date} onChange={(e) => setDraft({ ...draft, date: e.target.value })} /></label>
          <label>Địa điểm<input maxLength={180} value={draft.venue} onChange={(e) => setDraft({ ...draft, venue: e.target.value })} /></label>
          <div className="ev-actions">
            <button className="ev-primary" disabled={busy}>Lưu sự kiện</button>
            <button type="button" disabled={busy} onClick={() => setCreating(false)}>Hủy</button>
          </div>
        </form>
      )}

      {!creating && !loading && (
        <div className="ev-board">
          <aside className="ev-list" aria-label="Danh sách sự kiện">
            <div className="ev-filters" aria-label="Lọc theo trạng thái">
              <button type="button" aria-pressed={filter === 'active'} disabled={busy} onClick={() => { setFilter('active'); select('') }}>Đang làm</button>
              <button type="button" aria-pressed={filter === 'done'} disabled={busy} onClick={() => { setFilter('done'); select('') }}>Đã xong</button>
            </div>
            {events.length > 0 && <input type="search" aria-label="Tìm sự kiện" placeholder="Tìm sự kiện…" value={eventQuery}
              onChange={(e) => { setEventQuery(e.target.value); select('') }} />}
            {visible.map((item) => (
              <button type="button" className="ev-list-item" disabled={busy} aria-current={event?.id === item.id ? 'true' : undefined} key={item.id} onClick={() => select(item.id)}>
                <span><strong>{item.title}</strong><small>{dateLabel(item.date) || 'Chưa chốt ngày'}</small></span>
                <span className="ev-list-count"><Users size={13} />{item.guests.length}</span>
              </button>
            ))}
          </aside>
          {event ? (
          <div className="ev-detail">
          <header className="ev-event-heading">
            <h2>{event.title}</h2>
            <p><CalendarDays size={14} /> {dateLabel(event.date) || 'Chưa chốt ngày'}{event.venue && <><MapPin size={14} /> {event.venue}</>}</p>
            <div className="ev-actions">
              <button type="button" disabled={busy} onClick={() => startForm(event)}><Pencil size={15} /> Sửa</button>
              <button type="button" disabled={busy} onClick={() => startForm(event, true)}><Copy size={15} /> Nhân bản</button>
              <button type="button" disabled={busy} onClick={() => void act(() => setCompleted(event))}>
                {event.completedAt ? <><RotateCcw size={15} /> Mở lại</> : <><Check size={15} /> Đánh dấu đã xong</>}
              </button>
              <button type="button" className="ev-danger" disabled={busy} onClick={() => setDeleteId(event.id)}><Trash2 size={15} /> Xóa sự kiện</button>
              {!event.completedAt && <button type="button" disabled={busy} onClick={() => void act(() => openBrief(event))}>
                <MessageSquarePlus size={16} /> Nhờ AI dựng trang
              </button>}
            </div>
          </header>
          {event.completedAt && <div className="ev-completed"><Check size={16} /> Đã xong · Đã đóng đăng ký</div>}
          <div className="ev-grid">
            {!event.completedAt && (
            <article className="ev-card">
              <header className="ev-card-heading">
                <Smartphone size={19} />
                <div>
                  <h2>Đăng ký thử</h2>
                  <p>Dùng tên và email giả để xem đầy đủ luồng đăng ký.</p>
                </div>
              </header>
              <form className="ev-form" onSubmit={register}>
                <label>Họ tên<input required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} /></label>
                <label>Email<input required type="email" maxLength={160} value={email} onChange={(e) => setEmail(e.target.value)} /></label>
                <button className="ev-primary" disabled={busy}>Gửi đăng ký thử</button>
              </form>
              <div className="ev-phone">
                {previewUrl ? (
                  <>
                    <label>Link thử cho điện thoại<input readOnly value={previewUrl} onFocus={(e) => e.target.select()} /></label>
                    <p>Mở link trên điện thoại cùng Wi-Fi khi VIBE CODE đang chạy.</p>
                    <div className="ev-actions">
                      <button type="button" disabled={busy} onClick={() => void act(() => window.api.app.openExternal(previewUrl))}>
                        <ExternalLink size={15} /> Mở trên máy này
                      </button>
                      <button type="button" disabled={busy} onClick={() => void act(async () => {
                        await invoke('event_preview_stop')
                        setPreview(undefined)
                      })}>Tắt link thử</button>
                    </div>
                  </>
                ) : (
                  <button type="button" disabled={busy} onClick={() => void act(async () => {
                    const url = await invoke<string>('event_preview_start', { id: event.id })
                    setPreview({ id: event.id, url })
                  })}><Smartphone size={15} /> Bật link thử cùng Wi-Fi</button>
                )}
                {preview && !previewUrl && <button type="button" disabled={busy} onClick={() => void act(async () => {
                  await invoke('event_preview_stop')
                  setPreview(undefined)
                })}>Tắt link thử của sự kiện khác</button>}
              </div>
            </article>
            )}
            <article className="ev-card">
              <header className="ev-card-heading ev-guest-heading">
                <h2>Danh sách khách <span className="ev-count">{event.guests.length}</span></h2>
                <button type="button" title="Xuất toàn bộ khách của sự kiện" disabled={busy || !event.guests.length} onClick={() => void act(async () => {
                  const path = await invoke<string | null>('event_export', { id: event.id })
                  if (path) setFeedback({ text: `Đã lưu CSV tại ${path}`, error: false })
                })}><Download size={15} /> Xuất CSV</button>
              </header>
              {event.guests.length > 0 && <input className="ev-guest-search" type="search" aria-label="Tìm khách" placeholder="Tìm theo tên hoặc email…" value={guestQuery} onChange={(e) => setGuestQuery(e.target.value)} />}
              {event.guests.length === 0 ? <p>Khách đăng ký thử sẽ xuất hiện ở đây.</p> : !guests.length ? <p>Không tìm thấy khách phù hợp.</p> : (
                <ul className="ev-guests">
                  {guests.map((guest) => (
                    <li key={guest.email}>
                      <strong>{guest.name}</strong>
                      <span>{guest.email}</span>
                      <time dateTime={new Date(guest.submittedAt).toISOString()}>{timeFormat.format(guest.submittedAt)}</time>
                    </li>
                  ))}
                </ul>
              )}
            </article>
          </div>
          </div>
          ) : (
            <div className="ev-empty">
              <CalendarDays size={30} />
              <h2>{eventKey ? 'Không tìm thấy sự kiện phù hợp' : filter === 'done' ? 'Chưa có sự kiện đã xong' : 'Chưa có sự kiện đang làm'}</h2>
              {filter === 'active' && !eventKey && <button type="button" className="ev-primary" onClick={() => startForm()}><Plus size={16} /> Tạo sự kiện</button>}
            </div>
          )}
        </div>
      )}
      {toDelete && <DeleteEventDialog record={toDelete} onCancel={() => setDeleteId(null)} onDelete={() => deleteEvent(toDelete)} />}
      </div>
    </section>
  )
}

function DeleteEventDialog({ record, onCancel, onDelete }: { record: EventRecord; onCancel: () => void; onDelete: () => void }): React.JSX.Element {
  const ref = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    const dialog = ref.current
    dialog?.showModal()
    return () => dialog?.close()
  }, [])
  return (
    <dialog ref={ref} className="ev-delete-dialog" aria-labelledby="ev-delete-title" aria-describedby="ev-delete-message"
      onCancel={(e) => { e.preventDefault(); onCancel() }}>
      <h2 id="ev-delete-title">Xóa sự kiện này?</h2>
      <p id="ev-delete-message">“{record.title}” và {record.guests.length} đăng ký đã lưu sẽ bị xóa vĩnh viễn.</p>
      <div className="ev-actions">
        <button type="button" autoFocus onClick={onCancel}>Giữ lại</button>
        <button type="button" className="ev-danger-solid" onClick={onDelete}><Trash2 size={15} /> Xóa vĩnh viễn</button>
      </div>
    </dialog>
  )
}
