import { invoke } from '@tauri-apps/api/core'
import type { Account, DashboardStats, Order, Product } from './types'

export const api = {
  dashboard: () => invoke<DashboardStats>('dashboard_stats'),
  accounts: () => invoke<Account[]>('list_accounts'),
  products: (accountId?: string) => invoke<Product[]>('list_products', { accountId }),
  orders: (accountId?: string) => invoke<Order[]>('list_orders', { accountId }),
  addDemoAccount: (displayName: string) => invoke<Account>('add_demo_account', { displayName }),
  syncAccount: (accountId: string) => invoke<Account>('sync_account', { accountId }),
}
