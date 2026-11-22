# ADR 0009: AlgebraMesh を MIT License にする

- 状態: 採用
- 日付: 2026-11-03
- 関連: [ADR 0008](0008-license-audit.md), [licenses.md](../licenses.md) §2

## 背景

ADR 0008 の時点では, AlgebraMesh 本体のライセンスを所有者の判断として残していた.
候補と判断材料は licenses.md §2 にまとめてある.

## 決定

AlgebraMesh の全体 (Rust の crate, Python パッケージ, `drivers/` の CAS 用スクリプト, 試験, 文書) を MIT License とする.
著作権者は NAKANO Ryunosuke である.

- ルートに [LICENSE](../../LICENSE) を置く.
- `Cargo.toml` の `[workspace.package]` に `license = "MIT"` を置き, 各 crate は `license.workspace = true` で継承する.
- `bindings/python/pyproject.toml` に `license = "MIT"` を置く. `bindings/python/LICENSE` はルートの LICENSE の写しで, maturin が sdist と wheel に入れる.

## 理由

- Rust の依存 251 個の大半が MIT か `MIT OR Apache-2.0` であり, 同じ条件で済む.
- MIT は GPL-2.0 とも GPL-3.0 とも両立する. driver を GPL の CAS (Macaulay2, OSCAR) と組み合わせて配布しても衝突しない. driver だけ別のライセンスにする必要はない.
- 研究者も企業も条件を確かめずに使える. 改変の公開を求めることは目的にしていない.

## 結果

- 特許の明示的な許諾はない (Apache-2.0 との違い).
- 改変版の公開は求めない. 改変した scheduler を非公開のまま運用することもできる.
- Risa/Asir の非営利・非営業の制限は, Asir backend を使う場合と `agent-asir` イメージにだけ掛かる. 本体の MIT はこれを変えない.
- `deny.toml` は引き続き permissive なライセンスの依存だけを許す. copyleft の依存が入ると, バイナリ全体を MIT の条件だけで配布できなくなるためである.
