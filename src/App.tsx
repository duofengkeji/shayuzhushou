import { useEffect, useMemo, useState } from 'react'
import {
  Bell, Box, ChevronDown, CircleHelp, ClipboardList, CloudOff, ExternalLink,
  LayoutDashboard, MessageCircle, MoreHorizontal, Package, Plus, RefreshCw,
  Search, Settings, ShieldCheck, Store, UserRound, UsersRound,
} from 'lucide-react'
import logo from './assets/shark-butler-logo.png'
import { api } from './lib/api'
import type { Account, DashboardStats, Order, Product } from './lib/types'

type Page = 'dashboard' | 'workbench' | 'accounts' | 'products' | 'orders' | 'settings'

const nav: { id: Page; label: string; icon: typeof LayoutDashboard }[] = [
  { id: 'dashboard', label: '首页', icon: LayoutDashboard },
  { id: 'workbench', label: '客服', icon: MessageCircle },
  { id: 'accounts', label: '账号', icon: UsersRound },
  { id: 'products', label: '商品', icon: Package },
  { id: 'orders', label: '订单', icon: ClipboardList },
  { id: 'settings', label: '设置', icon: Settings },
]

function formatDate(value: string) {
  return new Intl.DateTimeFormat('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }).format(new Date(value))
}

function Status({ value }: { value: string }) {
  const kind = value.includes('有效') || value.includes('上架') || value.includes('完成') ? 'ok'
    : value.includes('异常') || value.includes('关闭') ? 'danger' : 'warn'
  return <span className={`status status-${kind}`}>{value}</span>
}

export default function App() {
  const [page, setPage] = useState<Page>('dashboard')
  const [accounts, setAccounts] = useState<Account[]>([])
  const [products, setProducts] = useState<Product[]>([])
  const [orders, setOrders] = useState<Order[]>([])
  const [stats, setStats] = useState<DashboardStats | null>(null)
  const [accountId, setAccountId] = useState('')
  const [loading, setLoading] = useState(true)
  const [notice, setNotice] = useState('')

  const activeAccount = accounts.find((item) => item.id === accountId) ?? accounts[0]

  const refresh = async () => {
    setLoading(true)
    try {
      const [nextStats, nextAccounts, nextProducts, nextOrders] = await Promise.all([
        api.dashboard(), api.accounts(), api.products(), api.orders(),
      ])
      setStats(nextStats); setAccounts(nextAccounts); setProducts(nextProducts); setOrders(nextOrders)
      setAccountId((current) => current || nextAccounts[0]?.id || '')
    } catch {
      setNotice('无法读取本地数据，请检查应用数据目录。')
    } finally { setLoading(false) }
  }

  useEffect(() => { void refresh() }, [])

  const filteredProducts = useMemo(() => accountId ? products.filter((item) => item.accountId === accountId) : products, [products, accountId])
  const filteredOrders = useMemo(() => accountId ? orders.filter((item) => item.accountId === accountId) : orders, [orders, accountId])

  const addAccount = async () => {
    const name = window.prompt('请输入店铺名称', `新店铺 ${accounts.length + 1}`)?.trim()
    if (!name) return
    await api.addDemoAccount(name)
    setNotice('演示账号已添加。真实平台接入仅支持已授权的官方接口。')
    await refresh()
  }

  const syncAccount = async () => {
    if (!activeAccount) return
    await api.syncAccount(activeAccount.id)
    setNotice(`${activeAccount.displayName} 已完成本地演示同步。`)
    await refresh()
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
          <div className="workspace-select">
            <img src={logo} alt="" /><div><strong>鲨鱼管家</strong><span><i /> 本地工作空间</span></div><ChevronDown size={16} />
          </div>
          <button className="round-button" onClick={addAccount} title="添加账号"><Plus size={20} /></button>
          <div className="top-actions">
            <span className="sync-label"><ShieldCheck size={16} /> 本地数据受保护</span>
            <button className="text-button" onClick={syncAccount}><RefreshCw size={15} /> 刷新</button>
          </div>
        </header>

        {notice && <div className="notice"><ShieldCheck size={16} />{notice}<button onClick={() => setNotice('')}>×</button></div>}

        <div className="content">
          {loading ? <Loading /> : page === 'dashboard' ? <Dashboard stats={stats} accounts={accounts} orders={orders} onGo={setPage} />
            : page === 'workbench' ? <Workbench account={activeAccount} orders={filteredOrders} />
              : page === 'accounts' ? <Accounts accounts={accounts} onAdd={addAccount} onSync={syncAccount} />
                : page === 'products' ? <Products items={filteredProducts} account={activeAccount} />
                  : page === 'orders' ? <Orders items={filteredOrders} account={activeAccount} />
                    : <SettingsPage />}
        </div>
      </section>
    </main>
  )
}

function Loading() { return <div className="loading"><span /><p>正在加载本地工作台…</p></div> }

function Dashboard({ stats, accounts, orders, onGo }: { stats: DashboardStats | null; accounts: Account[]; orders: Order[]; onGo: (p: Page) => void }) {
  const cards: Array<{ label: string; value: number; detail: string; icon: typeof UsersRound; target: Page }> = [
    { label: '已连接账号', value: stats?.totalAccounts ?? 0, detail: `${stats?.healthyAccounts ?? 0} 个授权有效`, icon: UsersRound, target: 'accounts' },
    { label: '在售商品', value: stats?.activeProducts ?? 0, detail: '来自本地演示数据', icon: Package, target: 'products' },
    { label: '待处理订单', value: stats?.pendingOrders ?? 0, detail: '需要优先跟进', icon: ClipboardList, target: 'orders' },
  ]
  return <div className="page dashboard">
    <div className="welcome"><div><p className="eyebrow">运营概览</p><h1>早上好，今天也让店铺井井有条。</h1><p>所有数据保存在当前设备；只有获得授权的平台连接才会进行同步。</p></div><button className="primary" onClick={() => onGo('accounts')}><Store size={17} />管理账号</button></div>
    <div className="metric-grid">{cards.map(({ label, value, detail, icon: Icon, target }) => <button className="metric-card" key={label} onClick={() => onGo(target)}><span>{label}</span><strong>{value}</strong><small>{detail}</small><Icon size={22} /></button>)}</div>
    <div className="two-column">
      <section className="panel"><div className="panel-head"><div><h2>账号健康度</h2><p>授权和同步状态一目了然</p></div><button className="icon-button" onClick={() => onGo('accounts')}><ExternalLink size={17} /></button></div>
        <div className="account-list">{accounts.map((item) => <div className="account-row" key={item.id}><div className="avatar">{item.displayName.slice(0, 1)}</div><div className="grow"><strong>{item.displayName}</strong><span>{item.alias} · 更新于 {formatDate(item.lastSyncAt)}</span></div><Status value={item.status} /></div>)}</div>
      </section>
      <section className="panel"><div className="panel-head"><div><h2>最近订单</h2><p>按创建时间展示</p></div><button className="icon-button" onClick={() => onGo('orders')}><ExternalLink size={17} /></button></div>
        <div className="order-list">{orders.slice(0, 4).map((order) => <div className="mini-order" key={order.id}><div><strong>{order.productTitle}</strong><span>{order.orderNo} · {formatDate(order.createdAt)}</span></div><div><b>¥{order.amount.toFixed(2)}</b><Status value={order.status} /></div></div>)}</div>
      </section>
    </div>
  </div>
}

function AccountPicker({ account }: { account?: Account }) { return <button className="filter-button"><Store size={16} />{account?.displayName ?? '全部账号'}<ChevronDown size={15} /></button> }

function Accounts({ accounts, onAdd, onSync }: { accounts: Account[]; onAdd: () => void; onSync: () => void }) { return <div className="page">
  <PageHead eyebrow="账号管理" title="连接你的店铺" description="账号凭据只存放在设备系统安全凭据库中。" action={<button className="primary" onClick={onAdd}><Plus size={17} />添加账号</button>} />
  <div className="account-cards">{accounts.map((account) => <article className="account-card" key={account.id}><div className="account-card-top"><div className="avatar large">{account.displayName.slice(0, 1)}</div><button className="icon-button"><MoreHorizontal size={18} /></button></div><h3>{account.displayName}</h3><p>{account.alias} · {account.platform}</p><Status value={account.status} /><div className="account-numbers"><span><b>{account.productCount}</b> 商品</span><span><b>{account.orderCount}</b> 订单</span></div><div className="card-footer"><span>同步于 {formatDate(account.lastSyncAt)}</span><button onClick={onSync}>立即同步</button></div></article>)}</div>
</div> }

function Products({ items, account }: { items: Product[]; account?: Account }) { return <div className="page">
  <PageHead eyebrow="商品管理" title="商品一览" description="基于本地缓存展示；写入操作必须经平台官方授权接口执行。" action={<button className="secondary"><Box size={17} />批量操作</button>} />
  <Toolbar account={account} placeholder="搜索商品标题、标签或商品 ID" />
  <section className="table-panel"><table><thead><tr><th>商品</th><th>价格</th><th>库存</th><th>状态</th><th>本地标签</th><th>最后同步</th><th /></tr></thead><tbody>{items.map((item) => <tr key={item.id}><td><div className="product-cell"><div className="product-image"><Package size={20} /></div><div><strong>{item.title}</strong><span>ID · {item.id}</span></div></div></td><td>¥{item.price.toFixed(2)}</td><td>{item.stock}</td><td><Status value={item.status} /></td><td><div className="tags">{item.tags.map((tag) => <span key={tag}>{tag}</span>)}</div></td><td>{formatDate(item.updatedAt)}</td><td><button className="icon-button"><MoreHorizontal size={18} /></button></td></tr>)}</tbody></table></section>
</div> }

function Orders({ items, account }: { items: Order[]; account?: Account }) { return <div className="page">
  <PageHead eyebrow="订单管理" title="订单中心" description="默认脱敏展示买家信息，导出时可按需选择字段。" action={<button className="secondary"><ExternalLink size={17} />导出 CSV</button>} />
  <Toolbar account={account} placeholder="搜索订单号、商品或买家" />
  <section className="table-panel"><table><thead><tr><th>订单号</th><th>商品</th><th>买家</th><th>金额</th><th>状态</th><th>创建时间</th><th>备注</th></tr></thead><tbody>{items.map((item) => <tr key={item.id}><td><strong>{item.orderNo}</strong></td><td>{item.productTitle}</td><td><span className="masked"><UserRound size={14} />{item.buyerMaskedName}</span></td><td><strong>¥{item.amount.toFixed(2)}</strong></td><td><Status value={item.status} /></td><td>{formatDate(item.createdAt)}</td><td className="muted">{item.note || '—'}</td></tr>)}</tbody></table></section>
</div> }

function Workbench({ account, orders }: { account?: Account; orders: Order[] }) { return <div className="workbench">
  <section className="conversation-panel"><div className="workbench-search"><Search size={17} /><input placeholder="搜索联系人 / 订单 / 备注" /></div><div className="tabs"><b>全部</b><span>待处理</span><span>已完成</span></div>{orders.map((order, index) => <button className={`conversation ${index === 0 ? 'selected' : ''}`} key={order.id}><div className="avatar">{order.buyerMaskedName.slice(0, 1)}</div><div><strong>{order.buyerMaskedName}</strong><span>{order.productTitle}</span><small>{formatDate(order.createdAt)}</small></div><Status value={order.status} /></button>)}</section>
  <section className="chat-panel"><header><div><p>{account?.displayName ?? '当前账号'}</p><span>演示工作台 · 消息能力待合规接入</span></div><button className="filter-button"><Store size={15} />{account?.platform ?? '平台'}</button></header><div className="chat-blank"><img src={logo} alt="" /><h2>客服工作台</h2><p>已保留会话、商品与订单联动布局。完成经授权的消息接入后即可启用消息同步与发送。</p><span><CloudOff size={15} /> 当前为本地演示模式</span></div><footer><div className="composer-toolbar"><Bell size={18} /><Package size={18} /><MoreHorizontal size={18} /></div><div className="composer-placeholder">请选择合规消息连接后开始使用</div><button className="primary disabled">发送</button></footer></section>
  <aside className="context-panel"><div className="context-tabs"><b>客服</b><span>快捷回复</span><span>商品</span><span>订单</span></div><div className="context-empty"><img src={logo} alt="" /><h3>待接入客服能力</h3><p>当前仅展示本地工作台框架。平台接口授权后，这里会显示客户与关联订单。</p><button className="secondary">查看接入说明</button></div></aside>
</div> }

function SettingsPage() { return <div className="page settings-page"><PageHead eyebrow="应用设置" title="本地优先，安全可控" description="这里的配置不会上传到第三方服务器。" />
  <section className="settings-card"><div><h3>数据与隐私</h3><p>账号会话凭据由系统安全凭据库托管；业务缓存保存在本机 SQLite 数据库。</p></div><ShieldCheck size={26} /></section>
  <section className="settings-card"><div><h3>平台连接</h3><p>仅启用平台官方 API 或取得书面授权的连接器。未配置时应用保持演示模式。</p></div><Store size={26} /></section>
  <section className="settings-card"><div><h3>关于鲨鱼管家</h3><p>v0.1.0 · Tauri 跨平台桌面应用 · macOS / Windows / Linux</p></div><img src={logo} alt="鲨鱼管家" /></section>
</div> }

function PageHead({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: React.ReactNode }) { return <div className="page-head"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{description}</p></div>{action}</div> }
function Toolbar({ account, placeholder }: { account?: Account; placeholder: string }) { return <div className="toolbar"><div className="search-field"><Search size={17} /><input placeholder={placeholder} /></div><AccountPicker account={account} /><button className="filter-button">全部状态<ChevronDown size={15} /></button></div> }
