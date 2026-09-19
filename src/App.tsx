import { useEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { listen } from '@tauri-apps/api/event'
import {
  Bell, CheckCircle2, ChevronDown, CircleHelp, ClipboardList, Download, ExternalLink,
  History, Image as ImageIcon, LayoutDashboard, LogIn, MessageCircle, Package, Pencil,
  Plus, RefreshCw, Search, Send, Settings, ShieldCheck, ShoppingBag, Smile,
  Store, Trash2, UserRound, UsersRound, X, Zap, ListPlus,
} from 'lucide-react'
import logo from './assets/shark-butler-logo.png'
import { api } from './lib/api'
import type { Account, AccountInput, ChatContact, ChatMessage, DashboardStats, Order, OrderInput, Product, ProductInput, QrLoginStart, QrLoginStatus, QuickReply, QuickReplyImage, SyncJob } from './lib/types'

type Page = 'dashboard' | 'workbench' | 'accounts' | 'products' | 'orders' | 'settings'
type Dialog =
  | { kind: 'account'; value?: Account }
  | { kind: 'product'; value?: Product }
  | { kind: 'order'; value?: Order }
  | { kind: 'conversation-name'; value: Account }
  | { kind: 'delete-account'; value: Account }
  | { kind: 'qr' }
  | null

const nav: { id: Page; label: string; icon: typeof LayoutDashboard }[] = [
  { id: 'dashboard', label: '首页', icon: LayoutDashboard },
  { id: 'workbench', label: '客服', icon: MessageCircle },
  { id: 'accounts', label: '账号', icon: UsersRound },
  { id: 'products', label: '商品', icon: Package },
  { id: 'orders', label: '订单', icon: ClipboardList },
  { id: 'settings', label: '设置', icon: Settings },
]

function formatDate(value: string) {
  if (!value) return '—'
  return new Intl.DateTimeFormat('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }).format(new Date(value))
}

function formatRelativeTime(value: string) {
  if (!value) return '—'
  const elapsed = Math.max(0, Date.now() - new Date(value).getTime())
  const minutes = Math.floor(elapsed / 60_000)
  if (minutes < 1) return '刚刚'
  if (minutes < 60) return `${minutes}分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}小时前`
  const days = Math.floor(hours / 24)
  return days < 7 ? `${days}天前` : formatDate(value)
}

function displayImageUrl(value: string) {
  const url = value.trim()
  if (url.startsWith('//')) return `https:${url}`
  // Existing local records created before the HTTPS normalization may still
  // contain Alibaba CDN http URLs. Render them securely without waiting for a
  // new remote sync to rewrite the cache.
  if (url.startsWith('http://') && /(^|\.)alicdn\.com(?:\/|$)/i.test(url.slice('http://'.length))) {
    return `https://${url.slice('http://'.length)}`
  }
  return url
}

let messageNotificationContext: AudioContext | null = null

function playMessageNotification() {
  try {
    const AudioContextCtor = window.AudioContext || (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    if (!AudioContextCtor) return
    messageNotificationContext ??= new AudioContextCtor()
    const context = messageNotificationContext
    const play = () => {
      const now = context.currentTime
      // A short three-note chime is easier to recognize than the previous
      // two-tone alert, while remaining unobtrusive during a busy session.
      ;[523.25, 659.25, 783.99].forEach((frequency, index) => {
        const start = now + index * 0.12
        const oscillator = context.createOscillator()
        const gain = context.createGain()
        oscillator.type = 'triangle'
        oscillator.frequency.setValueAtTime(frequency, start)
        gain.gain.setValueAtTime(0.0001, start)
        gain.gain.exponentialRampToValueAtTime(0.3, start + 0.012)
        gain.gain.exponentialRampToValueAtTime(0.0001, start + 0.2)
        oscillator.connect(gain)
        gain.connect(context.destination)
        oscillator.start(start)
        oscillator.stop(start + 0.22)
      })
    }
    if (context.state === 'suspended') void context.resume().then(play)
    else play()
  } catch {
    // A browser/WebView may block audio until the user interacts with it.
  }
}

function downloadFile(name: string, content: string, type: string) {
  const url = URL.createObjectURL(new Blob([content], { type }))
  const link = document.createElement('a'); link.href = url; link.download = name; link.click()
  URL.revokeObjectURL(url)
}

function Status({ value }: { value: string }) {
  const kind = value.includes('有效') || value.includes('上架') || value.includes('完成') ? 'ok'
    : value.includes('异常') || value.includes('关闭') ? 'danger' : 'warn'
  return <span className={`status status-${kind}`}>{value}</span>
}

function ImStatusBadge({ value }: { value?: string }) {
  const config: Record<string, [string, string]> = {
    connected: ['已连接', 'connected'],
    connecting: ['连接中', 'connecting'],
    disconnected: ['已断开', 'disconnected'],
    not_logged_in: ['未登录', 'not-logged-in'],
    stopped: ['未启动', 'stopped'],
    error: ['连接异常', 'error'],
  }
  const [label, kind] = config[value ?? 'not_logged_in'] ?? ['未知', 'stopped']
  return <span className={`im-status-badge ${kind}`}><span className="im-status-dot" />{label}</span>
}

export default function App() {
  const [page, setPage] = useState<Page>('dashboard')
  const [accounts, setAccounts] = useState<Account[]>([])
  const [products, setProducts] = useState<Product[]>([])
  const [orders, setOrders] = useState<Order[]>([])
  const [syncJobs, setSyncJobs] = useState<SyncJob[]>([])
  const [stats, setStats] = useState<DashboardStats | null>(null)
  const [accountId, setAccountId] = useState('')
  const [loading, setLoading] = useState(true)
  const [notice, setNotice] = useState('')
  const [dialog, setDialog] = useState<Dialog>(null)
  const [accountMenuOpen, setAccountMenuOpen] = useState(false)
  const [messageCenterOpen, setMessageCenterOpen] = useState(false)
  const [globalMessages, setGlobalMessages] = useState<Array<{ account: Account; contact: ChatContact }>>([])
  const [accountMenuPosition, setAccountMenuPosition] = useState({ top: 0, left: 0, width: 310 })
  const accountMenuButtonRef = useRef<HTMLButtonElement>(null)
  const [unreadTotals, setUnreadTotals] = useState<Record<string, number>>({})
  const [imStatuses, setImStatuses] = useState<Record<string, string>>({})
  const [unreadJumpRequest, setUnreadJumpRequest] = useState({ accountId: '', chatId: '', nonce: 0 })
  const globalMessageSnapshotRef = useRef(new Map<string, { unreadCount: number; latestMessageTime: string }>())
  const globalPollInFlightRef = useRef(false)
  const globalPollPendingRef = useRef(false)
  const pageRef = useRef(page)
  const [conversationTabIds, setConversationTabIds] = useState<string[]>(() => {
    try { return JSON.parse(localStorage.getItem('shark-butler-conversation-tabs') || '[]') as string[] } catch { return [] }
  })

  const activeAccount = accounts.find((item) => item.id === accountId)

  const refresh = async () => {
    setLoading(true)
    try {
      const [nextStats, nextAccounts, nextProducts, nextOrders, nextJobs, nextUnreadTotals, nextImStatuses] = await Promise.all([
        api.dashboard(), api.accounts(), api.products(), api.orders(), api.syncJobs(), api.chatUnreadTotals(), api.imStatuses(),
      ])
      setStats(nextStats); setAccounts(nextAccounts); setProducts(nextProducts); setOrders(nextOrders); setSyncJobs(nextJobs)
      setUnreadTotals(nextUnreadTotals)
      setImStatuses(nextImStatuses)
      setConversationTabIds((currentTabs) => {
        const validTabs = currentTabs.filter((id, index) => nextAccounts.some((account) => account.id === id) && currentTabs.indexOf(id) === index)
        const nextTabs = validTabs.length ? validTabs : nextAccounts[0] ? [nextAccounts[0].id] : []
        setAccountId((current) => nextTabs.includes(current) ? current : nextTabs[0] || '')
        return nextTabs
      })
    } catch {
      setNotice('无法读取本地数据，请检查应用数据目录。')
    } finally { setLoading(false) }
  }

  useEffect(() => { void refresh() }, [])

  useEffect(() => {
    localStorage.setItem('shark-butler-conversation-tabs', JSON.stringify(conversationTabIds))
  }, [conversationTabIds])

  useEffect(() => { if (page !== 'workbench') setAccountMenuOpen(false) }, [page])
  useEffect(() => { pageRef.current = page }, [page])

  useEffect(() => {
    let unlisten: (() => void) | undefined
    void listen<{ accountId: string; status: string }>('im-status', (event) => {
      setImStatuses((current) => ({ ...current, [event.payload.accountId]: event.payload.status }))
    }).then((stop) => { unlisten = stop })
    return () => unlisten?.()
  }, [])

  useEffect(() => {
    if (!accounts.length) return
    let cancelled = false
    let initialized = false
    const pollGlobalMessages = async () => {
      if (globalPollInFlightRef.current) {
        globalPollPendingRef.current = true
        return
      }
      globalPollInFlightRef.current = true
      try {
        const nextItems: Array<{ account: Account; contact: ChatContact }> = []
        let hasNewMessage = false
        const nextSnapshot = new Map<string, { unreadCount: number; latestMessageTime: string }>()
        for (const account of accounts) {
          try {
            const contacts = await api.chatContacts(account.id)
            for (const contact of contacts) {
              const key = `${account.id}:${contact.chatId}`
              const previous = globalMessageSnapshotRef.current.get(key)
              if (initialized && previous && (contact.unreadCount > previous.unreadCount || (contact.unreadCount > 0 && contact.latestMessageTime !== previous.latestMessageTime))) {
                hasNewMessage = true
              }
              nextSnapshot.set(key, { unreadCount: contact.unreadCount, latestMessageTime: contact.latestMessageTime })
              if (contact.unreadCount > 0) nextItems.push({ account, contact })
            }
          } catch {
            // Ignore accounts without a usable local session.
          }
        }
        if (cancelled) return
        globalMessageSnapshotRef.current = nextSnapshot
        setGlobalMessages(nextItems.sort((left, right) => new Date(right.contact.latestMessageTime).getTime() - new Date(left.contact.latestMessageTime).getTime()).slice(0, 8))
        if (hasNewMessage && pageRef.current !== 'workbench') playMessageNotification()
        initialized = true
        // The visible conversation list is pageable. Refresh the aggregate from
        // the local read watermark so the global badge includes every cached
        // conversation and immediately reflects a chat opened as read.
        try { setUnreadTotals(await api.chatUnreadTotals()) } catch { /* keep last known totals */ }
      } finally {
        globalPollInFlightRef.current = false
        if (globalPollPendingRef.current && !cancelled) {
          globalPollPendingRef.current = false
          void pollGlobalMessages()
        }
      }
    }
    void pollGlobalMessages()
    let unlisten: (() => void) | undefined
    void (async () => {
      const stop = await listen<{ accountId: string }>('chat-im-event', (event) => {
        if (!cancelled && accounts.some((account) => account.id === event.payload.accountId)) void pollGlobalMessages()
      })
      if (cancelled) stop()
      else unlisten = stop
      await Promise.all(accounts.map((account) => api.startChatListener(account.id).catch(() => undefined)))
    })()
    return () => {
      cancelled = true
      unlisten?.()
      void Promise.all(accounts.map((account) => api.stopChatListener(account.id).catch(() => undefined)))
    }
  // IM listeners belong to the application/account lifecycle, not the
  // currently visible page.  Including `page` here would disconnect every
  // account whenever the user opened 商品、订单 or 设置.
  }, [accounts.map((account) => `${account.id}:${account.status}:${account.remoteAccountId}`).join(',')])

  useEffect(() => {
    if (!accountMenuOpen) return
    const updatePosition = () => {
      const rect = accountMenuButtonRef.current?.getBoundingClientRect()
      if (!rect) return
      const edge = 12
      const width = Math.min(310, Math.max(220, window.innerWidth - edge * 2))
      const left = Math.min(Math.max(edge, rect.left), Math.max(edge, window.innerWidth - width - edge))
      setAccountMenuPosition({ top: rect.bottom + 7, left, width })
    }
    updatePosition()
    window.addEventListener('resize', updatePosition)
    window.addEventListener('scroll', updatePosition, true)
    return () => {
      window.removeEventListener('resize', updatePosition)
      window.removeEventListener('scroll', updatePosition, true)
    }
  }, [accountMenuOpen])

  useEffect(() => {
    if (!notice) return
    const timer = window.setTimeout(() => setNotice(''), 4000)
    return () => window.clearTimeout(timer)
  }, [notice])

  const filteredProducts = useMemo(() => accountId ? products.filter((item) => item.accountId === accountId) : products, [products, accountId])
  const filteredOrders = useMemo(() => accountId ? orders.filter((item) => item.accountId === accountId) : orders, [orders, accountId])
  const globalUnreadCount = Object.values(unreadTotals).reduce((total, count) => total + Math.max(0, count), 0)
  const handleChatRead = (readAccountId: string, readChatId: string) => {
    setGlobalMessages((current) => current.filter((item) => !(item.account.id === readAccountId && item.contact.chatId === readChatId)))
    void refreshUnreadTotals()
  }
  const handleError = (error: unknown) => setNotice(error instanceof Error ? error.message : String(error))
  const refreshUnreadTotals = async () => {
    try { setUnreadTotals(await api.chatUnreadTotals()) } catch (error) { handleError(error) }
  }

  const saveAccount = async (input: AccountInput, current?: Account) => {
    try { current ? await api.updateAccount(current.id, input) : await api.createAccount(input); setDialog(null); await refresh(); setNotice(current ? '账号信息已保存。' : '账号已添加到本地工作台。') } catch (error) { handleError(error) }
  }
  const saveProduct = async (input: ProductInput, current?: Product) => {
    try { current ? await api.updateProduct(current.id, input) : await api.createProduct(input); setDialog(null); await refresh(); setNotice(current ? '商品已更新。' : '商品已创建。') } catch (error) { handleError(error) }
  }
  const saveOrder = async (input: OrderInput, current?: Order) => {
    try { current ? await api.updateOrder(current.id, input.status, input.note) : await api.createOrder(input); setDialog(null); await refresh(); setNotice(current ? '订单状态和备注已保存。' : '订单已创建。') } catch (error) { handleError(error) }
  }
  const saveConversationName = async (account: Account, conversationName: string) => {
    try {
      const updated = await api.updateConversationName(account.id, conversationName)
      setAccounts((current) => current.map((item) => item.id === updated.id ? updated : item))
      setDialog(null)
      setNotice(conversationName.trim() ? '会话名称已保存。' : '已恢复使用账号名称。')
    } catch (error) { handleError(error) }
  }
  const remove = async (kind: 'product' | 'order', id: string) => {
    const labels = { product: '商品', order: '订单' }
    if (!window.confirm(`确认删除${labels[kind]}吗？此操作不可恢复。`)) return
    try { if (kind === 'product') await api.deleteProduct(id); if (kind === 'order') await api.deleteOrder(id); await refresh(); setNotice('已删除本地数据。') } catch (error) { handleError(error) }
  }
  const deleteAccount = async (account: Account) => {
    try {
      await api.deleteAccount(account.id)
      setDialog(null)
      setAccountMenuOpen(false)
      await refresh()
      setNotice(`账号“${account.displayName}”及其本地关联数据已删除。`)
    } catch (error) { handleError(error); throw error }
  }

  const syncAccount = async (account = activeAccount) => {
    if (!account) return
    try { const result = await api.syncAccount(account.id); setNotice(result.sourceConnected ? `${account.displayName} 本机同步完成：商品 ${result.productsChanged} 条，订单 ${result.ordersChanged} 条。` : `${account.displayName} 尚未完成本机扫码登录。`); await refresh() } catch (error) { handleError(error); await refresh() }
  }
  const setAccountsStatus = async (ids: string[], status: AccountInput['status']) => {
    try { await Promise.all(ids.map((id) => { const account = accounts.find((item) => item.id === id); return account ? api.updateAccount(id, { displayName: account.displayName, alias: account.alias, platform: account.platform, status, sourceUrl: account.sourceUrl, remoteAccountId: account.remoteAccountId }) : Promise.resolve() })); await refresh(); setNotice(`已更新 ${ids.length} 个账号的状态。`) } catch (error) { handleError(error) }
  }
  const bulkProducts = async (ids: string[], action: '已上架' | '已下架' | '删除') => {
    if (action === '删除' && !window.confirm(`确认删除选中的 ${ids.length} 个本地商品记录吗？`)) return
    try { const changed = action === '删除' ? await api.deleteProducts(ids) : await api.updateProductsStatus(ids, action); await refresh(); setNotice(`已处理 ${changed} 个商品。`) } catch (error) { handleError(error) }
  }
  const bulkOrders = async (ids: string[], status: string) => {
    try { const changed = await api.updateOrdersStatus(ids, status); await refresh(); setNotice(`已更新 ${changed} 个订单。`) } catch (error) { handleError(error) }
  }
  const exportBackup = async () => {
    try { const data = await api.exportBackup(); downloadFile(`鲨鱼管家备份-${new Date().toISOString().slice(0, 10)}.json`, JSON.stringify(data, null, 2), 'application/json'); setNotice('本地数据备份已导出。') } catch (error) { handleError(error) }
  }
  const finishQrLogin = async () => {
    setDialog(null); await refresh(); setNotice('扫码登录成功，本机会话已保存，账号已加入工作台。')
  }
  const openConversationTab = (id: string) => {
    setConversationTabIds((current) => current.includes(id) ? current : [...current, id])
    setAccountId(id)
    setAccountMenuOpen(false)
  }
  const jumpToUnreadConversation = (id: string) => {
    setAccountId(id)
    setUnreadJumpRequest((current) => ({ accountId: id, chatId: '', nonce: current.nonce + 1 }))
  }
  const openGlobalMessage = (item: { account: Account; contact: ChatContact }) => {
    setMessageCenterOpen(false)
    setGlobalMessages((current) => current.filter((entry) => !(entry.account.id === item.account.id && entry.contact.chatId === item.contact.chatId)))
    setConversationTabIds((current) => current.includes(item.account.id) ? current : [...current, item.account.id])
    setAccountId(item.account.id)
    setUnreadJumpRequest((current) => ({ accountId: item.account.id, chatId: item.contact.chatId, nonce: current.nonce + 1 }))
    setPage('workbench')
  }
  const closeConversationTab = (id: string) => {
    setConversationTabIds((current) => {
      const index = current.indexOf(id)
      const next = current.filter((item) => item !== id)
      if (accountId === id) setAccountId(next[Math.min(index, next.length - 1)] || '')
      return next
    })
  }

  return (
    <main className="shell">
      <aside className="rail" aria-label="主导航">
        <button className="brand-mark" onClick={() => setPage('dashboard')} aria-label="鲨鱼管家首页">
          <img src={logo} alt="鲨鱼管家" />
        </button>
        <nav>{nav.slice(0, 5).map(({ id, label, icon: Icon }) => (
          <button key={id} className={`rail-link ${page === id ? 'active' : ''}`} onClick={() => setPage(id)}>
            <Icon size={21} /><span>{label}</span>
          </button>
        ))}</nav>
        <div className="rail-bottom">
          <button className={`rail-link ${page === 'settings' ? 'active' : ''}`} onClick={() => setPage('settings')}><Settings size={21} /><span>设置</span></button>
          <button className="rail-link" onClick={() => setNotice('鲨鱼管家 v0.1 · 本地优先工作台')}><CircleHelp size={21} /><span>帮助</span></button>
        </div>
      </aside>

      <section className="app-column">
        <header className="topbar">
          {page === 'workbench' && <div className="conversation-tabs" aria-label="会话标签">
            <div className="conversation-tab-strip">{conversationTabIds.map((id) => accounts.find((account) => account.id === id)).filter((account): account is Account => Boolean(account)).map((account) => <div className={`conversation-tab ${account.id === activeAccount?.id ? 'active' : ''}`} key={account.id}><button className="conversation-tab-main" onClick={() => setAccountId(account.id)} onDoubleClick={() => jumpToUnreadConversation(account.id)} title="双击跳转到下一条未读会话"><AccountAvatar account={account} className="conversation-tab-avatar" />{(unreadTotals[account.id] ?? 0) > 0 && <em className="conversation-tab-unread" aria-label={`${unreadTotals[account.id]} 条未读消息`}>{unreadTotals[account.id] > 99 ? '99+' : unreadTotals[account.id]}</em>}<span><strong>{account.conversationName || account.displayName}</strong><small>{account.remoteAccountId || account.displayName} · {account.status}</small></span></button><button className="conversation-tab-action edit" onClick={() => setDialog({ kind: 'conversation-name', value: account })} title="自定义会话名称"><Pencil size={12} /></button><button className="conversation-tab-action close" onClick={() => closeConversationTab(account.id)} title="关闭会话标签"><X size={13} /></button></div>)}</div>
            <div className="account-menu-anchor"><button ref={accountMenuButtonRef} className={`round-button ${accountMenuOpen ? 'active' : ''}`} onClick={() => setAccountMenuOpen((open) => !open)} title="添加会话标签" aria-expanded={accountMenuOpen}><Plus size={20} /></button>{accountMenuOpen && createPortal(<div className="account-menu account-menu-portal" style={accountMenuPosition}><header><strong>选择会话账号</strong><span>{accounts.length} 个账号</span></header>{accounts.length ? accounts.map((account) => <button className={conversationTabIds.includes(account.id) ? 'selected' : ''} key={account.id} onClick={() => openConversationTab(account.id)}><AccountAvatar account={account} /><span><strong>{account.conversationName || account.displayName}</strong><small>{account.displayName} · {account.status}</small></span>{conversationTabIds.includes(account.id) && <CheckCircle2 size={17} />}</button>) : <p>暂无账号，请先到账号管理扫码登录。</p>}</div>, document.body)}</div>
          </div>}
          <div className="top-actions">
            <div className="message-center-anchor">
              <button className={`message-center-button ${messageCenterOpen ? 'active' : ''}`} onClick={() => setMessageCenterOpen((open) => !open)} aria-label={`未读消息 ${globalUnreadCount} 条`} aria-expanded={messageCenterOpen}>
                <Bell size={17} />
                {globalUnreadCount > 0 && <em>{globalUnreadCount > 99 ? '99+' : globalUnreadCount}</em>}
              </button>
              {messageCenterOpen && <div className="message-center-popover">
                <header><strong>未读消息</strong><span>{globalUnreadCount ? `${globalUnreadCount} 条 · ${globalMessages.length} 个会话` : '暂无未读消息'}</span></header>
                {globalMessages.length ? globalMessages.map((item) => <button className="message-center-item" key={`${item.account.id}:${item.contact.chatId}`} onClick={() => openGlobalMessage(item)}><AccountAvatar account={item.account} /><span><strong>{item.contact.otherUserName}</strong><small>{item.account.displayName} · {item.contact.latestMessage || '收到新消息'}</small></span><b>{formatRelativeTime(item.contact.latestMessageTime)}</b></button>) : <p className="message-center-empty">消息会在这里提醒</p>}
              </div>}
            </div>
            <span className="sync-label"><ShieldCheck size={16} /> 本地数据受保护</span>
            <button className="text-button" onClick={() => void syncAccount()}><RefreshCw size={15} /> 刷新</button>
          </div>
        </header>

        {notice && <div className="notice toast-notice" role="status" aria-live="polite"><ShieldCheck size={17} /><span>{notice}</span><button onClick={() => setNotice('')} aria-label="关闭提示">×</button></div>}

        <div className={`content ${page === 'workbench' ? 'workbench-content' : ''}`}>
          {loading ? <Loading /> : page === 'dashboard' ? <Dashboard stats={stats} accounts={accounts} orders={orders} onGo={setPage} />
            : page === 'workbench' ? <Workbench key={activeAccount?.id ?? 'empty'} account={activeAccount} products={products} onUnreadChanged={refreshUnreadTotals} onChatRead={handleChatRead} unreadJumpRequest={unreadJumpRequest} />
              : page === 'accounts' ? <Accounts accounts={accounts} syncJobs={syncJobs} imStatuses={imStatuses} onQrLogin={() => setDialog({ kind: 'qr' })} onEdit={(value) => setDialog({ kind: 'account', value })} onDelete={(value) => setDialog({ kind: 'delete-account', value })} onSync={(account) => void syncAccount(account)} onSetStatus={(ids, status) => void setAccountsStatus(ids, status)} />
                : page === 'products' ? <Products items={filteredProducts} account={activeAccount} onSync={() => void syncAccount()} onBulk={(ids, action) => void bulkProducts(ids, action)} onAdd={() => setDialog({ kind: 'product' })} onEdit={(value) => setDialog({ kind: 'product', value })} onDelete={(id) => void remove('product', id)} />
                  : page === 'orders' ? <Orders items={filteredOrders} account={activeAccount} onSync={() => void syncAccount()} onBulk={(ids, status) => void bulkOrders(ids, status)} onAdd={() => setDialog({ kind: 'order' })} onEdit={(value) => setDialog({ kind: 'order', value })} onDelete={(id) => void remove('order', id)} />
                    : <SettingsPage onExport={() => void exportBackup()} />}
        </div>
        {dialog?.kind === 'account' && <AccountDialog value={dialog.value} onClose={() => setDialog(null)} onSave={saveAccount} />}
        {dialog?.kind === 'product' && <ProductDialog accounts={accounts} selectedAccountId={activeAccount?.id} value={dialog.value} onClose={() => setDialog(null)} onSave={saveProduct} />}
        {dialog?.kind === 'order' && <OrderDialog accounts={accounts} selectedAccountId={activeAccount?.id} value={dialog.value} onClose={() => setDialog(null)} onSave={saveOrder} />}
        {dialog?.kind === 'conversation-name' && <ConversationNameDialog account={dialog.value} onClose={() => setDialog(null)} onSave={saveConversationName} />}
        {dialog?.kind === 'delete-account' && <DeleteAccountDialog account={dialog.value} onClose={() => setDialog(null)} onDelete={deleteAccount} />}
        {dialog?.kind === 'qr' && <QrLoginDialog onClose={() => setDialog(null)} onConnected={finishQrLogin} />}
      </section>
    </main>
  )
}

function Loading() { return <div className="loading"><span /><p>正在加载本地工作台…</p></div> }

function Dashboard({ stats, accounts, orders, onGo }: { stats: DashboardStats | null; accounts: Account[]; orders: Order[]; onGo: (p: Page) => void }) {
  const cards: Array<{ label: string; value: number; detail: string; icon: typeof UsersRound; target: Page }> = [
    { label: '已连接账号', value: stats?.totalAccounts ?? 0, detail: `${stats?.healthyAccounts ?? 0} 个授权有效`, icon: UsersRound, target: 'accounts' },
    { label: '在售商品', value: stats?.activeProducts ?? 0, detail: '按当前本地数据库统计', icon: Package, target: 'products' },
    { label: '待处理订单', value: stats?.pendingOrders ?? 0, detail: '需要优先跟进', icon: ClipboardList, target: 'orders' },
  ]
  return <div className="page dashboard">
    <div className="welcome"><div><p className="eyebrow">运营概览</p><h1>早上好，今天也让店铺井井有条。</h1><p>所有数据保存在当前设备；只有获得授权的平台连接才会进行同步。</p></div><button className="primary" onClick={() => onGo('accounts')}><Store size={17} />管理账号</button></div>
    <div className="metric-grid">{cards.map(({ label, value, detail, icon: Icon, target }) => <button className="metric-card" key={label} onClick={() => onGo(target)}><span>{label}</span><strong>{value}</strong><small>{detail}</small><Icon size={22} /></button>)}</div>
    <div className="two-column">
      <section className="panel"><div className="panel-head"><div><h2>账号健康度</h2><p>授权和同步状态一目了然</p></div><button className="icon-button" onClick={() => onGo('accounts')}><ExternalLink size={17} /></button></div>
        <div className="account-list">{accounts.map((item) => <div className="account-row" key={item.id}><AccountAvatar account={item} /><div className="grow"><strong>{item.displayName}</strong><span>{item.alias} · 更新于 {formatDate(item.lastSyncAt)}</span></div><Status value={item.status} /></div>)}</div>
      </section>
      <section className="panel"><div className="panel-head"><div><h2>最近订单</h2><p>按创建时间展示</p></div><button className="icon-button" onClick={() => onGo('orders')}><ExternalLink size={17} /></button></div>
        <div className="order-list">{orders.slice(0, 4).map((order) => <div className="mini-order" key={order.id}><div><strong>{order.productTitle}</strong><span>{order.orderNo} · {formatDate(order.createdAt)}</span></div><div><b>¥{order.amount.toFixed(2)}</b><Status value={order.status} /></div></div>)}</div>
      </section>
    </div>
  </div>
}

function AccountPicker({ account }: { account?: Account }) { return <div className="filter-button account-indicator"><Store size={16} />{account?.displayName ?? '全部账号'}</div> }

function AccountAvatar({ account, className = '' }: { account: Account; className?: string }) {
  const fallback = account.displayName.trim().slice(0, 1) || account.remoteAccountId.slice(0, 1) || '?'
  return <span className={`avatar account-avatar ${className}`}><span>{fallback}</span>{account.avatarUrl && <img src={displayImageUrl(account.avatarUrl)} alt={`${account.displayName}头像`} loading="lazy" onError={(event) => event.currentTarget.remove()} />}</span>
}

function Accounts({ accounts, syncJobs, imStatuses, onQrLogin, onEdit, onDelete, onSync, onSetStatus }: { accounts: Account[]; syncJobs: SyncJob[]; imStatuses: Record<string, string>; onQrLogin: () => void; onEdit: (account: Account) => void; onDelete: (account: Account) => void; onSync: (account: Account) => void; onSetStatus: (ids: string[], status: AccountInput['status']) => void }) {
  const [query, setQuery] = useState(''); const [status, setStatus] = useState('全部状态'); const [selected, setSelected] = useState<string[]>([])
  const visible = accounts.filter((account) => (status === '全部状态' || account.status === status) && `${account.displayName}${account.alias}${account.platform}`.toLowerCase().includes(query.toLowerCase()))
  const toggleAll = () => setSelected(selected.length === visible.length ? [] : visible.map((account) => account.id))
  const toggle = (id: string) => setSelected((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id])
  return <div className="page account-page">
    <PageHead eyebrow="账号管理" title="店铺账号与本机会话" description="支持本机扫码登录、批量启停、同步商品与订单，并保留同步记录。" action={<div className="head-actions"><button className="secondary" onClick={onQrLogin}><LogIn size={17} />扫码登录</button></div>} />
    <section className="table-panel accounts-table"><div className="account-table-head"><div><h2>账号列表 <span>{visible.length} 个账号</span></h2><p>已选择 {selected.length} 个账号</p></div><div className="account-batch-actions"><button className="secondary" disabled={!selected.length} onClick={() => onSetStatus(selected, '授权有效')}>批量启用</button><button className="secondary" disabled={!selected.length} onClick={() => onSetStatus(selected, '已停用')}>批量停用</button><button className="secondary" onClick={() => visible.forEach(onSync)}><RefreshCw size={15} />同步当前列表</button></div></div>
      <div className="toolbar account-toolbar"><div className="search-field"><Search size={17} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索店铺名、别名或平台" /></div><select className="filter-select" value={status} onChange={(event) => setStatus(event.target.value)}><option>全部状态</option><option>授权有效</option><option>即将过期</option><option>同步异常</option><option>已停用</option></select></div>
      <table><thead><tr><th><input aria-label="全选账号" type="checkbox" checked={Boolean(visible.length) && selected.length === visible.length} onChange={toggleAll} /></th><th>店铺账号</th><th>平台 / 别名</th><th>本机会话</th><th>IM 状态</th><th>状态</th><th>商品</th><th>订单</th><th>最后同步</th><th>操作</th></tr></thead><tbody>{visible.map((account) => <tr key={account.id}><td><input aria-label={`选择 ${account.displayName}`} type="checkbox" checked={selected.includes(account.id)} onChange={() => toggle(account.id)} /></td><td><div className="product-cell"><AccountAvatar account={account} /><div><strong>{account.displayName}</strong><span>{account.remoteAccountId || account.id.slice(0, 8)}</span></div></div></td><td><strong>{account.platform}</strong><br /><span className="muted">{account.alias || '未设置别名'}</span></td><td>{account.remoteAccountId ? <span className="source-connected"><CheckCircle2 size={13} />已登录</span> : <span className="muted">仅本地资料</span>}</td><td><ImStatusBadge value={imStatuses[account.id] ?? (account.remoteAccountId ? 'connecting' : 'not_logged_in')} /></td><td><Status value={account.status} /></td><td>{account.productCount}</td><td>{account.orderCount}</td><td>{formatDate(account.lastSyncAt)}</td><td><div className="row-actions"><button className="link-button" onClick={() => onSync(account)}>同步</button><button className="link-button" onClick={() => onEdit(account)}>编辑</button><button className="danger-link" onClick={() => onDelete(account)}>删除</button></div></td></tr>)}</tbody></table>{visible.length === 0 && <EmptyTable text="没有符合条件的账号" />}
    </section>
    <section className="panel sync-history"><div className="panel-head"><div><h2>最近同步记录</h2><p>显示最近 100 次同步结果，失败原因会保留在本机。</p></div><History size={18} /></div>{syncJobs.length ? <div className="sync-job-list">{syncJobs.slice(0, 8).map((job) => <div className="sync-job" key={job.id}><Status value={job.status} /><div><strong>{accounts.find((account) => account.id === job.accountId)?.displayName ?? job.accountId.slice(0, 8)}</strong><span>{job.resource} · {formatDate(job.startedAt)}</span>{job.errorMessage && <small>{job.errorMessage}</small>}</div></div>)}</div> : <EmptyTable text="还没有同步记录" />}</section>
  </div>
}

function Products({ items, account, onSync, onBulk, onAdd, onEdit, onDelete }: { items: Product[]; account?: Account; onSync: () => void; onBulk: (ids: string[], action: '已上架' | '已下架' | '删除') => void; onAdd: () => void; onEdit: (product: Product) => void; onDelete: (id: string) => void }) {
  const [query, setQuery] = useState(''); const [status, setStatus] = useState('全部状态'); const [selected, setSelected] = useState<string[]>([])
  const visible = items.filter((item) => (status === '全部状态' || item.status === status) && `${item.title}${item.id}${item.tags.join(' ')}`.toLowerCase().includes(query.toLowerCase()))
  const toggle = (id: string) => setSelected((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id])
  const toggleAll = () => setSelected(visible.length > 0 && visible.every((item) => selected.includes(item.id)) ? selected.filter((id) => !visible.some((item) => item.id === id)) : Array.from(new Set([...selected, ...visible.map((item) => item.id)])))
  return <div className="page">
    <PageHead eyebrow="商品管理" title="商品一览" description={account?.remoteAccountId ? '当前账号已完成本机登录；同步会直接拉取闲鱼商品最新状态。编辑与状态标记仅作本地运营备注。' : '本地创建、编辑、状态和库存变更会立即保存到 SQLite。'} action={<div className="head-actions"><button className="secondary" disabled={!account} onClick={onSync}><RefreshCw size={17} />同步商品</button><button className="primary" onClick={onAdd}><Plus size={17} />新建本地商品</button></div>} />
    <div className="toolbar"><div className="search-field"><Search size={17} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索商品标题、标签或商品 ID" /></div><AccountPicker account={account} /><select className="filter-select" value={status} onChange={(event) => setStatus(event.target.value)}><option>全部状态</option><option>已上架</option><option>已下架</option></select></div>
    <div className="batch-bar"><span>已选择 {selected.length} 个商品</span><button disabled={!selected.length} onClick={() => onBulk(selected, '已上架')}>本地标记上架</button><button disabled={!selected.length} onClick={() => onBulk(selected, '已下架')}>本地标记下架</button><button className="danger-link" disabled={!selected.length} onClick={() => onBulk(selected, '删除')}><Trash2 size={14} />删除记录</button></div>
    <section className="table-panel"><table><thead><tr><th><input aria-label="全选商品" type="checkbox" checked={Boolean(visible.length) && visible.every((item) => selected.includes(item.id))} onChange={toggleAll} /></th><th>商品</th><th>价格</th><th>库存</th><th>状态</th><th>本地标签</th><th>最后同步</th><th>操作</th></tr></thead><tbody>{visible.map((item) => <tr key={item.id}><td><input aria-label={`选择 ${item.title}`} type="checkbox" checked={selected.includes(item.id)} onChange={() => toggle(item.id)} /></td><td><div className="product-cell"><div className="product-image"><Package size={20} /></div><div><strong>{item.title}</strong><span>ID · {item.id}</span></div></div></td><td>¥{item.price.toFixed(2)}</td><td>{item.stock}</td><td><Status value={item.status} /></td><td><div className="tags">{item.tags.map((tag) => <span key={tag}>{tag}</span>)}</div></td><td>{formatDate(item.updatedAt)}</td><td><div className="row-actions"><button className="link-button" onClick={() => onEdit(item)}>编辑</button><button className="danger-link" onClick={() => onDelete(item.id)}>删除</button></div></td></tr>)}</tbody></table>{visible.length === 0 && <EmptyTable text="没有符合条件的商品" />}</section>
  </div>
}

function Orders({ items, account, onSync, onBulk, onAdd, onEdit, onDelete }: { items: Order[]; account?: Account; onSync: () => void; onBulk: (ids: string[], status: string) => void; onAdd: () => void; onEdit: (order: Order) => void; onDelete: (id: string) => void }) {
  const [query, setQuery] = useState(''); const [status, setStatus] = useState('全部状态'); const [startDate, setStartDate] = useState(''); const [endDate, setEndDate] = useState(''); const [selected, setSelected] = useState<string[]>([]); const [batchStatus, setBatchStatus] = useState('待发货')
  const visible = items.filter((item) => (status === '全部状态' || item.status === status) && (!startDate || item.createdAt.slice(0, 10) >= startDate) && (!endDate || item.createdAt.slice(0, 10) <= endDate) && `${item.orderNo}${item.productTitle}${item.buyerMaskedName}`.toLowerCase().includes(query.toLowerCase()))
  const toggle = (id: string) => setSelected((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id])
  const toggleAll = () => setSelected(visible.length > 0 && visible.every((item) => selected.includes(item.id)) ? selected.filter((id) => !visible.some((item) => item.id === id)) : Array.from(new Set([...selected, ...visible.map((item) => item.id)])))
  const exportOrders = () => { const rows = [['订单号', '商品', '买家', '金额', '状态', '创建时间', '备注'], ...visible.map((item) => [item.orderNo, item.productTitle, item.buyerMaskedName, item.amount.toFixed(2), item.status, item.createdAt, item.note])]; const csv = '\ufeff' + rows.map((row) => row.map((cell) => `"${String(cell).replace(/"/g, '""')}"`).join(',')).join('\n'); downloadFile(`鲨鱼管家订单-${new Date().toISOString().slice(0, 10)}.csv`, csv, 'text/csv;charset=utf-8') }
  return <div className="page">
    <PageHead eyebrow="订单管理" title="订单中心" description={account?.remoteAccountId ? '当前账号已完成本机登录；同步会直接拉取闲鱼订单最新状态。' : '默认脱敏展示买家信息，可更新状态、维护备注并按当前筛选导出。'} action={<div className="head-actions"><button className="secondary" disabled={!account} onClick={onSync}><RefreshCw size={17} />同步订单</button><button className="secondary" onClick={exportOrders}><ExternalLink size={17} />导出 CSV</button></div>} />
    <div className="toolbar order-toolbar"><div className="search-field"><Search size={17} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索订单号、商品或买家" /></div><AccountPicker account={account} /><select className="filter-select" value={status} onChange={(event) => setStatus(event.target.value)}><option>全部状态</option><option>待付款</option><option>待发货</option><option>待收货</option><option>已完成</option><option>退款中</option><option>已退款</option><option>已关闭</option></select><input className="date-filter" aria-label="开始日期" type="date" value={startDate} onChange={(event) => setStartDate(event.target.value)} /><input className="date-filter" aria-label="结束日期" type="date" value={endDate} onChange={(event) => setEndDate(event.target.value)} /><button className="primary" onClick={onAdd}><Plus size={16} />录入订单</button></div>
    <div className="batch-bar"><span>已选择 {selected.length} 个订单</span><select value={batchStatus} onChange={(event) => setBatchStatus(event.target.value)}><option>待付款</option><option>待发货</option><option>待收货</option><option>已完成</option><option>退款中</option><option>已退款</option><option>已关闭</option></select><button disabled={!selected.length} onClick={() => onBulk(selected, batchStatus)}>批量更新状态</button></div>
    <section className="table-panel"><table><thead><tr><th><input aria-label="全选订单" type="checkbox" checked={Boolean(visible.length) && visible.every((item) => selected.includes(item.id))} onChange={toggleAll} /></th><th>订单号</th><th>商品</th><th>买家</th><th>金额</th><th>状态</th><th>创建时间</th><th>备注</th><th>操作</th></tr></thead><tbody>{visible.map((item) => <tr key={item.id}><td><input aria-label={`选择订单 ${item.orderNo}`} type="checkbox" checked={selected.includes(item.id)} onChange={() => toggle(item.id)} /></td><td><strong>{item.orderNo}</strong></td><td>{item.productTitle}</td><td><span className="masked"><UserRound size={14} />{item.buyerMaskedName}</span></td><td><strong>¥{item.amount.toFixed(2)}</strong></td><td><Status value={item.status} /></td><td>{formatDate(item.createdAt)}</td><td className="muted">{item.note || '—'}</td><td><div className="row-actions"><button className="link-button" onClick={() => onEdit(item)}>处理</button><button className="danger-link" onClick={() => onDelete(item.id)}>删除</button></div></td></tr>)}</tbody></table>{visible.length === 0 && <EmptyTable text="没有符合条件的订单" />}</section>
  </div>
}

function EmptyTable({ text }: { text: string }) { return <div className="empty-table">{text}</div> }

function Modal({ title, children, onClose }: { title: string; children: React.ReactNode; onClose: () => void }) {
  return <div className="modal-backdrop" role="presentation" onMouseDown={onClose}><section className="modal" role="dialog" aria-modal="true" aria-label={title} onMouseDown={(event) => event.stopPropagation()}><header><h2>{title}</h2><button className="icon-button" onClick={onClose}>×</button></header>{children}</section></div>
}

function ConversationNameDialog({ account, onClose, onSave }: { account: Account; onClose: () => void; onSave: (account: Account, name: string) => void }) {
  const [name, setName] = useState(account.conversationName || account.displayName)
  return <Modal title="自定义会话名称" onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave(account, name) }}><label>会话名称<input autoFocus maxLength={30} value={name} onChange={(event) => setName(event.target.value)} placeholder={account.displayName} /></label><p className="form-note">仅修改顶部会话管理的显示名称，不会更改闲鱼账号名称。留空保存可恢复为“{account.displayName}”。</p><div className="modal-actions"><button type="button" className="secondary" onClick={() => setName('')}>恢复默认</button><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存名称</button></div></form></Modal>
}

function DeleteAccountDialog({ account, onClose, onDelete }: { account: Account; onClose: () => void; onDelete: (account: Account) => Promise<void> }) {
  const [deleting, setDeleting] = useState(false)
  const [error, setError] = useState('')
  const confirm = async () => {
    setDeleting(true); setError('')
    try { await onDelete(account) } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
      setDeleting(false)
    }
  }
  return <Modal title="删除账号" onClose={() => { if (!deleting) onClose() }}><div className="delete-account-confirm"><div className="delete-warning-icon"><Trash2 size={24} /></div><h3>确认删除“{account.displayName}”吗？</h3><p>将从当前电脑永久删除此账号及其登录会话、聊天记录、已读状态、同步记录、{account.productCount} 个商品和 {account.orderCount} 个订单。</p><p className="delete-warning-note">此操作不会删除闲鱼平台上的账号、商品或订单，但本地数据无法恢复。</p>{error && <div className="form-error">{error}</div>}<div className="modal-actions"><button className="secondary" disabled={deleting} onClick={onClose}>取消</button><button className="danger-button" disabled={deleting} onClick={() => void confirm()}><Trash2 size={16} />{deleting ? '正在删除…' : '确认删除'}</button></div></div></Modal>
}

