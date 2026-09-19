# 鲨鱼管家

本地优先的多店铺桌面工作台，基于 Tauri 2 + React + Rust + SQLite 构建，可运行于 macOS、Windows 和 Linux。

当前版本已接入本机闲鱼扫码登录、商品同步、订单同步、客服会话、历史消息和手动文本发送。数据落在鲨鱼管家自己的 SQLite，账号会话以 AES-256-GCM 加密；运行时不需要 HLSRental 或其他业务服务。新安装不再初始化演示数据。

## 开发

需要 Node.js 20+ 和 Rust stable：

```bash
npm install
npm run desktop:dev
```

执行静态检查与前端构建：

```bash
npm run check
npm run build
rustup run stable cargo check --manifest-path src-tauri/Cargo.toml
```

## 发布

推送 `v0.1.1` 形式的 Git 标签会触发 GitHub Actions，生成 macOS（Intel/Apple Silicon）、Windows（MSI/NSIS）和 Linux（AppImage/DEB）安装包，并创建草稿 Release。未配置 Apple/Windows 代码签名凭据时，安装包会以未签名状态发布。

## 许可证与参考

本仓库代码使用 MIT 许可证。`zhinianboke/xianyu-auto-reply` 用于产品与自动化方案参考；本地 HLSRental 项目仅作已运行链路的源码参考，鲨鱼管家不调用它的 API、数据库或配置。Tauri、React、Lucide 和 GitHub Actions 均通过各自的开源许可证引入。

详细方案见 [开发文档](docs/鲨鱼管家-开发文档.md)。
