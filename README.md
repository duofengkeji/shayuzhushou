# 鲨鱼管家

本地优先的多店铺桌面工作台，基于 Tauri 2 + React + Rust + SQLite 构建，可运行于 macOS、Windows 和 Linux。

当前版本完成了账号、商品、订单和客服工作台的可运行本地演示；所有演示数据均由应用初始化生成。真实平台连接必须使用官方 API 或其他已获书面授权的接口。

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
cargo check --manifest-path src-tauri/Cargo.toml
```

## 发布

推送 `v0.1.0` 形式的 Git 标签会触发 GitHub Actions，生成 macOS（Intel/Apple Silicon）、Windows（MSI/NSIS）和 Linux（AppImage/DEB）安装包，并创建草稿 Release。未配置 Apple/Windows 代码签名凭据时，安装包会以未签名状态发布。

## 许可证与参考

本仓库代码使用 MIT 许可证。`zhinianboke/xianyu-auto-reply` 仅被用于产品需求研究，未包含、复制或改编其中的 AGPL-3.0 代码或资产；Tauri、React、Lucide 和 GitHub Actions 均通过各自的开源许可证引入。

详细方案见 [开发文档](docs/鲨鱼管家-开发文档.md)。
