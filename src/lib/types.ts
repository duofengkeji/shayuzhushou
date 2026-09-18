export type AccountStatus = '授权有效' | '即将过期' | '同步异常' | '已停用'

export interface Account {
  id: string
  displayName: string
  alias: string
  platform: string
  status: AccountStatus
  lastSyncAt: string
  productCount: number
  orderCount: number
}

export interface Product {
  id: string
  accountId: string
  title: string
  price: number
  stock: number
  status: string
  updatedAt: string
  tags: string[]
}

export interface Order {
  id: string
  accountId: string
  orderNo: string
  productTitle: string
  buyerMaskedName: string
  amount: number
  status: string
  createdAt: string
  note: string
}

export interface DashboardStats {
  totalAccounts: number
  healthyAccounts: number
  activeProducts: number
  pendingOrders: number
}
