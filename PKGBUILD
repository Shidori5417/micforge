# Maintainer: Mehmet Karaca
#
# This PKGBUILD builds from the local source tree it lives in ($startdir),
# so no download is needed. Run `makepkg -si` from the repository root.
#
# Build output goes to $srcdir/target (inside src/, removed with it), so the
# repository's own target/ directory is never touched by makepkg.
#
# To build from a published git repository later (once the project is on
# GitHub), replace the source handling like this:
#
#   source=("git+https://github.com/Shidori5417/micforge.git")
#   sha256sums=('SKIP')
#
# then add `cd "$srcdir/micforge"` at the start of prepare(), build(),
# check() and package(), and use "$srcdir/micforge" instead of "$startdir"
# for LICENSE and the READMEs in package().

pkgname=micforge
pkgver=0.1.0
pkgrel=1
pkgdesc="Real-time microphone processor for PipeWire (TUI) with a virtual 'MicForge Mic' source"
arch=('x86_64' 'aarch64')
url="https://github.com/Shidori5417/micforge"
license=('GPL-3.0-or-later')
depends=('pipewire' 'wireplumber')
makedepends=('rust' 'clang' 'pkgconf' 'libpipewire')
# Rust builds do not use makepkg's C LTO flags; disable them to keep the
# linker invocation identical to a plain `cargo build`.
options=('!lto')

prepare() {
  cd "$startdir"
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/^host: //p')"
}

build() {
  cd "$startdir"
  export CARGO_TARGET_DIR="$srcdir/target"
  cargo build --release --locked
}

check() {
  cd "$startdir"
  export CARGO_TARGET_DIR="$srcdir/target"
  cargo test --workspace --release --locked
}

package() {
  install -Dm755 "$srcdir/target/release/micforge" "$pkgdir/usr/bin/micforge"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "$startdir/README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 "$startdir/README.en.md" "$pkgdir/usr/share/doc/$pkgname/README.en.md"
}
