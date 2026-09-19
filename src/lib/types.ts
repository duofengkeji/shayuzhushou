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
  sourceUrl: string
  remoteAccountId: string
  conversationName: string
  avatarUrl: string
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

export interface QuickReplyImage {
  name: string
  mimeType: string
  dataUrl: string
}

export interface QuickReply {
  id: string
  accountId: string
  title: string
  content: string
  shortCode: string
  images: QuickReplyImage[]
  updatedAt: string
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

export interface SyncResult {
  account: Account
  productsChanged: number
  ordersChanged: number
  sourceConnected: boolean
}

export interface SyncJob {
  id: string
  accountId: string
  resource: string
  status: string
  startedAt: string
  finishedAt: string
  errorMessage: string
}

export interface ChatContact {
  accountId: string
  chatId: string
  otherUserId: string
  otherUserName: string
  avatarUrl: string
  itemId: string
  itemTitle: string
  itemImageUrl: string
  orderStatus: string
  buyerTag: string
  latestMessage: string
  latestMessageTime: string
  unreadCount: number
}

export interface ChatMessage {
  id: string
  accountId: string
  chatId: string
  senderUserId: string
  senderUserName: string
  direction: 'incoming' | 'outgoing'
  contentKind: 'text' | 'image' | 'video' | 'location' | 'product' | string
  text: string
  mediaUrl: string
  sentAt: string
  sendStatus: string
}

export interface ChatContactsPage {
  items: ChatContact[]
  nextCursor: number | null
  hasMore: boolean
}

export interface ChatMessagesPage {
  items: ChatMessage[]
  nextCursor: number | null
  hasMore: boolean
}

export interface QrLoginStart {
  success: boolean
  sessionId: string
  qrCodeUrl: string
  message: string
}

export interface QrLoginStatus {
  success: boolean
  status: string
  message: string
  faceQrUrl: string
  verificationUrl: string
  accountId: string
  displayName: string
  isNewAccount: boolean
}

export interface BackupData {
  exportedAt: string
  accounts: Account[]
  products: Product[]
  orders: Order[]
}

export interface AccountInput {
  displayName: string
  alias: string
  platform: string
  status: AccountStatus
  sourceUrl: string
  remoteAccountId: string
}

export interface ProductInput {
  accountId: string
  title: string
  price: number
  stock: number
  status: string
  tags: string[]
}

export interface OrderInput {
  accountId: string
  productTitle: string
  buyerMaskedName: string
  amount: number
  status: string
  note: string
}