function AccountDialog({ value, onClose, onSave }: { value?: Account; onClose: () => void; onSave: (input: AccountInput, current?: Account) => void }) {
  const [displayName, setDisplayName] = useState(value?.displayName ?? ''); const [alias, setAlias] = useState(value?.alias ?? ''); const [platform, setPlatform] = useState(value?.platform ?? '闲鱼'); const [status, setStatus] = useState<AccountInput['status']>(value?.status ?? '授权有效')
  return <Modal title={value ? '编辑账号资料' : '添加本地账号资料'} onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave({ displayName, alias, platform, status, sourceUrl: value?.sourceUrl ?? '', remoteAccountId: value?.remoteAccountId ?? '' }, value) }}><label>店铺名称<input required autoFocus value={displayName} onChange={(event) => setDisplayName(event.target.value)} placeholder="例如：鲨鱼精选店" /></label><label>账号别名<input value={alias} onChange={(event) => setAlias(event.target.value)} placeholder="用于内部区分" /></label><label>平台<select value={platform} onChange={(event) => setPlatform(event.target.value)}><option>闲鱼</option><option>淘宝（仅本地资料）</option><option>其他平台（仅本地资料）</option></select></label><label>运营状态<select value={status} onChange={(event) => setStatus(event.target.value as AccountInput['status'])}><option>授权有效</option><option>即将过期</option><option>同步异常</option><option>已停用</option></select></label><p className="form-note">这里用于维护显示名称和备注。需要同步闲鱼真实数据时，请使用账号页的“扫码登录”。</p><div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存账号</button></div></form></Modal>
}

