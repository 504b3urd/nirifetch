# 打包

本目录存放 Arch Linux 的打包配方。`PKGBUILD` 同时服务两个场景：

- **本目录** —— 任何人 clone 上游仓库后都能自己构建
- **[AUR 仓库](https://aur.archlinux.org/packages/nirifetch)** —— 内容与其保持一致

## 为什么 PKGBUILD 不放仓库根目录

makepkg 的 `$srcdir` 就叫 `src/`，而它在解包前会先执行 `rm -rf "$srcdir"`。
上游仓库根目录的 `src/` 正是 Rust 源码目录 —— 在根目录跑 makepkg 会把源码删掉。

放进 `packaging/` 后 `$srcdir` 变成 `packaging/src/`，与源码目录不再冲突。
**永远不要在仓库根目录执行 makepkg。**

## 本地构建并安装

```sh
cd packaging
makepkg -si          # -s 自动装缺失的 makedepends，-i 构建完直接装
```

只要包不装，也可以只构建：

```sh
makepkg -f
sudo pacman -U nirifetch-0.2.0-1-x86_64.pkg.tar.zst
```

`build()` 用 `cargo build --release --locked`，`check()` 会跑完整测试套件
（201 个测试，不联网也不需要特定硬件）。

## 发布到 AUR

> AUR **只走 SSH**（`aur@aur.archlinux.org:22`），且需要账号里登记过你的公钥。
> 若该端口被网络环境拦截，这一步无法完成。

首次：

```sh
# 1. 生成并登记公钥（AUR → My Account → SSH Public Key）
ssh-keygen -t ed25519 -C "aur"
cat ~/.ssh/id_ed25519.pub

# 2. 克隆 AUR 仓库（此刻它还是空的）
git clone ssh://aur@aur.archlinux.org/nirifetch.git
cd nirifetch

# 3. 拷入配方，生成 .SRCINFO 后提交
cp /path/to/nirifetch/packaging/PKGBUILD .
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "Initial import: nirifetch 0.1.0-1"
git push
```

后续更新只需重复第 3 步。`.SRCINFO` **必须**重新生成 —— AUR 读的是它，
不是 PKGBUILD 本身。

## 升级版本时改什么

1. `PKGBUILD` 里的 `pkgver`（`pkgrel` 归 1）
2. `source` 里的 tag 名 —— 它由 `$pkgver` 拼出，通常无需改动
3. `sha256sums` —— **必须重新计算**：

```sh
# 装上 pacman-contrib 后可以直接算（推荐，避免手抄出错）
updpkgsums

# 或者手动
curl -sL https://github.com/504b3urd/nirifetch/archive/refs/tags/v0.2.0.tar.gz | sha256sum
```

4. 重新生成 `.SRCINFO`，推 AUR

> 上游要先打好并推送 tag，`source` 才拉得到东西。顺序不能反。

## 校验和的稳定性

`source` 指向的是 GitHub **按 tag 自动生成**的 tarball。历史上 GitHub
更换过压缩实现，导致一批 AUR 包的校验和集体失效。若哪天构建报校验失败，
先确认 tarball 本身没被重新生成，再按上面的步骤更新 `sha256sums`。
