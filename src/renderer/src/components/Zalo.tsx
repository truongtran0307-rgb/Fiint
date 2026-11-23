import { useEffect, useState } from 'react'
import { Check, Loader2, LogOut, QrCode, RefreshCw, TriangleAlert, Wifi, WifiOff } from 'lucide-react'
import type { ZaloState } from '../../../shared/types'
import { ZaloTasks } from './zalo/ZaloTasks'
import { Avatar } from './zalo/Avatar'
import './zalo/zalo.css'

/** Zalo tab: QR login, then just the connection status and Claude's task list (no messenger, no Windows notifications). */
export function ZaloPane(): React.JSX.Element {
  const [state, setState] = useState<ZaloState>({ status: 'off' })
  const [loaded, setLoaded] = useState(false)
  useEffect(() => {
    void window.api.zalo.state().then((s) => {
      setState(s)
      setLoaded(true)
    })
    return window.api.zalo.onState(setState)
  }, [])
  if (!loaded) return <div className="zl-login" />
  if (state.status === 'off' || state.status === 'qr' || (!state.self && state.status !== 'connecting')) return <ZaloLogin state={state} />
  return <ZaloConnected state={state} />
}

/* ================================ login ================================ */

function ZaloLogin({ state }: { state: ZaloState }): React.JSX.Element {
  const [busy, setBusy] = useState(false)
  const start = (): void => {
    setBusy(true)
    void window.api.zalo.login().finally(() => setBusy(false))
  }
  const waitingQr = state.status === 'qr' && !state.qr && !state.error
  return (
    <div className="zl-login">
      <div className="zl-login-card">
        {state.status === 'qr' && state.qr ? (
          <>
            <div className={'zl-qr' + (state.scanned ? ' scanned' : '')}>
              <img src={state.qr} alt="Mã QR đăng nhập Zalo" />
              {state.scanned && (
                <div className="zl-qr-done">
                  <Check size={34} />
                </div>
              )}
            </div>
            <h2>{state.scanned ? `Xin chào, ${state.scanned}` : 'Đăng nhập Zalo bằng mã QR'}</h2>
            {state.scanned ? (
              <p className="zl-login-sub">Bấm “Đăng nhập” trên điện thoại để hoàn tất.</p>
            ) : (
              <ol className="zl-steps">
                <li>Mở Zalo trên điện thoại</li>
                <li>
                  Bấm biểu tượng <QrCode size={13} /> cạnh ô tìm kiếm
                </li>
                <li>Hướng camera vào mã này để quét</li>
              </ol>
            )}
            <button className="zl-link-btn" onClick={() => void window.api.zalo.cancel()}>
              Huỷ
            </button>
          </>
        ) : (
          <>
            <div className="zl-logo">
              <span>Zalo</span>
            </div>
            <h2>Zalo trong VIBE CODE</h2>
            <p className="zl-login-sub">Kết nối tài khoản Zalo cá nhân để Claude đọc tin nhắn qua tool zalo_* và tóm tắt việc cần làm.</p>
            {(waitingQr || state.status === 'connecting') && (
              <p className="zl-login-sub">
                <Loader2 size={14} className="zl-spin" /> {state.status === 'connecting' ? 'Đang kết nối lại phiên đã lưu…' : 'Đang lấy mã QR…'}
              </p>
            )}
            {state.error && <p className="zl-login-error">{state.error}</p>}
            {!waitingQr && state.status !== 'connecting' && (
              <div className="zl-login-actions">
                <button className="zl-primary" disabled={busy} onClick={start}>
                  <QrCode size={16} /> Đăng nhập bằng QR
                </button>
                {state.status === 'error' && (
                  <button className="zl-secondary" onClick={() => void window.api.zalo.reconnect()}>
                    <RefreshCw size={14} /> Kết nối lại
                  </button>
                )}
              </div>
            )}
          </>
        )}
        <p className="zl-login-warn">
          <TriangleAlert size={13} /> Dùng API không chính thức (zca-js), giống cách Zalo Web hoạt động; Zalo có thể khoá tài khoản vì việc này. Mỗi tài khoản chỉ giữ một
          phiên web: mở chat.zalo.me ở nơi khác thì phiên ở đây bị ngắt.
        </p>
      </div>
    </div>
  )
}

/* ================================ connected ================================ */

/** Logged in: no messenger here (chats stay on the phone) — just the connection, plus Claude's "Việc của tôi" list. */
function ZaloConnected({ state }: { state: ZaloState }): React.JSX.Element {
  const [syncing, setSyncing] = useState(false)
  const offline = state.status !== 'online'
  const me = state.self
  return (
    <div className="zl-home">
      <div className="zl-conn-card">
        <Avatar id={me?.id ?? 'me'} name={me?.name ?? 'Zalo'} src={me?.avatar} size={46} />
        <div className="zl-conn-info">
          <div className="zl-conn-name">{me?.name || 'Zalo'}</div>
          <div className={'zl-conn-status ' + (state.status === 'online' ? 'ok' : state.status === 'connecting' ? 'wait' : 'err')}>
            {state.status === 'connecting' ? <Loader2 size={12} className="zl-spin" /> : state.status === 'online' ? <Wifi size={12} /> : <WifiOff size={12} />}
            {state.status === 'online' ? 'Đã kết nối — Claude đọc được tin nhắn qua tool zalo_*' : state.status === 'connecting' ? 'Đang kết nối…' : state.error || 'Mất kết nối'}
          </div>
        </div>
        <button
          className="zl-icon"
          title="Đồng bộ tất cả cuộc trò chuyện"
          disabled={offline || syncing}
          onClick={() => {
            setSyncing(true)
            void window.api.zalo.syncAll().finally(() => setSyncing(false))
          }}
        >
          <RefreshCw size={17} className={syncing ? 'zl-spin' : undefined} />
        </button>
        {state.status === 'error' && (
          <button className="zl-icon" title="Đăng nhập lại bằng QR" onClick={() => void window.api.zalo.login()}>
            <QrCode size={17} />
          </button>
        )}
        <button
          className="zl-icon danger"
          title="Đăng xuất"
          onClick={() => {
            if (window.confirm('Đăng xuất Zalo và xoá toàn bộ tin nhắn đã đồng bộ trên máy này?')) void window.api.zalo.logout()
          }}
        >
          <LogOut size={17} />
        </button>
      </div>
      <ZaloTasks />
    </div>
  )
}