function QrLoginDialog({ onClose, onConnected }: { onClose: () => void; onConnected: () => Promise<void> }) {
  const [session, setSession] = useState<QrLoginStart | null>(null)
  const [status, setStatus] = useState<QrLoginStatus | null>(null)
  const [working, setWorking] = useState(true)
  const [error, setError] = useState('')
  const autoGenerated = useRef(false)
  const generate = async () => {
    setWorking(true); setError(''); setStatus(null)
    try { setSession(await api.generateQrLogin()) } catch (value) { setError(value instanceof Error ? value.message : String(value)) } finally { setWorking(false) }
  }
  useEffect(() => {
    if (autoGenerated.current) return
    autoGenerated.current = true
    void generate()
  }, [])
  useEffect(() => {
    if (!session || ['success', 'failed', 'expired', 'cancelled'].includes(status?.status ?? '')) return
    let cancelled = false
    const poll = async () => {
      try {
        const next = await api.checkQrLoginStatus(session.sessionId)
        if (cancelled) return
        setStatus(next)
        if (next.status === 'success') await onConnected()
      } catch (value) { if (!cancelled) setError(value instanceof Error ? value.message : String(value)) }
    }
    void poll(); const timer = window.setInterval(() => void poll(), 2000)
    return () => { cancelled = true; window.clearInterval(timer) }
  }, [session, status?.status, onConnected])
  const qr = status?.status === 'verification_required' && status.faceQrUrl ? status.faceQrUrl : session?.qrCodeUrl
  return <Modal title="本机扫码登录闲鱼" onClose={onClose}><div className="qr-login"><p>二维码由鲨鱼管家在本机直接生成。请使用闲鱼 App 扫码并在手机端确认。</p>{qr ? <img className="qr-image" src={qr} alt="闲鱼登录二维码" /> : <div className="qr-placeholder"><LogIn size={32} /><span>{working ? '正在生成二维码…' : '二维码加载失败，请刷新'}</span></div>}<strong>{status?.message || session?.message || (working ? '正在生成二维码…' : '二维码生成失败')}</strong>{status?.verificationUrl && <a href={status.verificationUrl} target="_blank" rel="noreferrer">在浏览器中打开身份验证</a>}{error && <div className="form-error">{error}</div>}<div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button type="button" className="primary" disabled={working} onClick={() => void generate()}><RefreshCw size={16} />{working ? '生成中…' : '刷新二维码'}</button></div></div></Modal>
}

