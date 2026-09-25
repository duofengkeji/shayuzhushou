#!/bin/sh
set -eu

# One-click release helper for Shark Butler.
# Usage: sh scripts/release.sh

ROOT_DIR=$(git rev-parse --show-toplevel 2>/dev/null || true)
if [ -z "$ROOT_DIR" ]; then echo "错误：当前目录不在 Git 仓库中。" >&2; exit 1; fi
cd "$ROOT_DIR"

printf '%s' "请输入版本号（例如 0.1.6 或 v0.1.6）："
IFS= read -r VERSION_INPUT
VERSION=${VERSION_INPUT#v}
if ! printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$'; then
  echo "错误：版本号格式无效，请使用 semver，例如 0.1.6。" >&2; exit 1
fi

printf '%s' "请输入提交描述："
IFS= read -r COMMIT_MESSAGE
case "$COMMIT_MESSAGE" in *[![:space:]]*) ;; *) echo "错误：提交描述不能为空。" >&2; exit 1 ;; esac

TAG="v$VERSION"
REMOTE=${RELEASE_REMOTE:-origin}
LOCAL_TAG_EXISTS=false
REMOTE_TAG_EXISTS=false
if git show-ref --tags --verify --quiet "refs/tags/$TAG"; then LOCAL_TAG_EXISTS=true; fi
if git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null 2>&1; then REMOTE_TAG_EXISTS=true; fi

if [ "$LOCAL_TAG_EXISTS" = true ] || [ "$REMOTE_TAG_EXISTS" = true ]; then
  echo "tag $TAG 已存在：本地=$LOCAL_TAG_EXISTS，远程=$REMOTE_TAG_EXISTS。"
  printf '%s' "是否覆盖并重新推送该 tag？y/N "
  IFS= read -r OVERWRITE
  case "$OVERWRITE" in y|Y) ;; *) echo "已取消，未修改文件或提交。"; exit 0 ;; esac
fi

node - "$VERSION" <<'NODE'
const fs = require('node:fs')
const version = process.argv[2]
const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'))
const packageLock = JSON.parse(fs.readFileSync('package-lock.json', 'utf8'))
const tauriConfig = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8'))
if (!packageLock.packages?.['']) throw new Error('package-lock.json 缺少根项目')
packageJson.version = version
packageLock.version = version
packageLock.packages[''].version = version
tauriConfig.version = version
for (const [file, data] of [
  ['package.json', packageJson],
  ['package-lock.json', packageLock],
  ['src-tauri/tauri.conf.json', tauriConfig],
]) fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n')
NODE
RELEASE_VERSION="$VERSION" perl -0pi -e 's/(\[package\]\nname = "shark-butler"\nversion = ")[^"]+(")/$1 . $ENV{RELEASE_VERSION} . $2/e or die "Cargo.toml 中未找到项目版本\n"' src-tauri/Cargo.toml
RELEASE_VERSION="$VERSION" perl -0pi -e 's/(name = "shark-butler"\nversion = ")[^"]+(")/$1 . $ENV{RELEASE_VERSION} . $2/e or die "Cargo.lock 中未找到项目版本\n"' src-tauri/Cargo.lock

echo "已将版本更新为 ${VERSION}。正在执行检查..."
npm run check
cargo check --manifest-path src-tauri/Cargo.toml

# Include tracked local changes and this helper, leaving unrelated scratch files untouched.
git add -u
git add scripts/release.sh
if git diff --cached --quiet; then echo "错误：没有可提交的本地变更。" >&2; exit 1; fi
git commit -m "$COMMIT_MESSAGE"
git push "$REMOTE" HEAD

if [ "$LOCAL_TAG_EXISTS" = true ]; then git tag -d "$TAG"; fi
if [ "$REMOTE_TAG_EXISTS" = true ]; then git push "$REMOTE" ":refs/tags/$TAG"; fi
git tag -a "$TAG" -m "$TAG"
git push "$REMOTE" "$TAG"
echo "发布完成：$TAG"
