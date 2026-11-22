# ライセンス調査

この文書は, AlgebraMesh の配布物に含まれるソフトウェアのライセンス表示を調べた記録である (設計書 §11).
法的な助言ではない.
配布の可否を判断するときは, 各ソフトウェアのライセンス原文を優先する.

- 調査日: 2026-11-03
- 対象: v1.1.0 の `Cargo.lock` と, `deploy/docker/algebramesh.Dockerfile` から作ったイメージ
- 再調査の方法: Rust の依存は `cargo deny check licenses bans sources` (CI でも実行する), イメージは `sh tests/licenses/image-licenses.sh`

## 1. まとめ

| 配布物 | 主な中身 | ライセンス | 配布するときの条件 |
|---|---|---|---|
| `algebramesh` バイナリ, Python wheel | AlgebraMesh と Rust crate 251 個 | AlgebraMesh は MIT. 依存はすべて permissive (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, Unlicense) | AlgebraMesh と依存の著作権表示とライセンス文を同梱する |
| `agent-asir` イメージ | Risa/Asir 20260331 (OpenXM), Ubuntu 24.04 | Risa/Asir LIMITED USE LICENSE, BSD-2-Clause, LGPL (GMP, MPFR, MPC) | 非営利・非営業の目的に限る. 著作権表示を残す |
| `agent-macaulay2` イメージ | Macaulay2 1.24.11, 4ti2, normaliz ほか (Debian trixie) | GPL-2+, GPL-3+ が中心 | 対応するソースコードを入手できるようにする |
| `agent-oscar` イメージ | OSCAR 1.8.2, Julia 1.11.9, Singular, polymake, GAP ほか (Debian bookworm) | GPL-3+ (Oscar.jl), GPL-2/GPL-3/LGPL の成果物, MIT の Julia パッケージ | 対応するソースコードを入手できるようにする |
| `cli`, `python-client` イメージ | 上の `algebramesh` と Debian bookworm / Python 3.11 | Debian と Python のライセンス | 通常の Debian イメージと同じ |

最も強い制約は Risa/Asir の「非営利・非営業目的に限る」という条件である.
これは Asir backend と `agent-asir` イメージにだけ掛かり, AlgebraMesh の他の部分には及ばない.

## 2. AlgebraMesh 自身

AlgebraMesh は MIT License で公開する ([LICENSE](../LICENSE), [ADR 0009](adr/0009-mit-license.md)).
Rust の crate, Python パッケージ, driver (`drivers/`), 試験と文書のすべてが対象である.
Cargo.toml の `license` と `pyproject.toml` の `license` は `MIT` とした.
crate はいまも `publish = false` である.
以下は決めるときに使った判断の材料である.

### 2.1 Rust の依存

`Cargo.lock` の外部 crate 251 個のライセンスは次のとおりである (`cargo metadata` で集計).