function ProductDialog({ accounts, selectedAccountId, value, onClose, onSave }: { accounts: Account[]; selectedAccountId?: string; value?: Product; onClose: () => void; onSave: (input: ProductInput, current?: Product) => void }) {
  const [accountId, setAccountId] = useState(value?.accountId ?? selectedAccountId ?? accounts[0]?.id ?? ''); const [title, setTitle] = useState(value?.title ?? ''); const [price, setPrice] = useState(String(value?.price ?? '')); const [stock, setStock] = useState(String(value?.stock ?? '0')); const [status, setStatus] = useState(value?.status ?? '已上架'); const [tags, setTags] = useState(value?.tags.join(', ') ?? '')
  return <Modal title={value ? '编辑商品' : '新建商品'} onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave({ accountId, title, price: Number(price), stock: Number(stock), status, tags: tags.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean) }, value) }}><label>所属账号<select required value={accountId} onChange={(event) => setAccountId(event.target.value)}>{accounts.map((account) => <option value={account.id} key={account.id}>{account.displayName}</option>)}</select></label><label>商品标题<input required autoFocus value={title} onChange={(event) => setTitle(event.target.value)} /></label><div className="form-row"><label>价格（元）<input required min="0" step="0.01" type="number" value={price} onChange={(event) => setPrice(event.target.value)} /></label><label>库存<input required min="0" step="1" type="number" value={stock} onChange={(event) => setStock(event.target.value)} /></label></div><div className="form-row"><label>状态<select value={status} onChange={(event) => setStatus(event.target.value)}><option>已上架</option><option>已下架</option></select></label><label>标签（逗号分隔）<input value={tags} onChange={(event) => setTags(event.target.value)} placeholder="数码, 热销" /></label></div><div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存商品</button></div></form></Modal>
}

