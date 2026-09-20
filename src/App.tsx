import { useEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { listen } from '@tauri-apps/api/event'
import { isTauri } from '@tauri-apps/api/core'
import {
  Bell, CheckCircle2, ChevronDown, CircleHelp, Clipboard, ClipboardList, Download, ExternalLink,
  History, Image as ImageIcon, LayoutDashboard, LogIn, MessageCircle, Package, Pencil,
  Pin, Plus, RefreshCw, Search, Send, Settings, ShieldCheck, ShoppingBag, Smile,
  Store, Trash2, UserRound, UsersRound, Upload, X, Zap, ListPlus,
} from 'lucide-react'
import logo from './assets/shark-butler-logo.png'
import { api } from './lib/api'
import type { Account, AccountInput, AppLog, ChatContact, ChatEmoji, ChatMessage, CustomerItem, CustomerProfile, DashboardStats, Order, OrderInput, Product, ProductInput, QrLoginStart, QrLoginStatus, QuickReply, QuickReplyImage, SyncJob } from './lib/types'

type Page = 'dashboard' | 'workbench' | 'accounts' | 'products' | 'orders' | 'settings'
type Dialog =
  | { kind: 'account'; value?: Account }
  | { kind: 'product'; value?: Product }
  | { kind: 'order'; value?: Order }
  | { kind: 'conversation-name'; value: Account }
  | { kind: 'delete-account'; value: Account }
  | { kind: 'qr' }
  | null

type TradeDrawer =
  | { kind: 'ship'; order: Order }
  | { kind: 'cancel'; order: Order }
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

async function copyText(value: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value)
    return
  }
  const input = document.createElement('textarea')
  input.value = value
  input.style.position = 'fixed'
  input.style.opacity = '0'
  document.body.appendChild(input)
  input.select()
  document.execCommand('copy')
  input.remove()
}

async function copyOrderNumber(value: string) {
  try {
    await copyText(value)
    window.dispatchEvent(new CustomEvent('app-notice', { detail: '复制成功' }))
  } catch {
    window.dispatchEvent(new CustomEvent('app-notice', { detail: '复制失败，请重试' }))
  }
}

function formatEventTime(value: string) {
  if (!value) return ''
  const date = new Date(value)
  const parts = new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).formatToParts(date)
  const get = (type: Intl.DateTimeFormatPartTypes) => parts.find((part) => part.type === type)?.value ?? ''
  return `${get('month')}-${get('day')} ${get('hour')}:${get('minute')}`
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


type NotificationSoundId = `preset-${number}` | 'custom'
type NotificationSoundGroup = 'standard'
type NotificationSoundPreset = {
  id: NotificationSoundId
  name: string
  description: string
  accent: string
  group: NotificationSoundGroup
  sequence: Array<{ frequency: number; start: number; duration: number; type: OscillatorType }>
}

// 内置提示音均控制在 1～3 秒内，避免通知过长打扰客服工作。
const notificationSoundPresets: NotificationSoundPreset[] = [
  { id: 'preset-1', group: 'standard', name: '清脆提示', description: '轻快双音', accent: '#ffd400', sequence: [{ frequency: 660, start: 0, duration: .22, type: 'sine' }, { frequency: 880, start: .18, duration: .95, type: 'sine' }] },
  { id: 'preset-2', group: 'standard', name: '柔和铃声', description: '温和三音', accent: '#ffc56b', sequence: [{ frequency: 523, start: 0, duration: .25, type: 'triangle' }, { frequency: 659, start: .2, duration: .3, type: 'triangle' }, { frequency: 784, start: .42, duration: .9, type: 'triangle' }] },
  { id: 'preset-3', group: 'standard', name: '木琴短句', description: '明亮上行', accent: '#ff9e80', sequence: [{ frequency: 392, start: 0, duration: .18, type: 'triangle' }, { frequency: 494, start: .16, duration: .18, type: 'triangle' }, { frequency: 587, start: .32, duration: .18, type: 'triangle' }, { frequency: 784, start: .48, duration: .8, type: 'triangle' }] },
  { id: 'preset-4', group: 'standard', name: '气泡提醒', description: '短促双跳', accent: '#7dd3fc', sequence: [{ frequency: 740, start: 0, duration: .16, type: 'sine' }, { frequency: 988, start: .15, duration: .95, type: 'sine' }] },
  { id: 'preset-5', group: 'standard', name: '温暖和弦', description: '柔和叠音', accent: '#86efac', sequence: [{ frequency: 523, start: 0, duration: .46, type: 'sine' }, { frequency: 659, start: .03, duration: .46, type: 'sine' }, { frequency: 784, start: .06, duration: .95, type: 'sine' }] },
  { id: 'preset-6', group: 'standard', name: '轻铃回响', description: '两段回响', accent: '#c4b5fd', sequence: [{ frequency: 587, start: 0, duration: .22, type: 'triangle' }, { frequency: 880, start: .28, duration: .26, type: 'triangle' }, { frequency: 1175, start: .54, duration: .65, type: 'triangle' }] },
  { id: 'preset-7', group: 'standard', name: '订单到达', description: '稳重三拍', accent: '#fda4af', sequence: [{ frequency: 440, start: 0, duration: .2, type: 'square' }, { frequency: 554, start: .24, duration: .2, type: 'square' }, { frequency: 659, start: .48, duration: .7, type: 'square' }] },
  { id: 'preset-8', group: 'standard', name: '消息泡泡', description: '清亮上扬', accent: '#67e8f9', sequence: [{ frequency: 494, start: 0, duration: .18, type: 'sine' }, { frequency: 659, start: .16, duration: .18, type: 'sine' }, { frequency: 988, start: .32, duration: .8, type: 'sine' }] },
  { id: 'preset-9', group: 'standard', name: '安心提示', description: '低调双音', accent: '#a7f3d0', sequence: [{ frequency: 349, start: 0, duration: .35, type: 'triangle' }, { frequency: 523, start: .3, duration: .8, type: 'triangle' }] },
  { id: 'preset-10', group: 'standard', name: '闪电提醒', description: '快速三连', accent: '#fcd34d', sequence: [{ frequency: 784, start: 0, duration: .13, type: 'sawtooth' }, { frequency: 988, start: .14, duration: .13, type: 'sawtooth' }, { frequency: 1175, start: .28, duration: .85, type: 'sawtooth' }] },
]
const notificationSoundGroups: Array<{ id: NotificationSoundGroup; label: string; description: string }> = [
  { id: 'standard', label: '常用提示音', description: '日常消息提醒' },
]

const notificationSoundStorageKey = 'shark-butler-notification-sound'
const notificationCustomSoundStorageKey = 'shark-butler-notification-custom-sound'
function selectedNotificationSoundId(): NotificationSoundId {
  const value = localStorage.getItem(notificationSoundStorageKey)
  return value === 'custom' || notificationSoundPresets.some((item) => item.id === value) ? value as NotificationSoundId : 'preset-1'
}

function playToneSequence(context: AudioContext, sequence: NotificationSoundPreset['sequence']) {
  const now = context.currentTime
  sequence.forEach(({ frequency, start, duration, type }) => {
    const oscillator = context.createOscillator()
    const gain = context.createGain()
    oscillator.type = type
    oscillator.frequency.setValueAtTime(frequency, now + start)
    gain.gain.setValueAtTime(0.0001, now + start)
    gain.gain.exponentialRampToValueAtTime(0.22, now + start + .015)
    gain.gain.exponentialRampToValueAtTime(0.0001, now + start + duration)
    oscillator.connect(gain)
    gain.connect(context.destination)
    oscillator.start(now + start)
    oscillator.stop(now + start + duration + .02)
  })
}

