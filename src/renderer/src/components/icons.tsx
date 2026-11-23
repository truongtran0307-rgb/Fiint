import {
  Braces,
  Database,
  File,
  FileCode2,
  FileImage,
  FileJson,
  FileText,
  FileTerminal,
  Folder,
  FolderOpen,
  Lock,
  Package,
  Palette,
  Settings2,
  type LucideIcon
} from 'lucide-react'
import type { ChatStatus } from '../store'

const byExt: Record<string, [LucideIcon, string]> = {
  ts: [FileCode2, '#4f9cf9'],
  tsx: [FileCode2, '#5fd4e0'],
  js: [FileCode2, '#f5d76b'],
  jsx: [FileCode2, '#5fd4e0'],
  mjs: [FileCode2, '#f5d76b'],
  cjs: [FileCode2, '#f5d76b'],
  json: [FileJson, '#f5b46b'],
  jsonc: [FileJson, '#f5b46b'],
  md: [FileText, '#8dbcff'],
  mdx: [FileText, '#8dbcff'],
  txt: [FileText, '#9a9ab0'],
  css: [Palette, '#c49bff'],
  scss: [Palette, '#ff7eb6'],
  less: [Palette, '#8dbcff'],
  html: [FileCode2, '#ff8f6b'],
  vue: [FileCode2, '#5fd38d'],
  svelte: [FileCode2, '#ff7a59'],
  py: [FileCode2, '#6aa7ff'],
  rs: [FileCode2, '#ff9e6b'],
  go: [FileCode2, '#5fd4e0'],
  java: [FileCode2, '#ff8a6b'],
  cs: [FileCode2, '#a37bff'],
  cpp: [FileCode2, '#6aa7ff'],
  c: [FileCode2, '#6aa7ff'],
  h: [FileCode2, '#c49bff'],
  php: [FileCode2, '#9d9dff'],
  rb: [FileCode2, '#ff6b6b'],
  sh: [FileTerminal, '#5fd38d'],
  ps1: [FileTerminal, '#6aa7ff'],
  bat: [FileTerminal, '#5fd38d'],
  sql: [Database, '#f5c76b'],
  yml: [Settings2, '#ff8a8a'],
  yaml: [Settings2, '#ff8a8a'],
  toml: [Settings2, '#9a9ab0'],
  xml: [Braces, '#f5b46b'],
  png: [FileImage, '#c49bff'],
  jpg: [FileImage, '#c49bff'],
  jpeg: [FileImage, '#c49bff'],
  gif: [FileImage, '#c49bff'],
  svg: [FileImage, '#f5b46b'],
  webp: [FileImage, '#c49bff'],
  ico: [FileImage, '#c49bff'],
  lock: [Lock, '#7a7a90'],
  env: [Settings2, '#f5d76b']
}

export function FileIcon({ name, size = 15 }: { name: string; size?: number }): React.JSX.Element {
  const lower = name.toLowerCase()
  let entry: [LucideIcon, string] | undefined
  if (lower === 'package.json') entry = [Package, '#5fd38d']
  else if (lower.startsWith('.env')) entry = byExt.env
  else if (lower.startsWith('.git')) entry = [Settings2, '#ff7a59']
  else entry = byExt[lower.split('.').pop() ?? '']
  const [Icon, color] = entry ?? [File, '#8b8ba3']
  return <Icon size={size} color={color} strokeWidth={1.8} className="file-icon" />
}

export function FolderIcon({ open, size = 15 }: { open: boolean; size?: number }): React.JSX.Element {
  const Icon = open ? FolderOpen : Folder
  return <Icon size={size} color={open ? '#5aa9ff' : '#8e8e93'} strokeWidth={1.8} className="file-icon" />
}

export function StatusDot({ status, attention }: { status: ChatStatus; attention?: boolean }): React.JSX.Element {
  return <span className={`status-dot s-${status}${attention ? ' attention' : ''}`} />
}

export function Logo({ size = 22 }: { size?: number }): React.JSX.Element {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" className="logo">
      <defs>
        <linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#4da3ff" />
          <stop offset="1" stopColor="#0a6fe0" />
        </linearGradient>
      </defs>
      <rect x="1" y="1" width="30" height="30" rx="8" fill="url(#lg)" />
      <rect x="1.5" y="1.5" width="29" height="14" rx="7.5" fill="#ffffff" opacity="0.10" />
      {/* T */}
      <rect x="7.5" y="8" width="15" height="3.8" rx="1.9" fill="#fff" />
      <rect x="13.1" y="8" width="3.8" height="16" rx="1.9" fill="#fff" />
      {/* terminal cursor */}
      <rect x="19.5" y="20.6" width="5.5" height="3.4" rx="1.4" fill="#fff" opacity="0.8" />
    </svg>
  )
}

export const statusLabel: Record<ChatStatus, string> = {
  starting: 'Đang khởi động…',
  working: 'Đang làm',
  idle: 'Chờ bạn',
  exited: 'Đã kết thúc'
}

export function timeAgo(t: number): string {
  const s = Math.floor((Date.now() - t) / 1000)
  if (s < 60) return 'just now'
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ago`
  return `${Math.floor(h / 24)}d ago`
}