function OrderDialog({ accounts, selectedAccountId, value, onClose, onSave }: { accounts: Account[]; selectedAccountId?: string; value?: Order; onClose: () => void; onSave: (input: OrderInput, current?: Order) => void }) {
  const [accountId, setAccountId] = useState(value?.accountId ?? selectedAccountId ?? accounts[0]?.id ?? ''); const [productTitle, setProductTitle] = useState(value?.productTitle ?? ''); const [buyerMaskedName, setBuyerMaskedName] = useState(value?.buyerMaskedName ?? ''); const [amount, setAmount] = useState(String(value?.amount ?? '')); const [status, setStatus] = useState(value?.status ?? '待付款'); const [note, setNote] = useState(value?.note ?? '')
  return <Modal title={value ? `处理订单 ${value.orderNo}` : '录入订单'} onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave({ accountId, productTitle, buyerMaskedName, amount: Number(amount), status, note }, value) }}><label>所属账号<select disabled={Boolean(value)} required value={accountId} onChange={(event) => setAccountId(event.target.value)}>{accounts.map((account) => <option value={account.id} key={account.id}>{account.displayName}</option>)}</select></label><label>商品名称<input disabled={Boolean(value)} required autoFocus value={productTitle} onChange={(event) => setProductTitle(event.target.value)} /></label><div className="form-row"><label>买家标识<input disabled={Boolean(value)} required value={buyerMaskedName} onChange={(event) => setBuyerMaskedName(event.target.value)} placeholder="例如：张**" /></label><label>订单金额（元）<input disabled={Boolean(value)} required min="0" step="0.01" type="number" value={amount} onChange={(event) => setAmount(event.target.value)} /></label></div><label>订单状态<select value={status} onChange={(event) => setStatus(event.target.value)}><option>待付款</option><option>待发货</option><option>待收货</option><option>已完成</option><option>退款中</option><option>已退款</option><option>已关闭</option></select></label><label>内部备注<textarea value={note} onChange={(event) => setNote(event.target.value)} placeholder="仅保存在本机" rows={3} /></label><div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存订单</button></div></form></Modal>
}

