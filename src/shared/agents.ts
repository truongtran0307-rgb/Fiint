/**
 * Coding agents a chat can run. Claude Code is the full-featured one (session store, model/effort
 * pickers, browser/db/zalo tools); the others run as plain terminal chats in the same folder.
 */
export type AgentId = 'claude' | 'codex' | 'grok' | 'gemini' | 'qwen' | 'copilot' | 'opencode'

export interface AgentDef {
  id: AgentId
  label: string
  /** command on PATH */
  cmd: string
  /** global npm install command, shown when it isn't installed */
  install: string
  /** flags for "never ask for permission" (used when the chat is in bypass mode) */
  yolo: string[]
  /** args that pick the last conversation back up when a chat restarts ([] = start fresh) */
  resume: string[]
}

export const AGENTS: AgentDef[] = [
  { id: 'claude', label: 'Claude Code', cmd: 'claude', install: 'npm install -g @anthropic-ai/claude-code', yolo: [], resume: [] },
  { id: 'codex', label: 'Codex', cmd: 'codex', install: 'npm install -g @openai/codex', yolo: ['--dangerously-bypass-approvals-and-sandbox'], resume: ['resume', '--last'] },
  { id: 'grok', label: 'Grok', cmd: 'grok', install: 'npm install -g @vibe-kit/grok-cli', yolo: [], resume: [] },
  { id: 'gemini', label: 'Gemini', cmd: 'gemini', install: 'npm install -g @google/gemini-cli', yolo: ['--yolo'], resume: [] },
  { id: 'qwen', label: 'Qwen Code', cmd: 'qwen', install: 'npm install -g @qwen-code/qwen-code', yolo: ['--yolo'], resume: [] },
  { id: 'copilot', label: 'GitHub Copilot', cmd: 'copilot', install: 'npm install -g @github/copilot', yolo: ['--allow-all-tools'], resume: [] },
  { id: 'opencode', label: 'OpenCode', cmd: 'opencode', install: 'npm install -g opencode-ai', yolo: [], resume: [] }
]

export const agentDef = (id: AgentId | undefined): AgentDef => AGENTS.find((a) => a.id === (id ?? 'claude')) ?? AGENTS[0]
