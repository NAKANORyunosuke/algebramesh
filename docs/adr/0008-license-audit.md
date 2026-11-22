# ADR 0008: ライセンスの監査

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §11, [licenses.md](../licenses.md)

## 背景

設計書 §11 は, OpenXM, Asir, Macaulay2, OSCAR, Singular のライセンスと再配布条件を個別に確かめ, CI とコンテナ配布に反映することを求めている.
OpenXM のリポジトリの表示を全依存物に一般化してはならない.

## 決定

- 調べた結果は [licenses.md](../licenses.md) にまとめる. 事実 (どのファイルに何が書いてあるか) と, AlgebraMesh への影響を分けて書く.
- Rust の依存は cargo-deny で検査し, CI の `licenses` ジョブで毎回確かめる. 許可するのは permissive なライセンスだけとする (`deny.toml`). AlgebraMesh 本体のライセンスを決めるまで, copyleft の依存を入れて選択肢を狭めないためである.
- イメージの中身は `tests/licenses/image-licenses.sh` で一覧にする. CAS の版を上げたときに再実行する.
- AlgebraMesh 本体と driver のライセンスは所有者が決める. 候補と判断材料は licenses.md §2 に置く. (MIT に決めた: [ADR 0009](0009-mit-license.md))

## 確認した主な事実

- Risa/Asir の中核 (`asir2018/engine` 等) は Fujitsu Laboratories の LIMITED USE LICENSE であり, 非営利・非営業の目的に限られる. OpenXM の BSD-2-Clause は, 別の表示がない部分にだけ適用される.
- Macaulay2 と OSCAR のイメージは GPL-2+ と GPL-3+ のソフトウェアを含む.
- Rust の依存 251 個は permissive なライセンスで使える.

## 残る課題

- Rust 依存の著作権表示をバイナリと wheel に同梱する仕組み (`cargo about` 等). (済: `THIRD-PARTY-LICENSES.txt` をイメージに, `bindings/python/THIRD-PARTY-LICENSES.txt` を wheel に入れる)
- イメージを公開するときの, ソースの入手先の明示とライセンス一覧の同梱. (済: 各イメージの `/usr/share/doc/algebramesh/`)
- Julia の成果物のうち自動で判定できなかったもの (GAP_pkg_anupq, MUMPS_seq, Perl) と, FLINT の成果物のライセンスファイルの確認. (済: licenses.md §5)