function Workbench({ account, products, onUnreadChanged, onChatRead, unreadJumpRequest }: { account?: Account; products: Product[]; onUnreadChanged: () => Promise<void>; onChatRead: (accountId: string, chatId: string) => void; unreadJumpRequest: { accountId: string; chatId: string; nonce: number } }) {
  const CONTACT_BATCH = 30
  const MESSAGE_BATCH = 50
  const [contacts, setContacts] = useState<ChatContact[]>([])
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [selectedId, setSelectedId] = useState('')
  const [query, setQuery] = useState('')
  const [conversationStatus, setConversationStatus] = useState('全部')
  const [draft, setDraft] = useState('')
  const [emojiOpen, setEmojiOpen] = useState(false)
  const [quickReplyOpen, setQuickReplyOpen] = useState(false)
  const [quickReplyCommandOpen, setQuickReplyCommandOpen] = useState(false)
  const [quickReplies, setQuickReplies] = useState<QuickReply[]>([])
  const [quickReplyManaging, setQuickReplyManaging] = useState(false)
  const [editingQuickReply, setEditingQuickReply] = useState<QuickReply | undefined>()
  const [queuedReplyImages, setQueuedReplyImages] = useState<QuickReplyImage[]>([])
  const [productPickerOpen, setProductPickerOpen] = useState(false)
  const [composerNotice, setComposerNotice] = useState('')
  const [busy, setBusy] = useState(false)
  const [contactLoading, setContactLoading] = useState(false)
  const [messageLoading, setMessageLoading] = useState(false)
  const [contactLimit, setContactLimit] = useState(CONTACT_BATCH)
  const [messageStart, setMessageStart] = useState(0)
  const [contactCursor, setContactCursor] = useState<number | null>(null)
  const [messageCursor, setMessageCursor] = useState<number | null>(null)
  const [contactHasMore, setContactHasMore] = useState(true)
  const [messageHasMore, setMessageHasMore] = useState(true)
  const [error, setError] = useState('')
  const messageListRef = useRef<HTMLDivElement>(null)
  const conversationListRef = useRef<HTMLDivElement>(null)
  const imageInputRef = useRef<HTMLInputElement>(null)
  const emojiAnchorRef = useRef<HTMLDivElement>(null)
  const quickReplyAnchorRef = useRef<HTMLDivElement>(null)
  const quickReplyCommandRef = useRef<HTMLDivElement>(null)
  const productPickerRef = useRef<HTMLDivElement>(null)
  const contactLoadingRef = useRef(false)
  const pushRefreshPendingRef = useRef(false)
  const messageLoadingRef = useRef(false)
  const messageRefreshPendingRef = useRef(false)
  const knownMessageIdsRef = useRef<Set<string>>(new Set())
  const contactsRef = useRef<ChatContact[]>([])
  const processedUnreadJumpRef = useRef(0)
  const selected = contacts.find((item) => item.chatId === selectedId) ?? contacts[0]
  useEffect(() => { contactsRef.current = contacts }, [contacts])
  const accountProducts = products.filter((product) => product.accountId === account?.id && product.status === '已上架')
  const filteredContacts = contacts.filter((item) => {
    const upstreamStatus = `${item.orderStatus} ${item.latestMessage}`
    const matchesStatus = conversationStatus === '全部'
      || (conversationStatus === '待下单' ? !item.orderStatus && !/待付款|待发货|已发货|退款/.test(item.latestMessage) : upstreamStatus.includes(conversationStatus))
    return matchesStatus && `${item.otherUserName}${item.latestMessage}${item.itemTitle}`.toLowerCase().includes(query.toLowerCase())
  })
  const visibleContacts = filteredContacts.slice(0, contactLimit)
  const visibleMessages = messages.slice(messageStart)
  const requestedConversation = (items: ChatContact[]) => unreadJumpRequest.accountId === account?.id && unreadJumpRequest.nonce > processedUnreadJumpRef.current
    ? items.find((contact) => contact.chatId === unreadJumpRequest.chatId) ?? items.find((contact) => contact.unreadCount > 0) ?? items[0]
    : undefined

  const refreshContacts = async (remote = true, cursor: number | null = null) => {
    if (!account) { setContacts([]); setSelectedId(''); return }
    if (contactLoadingRef.current) return
    contactLoadingRef.current = true
    setContactLoading(true); setError('')
    try {
      if (cursor === null) {
        const local = await api.chatContacts(account.id)
        setContacts(local)
        setSelectedId((current) => requestedConversation(local)?.chatId ?? (local.some((item) => item.chatId === current) ? current : local[0]?.chatId ?? ''))
      }
      if (remote) {
        const page = await api.syncChatContacts(account.id, cursor)
        setContacts(page.items)
        setContactCursor(page.nextCursor)
        setContactHasMore(page.hasMore)
        if (cursor !== null) setContactLimit((current) => Math.min(page.items.length, current + CONTACT_BATCH))
        setSelectedId((current) => requestedConversation(page.items)?.chatId ?? (page.items.some((item) => item.chatId === current) ? current : page.items[0]?.chatId ?? ''))
      }
      void onUnreadChanged()
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
    } finally {
      contactLoadingRef.current = false
      setContactLoading(false)
      if (pushRefreshPendingRef.current) {
        pushRefreshPendingRef.current = false
        void refreshContacts(true)
      }
    }
  }

  useEffect(() => {
    setContacts([]); setMessages([]); setSelectedId('')
    setContactLimit(CONTACT_BATCH); setContactCursor(null); setContactHasMore(true)
    void refreshContacts(true)
  }, [account?.id])

  useEffect(() => {
    setQuickReplies([]); setEditingQuickReply(undefined); setQueuedReplyImages([]); setQuickReplyManaging(false)
    if (!account) return
    api.quickReplies(account.id).then(setQuickReplies).catch((nextError) => setError(nextError instanceof Error ? nextError.message : String(nextError)))
  }, [account?.id])

  useEffect(() => { setContactLimit(CONTACT_BATCH) }, [query])
  useEffect(() => { setContactLimit(CONTACT_BATCH) }, [conversationStatus])
  useEffect(() => {
    if (!composerNotice) return
    const timer = window.setTimeout(() => setComposerNotice(''), 3200)
    return () => window.clearTimeout(timer)
  }, [composerNotice])
  useEffect(() => {
    setEmojiOpen(false)
    setQuickReplyOpen(false)
    setQuickReplyCommandOpen(false)
    setComposerNotice('')
  }, [account?.id, selected?.chatId])

  // The native IM listener emits an event as soon as the seller WebSocket
  // receives a conversation/message push. Refresh only the affected account;
  // there is no frontend polling loop while the客服 page is open.
  useEffect(() => {
    if (!account) return
    let cancelled = false
    const refreshFromPush = async () => {
      if (contactLoadingRef.current) {
        pushRefreshPendingRef.current = true
        return
      }
      const selectedChatId = selected?.chatId
      try {
        const localContacts = await api.chatContacts(account.id)
        if (!cancelled) {
          setContacts(localContacts)
          setSelectedId((current) => localContacts.some((contact) => contact.chatId === current) ? current : localContacts[0]?.chatId ?? '')
          void onUnreadChanged()
        }
      } catch {
        // Keep the current list usable when the local cache is unavailable.
      }
      if (selectedChatId && !messageLoadingRef.current) {
        try {
          const localMessages = await api.chatMessages(account.id, selectedChatId)
          if (!cancelled) {
            knownMessageIdsRef.current = new Set(localMessages.map((message) => message.id))
            setMessages(localMessages)
          }
        } catch {
          // The remote refresh below can still recover the conversation.
        }
      }
      if (!selectedChatId) return
      if (messageLoadingRef.current) {
        messageRefreshPendingRef.current = true
        return
      }
      try {
        const previous = new Map(contactsRef.current.map((contact) => [contact.chatId, contact.unreadCount]))
        const contactsPage = await api.syncChatContacts(account.id, null)
        if (!cancelled) {
          const hasNewUnread = contactsPage.items.some((contact) => contact.unreadCount > (previous.get(contact.chatId) ?? 0))
          setContacts(contactsPage.items)
          setContactCursor(contactsPage.nextCursor)
          setContactHasMore(contactsPage.hasMore)
          if (hasNewUnread) playMessageNotification()
          void onUnreadChanged()
        }
        const page = await api.syncChatMessages(account.id, selectedChatId, null)
        if (cancelled) return
        const knownIds = knownMessageIdsRef.current
        const hasNewIncoming = knownIds.size > 0 && page.items.some((message) => message.direction === 'incoming' && !knownIds.has(message.id))
        knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
        setMessages((current) => {
          if (current.length === page.items.length && current.every((message, index) => message.id === page.items[index]?.id)) return current
          return page.items
        })
        setMessageCursor(page.nextCursor)
        setMessageHasMore(page.hasMore)
        if (hasNewIncoming) {
          playMessageNotification()
          requestAnimationFrame(() => {
            const list = messageListRef.current
            if (!list) return
            const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 120
            if (nearBottom) list.scrollTop = list.scrollHeight
          })
        }
      } catch {
        // Keep the local chat usable when the remote message sync is unavailable.
      }
    }
    let unlisten: (() => void) | undefined
    void listen<{ accountId: string }>('chat-im-event', (event) => {
      if (!cancelled && event.payload.accountId === account.id) void refreshFromPush()
    }).then((stop) => {
      if (cancelled) stop()
      else unlisten = stop
    })
    return () => { cancelled = true; unlisten?.() }
  }, [account?.id, selected?.chatId])
  useEffect(() => {
    if (!emojiOpen && !quickReplyOpen && !productPickerOpen && !quickReplyCommandOpen) return
    const closeOnOutsideClick = (event: MouseEvent) => {
      const target = event.target as Node
      if (emojiOpen && !emojiAnchorRef.current?.contains(target)) setEmojiOpen(false)
      if (quickReplyOpen && !quickReplyAnchorRef.current?.contains(target)) setQuickReplyOpen(false)
      if (productPickerOpen && !productPickerRef.current?.contains(target)) setProductPickerOpen(false)
      if (quickReplyCommandOpen && !quickReplyCommandRef.current?.contains(target)) setQuickReplyCommandOpen(false)
    }
    document.addEventListener('mousedown', closeOnOutsideClick)
    return () => document.removeEventListener('mousedown', closeOnOutsideClick)
  }, [emojiOpen, quickReplyOpen, productPickerOpen, quickReplyCommandOpen])

  useEffect(() => {
    if (!account || unreadJumpRequest.accountId !== account.id || unreadJumpRequest.nonce <= processedUnreadJumpRef.current || contacts.length === 0) return
    processedUnreadJumpRef.current = unreadJumpRequest.nonce
    const target = contacts.find((contact) => contact.chatId === unreadJumpRequest.chatId) ?? contacts.find((contact) => contact.unreadCount > 0) ?? contacts[0]
    const targetIndex = contacts.findIndex((contact) => contact.chatId === target.chatId)
    setQuery('')
    setConversationStatus('全部')
    setContactLimit((current) => Math.max(current, targetIndex + 1))
    setSelectedId(target.chatId)
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const list = conversationListRef.current
      if (!list) return
      if (!contacts.some((contact) => contact.unreadCount > 0)) {
        list.scrollTo({ top: 0, behavior: 'smooth' })
        return
      }
      list.querySelector<HTMLElement>(`[data-chat-id="${CSS.escape(target.chatId)}"]`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
    }))
  }, [account?.id, contacts, unreadJumpRequest])

  useEffect(() => {
    if (!account || !selected) return
    setContacts((current) => current.map((contact) => contact.chatId === selected.chatId ? { ...contact, unreadCount: 0 } : contact))
    void api.markChatRead(account.id, selected.chatId)
      .then(() => { onChatRead(account.id, selected.chatId); return onUnreadChanged() })
      .catch((nextError) => { setError(nextError instanceof Error ? nextError.message : String(nextError)) })
  }, [account?.id, selected?.chatId])

  useEffect(() => {
    if (!account || !selected) { setMessages([]); return }
    knownMessageIdsRef.current = new Set()
    let active = true
    const load = async () => {
      messageLoadingRef.current = true
      setMessageLoading(true); setError('')
      try {
        const local = await api.chatMessages(account.id, selected.chatId)
        if (active) {
          setMessages(local)
          knownMessageIdsRef.current = new Set(local.map((message) => message.id))
          setMessageStart(Math.max(0, local.length - MESSAGE_BATCH))
          requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
        }
        const page = await api.syncChatMessages(account.id, selected.chatId, null)
        if (active) {
          setMessages(page.items)
          knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
          setMessageStart(Math.max(0, page.items.length - MESSAGE_BATCH))
          setMessageCursor(page.nextCursor)
          setMessageHasMore(page.hasMore)
          requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
        }
      } catch (nextError) {
        if (active) setError(nextError instanceof Error ? nextError.message : String(nextError))
      } finally {
        messageLoadingRef.current = false
        if (active) setMessageLoading(false)
        if (active && messageRefreshPendingRef.current && account && selected) {
          messageRefreshPendingRef.current = false
          void api.syncChatMessages(account.id, selected.chatId, null).then((page) => {
            if (!active) return
            knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
            setMessages(page.items)
            setMessageCursor(page.nextCursor)
            setMessageHasMore(page.hasMore)
          }).catch(() => undefined)
        }
      }
    }
    setMessages([]); setMessageStart(0); setMessageCursor(null); setMessageHasMore(true)
    void load()
    return () => { active = false }
  }, [account?.id, selected?.chatId])

  const loadMoreContacts = () => {
    if (contactLoadingRef.current) return
    if (contactLimit < filteredContacts.length) {
      setContactLimit((current) => Math.min(filteredContacts.length, current + CONTACT_BATCH))
    } else if (contactHasMore && contactCursor !== null) {
      void refreshContacts(true, contactCursor)
    }
  }

  const loadOlderMessages = async () => {
    const list = messageListRef.current
    if (!list || messageLoadingRef.current || !account || !selected) return
    const oldHeight = list.scrollHeight
    const oldTop = list.scrollTop
    const restorePosition = () => requestAnimationFrame(() => {
      const current = messageListRef.current
      if (current) current.scrollTop = oldTop + current.scrollHeight - oldHeight
    })
    if (messageStart > 0) {
      setMessageStart((current) => Math.max(0, current - MESSAGE_BATCH))
      restorePosition()
      return
    }
    if (!messageHasMore || messageCursor === null) return
    messageLoadingRef.current = true
    setMessageLoading(true); setError('')
    try {
      const page = await api.syncChatMessages(account.id, selected.chatId, messageCursor)
      setMessages(page.items)
      setMessageStart(0)
      setMessageCursor(page.nextCursor)
      setMessageHasMore(page.hasMore)
      restorePosition()
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
    } finally {
      messageLoadingRef.current = false
      setMessageLoading(false)
      if (messageRefreshPendingRef.current && account && selected) {
        messageRefreshPendingRef.current = false
        void api.syncChatMessages(account.id, selected.chatId, null).then((page) => {
          knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
          setMessages(page.items)
          setMessageCursor(page.nextCursor)
          setMessageHasMore(page.hasMore)
        }).catch(() => undefined)
      }
    }
  }

  const send = async () => {
    if (!account || !selected || (!draft.trim() && !queuedReplyImages.length) || busy) return
    setBusy(true); setError('')
    try {
      if (draft.trim()) {
        const message = await api.sendChatMessage(account.id, selected.chatId, selected.otherUserId, draft)
        setMessages((current) => [...current, message])
      }
      for (const image of queuedReplyImages) await sendQuickReplyImage(image)
      setDraft('')
      setQueuedReplyImages([])
      await refreshContacts(false)
      requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
    } finally { setBusy(false) }
  }

  const sendQuickReplyImage = async (image: QuickReplyImage) => {
    if (!account || !selected) return
    const { width, height } = await new Promise<{ width: number; height: number }>((resolve) => {
      const preview = new window.Image()
      preview.onload = () => resolve({ width: preview.naturalWidth || 1, height: preview.naturalHeight || 1 })
      preview.onerror = () => resolve({ width: 1, height: 1 })
      preview.src = image.dataUrl
    })
    const message = await api.sendChatImage(account.id, selected.chatId, selected.otherUserId, image.name, image.mimeType, image.dataUrl, width, height)
    setMessages((current) => [...current, message])
  }

  const sendImage = async (file: File) => {
    if (!account || !selected || busy) return
    if (!file.type.startsWith('image/')) {
      setComposerNotice('请选择 JPG、PNG、WEBP 等图片文件')
      return
    }
    if (file.size > 8 * 1024 * 1024) {
      setComposerNotice('图片不能超过 8MB')
      return
    }
    setBusy(true); setError(''); setComposerNotice('正在上传并发送图片…')
    try {
      const imageData = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('读取图片失败'))
        reader.onerror = () => reject(new Error('读取图片失败'))
        reader.readAsDataURL(file)
      })
      const { width, height } = await new Promise<{ width: number; height: number }>((resolve) => {
        const image = new window.Image()
        image.onload = () => resolve({ width: image.naturalWidth || 1, height: image.naturalHeight || 1 })
        image.onerror = () => resolve({ width: 1, height: 1 })
        image.src = imageData
      })
      const message = await api.sendChatImage(account.id, selected.chatId, selected.otherUserId, file.name, file.type, imageData, width, height)
      setMessages((current) => [...current, message])
      setComposerNotice('图片已发送')
      await refreshContacts(false)
      requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
      setComposerNotice('图片已发送失败')
    } finally { setBusy(false) }
  }

  const insertProduct = (product: Product) => {
    const detail = `【商品】${product.title}${Number.isFinite(product.price) ? ` ¥${product.price}` : ''}`
    setDraft((current) => current ? `${current}\n${detail}` : detail)
    setProductPickerOpen(false)
    setComposerNotice('已插入商品，点击发送即可发送给客户')
  }

  const insertQuickReply = (reply: QuickReply, replaceCommand = false) => {
    const command = draft.match(/(^|\s)\/([a-zA-Z0-9_-]*)$/)
    const replyText = replaceCommand && command
      ? `${draft.slice(0, command.index)}${command[1] || ''}${reply.content}`
      : (draft ? `${draft}\n${reply.content}`.trim() : reply.content)
    setDraft(replyText)
    setQueuedReplyImages(reply.images)
    setQuickReplyOpen(false)
    setQuickReplyCommandOpen(false)
    setComposerNotice(reply.images.length ? `已插入“${reply.title}”的图文内容` : `已插入“${reply.title}”`)
  }

  const saveQuickReply = async (input: { title: string; content: string; shortCode: string; images: QuickReplyImage[] }) => {
    if (!account) return
    try {
      const reply = editingQuickReply
        ? await api.updateQuickReply(editingQuickReply.id, account.id, input.title, input.content, input.shortCode, input.images)
        : await api.createQuickReply(account.id, input.title, input.content, input.shortCode, input.images)
      setQuickReplies((current) => [reply, ...current.filter((item) => item.id !== reply.id)])
      setEditingQuickReply(reply)
      setComposerNotice('快捷回复已保存')
    } catch (nextError) { setError(nextError instanceof Error ? nextError.message : String(nextError)) }
  }

  const deleteQuickReply = async (reply: QuickReply) => {
    try { await api.deleteQuickReply(reply.id); setQuickReplies((current) => current.filter((item) => item.id !== reply.id)); if (editingQuickReply?.id === reply.id) setEditingQuickReply(undefined) }
    catch (nextError) { setError(nextError instanceof Error ? nextError.message : String(nextError)) }
  }

  const commandQuery = draft.match(/(?:^|\s)\/([a-zA-Z0-9_-]*)$/)?.[1]?.toLowerCase() ?? ''
  const matchingQuickReplies = commandQuery !== '' || /(^|\s)\/$/.test(draft)
    ? quickReplies.filter((reply) => reply.shortCode.toLowerCase().includes(commandQuery) || reply.title.toLowerCase().includes(commandQuery))
    : []

  return <div className="workbench">
    <section className="conversation-panel">
      <div className="conversation-status-tabs">{['全部', '待下单', '待付款', '待发货', '已发货', '退款中'].map((status) => <button className={conversationStatus === status ? 'active' : ''} key={status} onClick={() => setConversationStatus(status)}>{status}</button>)}</div>
      <div className="workbench-search"><Search size={17} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索联系人 / 消息 / 商品" /><button className="conversation-sync" onClick={() => void refreshContacts(true, null)} disabled={contactLoading} title="同步会话"><RefreshCw size={14} /></button></div>
      <div className="conversation-list" ref={conversationListRef} onScroll={(event) => { const list = event.currentTarget; if (list.scrollHeight - list.scrollTop - list.clientHeight < 48) loadMoreContacts() }}>{visibleContacts.map((contact) => <button data-chat-id={contact.chatId} className={`conversation ${selected?.chatId === contact.chatId ? 'selected' : ''}`} onClick={() => setSelectedId(contact.chatId)} key={contact.chatId}><ContactAvatar contact={contact} /><div className="conversation-copy"><div className="conversation-title"><strong>{contact.otherUserName}</strong>{contact.orderStatus && <span className={`conversation-trade-status ${contact.orderStatus === '交易成功' ? 'success' : contact.orderStatus === '交易关闭' ? 'closed' : contact.orderStatus === '退款中' ? 'refund' : 'pending'}`}>{contact.orderStatus}</span>}</div><div className="conversation-preview">{contact.buyerTag && <span className="conversation-buyer-tag">{contact.buyerTag}</span>}<span>{contact.latestMessage || '暂无消息'}</span></div><small>{formatRelativeTime(contact.latestMessageTime)}</small></div>{contact.itemImageUrl ? <span className="conversation-product-thumb"><img src={displayImageUrl(contact.itemImageUrl)} alt="" loading="lazy" onError={(event) => event.currentTarget.parentElement?.classList.add('image-error')} /><Package size={19} /></span> : contact.itemId ? <span className="conversation-product-thumb image-error"><Package size={19} /></span> : null}{contact.unreadCount > 0 && <em className="unread-count">{contact.unreadCount > 99 ? '99+' : contact.unreadCount}</em>}</button>)}{contactLoading && <div className="scroll-loader">正在加载更多会话…</div>}{!contactLoading && visibleContacts.length === filteredContacts.length && !contactHasMore && contacts.length > 0 && <div className="scroll-end">已加载全部会话</div>}</div>
    </section>
    <section className="chat-panel">
      <header><div className="chat-identity">{selected && <ContactAvatar contact={selected} />}<div><p>{selected?.otherUserName ?? '请选择会话'}</p><span>{account?.displayName ?? '当前账号'} · 本机闲鱼 IM</span></div></div><AccountPicker account={account} /></header>
      {error && <div className="chat-error">{error}</div>}
      {selected ? <>
        <div className="message-list" ref={messageListRef} onScroll={(event) => { if (event.currentTarget.scrollTop < 24) void loadOlderMessages() }}>{messageLoading && <div className="scroll-loader message-loader">正在加载更早消息…</div>}{!messageLoading && messageStart === 0 && !messageHasMore && messages.length > 0 && <div className="scroll-end">已到达最早消息</div>}{messages.length ? visibleMessages.map((message) => <div className={`message-row ${message.direction}`} key={message.id}><div className={`message-bubble ${message.contentKind === 'image' ? 'image-bubble' : ''}`}>{message.contentKind === 'image' && message.mediaUrl ? <img className="message-image" src={message.mediaUrl} alt="聊天图片" loading="lazy" /> : <p>{message.text || '[' + message.contentKind + ']'}</p>}<small>{formatDate(message.sentAt)} · {message.sendStatus}</small></div></div>) : <div className="chat-blank"><MessageCircle size={38} /><h2>暂无历史消息</h2><p>点击左侧“同步”后会从闲鱼拉取最新会话。</p></div>}</div>
        <footer className="chat-composer">
          <input ref={imageInputRef} className="composer-file-input" type="file" accept="image/*" onChange={(event) => { const file = event.target.files?.[0]; if (file) void sendImage(file); event.currentTarget.value = '' }} />
          <div className="composer-toolbar">
            <div className="composer-tools-left">
              <div className="composer-tool-anchor" ref={emojiAnchorRef}>
                <button className={`composer-tool-button ${emojiOpen ? 'active' : ''}`} type="button" aria-label="表情" title="表情" onClick={() => { setEmojiOpen((current) => !current); setQuickReplyOpen(false) }}><Smile size={19} /></button>
                {emojiOpen && <div className="composer-popover emoji-picker">{[
                  '😀', '😃', '😄', '😁', '😆', '😅', '🤣', '😂', '🙂', '🙃', '😉', '😊',
                  '😇', '🥰', '😍', '🤩', '😘', '😗', '😚', '😋', '😛', '😜', '🤪', '🤗',
                  '🤔', '🫡', '🤭', '🫢', '🤫', '🤐', '😐', '😑', '😶', '🫥', '🙄', '😏',
                  '😣', '😥', '😮', '🤐', '😯', '😪', '😫', '🥱', '😴', '🤤', '😌', '😛',
                  '😜', '😝', '🤤', '😒', '😓', '😔', '😕', '🙃', '🫠', '🙁', '☹️', '😖',
                  '😞', '😟', '😤', '😢', '😭', '😦', '😧', '😨', '😩', '🤯', '😬', '😰',
                  '😱', '🥵', '🥶', '😳', '🤪', '😵', '😡', '😠', '🤬', '😷', '🤒', '🤕',
                  '👍', '👎', '👌', '✌️', '🤞', '🤟', '🤘', '🤙', '👋', '👏', '🙌', '👐',
                  '🤲', '🙏', '💪', '🫶', '❤️', '🧡', '💛', '💚', '💙', '💜', '🖤', '🤍',
                  '💔', '❣️', '💕', '💞', '💓', '💗', '💖', '💘', '💝', '💟', '✨', '💫',
                  '⭐', '🌟', '🔥', '🎉', '🎊', '🥳', '🎁', '✅', '❌', '⚠️', '❓', '❗',
                  '💡', '💯', '💰', '🎯', '🚀', '☀️', '🌈', '☕', '🍵', '🍎', '🍉', '🍔',
                  '🍕', '🍜', '🎂', '🍻', '⚽', '🏀', '🎮', '📱', '💻', '📦', '🛍️', '🧧',
                ].map((emoji, index) => <button type="button" key={`${emoji}-${index}`} onClick={() => { setDraft((current) => current + emoji); setEmojiOpen(false) }}>{emoji}</button>)}</div>}
              </div>
              <button className="composer-tool-button" type="button" aria-label="发送图片" title="发送图片" onClick={() => imageInputRef.current?.click()}><ImageIcon size={19} /></button>
            </div>
            <div className="composer-tools-right">
              <div className="composer-tool-anchor" ref={productPickerRef}>
                <button className={`composer-tool-button ${productPickerOpen ? 'active' : ''}`} type="button" aria-label="插入商品" title="选择当前账号商品" onClick={() => { setProductPickerOpen((current) => !current); setQuickReplyOpen(false); setEmojiOpen(false) }}><ShoppingBag size={19} /></button>
                {productPickerOpen && <div className="composer-popover product-picker"><header><strong>当前账号商品</strong><small>{accountProducts.length} 件在售</small></header>{accountProducts.length ? accountProducts.map((product) => <button type="button" key={product.id} onClick={() => insertProduct(product)}><Package size={16} /><span><strong>{product.title}</strong><small>¥{product.price} · 库存 {product.stock}</small></span></button>) : <p>当前账号暂无已同步的在售商品。</p>}</div>}
              </div>
              <button className="composer-tool-button composer-ai-button" type="button" aria-label="AI 回复未生效" title="AI 回复尚未生效" onClick={() => setComposerNotice('AI 回复尚未生效，请先在设置中完成配置')}><span className="composer-tool-badge">未生效</span><span className="composer-ai-glyph">AI</span></button>
              <div className="composer-tool-anchor" ref={quickReplyAnchorRef}>
                <button className={`composer-tool-button ${quickReplyOpen ? 'active' : ''}`} type="button" aria-label="快捷回复" title="快捷回复" onClick={() => { setQuickReplyOpen((current) => !current); setEmojiOpen(false) }}><span className="composer-quick-glyph"><Zap size={12} /></span></button>
                {quickReplyOpen && <div className="composer-popover quick-reply-picker"><header><strong>快捷回复</strong><button type="button" onClick={() => { setQuickReplyOpen(false); setQuickReplyManaging(true); setEditingQuickReply(undefined) }}>管理</button></header>{quickReplies.length ? quickReplies.map((reply) => <button type="button" key={reply.id} onClick={() => insertQuickReply(reply)}><strong>{reply.title}<code>/{reply.shortCode}</code></strong><span>{reply.content || `含 ${reply.images.length} 张图片`}</span>{reply.images.length > 0 && <em>{reply.images.length} 图</em>}</button>) : <div className="quick-reply-empty"><p>暂无快捷回复</p><button type="button" onClick={() => { setQuickReplyOpen(false); setQuickReplyManaging(true); setEditingQuickReply(undefined) }}>去添加</button></div>}</div>}
              </div>
            </div>
          </div>
          {composerNotice && <div className="composer-tool-notice">{composerNotice}</div>}
          {queuedReplyImages.length > 0 && <div className="queued-reply-images">{queuedReplyImages.map((image, index) => <span key={`${image.name}-${index}`}><img src={image.dataUrl} alt="快捷回复图片" /><button type="button" aria-label="移除快捷回复图片" onClick={() => setQueuedReplyImages((current) => current.filter((_, imageIndex) => imageIndex !== index))}>×</button></span>)}</div>}
          <div className="composer-input-row"><div className="composer-command-anchor" ref={quickReplyCommandRef}><textarea value={draft} onChange={(event) => { const next = event.target.value; setDraft(next); setQuickReplyCommandOpen(/(?:^|\s)\/[a-zA-Z0-9_-]*$/.test(next)) }} onKeyDown={(event) => { if (event.key === 'Escape') { setQuickReplyCommandOpen(false); return } if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); void send() } }} placeholder="输入消息，输入 /简码 可插入快捷回复" />{quickReplyCommandOpen && <div className="quick-command-menu"><header><strong>快捷指令</strong><small>输入 /{commandQuery} 筛选</small></header>{matchingQuickReplies.length ? matchingQuickReplies.map((reply) => <button type="button" key={reply.id} onClick={() => insertQuickReply(reply, true)}><span><strong>{reply.title}</strong><small>/{reply.shortCode}</small></span><em>{reply.images.length ? `${reply.images.length} 图` : '文字'}</em></button>) : <p>没有匹配的快捷回复</p>}</div>}</div><button className="primary" disabled={(!draft.trim() && !queuedReplyImages.length) || busy} onClick={() => void send()}><Send size={15} />{busy ? '处理中' : '发送'}</button></div>
        </footer>
      </> : <div className="chat-blank"><MessageCircle size={42} /><h2>{account ? '暂无会话' : '请先添加账号'}</h2><p>{account ? '确保账号已扫码登录，然后点击左侧同步。' : '扫码登录后即可同步真实会话。'}</p></div>}
    </section>
    <aside className={`context-panel ${quickReplyManaging ? 'quick-reply-manager-panel' : ''}`}>{quickReplyManaging ? <QuickReplyManager replies={quickReplies} value={editingQuickReply} onClose={() => { setQuickReplyManaging(false); setEditingQuickReply(undefined) }} onSave={saveQuickReply} onEdit={setEditingQuickReply} onDelete={(reply) => void deleteQuickReply(reply)} /> : <><div className="context-tabs"><b>客户</b><span>商品</span><span>账号</span></div>{selected ? <div className="context-order"><h3>会话上下文</h3><dl><dt>所属账号</dt><dd>{account?.displayName ?? '—'}</dd><dt>客户</dt><dd>{selected.otherUserName}</dd><dt>闲鱼用户 ID</dt><dd>{selected.otherUserId}</dd><dt>关联商品</dt><dd>{selected.itemTitle || selected.itemId || '未识别'}</dd><dt>未读</dt><dd>{selected.unreadCount}</dd></dl></div> : <div className="context-empty"><img src={logo} alt="" /><p>选择会话后查看上下文。</p></div>}</>}</aside>
  </div>
}