function playMessageNotification(category: 'chat' | 'order' | 'system' = 'chat') {
  try {
    const toggleMap = JSON.parse(localStorage.getItem('shark-butler-notification-toggles') || '{"chat":true,"order":true,"system":true}') as Record<string, boolean>
    if (toggleMap[category] === false) return
    const selected = selectedNotificationSoundId()
    if (selected === 'custom') {
      const customDataUrl = localStorage.getItem(notificationCustomSoundStorageKey)
      if (customDataUrl) {
        const audio = new Audio(customDataUrl)
        audio.volume = .85
        void audio.play().catch(() => undefined)
        return
      }
    }
    const selectedPreset = notificationSoundPresets.find((item) => item.id === selected)
    const AudioContextCtor = window.AudioContext || (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    if (!AudioContextCtor) return
    messageNotificationContext ??= new AudioContextCtor()
    const context = messageNotificationContext
    const play = () => {
      const preset = selectedPreset ?? notificationSoundPresets[0]
      playToneSequence(context, preset.sequence)
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
  return <MainApp />
}

function MainApp() {
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
  const [logManagerOpen, setLogManagerOpen] = useState(false)
  const [globalMessages, setGlobalMessages] = useState<Array<{ account: Account; contact: ChatContact }>>([])
  const [accountMenuPosition, setAccountMenuPosition] = useState({ top: 0, left: 0, width: 310 })
  const accountMenuButtonRef = useRef<HTMLButtonElement>(null)
  const [unreadTotals, setUnreadTotals] = useState<Record<string, number>>({})
  const [imStatuses, setImStatuses] = useState<Record<string, string>>({})
  const [unreadJumpRequest, setUnreadJumpRequest] = useState({ accountId: '', chatId: '', nonce: 0 })
  const [quickReplyAutoSuggest, setQuickReplyAutoSuggest] = useState(() => localStorage.getItem('shark-butler-quick-reply-auto-suggest') === 'true')
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

  // Workbench background updates only need the order records. Keeping this
  // separate from refresh() avoids toggling the app-wide loading screen and
  // remounting the active conversation while a related order is syncing.
  const refreshWorkbenchOrders = async () => {
    try { setOrders(await api.orders()) } catch { /* The existing cards remain usable. */ }
  }

  useEffect(() => { void refresh() }, [])

  useEffect(() => {
    localStorage.setItem('shark-butler-conversation-tabs', JSON.stringify(conversationTabIds))
  }, [conversationTabIds])

  useEffect(() => {
    localStorage.setItem('shark-butler-quick-reply-auto-suggest', String(quickReplyAutoSuggest))
  }, [quickReplyAutoSuggest])

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

  useEffect(() => {
    const handleNotice = (event: Event) => {
      const message = (event as CustomEvent<string>).detail
      if (message) setNotice(message)
    }
    window.addEventListener('app-notice', handleNotice)
    return () => window.removeEventListener('app-notice', handleNotice)
  }, [])

  useEffect(() => {
    const disableBrowserMenu = (event: MouseEvent) => event.preventDefault()
    document.addEventListener('contextmenu', disableBrowserMenu)
    return () => document.removeEventListener('contextmenu', disableBrowserMenu)
  }, [])

  const filteredProducts = useMemo(() => accountId ? products.filter((item) => item.accountId === accountId) : products, [products, accountId])
  const filteredOrders = useMemo(() => accountId ? orders.filter((item) => item.accountId === accountId) : orders, [orders, accountId])
  const globalUnreadCount = Object.values(unreadTotals).reduce((total, count) => total + Math.max(0, count), 0)
  const handleChatRead = (readAccountId: string, readChatId: string, readCount: number) => {
    setUnreadTotals((current) => ({
      ...current,
      [readAccountId]: Math.max(0, (current[readAccountId] ?? 0) - Math.max(0, readCount)),
    }))
    setGlobalMessages((current) => current.filter((item) => !(item.account.id === readAccountId && item.contact.chatId === readChatId)))
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
    <main className={`shell ${page === 'settings' ? 'settings-shell' : ''}`}>
      <aside className="rail" aria-label="主导航">
        <button className="brand-mark" onClick={() => setPage('dashboard')} aria-label="鲨鱼管家首页">
          <img src={logo} alt="鲨鱼管家" />
        </button>
        <nav>{nav.slice(0, 5).map(({ id, label, icon: Icon }) => (
          <button key={id} className={`rail-link ${page === id ? 'active' : ''}`} onClick={() => setPage(id)}>
            <span className="rail-link-icon"><Icon size={21} />{id === 'workbench' && globalUnreadCount > 0 && <em className="rail-unread-badge" aria-label={`${globalUnreadCount} 条未读消息`}>{globalUnreadCount > 99 ? '99+' : globalUnreadCount}</em>}</span><span>{label}</span>
          </button>
        ))}</nav>
        <div className="rail-bottom">
          <button className={`rail-link ${page === 'settings' ? 'active' : ''}`} onClick={() => setPage('settings')}><Settings size={21} /><span>设置</span></button>
          <button className="rail-link" onClick={() => setNotice('鲨鱼管家 v0.1 · 本地优先工作台')}><CircleHelp size={21} /><span>帮助</span></button>
        </div>
      </aside>

      <section className="app-column">
        <header className={`topbar ${page === 'settings' ? 'settings-topbar' : ''}`}>
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

        <div className={`content ${page === 'workbench' ? 'workbench-content' : ''} ${page === 'settings' ? 'settings-content' : ''}`}>
          {loading ? <Loading /> : page === 'dashboard' ? <Dashboard stats={stats} accounts={accounts} orders={orders} onGo={setPage} />
            : page === 'workbench' ? <Workbench key={activeAccount?.id ?? 'empty'} account={activeAccount} products={products} orders={orders} onOrderUpdated={() => void refreshWorkbenchOrders()} imConnected={Boolean(activeAccount?.remoteAccountId)} quickReplyAutoSuggest={quickReplyAutoSuggest} onUnreadChanged={refreshUnreadTotals} onChatRead={handleChatRead} unreadJumpRequest={unreadJumpRequest} />
              : page === 'accounts' ? <Accounts accounts={accounts} syncJobs={syncJobs} imStatuses={imStatuses} onQrLogin={() => setDialog({ kind: 'qr' })} onEdit={(value) => setDialog({ kind: 'account', value })} onDelete={(value) => setDialog({ kind: 'delete-account', value })} onSync={(account) => void syncAccount(account)} onSetStatus={(ids, status) => void setAccountsStatus(ids, status)} />
                : page === 'products' ? <Products items={filteredProducts} account={activeAccount} onSync={() => void syncAccount()} onBulk={(ids, action) => void bulkProducts(ids, action)} onAdd={() => setDialog({ kind: 'product' })} onEdit={(value) => setDialog({ kind: 'product', value })} onDelete={(id) => void remove('product', id)} />
                  : page === 'orders' ? <Orders items={filteredOrders} account={activeAccount} onSync={() => void syncAccount()} onBulk={(ids, status) => void bulkOrders(ids, status)} onAdd={() => setDialog({ kind: 'order' })} onEdit={(value) => setDialog({ kind: 'order', value })} onDelete={(id) => void remove('order', id)} />
                    : <SettingsPage quickReplyAutoSuggest={quickReplyAutoSuggest} onQuickReplyAutoSuggestChange={setQuickReplyAutoSuggest} onExport={() => void exportBackup()} onOpenLogs={() => setLogManagerOpen(true)} onQrLogin={() => setDialog({ kind: 'qr' })} />}
        </div>
        {dialog?.kind === 'account' && <AccountDialog value={dialog.value} onClose={() => setDialog(null)} onSave={saveAccount} />}
        {dialog?.kind === 'product' && <ProductDialog accounts={accounts} selectedAccountId={activeAccount?.id} value={dialog.value} onClose={() => setDialog(null)} onSave={saveProduct} />}
        {dialog?.kind === 'order' && <OrderDialog accounts={accounts} selectedAccountId={activeAccount?.id} value={dialog.value} onClose={() => setDialog(null)} onSave={saveOrder} />}
        {dialog?.kind === 'conversation-name' && <ConversationNameDialog account={dialog.value} onClose={() => setDialog(null)} onSave={saveConversationName} />}
        {dialog?.kind === 'delete-account' && <DeleteAccountDialog account={dialog.value} onClose={() => setDialog(null)} onDelete={deleteAccount} />}
        {logManagerOpen && <LogManager onClose={() => setLogManagerOpen(false)} />}
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
    <section className="table-panel"><table><thead><tr><th><input aria-label="全选商品" type="checkbox" checked={Boolean(visible.length) && visible.every((item) => selected.includes(item.id))} onChange={toggleAll} /></th><th>商品</th><th>价格</th><th>库存</th><th>状态</th><th>本地标签</th><th>最后同步</th><th>操作</th></tr></thead><tbody>{visible.map((item) => <tr key={item.id}><td><input aria-label={`选择 ${item.title}`} type="checkbox" checked={selected.includes(item.id)} onChange={() => toggle(item.id)} /></td><td><div className="product-cell"><div className="product-image">{item.imageUrl ? <img src={displayImageUrl(item.imageUrl)} alt={item.title} loading="lazy" /> : <Package size={20} />}</div><div><strong>{item.title}</strong><span>ID · {item.id}</span></div></div></td><td>¥{item.price.toFixed(2)}</td><td>{item.stock}</td><td><Status value={item.status} /></td><td><div className="tags">{item.tags.map((tag) => <span key={tag}>{tag}</span>)}</div></td><td>{formatDate(item.updatedAt)}</td><td><div className="row-actions"><button className="link-button" onClick={() => onEdit(item)}>编辑</button><button className="danger-link" onClick={() => onDelete(item.id)}>删除</button></div></td></tr>)}</tbody></table>{visible.length === 0 && <EmptyTable text="没有符合条件的商品" />}</section>
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
  const [accountId, setAccountId] = useState(value?.accountId ?? selectedAccountId ?? accounts[0]?.id ?? ''); const [title, setTitle] = useState(value?.title ?? ''); const [imageUrl, setImageUrl] = useState(value?.imageUrl ?? ''); const [price, setPrice] = useState(String(value?.price ?? '')); const [stock, setStock] = useState(String(value?.stock ?? '0')); const [status, setStatus] = useState(value?.status ?? '已上架'); const [tags, setTags] = useState(value?.tags.join(', ') ?? '')
  return <Modal title={value ? '编辑商品' : '新建商品'} onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave({ accountId, title, imageUrl, price: Number(price), stock: Number(stock), status, tags: tags.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean) }, value) }}><label>所属账号<select required value={accountId} onChange={(event) => setAccountId(event.target.value)}>{accounts.map((account) => <option value={account.id} key={account.id}>{account.displayName}</option>)}</select></label><label>商品标题<input required autoFocus value={title} onChange={(event) => setTitle(event.target.value)} /></label><label>商品图片地址<input type="url" value={imageUrl} onChange={(event) => setImageUrl(event.target.value)} placeholder="https://..." /></label><div className="form-row"><label>价格（元）<input required min="0" step="0.01" type="number" value={price} onChange={(event) => setPrice(event.target.value)} /></label><label>库存<input required min="0" step="1" type="number" value={stock} onChange={(event) => setStock(event.target.value)} /></label></div><div className="form-row"><label>状态<select value={status} onChange={(event) => setStatus(event.target.value)}><option>已上架</option><option>已下架</option></select></label><label>标签（逗号分隔）<input value={tags} onChange={(event) => setTags(event.target.value)} placeholder="数码, 热销" /></label></div><div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存商品</button></div></form></Modal>
}

function OrderDialog({ accounts, selectedAccountId, value, onClose, onSave }: { accounts: Account[]; selectedAccountId?: string; value?: Order; onClose: () => void; onSave: (input: OrderInput, current?: Order) => void }) {
  const [accountId, setAccountId] = useState(value?.accountId ?? selectedAccountId ?? accounts[0]?.id ?? ''); const [productTitle, setProductTitle] = useState(value?.productTitle ?? ''); const [buyerMaskedName, setBuyerMaskedName] = useState(value?.buyerMaskedName ?? ''); const [amount, setAmount] = useState(String(value?.amount ?? '')); const [status, setStatus] = useState(value?.status ?? '待付款'); const [note, setNote] = useState(value?.note ?? '')
  return <Modal title={value ? `处理订单 ${value.orderNo}` : '录入订单'} onClose={onClose}><form className="form-grid" onSubmit={(event) => { event.preventDefault(); onSave({ accountId, productTitle, buyerMaskedName, amount: Number(amount), status, note }, value) }}><label>所属账号<select disabled={Boolean(value)} required value={accountId} onChange={(event) => setAccountId(event.target.value)}>{accounts.map((account) => <option value={account.id} key={account.id}>{account.displayName}</option>)}</select></label><label>商品名称<input disabled={Boolean(value)} required autoFocus value={productTitle} onChange={(event) => setProductTitle(event.target.value)} /></label><div className="form-row"><label>买家标识<input disabled={Boolean(value)} required value={buyerMaskedName} onChange={(event) => setBuyerMaskedName(event.target.value)} placeholder="例如：张**" /></label><label>订单金额（元）<input disabled={Boolean(value)} required min="0" step="0.01" type="number" value={amount} onChange={(event) => setAmount(event.target.value)} /></label></div><label>订单状态<select value={status} onChange={(event) => setStatus(event.target.value)}><option>待付款</option><option>待发货</option><option>待收货</option><option>已完成</option><option>退款中</option><option>已退款</option><option>已关闭</option></select></label><label>内部备注<textarea value={note} onChange={(event) => setNote(event.target.value)} placeholder="仅保存在本机" rows={3} /></label><div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>取消</button><button className="primary" type="submit">保存订单</button></div></form></Modal>
}

function OrderDetailModal({ order, product, onClose }: { order: Order; product?: Product; onClose: () => void }) {
  const meta = orderStatusMeta(order.status)
  const detailDate = order.createdAt ? new Intl.DateTimeFormat('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(new Date(order.createdAt)) : '—'
  const timeline = [
    { label: '已拍下', done: true },
    { label: '已付款', done: meta.success || meta.refunded || /待发货|待收货|已完成/.test(order.status) },
    { label: '已发货', done: meta.success || meta.refunded || /待收货/.test(order.status) },
    { label: meta.success ? '交易成功' : meta.refunded ? '交易关闭' : '交易完成', done: meta.success || meta.refunded },
    { label: '已评价', done: false },
  ]
  return <div className="order-detail-drawer-backdrop" role="presentation" onMouseDown={onClose}><aside className="order-detail-drawer" role="dialog" aria-modal="true" aria-label="订单详情" onMouseDown={(event) => event.stopPropagation()}><header><div><span>订单详情</span><small>订单编号 {order.orderNo}</small></div><button className="icon-button" onClick={onClose}>×</button></header><div className="order-detail-modal">
    {!meta.closed && <div className="order-detail-progress" aria-label="订单进度">{timeline.map((step, index) => <div className={`order-detail-step ${step.done ? 'done' : ''}`} key={step.label}><span className="order-detail-step-dot">{step.done ? '✓' : index + 1}</span><strong>{step.label}</strong><small>{step.done ? detailDate : '—'}</small>{index < timeline.length - 1 && <i />}</div>)}</div>}
    <section className={`order-detail-summary ${meta.closed ? 'closed' : ''}`}><h3>{meta.label}</h3>{meta.closed && <p className="order-detail-status-desc">{meta.refunded ? '退款成功' : '买家取消了订单'}</p>}{meta.success && <div className="order-detail-actions"><button type="button" className="primary">查看评价</button><button type="button" className="secondary">查看钱款</button></div>}<div className="order-detail-note"><span>备注：{order.note || '暂无'}</span><button type="button" onClick={() => void copyOrderNumber(order.orderNo)}><Clipboard size={13} />复制订单号</button></div></section>
    <section className="order-detail-section"><h4>订单信息</h4><div className="order-detail-info-grid"><div><h5>交易信息</h5><dl><div><dt>订单编号</dt><dd>{order.orderNo} <button type="button" onClick={() => void copyOrderNumber(order.orderNo)} title="复制订单编号"><Clipboard size={12} /></button></dd></div>{meta.closed ? <><div><dt>支付宝交易号</dt><dd>—</dd></div><div><dt>下单时间</dt><dd>{detailDate}</dd></div><div><dt>交易关闭时间</dt><dd>{detailDate}</dd></div></> : <><div><dt>下单时间</dt><dd>{detailDate}</dd></div><div><dt>付款时间</dt><dd>{meta.success || meta.refunded ? detailDate : '—'}</dd></div><div><dt>发货时间</dt><dd>{meta.success || meta.refunded ? detailDate : '—'}</dd></div><div><dt>成交时间</dt><dd>{meta.success ? detailDate : '—'}</dd></div></>}</dl></div><div><h5>买家信息</h5><dl><div><dt>买家昵称</dt><dd>{order.buyerMaskedName || '—'}</dd></div><div><dt>收货信息</dt><dd>已隐藏</dd></div></dl></div></div></section>
    <section className="order-detail-section"><h4>商品信息</h4><div className="order-detail-product-table"><div className="order-detail-product-head"><span>商品信息</span><span>单价/数量</span><span>优惠</span></div><div className="order-detail-product-row"><span className="related-order-image">{product?.imageUrl ? <img src={displayImageUrl(product.imageUrl)} alt="" /> : <Package size={22} />}</span><div className="order-detail-product-name"><strong>{product?.title || order.productTitle || '未命名商品'}</strong><small>规格：一天</small></div><div className="order-detail-product-price"><b>¥{(product?.price ?? order.amount).toFixed(2)}</b><span>×1</span></div><span>—</span></div><div className="order-detail-totals"><div><span>成交价</span><b>¥{order.amount.toFixed(2)}</b></div><div><span>软件服务费</span><b>-¥0.03</b></div><div><span>预计到手</span><b className="price">¥{Math.max(0, order.amount - 0.03).toFixed(2)}</b></div></div></div></section>
    <div className="modal-actions"><button type="button" className="secondary" onClick={onClose}>关闭</button></div>
  </div></aside></div>
}

function MessageText({ text, emojis }: { text: string; emojis: ChatEmoji[] }) {
  const emojiByAlias = useMemo(() => new Map(emojis.map((emoji) => [emoji.iconAlias, emoji.iconUrl])), [emojis])
  const parts = text.split(/(\[[^\]\r\n]{1,40}\])/g)
  return <p className="message-text">{parts.map((part, index) => {
    const iconUrl = emojiByAlias.get(part)
    return iconUrl
      ? <img className="message-inline-emoji" src={displayImageUrl(iconUrl)} alt={part} title={part} key={`${part}-${index}`} loading="lazy" />
      : <span key={`${part}-${index}`}>{part}</span>
  })}</p>
}

type TradeCardMeta = { title: string; subtitle: string; action: string }

function systemNoticeText(message: ChatMessage) {
  const raw = (message.cardTitle || message.text).replace(/^\[|\]$/g, '').trim()
  if (/^[你您]已发货$/.test(raw)) return '您已发货'
  if (/^买家(?:已)?确认收货[，,、 ]*交易成功$/.test(raw)) return '买家确认收货，交易成功'
  if (/^退款成功[，,、 ]*钱款已原路退返$/.test(raw)) return '退款成功，钱款已原路退返'
  return ''
}

function tradeCardMeta(message: ChatMessage): TradeCardMeta | null {
  if (message.contentKind !== 'product') return null
  const raw = (message.cardTitle || message.text).replace(/^\[|\]$/g, '').trim()
  if (!raw) return null
  if (systemNoticeText(message)) return null
  if (/快给\s*ta\s*一个评价吧/i.test(raw)) return { title: raw, subtitle: message.cardSubtitle || '说说这次的交易体验，帮助更多人', action: '去评价' }
  if (/完成了评价|期待你的评价/.test(raw)) return { title: '我完成了评价', subtitle: message.cardSubtitle || '期待你的评价', action: '查看评价' }
  if (/退款/.test(raw)) return { title: raw.includes('申请') ? raw : '我发起了退款申请', subtitle: message.cardSubtitle || '等待你处理，请确认操作', action: '去处理' }
  if (/待发货|已付款|成功小刀/.test(raw)) return { title: raw, subtitle: message.cardSubtitle || '买家已付款，请包装好商品并按约定地址发货', action: '去发货' }
  if (/修改价格|等待.*付款/.test(raw)) return { title: raw, subtitle: message.cardSubtitle || '请确认价格与协商一致，并在24小时内付款', action: '' }
  if (/拍下|待付款|未付款/.test(raw)) return { title: raw, subtitle: message.cardSubtitle || '请双方沟通及时确认价格', action: '修改价格' }
  if (/待完成|交易完成|已完成/.test(raw)) return { title: raw, subtitle: message.cardSubtitle || '交易正在按约定流程进行', action: '' }
  return null
}

function productLinkTitle(message: ChatMessage) {
  const match = message.text.trim().match(/^\[链接\]\s*(.+)$/)
  return match?.[1]?.trim() || ''
}

function isProductShare(message: ChatMessage) {
  return Boolean(productLinkTitle(message)) || ((message.contentKind === 'product' || Boolean(message.cardTitle)) && !tradeCardMeta(message))
}

function productTargetUrl(message: ChatMessage, contact: ChatContact) {
  const explicit = message.targetUrl?.trim()
    || message.text.match(/https?:\/\/[^\s\]]+/i)?.[0]
    || ''
  if (explicit.startsWith('//')) return `https:${explicit}`
  if (/^https?:\/\//i.test(explicit)) return explicit
  return contact.itemId ? `https://www.goofish.com/item?id=${encodeURIComponent(contact.itemId)}` : ''
}

type ProductCardFallback = { title: string; imageUrl: string; price: string }

function MessageBody({ message, emojis, onProductPreview, onTradeAction, productFallback }: { message: ChatMessage; emojis: ChatEmoji[]; onProductPreview: (message: ChatMessage) => void; onTradeAction: (action: string) => void; productFallback?: ProductCardFallback }) {
  const notice = systemNoticeText(message)
  if (notice) return <span className="system-notice-text">{notice}</span>
  if (message.contentKind === 'image' && message.mediaUrl) return <img className="message-image" src={displayImageUrl(message.mediaUrl)} alt="聊天图片" loading="lazy" />
  if (message.contentKind === 'expression' && message.mediaUrl) return <img className="message-expression" src={displayImageUrl(message.mediaUrl)} alt={message.text || '闲鱼表情'} title={message.text} loading="lazy" />
  const linkTitle = productLinkTitle(message)
  if (linkTitle) {
    const imageUrl = message.mediaUrl || productFallback?.imageUrl || ''
    const price = message.cardPrice || productFallback?.price || ''
    return <button type="button" className="message-card product-share-card link-share-card" onClick={() => onProductPreview(message)}><span className="product-share-media">{imageUrl ? <img src={displayImageUrl(imageUrl)} alt={linkTitle} loading="lazy" /> : <Package size={28} />}</span><span className="message-card-copy"><strong>{message.cardTitle || productFallback?.title || linkTitle}</strong>{price && <b>{price.startsWith('¥') ? price : `¥${price}`}</b>}</span></button>
  }
  const tradeCard = tradeCardMeta(message)
  if (tradeCard && /快给\s*ta\s*一个评价吧/i.test(tradeCard.title)) return <article className="evaluation-message-card"><div><strong>{tradeCard.title}</strong><span>{tradeCard.subtitle}</span></div><button type="button" onClick={() => onTradeAction(tradeCard.action)}>{tradeCard.action}</button></article>
  if (tradeCard) return <article className="trade-message-card"><strong>{tradeCard.title}</strong><div className="trade-message-card-detail"><span>{tradeCard.subtitle}</span>{tradeCard.action && <button type="button" onClick={() => onTradeAction(tradeCard.action)}>{tradeCard.action}</button>}</div></article>
  if (message.contentKind === 'product' || message.cardTitle) return <button type="button" className="message-card product-share-card" onClick={() => onProductPreview(message)}><span className="product-share-media">{message.mediaUrl ? <img src={displayImageUrl(message.mediaUrl)} alt="商品图片" loading="lazy" /> : <Package size={25} />}</span><span className="message-card-copy"><strong>{message.cardTitle || message.text || '商品分享'}</strong>{message.cardSubtitle && <span>{message.cardSubtitle}</span>}{message.cardPrice && <b>{message.cardPrice.startsWith('¥') ? message.cardPrice : `¥${message.cardPrice}`}</b>}</span></button>
  return <MessageText text={message.text || `[${message.contentKind}]`} emojis={emojis} />
}

function VirtualProductDetail({ url, onClose }: { url: string; onClose: () => void }) {
  const [position, setPosition] = useState(() => ({ x: Math.max(20, window.innerWidth / 2 - 195), y: Math.max(20, window.innerHeight / 2 - 390) }))
  const beginDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest('button')) return
    event.preventDefault()
    const startX = event.clientX
    const startY = event.clientY
    const origin = position
    const move = (next: PointerEvent) => setPosition({
      x: Math.max(8, Math.min(window.innerWidth - 370, origin.x + next.clientX - startX)),
      y: Math.max(8, Math.min(window.innerHeight - 80, origin.y + next.clientY - startY)),
    })
    const stop = () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', stop) }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', stop)
  }
  return <div className="virtual-product-window" style={{ left: position.x, top: position.y }} role="dialog" aria-label="闲鱼宝贝详情">
    <div className="virtual-product-window-title" onPointerDown={beginDrag}><span>闲鱼宝贝详情</span><button type="button" onClick={onClose} aria-label="关闭"><X size={15} /></button></div>
    <iframe className="virtual-product-window-frame" src={url} title="闲鱼宝贝详情" />
  </div>
}

