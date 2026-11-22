# ADR 0007: GKZ の indicial ideal と級数解

- 状態: 採用 (対数項と正規化は [ADR 0011](0011-gkz-logarithmic-and-canonical-series.md) で拡張した)
- 日付: 2026-11-03
- 関連: 設計書 §9.3 の第2段階 (`gkz.indicial`) と第3段階 (`gkz.series`)

## 背景

設計書 §9.3 は, weight を与えたときの indicial ideal と指数の候補 (第2段階), 局所級数 (第3段階) を求めている.
第3段階では「収束・共鳴・対数項を区別する」ことが条件である.

Macaulay2 1.24.11 の HolonomicSystems には `indicialIdeal`, `distraction`, `cssExpts`, `cssExptsMult`, `solveFrobeniusIdeal`, `truncatedCanonicalSeries` がある.
ただし `truncatedCanonicalSeries` には説明文書がない.
ソースには「開始項の指数が負や有理数だと失敗する」という FIXME があり, GKZ の例は試験からコメントアウトされている.
有理数の指数は GKZ では普通に現れるので, この関数には頼れない.

## 決定

### 計算の分担

| 処理 | 担当 |
|---|---|
| `in_(-w,w)(H_A(beta))` が torus-fixed かの判定, indicial ideal, その準素分解と根, holonomic rank | Macaulay2 (`isTorusFixed`, `distraction`, `primaryDecomposition`, `holonomicRank`) |
| Γ級数の係数 | driver (`algebramesh.m2`) の中で Macaulay2 が計算する |
| 指数が根であることと `A v = beta` の確認, 級数が方程式を満たすことの確認 | Rust (`algebramesh-gkz::series`) |
| cone(A) の facet, 共鳴, 斉次性 | Rust (`algebramesh-gkz::cone`) |

数式処理のアルゴリズムは Rust で再実装しない, という方針 (README) に従い, 級数の係数は CAS 側で計算する.
Rust が行うのは, 返ってきた値の代入による検算と, 入力だけから決まる組合せ的な性質の計算である.

### 級数の形

重複度 1 の指数 `v` に対して, Γ級数 (SST 第3章)

`phi_v = sum_u [v]_(u_-) / [v+u]_(u_+) x^(v+u)`

を `w . u <= order` で打ち切って返す.
和は `u in ker_ZZ A` のうち, `v + u` の負の整数成分の位置が `v` と同じになるものにわたる.
係数が 0 でない `u` は凸多面体の格子点なので, driver は Polyhedra の `latticePoints` で列挙する.
多面体が有界でない (weight がこの指数に合わない) 場合は `unsupported` を返す.

### 検算

Rust は級数を受け取ると, 正規順序の作用素 `x^a d^b` を `x^a d^b x^e = [e]_b x^(e-b+a)` で項ごとに作用させ, 次を確かめる.

1. 各項の `u` が `ker A` に入り, `w . u <= order` で, `u != 0` なら `w . u > 0` (`x^v` が先頭項).
2. Euler 作用素が級数を厳密に消す.
3. toric ideal の各生成元 `g` について, `g` を作用させた結果の各項は, 計算範囲の外 (`w . u > order`) の項が関わるものに限られる. 範囲内の項だけで決まる係数が 0 でなければ, その級数は解ではない.

toric 生成元と Euler 作用素が GKZ ideal を生成することは Macaulay2 で判定する (`gkz.generate` と同じ).
1, 2 が成り立たないのは CAS か driver の誤りなので, ジョブを失敗させる.
3 が成り立たないのは数学的に起こりうること (共鳴する parameter で, `v` の負の台が極小でない場合) なので, その指数の状態を `not_a_solution` として返す.

### 状態

| 状態 | 意味 |
|---|---|
| `verified_to_order` | 打ち切った級数が, 計算した次数まで方程式を満たす |
| `not_a_solution` | Γ級数が解にならない. 理由を `reason` に入れる |
| `logarithmic` | 指数の重複度が 2 以上. 対数項を含む解を計算する ([ADR 0011](0011-gkz-logarithmic-and-canonical-series.md)) |
| `unsupported` | 係数の台が weight で有界にならない, または項数が上限 (10000) を超える |

### 共鳴と収束

- 共鳴: cone(A) の各 facet の原始的な内向き法線 `h` について, `h(beta)` が `h(ZZ A)` に入るとき `beta` はその facet に沿って共鳴するとする (`beta in ZZ A + span(F)` と同値). facet は `d - 1` 本の列の組をすべて調べて求めるので, CAS に依存せず漏れがない.
- 収束: `(1, ..., 1)` が A の行空間に入る (A が斉次) とき, `H_A(beta)` は regular holonomic である (Hotta). regular holonomic な系の canonical series は収束する (SST 第2章). AlgebraMesh は収束を数値的にも記号的にも証明せず, `parameters.homogeneous` を報告するだけである.

## 制限

- 対数項を含む級数と canonical series への正規化は ADR 0011 で扱う.
- 指数が有理数でない場合と, indicial ideal が 0 次元でない場合 (weight が generic でない) は, ジョブを失敗させる.
- 整数の範囲 (Issue #11): weight と格子のずらしは `i64` で受け取る. weight とずらしの内積 `w . u` は任意精度の整数で正確に計算する (`i128` を超えうる). 対数の次数の和は `u64` で計算する. 演算子を掛けたときのずらし `u - b + a` や正規化での `delta + u` が `i64` を超える場合は, 値を折り返さず, 級数を `Malformed` として断る.
- facet の列挙の上限 (Issue #24): cone(A) の facet は `d - 1` 列の部分集合をすべて調べて求める. 部分集合の数 `C(n, d-1)` が 200,000 を超えるか, `C(n, d-1) * d^4` (行列式の作業量の目安) が `200,000 * 4^4` を超える A は `TooLarge` として断る. 二項係数は `u128` で正確に計算し, 上限を超えた時点で打ち切る (以前は飽和する乗算の後で割り算を続けたため, `C(100, 95) = 75,287,520` が 0 になり上限を通り抜けた). Macaulay2 の要求の解析 (この列挙を含む) はジョブの期限の下で行う.
- 第4段階 (数値計算) は実装しない. 基底, 分岐の経路, 誤差モデルの整合性を保証する方法が決まっていないため.