function ContactAvatar({ contact }: { contact: ChatContact }) {
  const isNotice = contact.otherUserName === '通知消息' || contact.chatId.toLowerCase().includes('notice')
  if (isNotice) return <div className="avatar contact-avatar system-avatar"><Bell size={22} /></div>
  const fallback = contact.otherUserName.trim().slice(0, 1) || '?'
  return <div className="avatar contact-avatar"><span>{fallback}</span>{contact.avatarUrl && <img src={displayImageUrl(contact.avatarUrl)} alt="" loading="lazy" onError={(event) => event.currentTarget.remove()} />}</div>
}

function QuickReplyManager({ replies, value, onClose, onSave, onEdit, onDelete }: { replies: QuickReply[]; value?: QuickReply; onClose: () => void; onSave: (input: { title: string; content: string; shortCode: string; images: QuickReplyImage[] }) => void; onEdit: (reply: QuickReply | undefined) => void; onDelete: (reply: QuickReply) => void }) {
  const [title, setTitle] = useState('')
  const [content, setContent] = useState('')
  const [shortCode, setShortCode] = useState('')
  const [images, setImages] = useState<QuickReplyImage[]>([])
  const [mode, setMode] = useState<'list' | 'editor'>('list')
  const fileRef = useRef<HTMLInputElement>(null)
  useEffect(() => { setTitle(value?.title ?? ''); setContent(value?.content ?? ''); setShortCode(value?.shortCode ?? ''); setImages(value?.images ?? []) }, [value?.id])
  const beginCreate = () => { onEdit(undefined); setTitle(''); setContent(''); setShortCode(''); setImages([]); setMode('editor') }
  const beginEdit = (reply: QuickReply) => { onEdit(reply); setMode('editor') }
  const chooseImages = async (files: FileList | null) => {
    const valid = Array.from(files ?? []).filter((file) => file.type.startsWith('image/') && file.size <= 5 * 1024 * 1024).slice(0, Math.max(0, 6 - images.length))
    const next = await Promise.all(valid.map((file) => new Promise<QuickReplyImage>((resolve, reject) => { const reader = new FileReader(); reader.onload = () => typeof reader.result === 'string' ? resolve({ name: file.name, mimeType: file.type, dataUrl: reader.result }) : reject(new Error('图片读取失败')); reader.onerror = () => reject(new Error('图片读取失败')); reader.readAsDataURL(file) })))
    setImages((current) => [...current, ...next])
  }
  const save = () => { if (!title.trim() || !shortCode.trim() || (!content.trim() && !images.length)) return; onSave({ title, content, shortCode, images }) }
  return <div className="quick-reply-manager">
    <header><div><strong>快捷回复</strong><small>{replies.length} 条 · 输入 /简码 快速插入</small></div><button type="button" className="icon-text-button" onClick={onClose}><X size={16} />关闭</button></header>
    {mode === 'list' ? <div className="quick-reply-manager-list quick-reply-manager-all"><button className="quick-reply-new" type="button" onClick={beginCreate}><Plus size={15} />新增快捷回复</button>{replies.length ? replies.map((reply) => <button type="button" key={reply.id} onClick={() => beginEdit(reply)}><span><strong>{reply.title}</strong><small>/{reply.shortCode} · {reply.images.length ? `${reply.images.length} 图` : '文字'}</small></span><Pencil size={13} /></button>) : <p>暂无快捷回复，点击上方按钮添加。</p>}</div> : <div className="quick-reply-editor"><div className="quick-reply-editor-heading"><h3>{value ? '编辑快捷回复' : '新增快捷回复'}</h3><button type="button" onClick={() => setMode('list')}>返回列表</button></div><label>标题<input value={title} onChange={(event) => setTitle(event.target.value)} placeholder="例如：欢迎咨询" /></label><label>快捷回复简码<input value={shortCode} onChange={(event) => setShortCode(event.target.value.replace(/^\/+/, ''))} placeholder="例如：hello（输入 /hello 使用）" /></label><label>图文内容<textarea value={content} onChange={(event) => setContent(event.target.value)} placeholder="输入文字内容；可在下方添加图片，发送时按文字、图片顺序发送。" rows={5} /></label><input ref={fileRef} className="composer-file-input" type="file" accept="image/*" multiple onChange={(event) => { void chooseImages(event.target.files); event.currentTarget.value = '' }} /><div className="quick-reply-images"><div><strong>图片附件</strong><button type="button" onClick={() => fileRef.current?.click()} disabled={images.length >= 6}><ImageIcon size={14} />添加图片</button></div>{images.length ? <div className="quick-reply-image-grid">{images.map((image, index) => <span key={`${image.name}-${index}`}><img src={image.dataUrl} alt={image.name} /><button type="button" aria-label="移除图片" onClick={() => setImages((current) => current.filter((_, imageIndex) => imageIndex !== index))}>×</button></span>)}</div> : <p>支持图片与文字组合，最多 6 张图片。</p>}</div><div className="quick-reply-editor-actions">{value && <button type="button" className="danger-text" onClick={() => { onDelete(value); setMode('list') }}><Trash2 size={14} />删除</button>}<button type="button" className="primary" onClick={save}>保存快捷回复</button></div></div>}
  </div>
}

