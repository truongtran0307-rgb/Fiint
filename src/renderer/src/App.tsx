import { Suspense, lazy, useEffect } from 'react'
import { Group, Panel, Separator, useDefaultLayout } from 'react-resizable-panels'
import { useStore } from './store'
import { applyTheme, detectClaude, loadSessions, openFolder, refreshGit, showChatBrowser } from './actions'
import { onGlobalKey } from './shortcuts'
import { focusTerm } from './terminals'
import { reloadIfClean } from './fileModelsLite'
import { QuickOpen, StatusBar, TitleBar, Welcome } from './components/Chrome'
import { Sidebar } from './components/Sidebar'
import { EditorArea } from './components/Editor'
import { ClaudePanel } from './components/Claude'
import { ContextMenuHost } from './components/ContextMenu'

const EventsPane = lazy(() => import('./components/Events').then((m) => ({ default: m.EventsPane })))

function useGlobalKeys(): void {
  useEffect(() => {
    // capture phase: runs before xterm / monaco swallow the keystroke (see shortcuts.ts)
    window.addEventListener('keydown', onGlobalKey, true)
    return () => window.removeEventListener('keydown', onGlobalKey, true)
  }, [])
  // "new Zalo task" notification clicked → show the Zalo tab
  useEffect(() => window.api.zalo.onShowTasks(() => useStore.setState({ centerView: 'zalo' })), [])
}

function useBoot(): void {
  useEffect(() => {
    ;(async () => {
      const [settings] = await Promise.all([window.api.app.settings(), detectClaude()])
      useStore.setState({ recent: settings.recentFolders, settings })
      applyTheme()
      if (settings.lastFolder) openFolder(settings.lastFolder).catch(() => {})
    })()

    window.api.win.state().then((win) => useStore.setState({ win }))
    loadSessions()

    let historyTimer: ReturnType<typeof setTimeout> | undefined
    const offs = [
      window.api.fs.onChanged((paths) => {
        refreshGit()
        // the explorer's folders listen to 'fs-changed' and re-read only the ones touched
        for (const p of paths) reloadIfClean(p)
        window.dispatchEvent(new CustomEvent('fs-changed', { detail: paths }))
      }),
      window.api.fs.onGitChanged(() => {
        refreshGit()
        window.dispatchEvent(new Event('git-changed'))
      }),
      window.api.sessions.onChanged(() => {
        if (historyTimer !== undefined) return
        historyTimer = setTimeout(() => {
          historyTimer = undefined
          loadSessions()
        }, 850)
      }),
      window.api.browser.onOpen(showChatBrowser),
      window.api.win.onState((win) => useStore.setState({ win })),
      // drop-down just slid in → put the cursor straight into the active chat
      window.api.win.onShown(() => {
        const id = useStore.getState().activeChatId
        if (id) focusTerm(id)
      })
    ]
    return () => {
      clearTimeout(historyTimer)
      offs.forEach((off) => off())
    }
  }, [])
}

function Handle({ dir }: { dir: 'h' | 'v' }): React.JSX.Element {
  return <Separator className={`resize-handle ${dir}`} />
}

export default function App(): React.JSX.Element {
  useBoot()
  useGlobalKeys()
  const root = useStore((s) => s.root)
  const centerView = useStore((s) => s.centerView)
  const sidebarOpen = useStore((s) => s.sidebarOpen)
  const quickOpen = useStore((s) => s.quickOpen)
  const dropdown = useStore((s) => s.win?.mode === 'dropdown')
  // layouts are remembered per set of visible panels (sidebar / bottom panel can be toggled)
  const layoutH = useDefaultLayout({ id: 'layout-h', panelIds: sidebarOpen ? ['sidebar', 'center', 'claude'] : ['center', 'claude'], storage: localStorage })

  return (
    <div className={`app${dropdown ? ' mode-dropdown' : ''}`}>
      <TitleBar />
      {!root && centerView === 'events' ? (
        <div className="workbench">
          <Suspense fallback={null}>
            <EventsPane standalone />
          </Suspense>
        </div>
      ) : !root ? (
        <Welcome />
      ) : (
        <div className="workbench">
          <Group orientation="horizontal" className="panels" defaultLayout={layoutH.defaultLayout} onLayoutChanged={layoutH.onLayoutChanged}>
            {sidebarOpen && (
              <>
                <Panel id="sidebar" defaultSize="19%" minSize="13%" maxSize="35%">
                  <Sidebar />
                </Panel>
                <Handle dir="h" />
              </>
            )}
            <Panel id="center" defaultSize="45%" minSize="20%">
              <EditorArea />
            </Panel>
            <Handle dir="h" />
            <Panel id="claude" defaultSize="36%" minSize="22%">
              <ClaudePanel />
            </Panel>
          </Group>
        </div>
      )}
      <StatusBar />
      {quickOpen && <QuickOpen />}
      <ContextMenuHost />
    </div>
  )
}