| ライセンス表記 | crate 数 |
|---|---|
| `MIT OR Apache-2.0` と同等の表記 | 168 |
| `MIT` | 56 |
| `Apache-2.0` | 6 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` ほか LLVM-exception を含むもの | 6 |
| ISC, Zlib, BSD-2-Clause, BSD-3-Clause, Unlicense, Unicode-3.0 を含むもの | 15 |

copyleft のライセンスでしか使えない crate はない.
`r-efi` は `MIT OR Apache-2.0 OR LGPL-2.1-or-later` なので MIT で使える.
`ring` は `Apache-2.0 AND ISC` である.
`deny.toml` は permissive なライセンスだけを許可し, CI の `licenses` ジョブで確かめる.
copyleft の依存が増えると CI が失敗するので, 本体のライセンスの選択肢が狭まらない.

バイナリと Python wheel には依存の目的コードが入るので, 配布するときは各 crate の著作権表示とライセンス文を同梱する (MIT, BSD, Apache-2.0 の条件).
この同梱はまだ自動化していない.

### 2.2 CAS との関係

AlgebraMesh は CAS のライブラリをリンクしない.

- Asir とは OpenXM の TCP 接続で通信する.
- Macaulay2 と OSCAR は子プロセスとして起動し, ファイルと標準出力で通信する.

したがって, 本体のコードが CAS のライセンスに拘束される経路は見当たらない.
ただし `drivers/macaulay2/algebramesh.m2` と `drivers/oscar/algebramesh.jl` は, GPL の CAS の中で読み込まれ, その関数を呼ぶスクリプトである.
MIT License は GPL-2.0 とも GPL-3.0 とも両立するので, driver も MIT のままでよい.
driver と CAS を組み合わせたものを配布する場合は, その全体が CAS の GPL の条件に従う.
Risa/Asir の LIMITED USE LICENSE も, 通信するだけの本体には及ばない (§3.3).

### 2.3 検討した候補

| 候補 | 特徴 |
|---|---|
| `MIT` (採用) | 短く単純. 依存の大半と同じで, GPL とも両立する. 特許の条項はない |
| `MIT OR Apache-2.0` | Rust で最も多い組み合わせ. Apache-2.0 の側に特許の条項がある |
| `Apache-2.0` | 特許条項がある. GPL-2.0-only とは両立しない |
| `MPL-2.0` | ファイル単位で改変の公開を求める |
| `GPL-3.0-or-later` | Macaulay2, OSCAR 側と揃う. バイナリを配布する人に改変の公開を求める |
| `AGPL-3.0-or-later` | 改変した scheduler をネットワーク越しに使わせる場合にも, ソースの公開を求める |

## 3. Risa/Asir (`agent-asir`)

### 3.1 確認した事実

`agent-asir` は `ghcr.io/nakanoryunosuke/risa-asir-container` を digest で指定して使う ([ADR 0002](adr/0002-asir-runtime-image.md)).
イメージの中で次を確かめた.

- `/OpenXM/LICENSE`: BSD-2-Clause. ただし下位のディレクトリに別のライセンスや著作権表示があれば, そちらが優先すると書いてある.
- `/OpenXM_contrib2/LICENSE`: ソースに明記したものには `Copyright.asir` を適用し, それ以外は BSD-2-Clause とする.
- `/OpenXM_contrib2/asir2018/LICENSE` と `asir2018/engine/*.c` の冒頭: Fujitsu Laboratories Limited (FLL) の Risa/Asir LIMITED USE LICENSE AGREEMENT.
- `asir2018` の C のソースとヘッダ 189 個のうち 142 個の冒頭に, この LIMITED USE LICENSE がある. 表示のないもの (`engine/Z.c`, `engine/gmpq.c` など) は `OpenXM_contrib2/LICENSE` により BSD-2-Clause である. `ox_asir` のバイナリは両方を含むので, 全体として LIMITED USE LICENSE の条件に従う.
- `ox_asir` が動的にリンクするのは Boehm GC, libedit, GMP, MPFR, MPC, X11 系のライブラリ, glibc である. PARI はリンクしていない.
- ただしイメージには PARI 2.17.3 (GPL-2.0-or-later, `/OpenXM/src/pari/work/pari-2.17.3/COPYING`) の `libpari` と, それを使う OpenXM サーバ `/OpenXM/bin/ox_pari` が入っている. `ox_asir` は PARI を必要とするとき, `ox_pari` を別のプロセスとして起動して OpenXM で通信する.
- LIMITED USE LICENSE の権利者 FLL (株式会社富士通研究所) は, 2021-04-01 に富士通株式会社に吸収合併されて解散した ([富士通の発表](https://pr.fujitsu.com/jp/news/2021/01/28-1.pdf)). 権利は富士通株式会社が承継したと考えられる. ライセンスの文面は FLL のままであり, 商用の許諾の問い合わせ先は書かれていない. 条件 4 の通知先のアドレス (`flab.fujitsu.co.jp`) がいまも届くかは確かめていない.
- 神戸大学の OpenXM の配布ページ ([License](https://www.math.kobe-u.ac.jp/OpenXM/Current/doc/index-license.html)) も, OpenXM/src は BSD, GPL, LGPL のいずれかで, Risa/Asir の元のソースには FLL のライセンスが適用されると説明している.

### 3.2 LIMITED USE LICENSE の条件

1. 使用, 複製, 改変, 再配布は, 非営利かつ非営業の目的に限って許される (例として学術, 研究, 内部業務が挙がっている). 商用の許諾はない.
2. 複製には FLL の著作権表示を付ける.
3. Risa/Asir を使って得た結果を発表するときは, Risa/Asir とその著作権者を明示的に参照する.
4. 改変した場合は, 改変の仕様か改変部分のソースを FLL に電子メールで知らせる.

### 3.3 AlgebraMesh への影響

- Asir backend を使うジョブと `agent-asir` イメージの配布は, 非営利・非営業の目的に限られる. Macaulay2 と OSCAR の backend にはこの制約はない.
- AlgebraMesh は Asir を改変しない (公開イメージをそのまま使う). したがって条件 4 の通知は生じない.
- 条件 3 のため, Asir backend の結果を論文等に使う場合は Risa/Asir を引用する. ジョブ記録には `backend` と `backend_version` が残るので, どの結果が Asir によるかは後から特定できる.
- イメージには `/OpenXM/Copyright` と各ソースの著作権表示が残っており, 条件 2 は満たしている.
- GMP, MPFR, MPC は LGPL-3.0-or-later (GMP は GPL-2.0-or-later との選択) であり, 動的リンクなので差し替え可能性の条件も満たす.
- AlgebraMesh は `ox_pari` を使わないが, イメージを配布する場合は PARI についても GPL の条件 (対応するソースを入手できるようにすること) が掛かる. PARI のソースはイメージの `/OpenXM/src/pari` に入っている.
- 「内部業務」(internal business use) が非営利の例として挙がっているため, 企業の内部利用がどこまで許されるかは文面だけでは判断しにくい. 商用の利用を考える場合は権利者に確かめる.

### 3.4 Risa-Asir-container リポジトリ

`NAKANORyunosuke/Risa-Asir-container` のファイル (Dockerfile, スクリプト, compose 定義, 文書) は MIT License である (2026-11-03 に LICENSE を追加した).
README に, ビルドしたイメージに入る OpenXM, Risa/Asir, PARI はそれぞれのライセンスに従うと書いてある.
AlgebraMesh が digest で指定している公開イメージ自体は変わらない.

## 4. Macaulay2 (`agent-macaulay2`)

Debian trixie のパッケージをそのまま入れている.
主なパッケージの `/usr/share/doc/<package>/copyright` の License 欄は次のとおりである.

| パッケージ | 版 | License 欄 |
|---|---|---|
| macaulay2 | 1.24.11+ds-5+b1 | GPL-2+ が中心. ほかに GPL-3+, LGPL-3.0+, BSD, Apache-2.0, CECILL-B, Expat |
| 4ti2 | 1.6.10+ds-1.1+b2 | GPL-2+, public-domain |
| normaliz | 3.10.4+ds-1 | GPL-3+ |
| libsingular4m4n1 | 1:4.4.1+ds-2 | GPL-2+, GPL-2-or-GPL-3, GPL-3+, LGPL-2+ ほか |
| libflint19 | 3.1.3-2 | LGPL-2.1+, GPL-2+, BSD-2-clause |

全パッケージの一覧は `tests/licenses/image-licenses.sh` で出力できる (2026-11-03 時点で 144 パッケージ).

イメージを配布する場合, GPL の条件により, 含まれるバイナリに対応するソースコードを受け取れるようにする.
Debian のパッケージは版ごとにソースが Debian のアーカイブ (snapshot.debian.org) に残るので, パッケージと版の一覧を添えてそこを示すのが実際的である.
AlgebraMesh のバイナリは同じイメージに入っているだけの別のプログラムであり, GPL の対象を広げない.

## 5. OSCAR (`agent-oscar`)

`julia:1.11-bookworm` の上に OSCAR 1.8.2 を precompile して入れている.

- Oscar.jl: GNU General Public License, Version 3.0 以降 (`LICENSE.md`).
- Julia 本体: MIT.
- Julia パッケージ 113 個 (`/opt/julia-depot/packages`): MIT 103, BSD 4, GPL-3 3 (Oscar, Singular.jl, AlgebraicSolving), LGPL 2 (GAP.jl, Polymake.jl), その他 1.
- バイナリ成果物 (`/opt/julia-depot/artifacts`) には GPL-2, GPL-3, LGPL のものが多い. 例: Singular (GPL-2 or GPL-3), polymake, GAP とその package, normaliz, TOPCOM, lrslib, cddlib.

この分類はスクリプトが最初のライセンスファイルを読んで判定したもので, 誤りがありうる.
スクリプトで判定できなかったものと, 判定が誤っていたものは, 原文を読んで次のように確かめた.

| 成果物 | ライセンスファイル | 確かめた内容 |
|---|---|---|
| FLINT (`libflint.so.24`, FLINT 3) | GPL-3 の本文だけ (`COPYING`) | ヘッダ (`include/flint/flint.h` ほか) に LGPL-3.0-or-later と書いてある. FLINT 3 は LGPL-3.0-or-later である |
| GAP_pkg_anupq | `LICENSE` | Artistic License 2.0 |
| MUMPS_seq | `LICENSE` | CeCILL-C (LGPL に近い copyleft). AMD ordering の一部などは BSD-3-Clause. 論文で引用することが求められている (義務ではない) |
| Perl (`libperl.so`) | GPL-1 の本文だけ (`Copying`) | Perl は「GPL-1.0-or-later か Artistic License のどちらか」で配布されている. 成果物には Artistic License の本文が入っていない |

配布するときの条件は Macaulay2 のイメージと同じく, ソースコードを受け取れるようにすることである.
Julia の成果物は BinaryBuilder の build recipe (Yggdrasil) から再現できるので, 成果物の一覧と recipe を示す方法がある.

## 6. 現状と今後

- いまはどのイメージも公開していない (リポジトリは private で, イメージは手元と CI で作るだけ). 配布に伴う条件は, イメージを公開する時点で満たせばよい.
- Rust 依存の著作権表示とライセンス文は, cargo-about で `THIRD-PARTY-LICENSES.txt` に生成する (`about.toml`, `about.hbs`). `Cargo.lock` と合っていることを CI の `licenses` ジョブで確かめる.
- すべてのイメージの `/usr/share/doc/algebramesh/` に次を入れる.
  - `LICENSE` と `THIRD-PARTY-LICENSES.txt`.
  - agent のイメージには `NOTICE` (`deploy/notices/`). そのイメージの CAS のライセンスと, ソースの入手先を書いてある. `agent-asir` の `NOTICE` には, Risa/Asir が非営利・非営業目的に限られることを書いてある.
  - `agent-macaulay2` には `debian-packages.txt` (build 時の全パッケージと版). 各版のソースは snapshot.debian.org から入手できる.
  - `agent-oscar` には `julia-artifacts.txt` (build 時の成果物とライセンスの一覧). パッケージの版は `/opt/algebramesh-oscar/Manifest.toml` にある.
- AlgebraMesh 自身の LICENSE は wheel と sdist に入る (`bindings/python/LICENSE` はルートの LICENSE の写しで, maturin が `.dist-info/licenses` に入れる). wheel と sdist には `bindings/python/THIRD-PARTY-LICENSES.txt` も入る. これは Python の拡張モジュールに入る crate だけを対象に cargo-about で生成したもので, CI で `Cargo.lock` との一致を確かめる.
- イメージを公開するときは, GPL のソフトウェアのソースを, NOTICE に書いた入手先だけで済ませるか, イメージとともに提供するかを決める.