function SettingsPage({ onExport }: { onExport: () => void }) { return <div className="page settings-page"><PageHead eyebrow="应用设置" title="本地优先，安全可控" description="账号会话、商品、订单和同步记录都保存在当前电脑。" />
  <section className="settings-card"><div><h3>导出本地备份</h3><p>导出账号资料、商品和订单为 JSON 文件，便于迁移与排查。</p></div><button className="secondary" onClick={onExport}><Download size={17} />导出备份</button></section>
  <section className="settings-card"><div><h3>本机闲鱼连接器</h3><p>扫码登录、商品和订单同步直接在鲨鱼管家中运行，不依赖 HLSRental 服务。</p></div><Store size={26} /></section>
  <section className="settings-card"><div><h3>关于鲨鱼管家</h3><p>v0.1.0 · Tauri 跨平台桌面应用 · macOS / Windows / Linux</p></div><img src={logo} alt="鲨鱼管家" /></section>
</div> }

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: React.ReactNode }) { return <div className="page-head"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{description}</p></div>{action}</div> }
function Toolbar({ account, placeholder }: { account?: Account; placeholder: string }) { return <div className="toolbar"><div className="search-field"><Search size={17} /><input placeholder={placeholder} /></div><AccountPicker account={account} /><button className="filter-button">全部状态<ChevronDown size={15} /></button></div> }