function Workbench({ account, products, orders, onOrderUpdated, imConnected, quickReplyAutoSuggest, onUnreadChanged, onChatRead, unreadJumpRequest }: { account?: Account; products: Product[]; orders: Order[]; onOrderUpdated: () => void; imConnected: boolean; quickReplyAutoSuggest: boolean; onUnreadChanged: () => Promise<void>; onChatRead: (accountId: string, chatId: string, unreadCount: number) => void; unreadJumpRequest: { accountId: string; chatId: string; nonce: number } }) {
  const CONTACT_BATCH = 30
  const MESSAGE_BATCH = 50
  const [contacts, setContacts] = useState<ChatContact[]>([])
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [selectedId, setSelectedId] = useState('')
  const [query, setQuery] = useState('')
  const [conversationStatus, setConversationStatus] = useState('全部')
  const [pinnedChatIds, setPinnedChatIds] = useState<string[]>([])
  const [conversationMenu, setConversationMenu] = useState<{ contact: ChatContact; x: number; y: number } | null>(null)
  const [draft, setDraft] = useState('')
  const [emojiOpen, setEmojiOpen] = useState(false)
  const [emojis, setEmojis] = useState<ChatEmoji[]>([])
  const [quickReplyOpen, setQuickReplyOpen] = useState(false)
  const [quickReplyCommandOpen, setQuickReplyCommandOpen] = useState(false)
  const [quickReplies, setQuickReplies] = useState<QuickReply[]>([])
  const [quickReplyManaging, setQuickReplyManaging] = useState(false)
  const [editingQuickReply, setEditingQuickReply] = useState<QuickReply | undefined>()
  const [queuedReplyImages, setQueuedReplyImages] = useState<QuickReplyImage[]>([])
  const [productPickerOpen, setProductPickerOpen] = useState(false)
  const [composerNotice, setComposerNotice] = useState('')
  const [webProductPreviewUrl, setWebProductPreviewUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [contactLoading, setContactLoading] = useState(false)
  const [messageLoading, setMessageLoading] = useState(false)
  const [contactLimit, setContactLimit] = useState(CONTACT_BATCH)
  const [messageStart, setMessageStart] = useState(0)
  const [contactCursor, setContactCursor] = useState<number | null>(null)
  const [messageCursor, setMessageCursor] = useState<number | null>(null)
  const [contactHasMore, setContactHasMore] = useState(true)
  const [messageHasMore, setMessageHasMore] = useState(true)
  const [customerProfile, setCustomerProfile] = useState<CustomerProfile | undefined>()
  const [customerProfileLoading, setCustomerProfileLoading] = useState(false)
  const [customerProfileError, setCustomerProfileError] = useState('')
  const [customerProductTab, setCustomerProductTab] = useState<'current' | 'favorite' | 'consulted'>('current')
  const [customerRemarkOpen, setCustomerRemarkOpen] = useState(false)
  const [tradeDrawer, setTradeDrawer] = useState<TradeDrawer>(null)
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
  useEffect(() => {
    setConversationMenu(null)
    if (!account) { setPinnedChatIds([]); return }
    try {
      const saved = JSON.parse(localStorage.getItem(`shark-butler-pinned-chats:${account.id}`) || '[]')
      setPinnedChatIds(Array.isArray(saved) ? saved.filter((value): value is string => typeof value === 'string') : [])
    } catch { setPinnedChatIds([]) }
  }, [account?.id])
  useEffect(() => {
    if (account) localStorage.setItem(`shark-butler-pinned-chats:${account.id}`, JSON.stringify(pinnedChatIds))
  }, [account?.id, pinnedChatIds])
  useEffect(() => {
    const closeMenu = () => setConversationMenu(null)
    document.addEventListener('mousedown', closeMenu)
    return () => document.removeEventListener('mousedown', closeMenu)
  }, [])
  useEffect(() => {
    const list = conversationListRef.current
    if (!list) return
    const openMenu = (event: MouseEvent) => {
      const target = (event.target as Element | null)?.closest<HTMLButtonElement>('button.conversation')
      const chatId = target?.dataset.chatId
      const contact = chatId ? contactsRef.current.find((item) => item.chatId === chatId) : undefined
      if (!contact) return
      event.preventDefault()
      setConversationMenu({ contact, x: event.clientX, y: event.clientY })
    }
    list.addEventListener('contextmenu', openMenu)
    return () => list.removeEventListener('contextmenu', openMenu)
  }, [account?.id])
  useEffect(() => {
    let active = true
    setCustomerProductTab('current')
    setCustomerProfile(undefined)
    setCustomerProfileError('')
    if (!account || !selected) return () => { active = false }
    setCustomerProfileLoading(true)
    void api.customerProfile(account.id, selected.chatId)
      .then((profile) => { if (active) setCustomerProfile(profile) })
      .catch((error) => { if (active) setCustomerProfileError(error instanceof Error ? error.message : String(error)) })
      .finally(() => { if (active) setCustomerProfileLoading(false) })
    return () => { active = false }
  }, [account?.id, selected?.chatId])
  const saveCustomerRemark = async (remark: string) => {
    if (!account || !selected) return
    await api.updateCustomerRemark(account.id, selected.chatId, remark)
    setCustomerProfile((current) => current ? { ...current, remark } : current)
    setCustomerRemarkOpen(false)
    setComposerNotice('买家备注已同步到闲鱼')
  }
  const accountProducts = products.filter((product) => product.accountId === account?.id && product.status === '已上架')
  const openProductPreview = async (message: ChatMessage) => {
    if (!selected) return
    const targetUrl = productTargetUrl(message, selected)
    if (!targetUrl) {
      setComposerNotice('该商品消息没有返回可打开的链接')
      return
    }
    if (!isTauri()) {
      setWebProductPreviewUrl(targetUrl)
      return
    }
    if (!account) return
    try {
      await api.openProductDetail(account.id, targetUrl)
    } catch (nextError) {
      setComposerNotice(`商品详情窗口打开失败：${nextError instanceof Error ? nextError.message : String(nextError)}`)
    }
  }
  const openOrderAction = async (order: Order, action: string) => {
    if (!isTauri()) {
      setComposerNotice('订单发货需要在已扫码登录的桌面端完成')
      return
    }
    if (action === '提醒收货') {
      try {
        await api.remindOrderReceipt(order.accountId, order.orderNo)
        setComposerNotice('已提醒买家确认收货')
      } catch (nextError) {
        setComposerNotice(`提醒收货失败：${nextError instanceof Error ? nextError.message : String(nextError)}`)
      }
      return
    }
    setTradeDrawer({ kind: action === '取消订单' ? 'cancel' : 'ship', order })
  }
  const openTradeAction = async (action: string) => {
    if (!account || !selected) return
    const status = action === '去发货' ? '待发货' : '全部'
    try {
      const candidates = await api.relatedOrders(account.id, selected.chatId, status)
      const order = candidates.find((item) => action !== '去发货' || item.status.includes('待发货'))
      if (!order) {
        setComposerNotice('暂未找到关联的待发货订单，请先同步订单后重试')
        return
      }
      await openOrderAction(order, action)
    } catch (nextError) {
      setComposerNotice(`读取关联订单失败：${nextError instanceof Error ? nextError.message : String(nextError)}`)
    }
  }
  const filteredContacts = contacts.filter((item) => {
    const upstreamStatus = `${item.orderStatus} ${item.latestMessage}`
    const matchesStatus = conversationStatus === '全部'
      || (conversationStatus === '待下单' ? !item.orderStatus && !/待付款|待发货|已发货|退款/.test(item.latestMessage) : upstreamStatus.includes(conversationStatus))
    return matchesStatus && `${item.otherUserName}${item.latestMessage}${item.itemTitle}`.toLowerCase().includes(query.toLowerCase())
  })
  const sortedContacts = [...filteredContacts].sort((left, right) => {
    const leftPinned = pinnedChatIds.includes(left.chatId)
    const rightPinned = pinnedChatIds.includes(right.chatId)
    if (leftPinned !== rightPinned) return leftPinned ? -1 : 1
    return 0
  })
  const visibleContacts = sortedContacts.slice(0, contactLimit)
  const visibleMessages = messages.slice(messageStart)
  const requestedConversation = (items: ChatContact[]) => unreadJumpRequest.accountId === account?.id && unreadJumpRequest.nonce > processedUnreadJumpRef.current
    ? items.find((contact) => contact.chatId === unreadJumpRequest.chatId) ?? items.find((contact) => contact.unreadCount > 0) ?? items[0]
    : undefined

  const refreshContacts = async (remote = !imConnected, cursor: number | null = null) => {
    if (!account) { setContacts([]); setSelectedId(''); return }
    if (contactLoadingRef.current) return
    contactLoadingRef.current = true
    setContactLoading(true); setError('')
    try {
      let localContactsEmpty = false
      if (cursor === null) {
        const local = await api.chatContacts(account.id)
        localContactsEmpty = local.length === 0
        setContacts(local)
        setSelectedId((current) => requestedConversation(local)?.chatId ?? (local.some((item) => item.chatId === current) ? current : local[0]?.chatId ?? ''))
      }
      // A newly re-added account has no local contacts after its old account
      // record was deleted, so bootstrap the list even when IM is connected.
      if (remote || localContactsEmpty) {
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

  const toggleConversationPin = async (contact: ChatContact) => {
    if (!account) return
    const pinned = !pinnedChatIds.includes(contact.chatId)
    try {
      await api.setChatPinned(account.id, contact.chatId, pinned)
      setPinnedChatIds((current) => pinned ? [contact.chatId, ...current.filter((id) => id !== contact.chatId)] : current.filter((id) => id !== contact.chatId))
      setConversationMenu(null)
      setComposerNotice(pinned ? '会话已置顶' : '已取消置顶')
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
    }
  }

  const deleteConversation = async (contact: ChatContact) => {
    if (!account || !window.confirm(`确定删除与“${contact.otherUserName || '该用户'}”的会话吗？\n本地消息缓存和闲鱼会话都会被删除。`)) return
    try {
      await api.deleteChatConversation(account.id, contact.chatId)
      const nextContacts = contacts.filter((item) => item.chatId !== contact.chatId)
      setContacts(nextContacts)
      setPinnedChatIds((current) => current.filter((id) => id !== contact.chatId))
      if (selectedId === contact.chatId) {
        setSelectedId(nextContacts[0]?.chatId ?? '')
        setMessages([])
      }
      setConversationMenu(null)
      setComposerNotice('会话已删除')
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
    }
  }

  useEffect(() => {
    setContacts([]); setMessages([]); setSelectedId('')
    setContactLimit(CONTACT_BATCH); setContactCursor(null); setContactHasMore(true)
    void refreshContacts(!imConnected)
  }, [account?.id, imConnected])

  useEffect(() => {
    setQuickReplies([]); setEditingQuickReply(undefined); setQueuedReplyImages([]); setQuickReplyManaging(false)
    if (!account) return
    api.quickReplies(account.id).then(setQuickReplies).catch((nextError) => setError(nextError instanceof Error ? nextError.message : String(nextError)))
  }, [account?.id])

  useEffect(() => {
    setEmojis([])
    if (!account?.remoteAccountId) return
    let cancelled = false
    api.chatEmojis(account.id).then((items) => { if (!cancelled) setEmojis(items) }).catch(() => undefined)
    api.syncChatEmojis(account.id).then((items) => { if (!cancelled) setEmojis(items) }).catch(() => undefined)
    return () => { cancelled = true }
  }, [account?.id, account?.remoteAccountId])

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
    const refreshFromPush = async (requiresSync = false, pushChatId = '') => {
      if (contactLoadingRef.current) {
        pushRefreshPendingRef.current = true
        return
      }
      const selectedChatId = selected?.chatId
      try {
        const localContacts = await api.chatContacts(account.id)
        if (!cancelled) {
          const activeContact = selectedChatId ? localContacts.find((contact) => contact.chatId === selectedChatId) : undefined
          // Messages received while this conversation is open are read
          // immediately. Do not let a later push repaint an active chat with
          // a red point or add it back into account/global totals.
          const displayContacts = activeContact
            ? localContacts.map((contact) => contact.chatId === selectedChatId ? { ...contact, unreadCount: 0 } : contact)
            : localContacts
          setContacts(displayContacts)
          setSelectedId((current) => localContacts.some((contact) => contact.chatId === current) ? current : localContacts[0]?.chatId ?? '')
          if (activeContact && activeContact.unreadCount > 0) {
            onChatRead(account.id, selectedChatId, activeContact.unreadCount)
            void api.markChatRead(account.id, selectedChatId).then(() => onUnreadChanged()).catch(() => undefined)
          } else {
            void onUnreadChanged()
          }
        }
      } catch {
        // Keep the current list usable when the local cache is unavailable.
      }
      if (selectedChatId && !messageLoadingRef.current) {
        try {
          const localMessages = await api.chatMessages(account.id, selectedChatId)
          if (!cancelled) {
            const knownIds = knownMessageIdsRef.current
            const hasNewMessage = knownIds.size > 0 && localMessages.some((message) => !knownIds.has(message.id))
            knownMessageIdsRef.current = new Set(localMessages.map((message) => message.id))
            setMessages(localMessages)
            if (hasNewMessage) {
              requestAnimationFrame(() => requestAnimationFrame(() => {
                const list = messageListRef.current
                if (!list) return
                list.scrollTop = list.scrollHeight
              }))
            }
          }
        } catch {
          // The remote refresh below can still recover the conversation.
        }
      }
      // Pushes are already persisted locally. Avoid issuing history RPCs for
      // every event while the singleton IM socket is connected: the gateway
      // rate-limits that pattern and can reject subsequent sends.
      const syncChatId = pushChatId || selectedChatId
      if (!syncChatId || (imConnected && !requiresSync)) return
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
        const page = await api.syncChatMessages(account.id, syncChatId, null)
        if (cancelled) return
        if (syncChatId !== selectedChatId) return
        const knownIds = knownMessageIdsRef.current
        const hasNewIncoming = knownIds.size > 0 && page.items.some((message) => message.direction === 'incoming' && !knownIds.has(message.id))
        knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
        setMessages((current) => {
          if (current.length === page.items.length && current.every((message, index) => {
            const next = page.items[index]
            return message.id === next?.id
              && message.readStatus === next.readStatus
              && message.sendStatus === next.sendStatus
          })) return current
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
    void listen<{ accountId: string; requiresSync?: boolean; chatId?: string }>('chat-im-event', (event) => {
      if (!cancelled && event.payload.accountId === account.id) void refreshFromPush(Boolean(event.payload.requiresSync), event.payload.chatId ?? '')
    }).then((stop) => {
      if (cancelled) stop()
      else unlisten = stop
    })
    return () => { cancelled = true; unlisten?.() }
  }, [account?.id, selected?.chatId, imConnected])
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
    const unreadCount = selected.unreadCount
    setContacts((current) => current.map((contact) => contact.chatId === selected.chatId ? { ...contact, unreadCount: 0 } : contact))
    if (unreadCount > 0) onChatRead(account.id, selected.chatId, unreadCount)
    void api.markChatRead(account.id, selected.chatId)
      .then(() => onUnreadChanged())
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
        // A freshly re-added account has a valid remote login but no local
        // chat cache because deleting the old account intentionally removed
        // all messages. Bootstrap that cache from the remote history even
        // when the singleton IM connection is already online.
        if (!imConnected || local.length === 0) {
          const page = await api.syncChatMessages(account.id, selected.chatId, null)
          if (active) {
            setMessages(page.items)
            knownMessageIdsRef.current = new Set(page.items.map((message) => message.id))
            setMessageStart(Math.max(0, page.items.length - MESSAGE_BATCH))
            setMessageCursor(page.nextCursor)
            setMessageHasMore(page.hasMore)
            requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
          }
        } else if (active) {
          setMessageHasMore(false)
        }
      } catch (nextError) {
        if (active) setError(nextError instanceof Error ? nextError.message : String(nextError))
      } finally {
        messageLoadingRef.current = false
        if (active) setMessageLoading(false)
        if (active && !imConnected && messageRefreshPendingRef.current && account && selected) {
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
  }, [account?.id, selected?.chatId, imConnected])

  const loadMoreContacts = () => {
    if (contactLoadingRef.current) return
    if (contactLimit < filteredContacts.length) {
      setContactLimit((current) => Math.min(filteredContacts.length, current + CONTACT_BATCH))
    } else if (!imConnected && contactHasMore && contactCursor !== null) {
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
    if (imConnected || !messageHasMore || messageCursor === null) return
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
      if (!imConnected && messageRefreshPendingRef.current && account && selected) {
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

  const insertProduct = async (product: Product) => {
    if (!account || !selected || busy) return
    const parts = product.id.split('-')
    const itemId = parts[0] === 'SRC' && parts[1] === 'P' ? parts.slice(3).join('-') : ''
    if (!itemId) {
      setProductPickerOpen(false)
      setComposerNotice('该商品还没有闲鱼商品 ID，请先同步账号商品')
      return
    }
    setBusy(true); setError(''); setProductPickerOpen(false); setComposerNotice('正在发送商品卡…')
    try {
      const message = await api.sendChatProduct(account.id, selected.chatId, selected.otherUserId, itemId, product.title, product.imageUrl, product.price)
      setMessages((current) => [...current, message])
      setDraft('')
      setComposerNotice('商品链接已发送')
      await refreshContacts(false)
      requestAnimationFrame(() => { const list = messageListRef.current; if (list) list.scrollTop = list.scrollHeight })
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
      setComposerNotice('商品链接发送失败')
    } finally { setBusy(false) }
  }

  const insertQuickReply = (reply: QuickReply, replaceCommand = false, replaceDraft = false) => {
    const command = draft.match(/(^|\s)\/([a-zA-Z0-9_-]*)$/)
    const replyText = replaceDraft
      ? reply.content
      : replaceCommand && command
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

  const commandMatch = draft.match(/(?:^|\s)\/([a-zA-Z0-9_-]*)$/)
  const commandQuery = commandMatch?.[1]?.toLowerCase() ?? ''
  const commandActive = Boolean(commandMatch)
  const autoSuggestQuery = draft.trim().toLowerCase()
  const matchingQuickReplies = commandActive
    ? quickReplies.filter((reply) => reply.shortCode.toLowerCase().includes(commandQuery) || reply.title.toLowerCase().includes(commandQuery))
    : quickReplyAutoSuggest && autoSuggestQuery.length >= 2
      ? quickReplies.filter((reply) => {
        const searchable = `${reply.title} ${reply.content}`.toLowerCase()
        return searchable.includes(autoSuggestQuery) || autoSuggestQuery.includes(reply.title.toLowerCase())
      })
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
        <div className="message-list" ref={messageListRef} onScroll={(event) => { if (event.currentTarget.scrollTop < 24) void loadOlderMessages() }}>{messageLoading && <div className="scroll-loader message-loader">正在加载更早消息…</div>}{!messageLoading && messageStart === 0 && !messageHasMore && messages.length > 0 && <div className="scroll-end">已到达最早消息</div>}{messages.length ? visibleMessages.map((message) => {
          // 回执仅使用闲鱼消息模型的 readStatus：2 为已读，其余返回值为未读。
          // 服务端没有返回该字段或明确不支持时不臆造“已发送”状态。
          const receipt = message.readStatus === 'read' ? '已读' : message.readStatus === 'unread' ? '未读' : null
          const isOutgoing = message.direction === 'outgoing'
          const systemNotice = systemNoticeText(message)
          const tradeCard = tradeCardMeta(message)
          const evaluationPrompt = Boolean(tradeCard && /快给\s*ta\s*一个评价吧/i.test(tradeCard.title))
          const emphasizedTradeCard = Boolean(tradeCard && (/修改价格|等待.*付款|我完成了评价/.test(tradeCard.title)))
          const senderAvatar = isOutgoing
            ? account
              ? <AccountAvatar account={account} className="message-avatar" />
              : <span className="avatar account-avatar message-avatar"><span>{(message.senderUserName || '我').slice(0, 1)}</span></span>
            : <ContactAvatar contact={selected} />
          const senderName = isOutgoing ? account?.displayName || message.senderUserName || '我' : message.senderUserName || selected.otherUserName
          if (systemNotice) return <div className="system-event-group" key={message.id}><time>{formatEventTime(message.sentAt)}</time><div className="message-row system-notice-row"><div className="message-bubble system-notice-bubble"><MessageBody message={message} emojis={emojis} onProductPreview={openProductPreview} onTradeAction={(action) => void openTradeAction(action)} /></div></div></div>
          if (evaluationPrompt) return <div className="system-event-group evaluation-event-group" key={message.id}><time>{formatEventTime(message.sentAt)}</time><div className="message-bubble trade-bubble evaluation-prompt"><MessageBody message={message} emojis={emojis} onProductPreview={openProductPreview} onTradeAction={(action) => void openTradeAction(action)} /></div></div>
          const productShare = isProductShare(message)
          const linkTitle = productLinkTitle(message)
          const linkedProduct = products.find((item) => item.id === selected.itemId || item.title === linkTitle || item.title === message.cardTitle)
          const sameConversationProduct = Boolean(linkedProduct && selected.itemId && linkedProduct.id.endsWith(`-${selected.itemId}`))
          const productFallback = { title: linkedProduct?.title || linkTitle || selected.itemTitle || '', imageUrl: linkedProduct?.imageUrl || (sameConversationProduct ? selected.itemImageUrl : ''), price: linkedProduct ? String(linkedProduct.price) : '' }
          return <div className={`message-row ${message.direction}`} key={message.id}>{!isOutgoing && senderAvatar}<div className="message-stack"><div className="message-sender-line"><span>{senderName}</span><time>{formatDate(message.sentAt)}</time></div><div className={`message-bubble ${message.contentKind === 'image' ? 'image-bubble' : ''} ${message.contentKind === 'expression' ? 'expression-bubble' : ''} ${productShare ? 'card-bubble product-share-bubble' : ''} ${tradeCard ? 'trade-bubble' : ''} ${evaluationPrompt ? 'evaluation-prompt' : ''} ${emphasizedTradeCard ? 'emphasized' : ''}`}><MessageBody message={message} emojis={emojis} onProductPreview={openProductPreview} onTradeAction={(action) => void openTradeAction(action)} productFallback={productFallback} /></div>{isOutgoing && receipt && <small className="message-receipt">{receipt}</small>}</div>{isOutgoing && senderAvatar}</div>
        }) : <div className="chat-blank"><MessageCircle size={38} /><h2>暂无历史消息</h2><p>点击左侧“同步”后会从闲鱼拉取最新会话。</p></div>}</div>
        <footer className="chat-composer">
          <input ref={imageInputRef} className="composer-file-input" type="file" accept="image/*" onChange={(event) => { const file = event.target.files?.[0]; if (file) void sendImage(file); event.currentTarget.value = '' }} />
          <div className="composer-toolbar">
            <div className="composer-tools-left">
              <div className="composer-tool-anchor" ref={emojiAnchorRef}>
                <button className={`composer-tool-button ${emojiOpen ? 'active' : ''}`} type="button" aria-label="表情" title="表情" onClick={() => { setEmojiOpen((current) => !current); setQuickReplyOpen(false) }}><Smile size={19} /></button>
                {emojiOpen && <div className="composer-popover emoji-picker">{emojis.length ? emojis.map((emoji) => <button type="button" key={emoji.iconAlias} title={emoji.iconAlias} onClick={() => { setDraft((current) => current + emoji.iconAlias); setEmojiOpen(false) }}><img src={displayImageUrl(emoji.iconUrl)} alt={emoji.iconAlias} loading="lazy" /></button>) : <p>正在加载闲鱼官方表情…</p>}</div>}
              </div>
              <button className="composer-tool-button" type="button" aria-label="发送图片" title="发送图片" onClick={() => imageInputRef.current?.click()}><ImageIcon size={19} /></button>
            </div>
            <div className="composer-tools-right">
              <div className="composer-tool-anchor" ref={productPickerRef}>
                <button className={`composer-tool-button ${productPickerOpen ? 'active' : ''}`} type="button" aria-label="插入商品" title="选择当前账号商品" onClick={() => { setProductPickerOpen((current) => !current); setQuickReplyOpen(false); setEmojiOpen(false) }}><ShoppingBag size={19} /></button>
                {productPickerOpen && <div className="composer-popover product-picker"><header><strong>当前账号商品</strong><small>{accountProducts.length} 件在售</small></header>{accountProducts.length ? accountProducts.map((product) => <button type="button" key={product.id} onClick={() => void insertProduct(product)}><span className="product-picker-thumb"><Package size={16} />{product.imageUrl && <img src={displayImageUrl(product.imageUrl)} alt="" loading="lazy" onError={(event) => { event.currentTarget.style.display = 'none' }} />}</span><span><strong>{product.title}</strong><small>¥{product.price} · 库存 {product.stock}</small></span></button>) : <p>当前账号暂无已同步的在售商品。</p>}</div>}
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
          <div className="composer-input-row"><div className="composer-command-anchor" ref={quickReplyCommandRef}><textarea value={draft} onChange={(event) => { const next = event.target.value; setDraft(next); setQuickReplyCommandOpen(/(?:^|\s)\/[a-zA-Z0-9_-]*$/.test(next) || (quickReplyAutoSuggest && next.trim().length >= 2)) }} onPaste={(event) => { const image = Array.from(event.clipboardData.items).find((item) => item.kind === 'file' && item.type.startsWith('image/'))?.getAsFile(); if (!image) return; event.preventDefault(); void sendImage(image) }} onKeyDown={(event) => { if (event.key === 'Escape') { setQuickReplyCommandOpen(false); return } if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); void send() } }} placeholder={'通过设置启用「自动联想」或输入“/”唤起快捷回复\n「Command + V」可直接发送截图或复制的图片\n「Shift + Enter」进行内容换行'} />{quickReplyCommandOpen && <div className="quick-command-menu"><header><strong>{commandActive ? '快捷指令' : '自动联想'}</strong><small>{commandActive ? `输入 /${commandQuery} 筛选` : '根据输入匹配快捷回复'}</small></header>{matchingQuickReplies.length ? matchingQuickReplies.map((reply) => <button type="button" key={reply.id} onClick={() => insertQuickReply(reply, commandActive, !commandActive)}><span><strong>{reply.title}</strong><small>/{reply.shortCode}</small></span><em>{reply.images.length ? `${reply.images.length} 图` : '文字'}</em></button>) : <p>{commandActive ? '没有匹配的快捷回复' : '暂无联想结果'}</p>}</div>}</div><button className="primary" disabled={(!draft.trim() && !queuedReplyImages.length) || busy} onClick={() => void send()}><Send size={15} />{busy ? '处理中' : '发送'}</button></div>
        </footer>
      </> : <div className="chat-blank"><MessageCircle size={42} /><h2>{account ? '暂无会话' : '请先添加账号'}</h2><p>{account ? '确保账号已扫码登录，然后点击左侧同步。' : '扫码登录后即可同步真实会话。'}</p></div>}
    </section>
    <aside className={`context-panel ${quickReplyManaging ? 'quick-reply-manager-panel' : ''}`}>{quickReplyManaging ? <QuickReplyManager replies={quickReplies} value={editingQuickReply} onClose={() => { setQuickReplyManaging(false); setEditingQuickReply(undefined) }} onSave={saveQuickReply} onEdit={setEditingQuickReply} onDelete={(reply) => void deleteQuickReply(reply)} /> : selected ? <CustomerContextPanel contact={selected} profile={customerProfile} inventory={products} orders={orders} onOrderUpdated={onOrderUpdated} onOrderAction={openOrderAction} loading={customerProfileLoading} error={customerProfileError} activeTab={customerProductTab} onTabChange={setCustomerProductTab} onEditRemark={() => setCustomerRemarkOpen(true)} /> : <div className="context-empty"><img src={logo} alt="" /><p>选择会话后查看客户资料、交易统计和商品足迹。</p></div>}</aside>
    {conversationMenu && createPortal(<div className="conversation-context-menu" style={{ left: Math.min(conversationMenu.x, window.innerWidth - 176), top: Math.min(conversationMenu.y, window.innerHeight - 104) }} onMouseDown={(event) => event.stopPropagation()}><button type="button" onClick={() => void toggleConversationPin(conversationMenu.contact)}><Pin size={15} />{pinnedChatIds.includes(conversationMenu.contact.chatId) ? '取消置顶' : '置顶'}</button><button type="button" className="danger" onClick={() => void deleteConversation(conversationMenu.contact)}><Trash2 size={15} />删除</button></div>, document.body)}
    {webProductPreviewUrl && <VirtualProductDetail url={webProductPreviewUrl} onClose={() => setWebProductPreviewUrl('')} />}
    {customerRemarkOpen && selected && <CustomerRemarkDialog value={customerProfile?.remark ?? ''} onClose={() => setCustomerRemarkOpen(false)} onSave={saveCustomerRemark} />}
    {tradeDrawer?.kind === 'ship' && <ShipOrderDrawer accountId={tradeDrawer.order.accountId} order={tradeDrawer.order} onClose={() => setTradeDrawer(null)} onDone={() => { setTradeDrawer(null); onOrderUpdated(); setComposerNotice('已发货，订单状态已同步') }} />}
    {tradeDrawer?.kind === 'cancel' && <CancelOrderDrawer accountId={tradeDrawer.order.accountId} order={tradeDrawer.order} onClose={() => setTradeDrawer(null)} onDone={() => { setTradeDrawer(null); onOrderUpdated(); setComposerNotice('订单已取消，状态已同步') }} />}
  </div>
}

function ContactAvatar({ contact }: { contact: ChatContact }) {
  const isNotice = contact.otherUserName === '通知消息' || contact.chatId.toLowerCase().includes('notice')
  if (isNotice) return <div className="avatar contact-avatar system-avatar"><Bell size={22} /></div>
  const fallback = contact.otherUserName.trim().slice(0, 1) || '?'
  return <div className="avatar contact-avatar"><span>{fallback}</span>{contact.avatarUrl && <img src={displayImageUrl(contact.avatarUrl)} alt="" loading="lazy" onError={(event) => event.currentTarget.remove()} />}</div>
}

function customerItemDate(value: string) {
  if (!value) return ''
  const timestamp = Number(value)
  if (Number.isFinite(timestamp) && timestamp > 0) return formatRelativeTime(new Date(timestamp).toISOString())
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : formatRelativeTime(date.toISOString())
}

function customerCreditTone(value: string) {
  if (value.includes('极好')) return 'excellent'
  if (value.includes('优秀')) return 'great'
  if (value.includes('良好') || value.includes('较好')) return 'good'
  if (value.includes('较差')) return 'poor'
  return 'neutral'
}

function orderStatusMeta(status: string) {
  const value = status.trim()
  const refunded = /退款成功|已退款|退款完成/.test(value)
  const shipped = !refunded && /待收货|已发货|已寄件/.test(value)
  const success = !refunded && /交易成功|已完成|完成/.test(value)
  const closed = refunded || /交易关闭|已关闭|关闭|取消/.test(value)
  return {
    label: success ? '交易成功' : closed ? '交易关闭' : shipped ? '已发货' : value || '交易进行中',
    shipped,
    success,
    closed,
    refunded,
  }
}

function CustomerContextPanel({ contact, profile, inventory, orders, onOrderUpdated, onOrderAction, loading, error, activeTab, onTabChange, onEditRemark }: { contact: ChatContact; profile?: CustomerProfile; inventory: Product[]; orders: Order[]; onOrderUpdated: () => void; onOrderAction: (order: Order, action: string) => Promise<void>; loading: boolean; error: string; activeTab: 'current' | 'favorite' | 'consulted'; onTabChange: (tab: 'current' | 'favorite' | 'consulted') => void; onEditRemark: () => void }) {
  const [orderFilter, setOrderFilter] = useState('全部')
  const [relatedOrders, setRelatedOrders] = useState<Order[]>([])
  const [editingOrderId, setEditingOrderId] = useState('')
  const [orderNote, setOrderNote] = useState('')
  const [savingNote, setSavingNote] = useState(false)
  const [detailOrder, setDetailOrder] = useState<Order | null>(null)
  const [relatedOrdersSyncing, setRelatedOrdersSyncing] = useState(false)
  const displayName = profile?.displayName || contact.otherUserName
  const avatarUrl = profile?.avatarUrl || contact.avatarUrl
  const rawCredit = profile?.creditLevel || contact.buyerTag
  const credit = rawCredit ? (rawCredit.includes('买家信用') ? rawCredit : `买家信用${rawCredit}`) : ''
  const details = [
    profile?.city,
    profile?.lastActiveText,
    profile?.goodReviewRate && `好评率 ${profile.goodReviewRate}${profile.goodReviewRate.includes('%') ? '' : '%'}`,
  ].filter(Boolean) as string[]
  const products: CustomerItem[] = activeTab === 'current'
    ? profile?.currentItems ?? [{ itemId: contact.itemId, title: contact.itemTitle, imageUrl: contact.itemImageUrl, price: '', fishCoin: '', status: '', exposureCount: '', viewCount: '', wantCount: '', visitedAt: contact.latestMessageTime }].filter((item) => item.itemId || item.title)
    : activeTab === 'favorite' ? profile?.favoriteItems ?? [] : profile?.consultedItems ?? []
  const tabEmptyCopy = activeTab === 'current' ? '当前会话没有关联商品。' : activeTab === 'favorite' ? '闲鱼暂未返回收藏/浏览商品。' : '闲鱼暂未返回咨询过的商品。'
  useEffect(() => {
    let active = true
    // Never leave the previous conversation's orders visible while the new
    // buyer-specific query is in flight.
    setRelatedOrders([])
    setDetailOrder(null)
    setRelatedOrdersSyncing(true)
    // Related-order cards must not be a stale SQLite snapshot: when the
    // conversation changes, refresh the official seller order list first.
    void api.syncAccount(contact.accountId)
      .then(() => { if (active) onOrderUpdated() })
      .catch(() => undefined)
      .finally(() => { if (active) setRelatedOrdersSyncing(false) })
    return () => { active = false }
  }, [contact.accountId, contact.chatId])
  useEffect(() => {
    let active = true
    void (async () => {
      const items = await api.relatedOrders(contact.accountId, contact.chatId, orderFilter)
      if (active) setRelatedOrders(items.slice(0, 8))
    })()
      .catch(() => { if (active) setRelatedOrders([]) })
    return () => { active = false }
  }, [contact.accountId, contact.chatId, orderFilter, orders])
  const orderTabs = ['全部', '待付款', '待发货', '已发货', '退款中', '交易关闭', '交易成功']
  const saveOrderNote = async (order: Order) => {
    setSavingNote(true)
    try {
      const updatedOrder = await api.updateOrder(order.id, order.status, orderNote)
      setRelatedOrders((current) => current.map((item) => item.id === updatedOrder.id ? updatedOrder : item))
      setEditingOrderId('')
    } finally { setSavingNote(false) }
  }
  return <div className="customer-context">
    <div className="context-tabs customer-context-tabs"><button className="active" type="button">买家信息</button><button type="button">快捷回复</button></div>
    <section className="customer-identity">
      <div className="customer-profile-avatar"><span>{displayName.trim().slice(0, 1) || '?'}</span>{avatarUrl && <img src={displayImageUrl(avatarUrl)} alt="" onError={(event) => event.currentTarget.remove()} />}</div>
      <div><strong>{displayName}</strong><button type="button" className="customer-remark-button" onClick={onEditRemark}><span>{profile?.remark || '给买家添加备注'}</span><Pencil size={12} /></button></div>
      {loading && <i className="customer-syncing">同步中</i>}
    </section>
    <div className="customer-detail-line">{credit && <span className={`customer-credit ${customerCreditTone(credit)}`}>{credit}</span>}{details.length ? details.map((detail, index) => <span key={`${detail}-${index}`}>{detail}</span>) : !credit && <span>闲鱼买家资料暂未返回</span>}</div>
    <section className="customer-stats">
      <small>{profile?.officialSynced ? `数据更新至 ${profile.dataUpdatedAt || '—'}` : '等待官方客户资料同步'}</small>
      <div>
        <article><strong>{profile?.purchaseCount || '—'}<em>{profile?.purchaseCount ? '次' : ''}</em></strong><span>本店购买</span></article>
        <article><strong>{profile?.totalSpend ? `¥${profile.totalSpend.replace(/^¥/, '')}` : '—'}</strong><span>本店累计消费</span></article>
        <article><strong>{profile?.averageOrderValue ? `¥${profile.averageOrderValue.replace(/^¥/, '')}` : '—'}</strong><span>本店平均笔单价</span></article>
      </div>
    </section>
    <div className="customer-product-tabs" role="tablist">
      <button type="button" className={activeTab === 'current' ? 'active' : ''} onClick={() => onTabChange('current')}>当前宝贝</button>
      <button type="button" className={activeTab === 'favorite' ? 'active' : ''} onClick={() => onTabChange('favorite')}>收藏宝贝</button>
      <button type="button" className={activeTab === 'consulted' ? 'active' : ''} onClick={() => onTabChange('consulted')}>咨询过的宝贝</button>
    </div>
    <div className="customer-product-list">{products.length ? products.slice(0, 10).map((item, index) => activeTab === 'current'
      ? <article className="customer-current-item" key={`${item.itemId || item.title}-${index}`}><div className="customer-current-image">{item.imageUrl ? <img src={displayImageUrl(item.imageUrl)} alt="" loading="lazy" onError={(event) => event.currentTarget.parentElement?.classList.add('image-error')} /> : null}<Package size={21} />{item.status && <span>{item.status}</span>}</div><div className="customer-current-copy"><strong>{item.title || `商品 ${item.itemId}`}</strong><small>曝光：{item.exposureCount || '—'} <i>｜</i> 浏览：{item.viewCount || '—'} <i>｜</i> 想要：{item.wantCount || '—'}</small><b>{item.price && `¥${item.price.replace(/^¥/, '')}`}{item.fishCoin && <em>{item.price ? ' + ' : ''}{item.fishCoin}闲鱼币</em>}</b></div></article>
      : <article className="customer-footprint-item" key={`${item.itemId || item.title}-${index}`}><div className="customer-footprint-image">{item.imageUrl ? <img src={displayImageUrl(item.imageUrl)} alt={item.title || '商品图片'} loading="lazy" onError={(event) => event.currentTarget.parentElement?.classList.add('image-error')} /> : <Package size={17} />}{item.visitedAt && <small>{customerItemDate(item.visitedAt)}</small>}</div>{item.price && <b>¥{item.price.replace(/^¥/, '')}</b>}</article>) : <p>{tabEmptyCopy}</p>}</div>
    <section className="related-orders"><div className="related-orders-head"><strong>关联订单</strong><small>{relatedOrdersSyncing ? '同步中…' : '⋯'}</small></div><div className="related-order-tabs">{orderTabs.map((tab) => <button type="button" className={orderFilter === tab ? 'active' : ''} key={tab} onClick={() => setOrderFilter(tab)}>{tab}</button>)}</div>{relatedOrders.length ? <div className="related-order-list">{relatedOrders.map((order) => { const product = inventory.find((item) => item.accountId === order.accountId && (item.id.endsWith(`-${order.itemId}`) || item.title === order.productTitle || item.title.includes(order.productTitle) || order.productTitle.includes(item.title))); const displayTitle = product?.title || order.productTitle || '未命名商品'; const meta = orderStatusMeta(order.status); const pendingShipment = order.status.includes('待发货'); return <article className={`related-order-card ${meta.closed ? 'closed' : ''} ${meta.success ? 'completed' : ''}`} key={order.id}><div className="related-order-meta"><span className={`related-order-status ${meta.success ? 'success' : ''}`}>{meta.label}</span><small>订单编号 {order.orderNo} <button type="button" className="copy-order-id" title="复制订单编号" onClick={() => void copyOrderNumber(order.orderNo)}><Clipboard size={11} /></button></small><button type="button" className="order-detail-link" onClick={() => setDetailOrder(order)}>详情</button></div><div className="related-order-date">下单 {order.createdAt ? formatDate(order.createdAt) : '时间未知'}{meta.success && `  付款 ${order.createdAt ? formatDate(order.createdAt) : ''}`}</div><div className="related-order-title"><span className="related-order-image">{product?.imageUrl ? <img src={displayImageUrl(product.imageUrl)} alt={displayTitle} loading="lazy" onError={(event) => event.currentTarget.style.display = 'none'} /> : <Package size={18} />}</span><span className="related-order-copy"><strong>{displayTitle}</strong><small>规格：一天</small></span><b>¥{(product?.price ?? order.amount).toFixed(2)}<small>[共1件]</small></b></div><div className="related-order-details"><div><span>成交价</span><strong>¥{order.amount.toFixed(2)}{meta.closed && !meta.refunded && <em>（含运费）</em>}</strong></div><div><span>发货状态</span><span>{meta.shipped || meta.success || meta.refunded ? '已发货' : '未发货'}</span></div>{meta.shipped && <div><span>发货时间</span><span>{order.createdAt ? formatDate(order.createdAt) : '时间未知'}</span></div>}<div><span>{meta.closed ? '完结时间' : '下单时间'}</span><span>{order.createdAt ? formatDate(order.createdAt) : '时间未知'}</span></div><div className="order-note-row"><span>订单备注</span>{editingOrderId === order.id ? <span className="order-note-editor"><input value={orderNote} onChange={(event) => setOrderNote(event.target.value)} autoFocus /><button type="button" disabled={savingNote} onClick={() => void saveOrderNote(order)}>保存</button></span> : <button type="button" className="order-note-button" onClick={() => { setEditingOrderId(order.id); setOrderNote(order.note) }}>{order.note || '添加备注'} <Pencil size={11} /></button>}</div></div>{pendingShipment && <div className="related-order-actions"><button type="button" className="primary" onClick={() => void onOrderAction(order, '去发货')}>去发货</button><button type="button" className="secondary" onClick={() => void onOrderAction(order, '取消订单')}>取消订单</button></div>}{meta.shipped && !meta.success && !meta.refunded && <div className="related-order-actions"><button type="button" className="primary" onClick={() => void onOrderAction(order, '提醒收货')}>提醒收货</button></div>}{meta.success && !meta.refunded && <div className="related-order-actions"><button type="button">查看评价</button><button type="button">查看钱款</button></div>}{meta.refunded && <div className="refund-success-banner">退款成功 <b>›</b></div>}</article> })}</div> : <p className="related-orders-empty">当前筛选暂无订单</p>}</section>{detailOrder && <OrderDetailModal order={detailOrder} product={inventory.find((item) => item.accountId === detailOrder.accountId && (item.id.endsWith(`-${detailOrder.itemId}`) || item.title === detailOrder.productTitle || detailOrder.productTitle.includes(item.id.split("-").pop() || "")))} onClose={() => setDetailOrder(null)} />}
    {(profile?.syncNote || error) && <p className="customer-sync-note">{profile?.syncNote || `客户资料同步失败：${error}`}</p>}
  </div>
}

function CustomerRemarkDialog({ value, onClose, onSave }: { value: string; onClose: () => void; onSave: (remark: string) => Promise<void> }) {
  const [remark, setRemark] = useState(value)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const save = async () => {
    setSaving(true); setError('')
    try { await onSave(remark); } catch (nextError) { setError(nextError instanceof Error ? nextError.message : String(nextError)); setSaving(false) }
  }
  return <Modal title="给买家添加备注" onClose={() => { if (!saving) onClose() }}><div className="customer-remark-dialog"><textarea autoFocus maxLength={50} rows={6} value={remark} onChange={(event) => setRemark(event.target.value)} placeholder="请输入用户备注信息，此备注信息不对用户展示" /><div className="customer-remark-count">{remark.length} / 50</div>{error && <div className="form-error">{error}</div>}<div className="modal-actions"><button type="button" className="secondary" disabled={saving} onClick={onClose}>取消</button><button type="button" className="primary" disabled={saving} onClick={() => void save()}>{saving ? '保存中…' : '确定'}</button></div></div></Modal>
}

function ShipOrderDrawer({ accountId, order, onClose, onDone }: { accountId: string; order: Order; onClose: () => void; onDone: () => void }) {
  const [mode, setMode] = useState<'dummy' | 'offline'>('dummy')
  const [description, setDescription] = useState('')
  const [mailNo, setMailNo] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState('')
  const submit = async () => {
    setSubmitting(true); setError('')
    try {
      if (mode === 'offline') await api.shipOrderWithLogistics(accountId, order.orderNo, mailNo)
      else await api.shipOrderWithoutParcel(accountId, order.orderNo, description)
      onDone()
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
      setSubmitting(false)
    }
  }
  return <div className="trade-drawer-backdrop" role="presentation" onMouseDown={() => { if (!submitting) onClose() }}><aside className="trade-drawer" role="dialog" aria-modal="true" aria-label="去发货" onMouseDown={(event) => event.stopPropagation()}><header><div><h2>去发货</h2><small>订单号 {order.orderNo}</small></div><button type="button" className="icon-button" disabled={submitting} onClick={onClose}><X size={19} /></button></header><div className="trade-drawer-tabs"><button className={mode === 'dummy' ? 'active' : ''} type="button" disabled={submitting} onClick={() => { setMode('dummy'); setError('') }}>无需寄件</button><button className={mode === 'offline' ? 'active' : ''} type="button" disabled={submitting} onClick={() => { setMode('offline'); setError('') }}>我已寄出</button></div><main>{mode === 'dummy' ? <><p className="trade-drawer-intro">无需填写物流单号。确认后，闲鱼会将此订单更新为已发货，等待买家确认收货。</p><label className="trade-text-label">相关描述 <span>选填</span><textarea maxLength={200} rows={6} value={description} onChange={(event) => setDescription(event.target.value)} placeholder="可填写发货说明、服务使用说明等" /><small>{description.length} / 200</small></label></> : <><p className="trade-drawer-intro">填写快递单号后，系统会调用闲鱼官方接口识别快递公司，并使用账号在闲鱼设置的默认寄件地址发货。</p><label className="trade-text-label">快递单号 <input autoFocus value={mailNo} maxLength={80} onChange={(event) => setMailNo(event.target.value)} placeholder="请输入快递单号" /></label><small className="trade-shipping-hint">快递公司将由闲鱼根据单号自动识别。</small></>}{error && <div className="form-error">{error}</div>}</main><footer><button type="button" className="secondary" disabled={submitting} onClick={onClose}>取消</button><button type="button" className="primary" disabled={submitting || (mode === 'offline' && !mailNo.trim())} onClick={() => void submit()}>{submitting ? '发货中…' : '确认发货'}</button></footer></aside></div>
}

function CancelOrderDrawer({ accountId, order, onClose, onDone }: { accountId: string; order: Order; onClose: () => void; onDone: () => void }) {
  const reasons = ['不想卖了', '宝贝已出售', '买家联系不上', '与买家协商一致', '其他原因']
  const [reason, setReason] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState('')
  const submit = async () => {
    if (!reason) { setError('请选择关闭交易原因'); return }
    setSubmitting(true); setError('')
    try {
      await api.cancelOrderBySeller(accountId, order.orderNo, reason)
      onDone()
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : String(nextError))
      setSubmitting(false)
    }
  }
  return <div className="trade-drawer-backdrop" role="presentation" onMouseDown={() => { if (!submitting) onClose() }}><aside className="trade-drawer cancel-order-drawer" role="dialog" aria-modal="true" aria-label="取消订单" onMouseDown={(event) => event.stopPropagation()}><header><div><h2>关闭交易</h2><small>订单号 {order.orderNo}</small></div><button type="button" className="icon-button" disabled={submitting} onClick={onClose}><X size={19} /></button></header><main><p className="trade-drawer-warning">关闭后订单不可恢复，请确认已与买家沟通。</p><fieldset className="cancel-reason-list"><legend>请选择关闭原因</legend>{reasons.map((item) => <label key={item}><input type="radio" name="cancel-reason" value={item} checked={reason === item} onChange={() => setReason(item)} />{item}</label>)}</fieldset>{error && <div className="form-error">{error}</div>}</main><footer><button type="button" className="secondary" disabled={submitting} onClick={onClose}>暂不关闭</button><button type="button" className="danger-button" disabled={submitting || !reason} onClick={() => void submit()}>{submitting ? '关闭中…' : '确认关闭交易'}</button></footer></aside></div>
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

function LogManager({ onClose }: { onClose: () => void }) {
  const [logs, setLogs] = useState<AppLog[]>([])
  const [failed, setFailed] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    let alive = true
    const load = async () => {
      try {
        const entries = await api.appLogs(500)
        if (!alive) return
        setLogs(entries)
        setFailed(false)
      } catch {
        if (alive) setFailed(true)
      }
    }
    void load()
    const timer = window.setInterval(() => void load(), 1_000)
    return () => { alive = false; window.clearInterval(timer) }
  }, [])
  useEffect(() => {
    const target = scrollRef.current
    if (target) target.scrollTop = target.scrollHeight
  }, [logs])
  return <Modal title="日志管理" onClose={onClose}><div className="log-manager"><div className="log-manager-status"><span><i />自动刷新中 · 每秒更新</span><small>{logs.length} 条本地日志</small></div><div ref={scrollRef} className="log-list" aria-live="polite">{failed ? <p className="log-empty">日志暂时无法读取。</p> : logs.length ? logs.map((entry) => <article className={`log-entry ${entry.level}`} key={entry.id}><time>{formatDate(entry.createdAt)}</time><span className="log-level">{entry.level}</span><span className="log-category">{entry.category}</span><p>{entry.accountId ? `[${entry.accountId.slice(0, 8)}] ` : ''}{entry.message}</p></article>) : <p className="log-empty">暂无日志，连接 IM 后会在这里显示协议阶段与错误。</p>}</div></div></Modal>
}

type SettingsSection = 'general' | 'account' | 'service' | 'notifications' | 'data' | 'about'

function SettingsPage({ quickReplyAutoSuggest, onQuickReplyAutoSuggestChange, onExport, onOpenLogs, onQrLogin }: { quickReplyAutoSuggest: boolean; onQuickReplyAutoSuggestChange: (value: boolean) => void; onExport: () => void; onOpenLogs: () => void; onQrLogin: () => void }) {
  const [section, setSection] = useState<SettingsSection>('general')
  const sections: Array<{ id: SettingsSection; label: string; icon: typeof Settings }> = [
    { id: 'general', label: '通用设置', icon: Settings },
    { id: 'account', label: '账号登录', icon: UserRound },
    { id: 'service', label: '客服设置', icon: MessageCircle },
    { id: 'notifications', label: '通知提醒', icon: Bell },
    { id: 'data', label: '数据与备份', icon: Download },
    { id: 'about', label: '系统信息', icon: CircleHelp },
  ]
  const title = sections.find((item) => item.id === section)?.label ?? '通用设置'

  return <div className="settings-layout settings-layout-reference">
    <aside className="settings-nav" aria-label="设置分类">
      <div className="settings-nav-title"><span>设置管理</span></div>
      <div className="settings-nav-list">{sections.map(({ id, label }) => <button type="button" key={id} className={section === id ? 'active' : ''} onClick={() => setSection(id)}><span>{label}</span></button>)}</div>
      <p className="settings-nav-note"><ShieldCheck size={14} />设置仅保存在当前电脑</p>
    </aside>

    <div className="settings-main">
      <header className="settings-main-head"><div><p>设置管理</p><h1>{title}</h1><span>本地优先，安全可控</span></div></header>
      {section === 'general' && <>
        <SettingsGroup title="快捷回复" description="让常用话术在客服会话中更快出现。">
          <div className="settings-row"><div><h3>快捷回复自动联想</h3><p>输入文字时，按已有快捷回复的标题和内容显示匹配建议。</p></div><label className="settings-switch"><b>{quickReplyAutoSuggest ? '已开启' : '已关闭'}</b><input type="checkbox" checked={quickReplyAutoSuggest} onChange={(event) => onQuickReplyAutoSuggestChange(event.target.checked)} /><span aria-hidden="true" /></label></div>
        </SettingsGroup>
        <SettingsGroup title="本机连接器" description="账号会话、商品和订单同步均直接在鲨鱼管家中运行。">
          <div className="settings-row"><div className="settings-row-with-icon"><span className="settings-symbol"><Store size={18} /></span><div><h3>连接服务状态</h3><p>无需额外服务器，登录后的会话将保存在本机。</p></div></div><span className="settings-status"><i />已就绪</span></div>
        </SettingsGroup>
      </>}
      {section === 'account' && <SettingsGroup title="账号登录" description="通过扫码连接闲鱼账号，账号信息会加入当前工作台。"><div className="settings-row"><div className="settings-row-with-icon"><span className="settings-symbol"><UserRound size={18} /></span><div><h3>扫码登录闲鱼</h3><p>打开二维码后使用闲鱼 App 扫描，登录成功后自动保存本机会话。</p></div></div><button className="settings-action" onClick={onQrLogin}>前往扫码<ExternalLink size={15} /></button></div></SettingsGroup>}
      {section === 'service' && <SettingsGroup title="客服偏好" description="客服工作台会根据以下设置提供辅助。"><div className="settings-row"><div className="settings-row-with-icon"><span className="settings-symbol"><MessageCircle size={18} /></span><div><h3>快捷回复自动联想</h3><p>开启后，输入内容会自动匹配已有的话术，按需选择发送。</p></div></div><label className="settings-switch"><b>{quickReplyAutoSuggest ? '已开启' : '已关闭'}</b><input type="checkbox" checked={quickReplyAutoSuggest} onChange={(event) => onQuickReplyAutoSuggestChange(event.target.checked)} /><span aria-hidden="true" /></label></div></SettingsGroup>}
      {section === 'notifications' && <NotificationSettings />}
      {section === 'data' && <>
        <SettingsGroup title="本地备份" description="导出的文件可用于迁移和问题排查。"><div className="settings-row"><div className="settings-row-with-icon"><span className="settings-symbol"><Download size={18} /></span><div><h3>导出本地备份</h3><p>导出账号资料、商品和订单为 JSON 文件。</p></div></div><button className="settings-action" onClick={onExport}>导出备份<Download size={15} /></button></div></SettingsGroup>
        <SettingsGroup title="运行记录" description="查看连接、同步和错误日志，便于定位问题。"><div className="settings-row"><div className="settings-row-with-icon"><span className="settings-symbol"><History size={18} /></span><div><h3>日志管理</h3><p>日志会自动刷新并跟随最新记录。</p></div></div><button className="settings-action" onClick={onOpenLogs}>查看日志<ExternalLink size={15} /></button></div></SettingsGroup>
      </>}
      {section === 'about' && <SettingsGroup title="鲨鱼管家" description="专为闲鱼商家打造的本地优先桌面工作台。"><div className="settings-about"><img src={logo} alt="鲨鱼管家" /><div><h3>鲨鱼管家</h3><p>v0.1.2 · Tauri 跨平台桌面应用</p><span>macOS / Windows / Linux</span></div></div></SettingsGroup>}
    </div>
  </div>
}

function SettingsGroup({ title, description, children }: { title: string; description: string; children: React.ReactNode }) { return <section className="settings-group"><header><h2>{title}</h2><p>{description}</p></header>{children}</section> }

type CustomNotificationSound = { name: string; duration: number }

function NotificationSettings() {
  const [selectedSound, setSelectedSound] = useState<NotificationSoundId>(() => selectedNotificationSoundId())
  const [customSound, setCustomSound] = useState<CustomNotificationSound | null>(() => {
    try { return JSON.parse(localStorage.getItem(notificationCustomSoundStorageKey.replace('sound', 'meta')) || 'null') as CustomNotificationSound | null } catch { return null }
  })
  const [uploadError, setUploadError] = useState('')
  const [uploading, setUploading] = useState(false)
  const [notificationToggles, setNotificationToggles] = useState<Record<string, boolean>>(() => {
    try { return { chat: true, order: true, system: true, offline: false, ...JSON.parse(localStorage.getItem('shark-butler-notification-toggles') || '{}') as Record<string, boolean> } } catch { return { chat: true, order: true, system: true, offline: false } }
  })
  const fileRef = useRef<HTMLInputElement>(null)

  const updateToggle = (key: string, value: boolean) => {
    setNotificationToggles((current) => {
      const next = { ...current, [key]: value }
      localStorage.setItem('shark-butler-notification-toggles', JSON.stringify(next))
      return next
    })
  }

  const selectSound = (id: NotificationSoundId) => {
    setSelectedSound(id)
    localStorage.setItem(notificationSoundStorageKey, id)
  }

  const previewSound = async (id: NotificationSoundId) => {
    if (id === 'custom') {
      const dataUrl = localStorage.getItem(notificationCustomSoundStorageKey)
      if (dataUrl) { const audio = new Audio(dataUrl); audio.volume = .85; void audio.play().catch(() => undefined); }
      return
    }
    const preset = notificationSoundPresets.find((item) => item.id === id)
    try {
      const AudioContextCtor = window.AudioContext || (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
      if (!AudioContextCtor) return
      messageNotificationContext ??= new AudioContextCtor()
      if (messageNotificationContext.state === 'suspended') await messageNotificationContext.resume()
      if (preset) playToneSequence(messageNotificationContext, preset.sequence)
    } catch { /* 浏览器禁止自动播放时，用户再次点击试听即可。 */ }
  }

  const handleUpload = async (file: File | undefined) => {
    if (!file) return
    setUploadError(''); setUploading(true)
    try {
      if (!file.type.startsWith('audio/')) throw new Error('请选择音频文件。')
      const AudioContextCtor = window.AudioContext || (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
      if (!AudioContextCtor) throw new Error('当前环境不支持音频解析。')
      const context = new AudioContextCtor()
      const decoded = await context.decodeAudioData(await file.arrayBuffer())
      const duration = decoded.duration
      await context.close()
      if (duration < 1 || duration > 3) throw new Error(`音频时长为 ${duration.toFixed(1)} 秒，请上传 1～3 秒的文件。`)
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('音频读取失败。'))
        reader.onerror = () => reject(new Error('音频读取失败。'))
        reader.readAsDataURL(file)
      })
      localStorage.setItem(notificationCustomSoundStorageKey, dataUrl)
      localStorage.setItem(notificationCustomSoundStorageKey.replace('sound', 'meta'), JSON.stringify({ name: file.name, duration }))
      localStorage.setItem(notificationSoundStorageKey, 'custom')
      setCustomSound({ name: file.name, duration })
      setSelectedSound('custom')
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : '音频处理失败，请换一个文件重试。')
    } finally {
      setUploading(false)
      if (fileRef.current) fileRef.current.value = ''
    }
  }

  return <>
    <SettingsGroup title="通知开关" description="控制店铺消息和系统状态的提示提醒。">
      <div className="notification-toggle-list">
        {[
          ['聊天新消息', '收到买家消息时播放提示音。'],
          ['订单消息', '新订单、付款和售后消息使用同一提示音。'],
          ['系统消息', '同步完成、连接异常等系统事件提醒。'],
          ['离线提示', '应用切到后台后仍保留消息提醒。'],
        ].map(([label, description], index) => { const key = ['chat', 'order', 'system', 'offline'][index]; return <label className="notification-toggle" key={label}><span><b>{label}</b><small>{description}</small></span><input type="checkbox" checked={Boolean(notificationToggles[key])} onChange={(event) => updateToggle(key, event.target.checked)} /><i aria-hidden="true" /></label> })}
      </div>
    </SettingsGroup>
    <SettingsGroup title="提示音" description="选择一个内置提示音，或上传 1～3 秒的自定义音频。">
      <div className="notification-sound-groups">{notificationSoundGroups.map((group) => <section className="notification-sound-group" key={group.id}>
        <header><div><h3>{group.label}</h3><p>{group.description}</p></div><span>{notificationSoundPresets.filter((preset) => preset.group === group.id).length} 个</span></header>
        <div className="notification-sound-grid">{notificationSoundPresets.filter((preset) => preset.group === group.id).map((preset) => <div className={`notification-sound-card ${selectedSound === preset.id ? 'selected' : ''}`} key={preset.id}>
          <button type="button" className="notification-sound-main" onClick={() => selectSound(preset.id)}><span className="notification-sound-icon" style={{ background: preset.accent }}><Bell size={15} /></span><span><b>{preset.name}</b><small>{preset.description} · 1～2 秒</small></span><em>{selectedSound === preset.id ? '使用中' : '选择'}</em></button>
          <button type="button" className="notification-preview-button" onClick={() => void previewSound(preset.id)} aria-label={`试听${preset.name}`}><Send size={13} />试听</button>
        </div>)}</div>
      </section>)}</div>
      <div className={`notification-custom ${selectedSound === 'custom' ? 'selected' : ''}`}>
        <div className="notification-custom-copy"><span className="notification-sound-icon custom"><Upload size={15} /></span><div><b>{customSound?.name || '自定义音频'}</b><small>{customSound ? `${customSound.duration.toFixed(1)} 秒 · 已通过时长校验` : '支持 MP3、WAV、M4A，时长必须为 1～3 秒'}</small></div></div>
        <div className="notification-custom-actions"><input ref={fileRef} type="file" accept="audio/*" hidden onChange={(event) => void handleUpload(event.target.files?.[0])} /><button type="button" className="settings-action" onClick={() => fileRef.current?.click()} disabled={uploading}>{uploading ? '解析中…' : '上传音频'}</button>{customSound && <button type="button" className="notification-preview-button" onClick={() => void previewSound('custom')}>试听</button>}</div>
      </div>
      {uploadError && <p className="notification-upload-error" role="alert">{uploadError}</p>}
    </SettingsGroup>
  </>
}

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: React.ReactNode }) { return <div className="page-head"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{description}</p></div>{action}</div> }
function Toolbar({ account, placeholder }: { account?: Account; placeholder: string }) { return <div className="toolbar"><div className="search-field"><Search size={17} /><input placeholder={placeholder} /></div><AccountPicker account={account} /><button className="filter-button">全部状态<ChevronDown size={15} /></button></div> }
