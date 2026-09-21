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
  memberName: string
}

export interface Product {
  id: string
  accountId: string
  title: string
  imageUrl: string
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
  itemId: string
  itemImageUrl: string
  productTitle: string
  specification: string
  buyerMaskedName: string
  amount: number
  statusCode: string
  status: string
  shippingRefundStatus: string
  refundAmount: number
  createdAt: string
  note: string
}

export interface Member {
  id: string
  accountId: string
  buyerId: string
  displayName: string
  phoneMasked: string
  addressMasked: string
  phoneAvailable: boolean
  addressAvailable: boolean
  firstOrderAt: string
  lastOrderAt: string
  orderCount: number
  paidOrderCount: number
  totalSpend: number
  averageOrderValue: number
  lastOrderStatus: string
  remark: string
  tags: string[]
  status: string
  createdAt: string
  updatedAt: string
  lastSyncedAt: string
}

export interface MemberOrder {
  order: Order
  matchedBy: string
}

export interface OrderDetail {
  order: Order
  paidAt: string
  shippedAt: string
  completedAt: string
  closedAt: string
  serviceFee?: number
  refundAmount?: number
}

export interface DashboardStats {
  totalAccounts: number
  healthyAccounts: number
  activeProducts: number
  pendingOrders: number
}

export interface ImVerificationState {
  required: boolean
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
  /** Official IM receipt: readStatus=2 => read; any returned other value => unread. */
  readStatus: 'read' | 'unread' | 'unsupported' | 'unknown' | string
  cardTitle: string
  cardSubtitle: string
  cardPrice: string
  targetUrl: string
}

export interface ChatEmoji {
  iconAlias: string
  iconUrl: string
}

export interface ChatContactsPage {
  items: ChatContact[]
  nextCursor: number | null
  hasMore: boolean
}

export interface CustomerItem {
  itemId: string
  title: string
  imageUrl: string
  price: string
  fishCoin: string
  status: string
  exposureCount: string
  viewCount: string
  wantCount: string
  visitedAt: string
}

export interface CustomerProfile {
  accountId: string
  chatId: string
  userId: string
  displayName: string
  avatarUrl: string
  remark: string
  creditLevel: string
  city: string
  lastActiveText: string
  goodReviewRate: string
  dataUpdatedAt: string
  purchaseCount: string
  totalSpend: string
  averageOrderValue: string
  currentItems: CustomerItem[]
  favoriteItems: CustomerItem[]
  consultedItems: CustomerItem[]
  officialSynced: boolean
  syncNote: string
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

export interface AppLog {
  id: number
  createdAt: string
  level: 'info' | 'warn' | 'error' | string
  category: string
  accountId: string
  message: string
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
  imageUrl: string
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

export interface RefundDetail {
  orderNo: string
  refundId: string
  status: string
  statusCode: string
  reason: string
  description: string
  amount: number
  createTime: string
  timeoutText: string
  deadlineAt: string
  receivedStatus: string
  returnGoodsStatus: string
  buyerEvidence: string
  freightStatus: string
  customerService: string
  buyerName: string
  productTitle: string
}

export interface RefundVerification {
  required: boolean
  verificationUrl: string
  authToken: string
  message: string
}
