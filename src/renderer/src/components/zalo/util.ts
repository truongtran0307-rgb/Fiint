/* ---------------- colors: Telegram's seven peer colors ---------------- */

const PEER = ['#e17076', '#f0a35e', '#a695e7', '#7bc862', '#5fbfc2', '#65aadd', '#ee7aae']
const PEER_DARK = ['#c94f5a', '#e0833a', '#8774d9', '#56ad42', '#3ea4a8', '#4590c9', '#d95c97']

function hash(s: string): number {
  let h = 0
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0
  return Math.abs(h)
}
export const peerGradient = (id: string): string => {
  const i = hash(id) % PEER.length
  return `linear-gradient(180deg, ${PEER[i]} 0%, ${PEER_DARK[i]} 100%)`
}

export function initials(name: string): string {
  const words = name
    .replace(/[^\p{L}\p{N}\s]/gu, ' ')
    .trim()
    .split(/\s+/)
    .filter(Boolean)
  if (!words.length) return '?'
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase()
  return (words[0][0] + words[words.length - 1][0]).toUpperCase()
}

/* ---------------- time ---------------- */

const sameDay = (a: Date, b: Date): boolean => a.toDateString() === b.toDateString()

const hhmm = (ts: number): string => new Date(ts).toLocaleTimeString('vi-VN', { hour: '2-digit', minute: '2-digit', hour12: false })

/** list time: 14:05 today, weekday this week, dd/mm this year, dd/mm/yy before */
export function listTime(ts: number): string {
  if (!ts) return ''
  const d = new Date(ts)
  const now = new Date()
  if (sameDay(d, now)) return hhmm(ts)
  const days = (now.getTime() - d.getTime()) / 86_400_000
  if (days < 6) return d.toLocaleDateString('vi-VN', { weekday: 'short' })
  if (d.getFullYear() === now.getFullYear()) return d.toLocaleDateString('vi-VN', { day: '2-digit', month: '2-digit' })
  return d.toLocaleDateString('vi-VN', { day: '2-digit', month: '2-digit', year: '2-digit' })
}
