# ADR 0023: 項順序

- 状態: 採用 (型, JSON の形, 要求の名前は提案)
- 日付: 2026-11-07
- 関連: [ロードマップ A3](../roadmap.md), Issue #64, [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0022](0022-operation-types-and-signatures.md), 設計書 §6

## 背景

- 項順序は `RingSpec` のメタデータにとどまり, `MonomialOrder` は `lex`, `grlex`, `grevlex` の 3 つだけだった.
- Macaulay2 と OSCAR の driver は順序を指定せずに環を作るので, CAS の既定の順序が使われていた.
- 順序に依存する operation (Groebner basis, 正規形) を足す前に, 宣言した順序を CAS に渡す必要がある (設計書 §6 の規則 2: CAS の既定値に依存しない). Rust の比較と CAS の先頭項の食い違いは, ロードマップ §8 のリスクの 1 つである.

## 決定

**順序の型と JSON.**
- `MonomialOrder` に, 重みと同点の決め方を組にした順序 `Weights { weights, then }` と, block 順序 `Block(...)` を足す. `then` と各区画の順序は, 基本の 3 つ (`lex`, `grlex`, `grevlex`) のどれかである.

  | 順序 | JSON |
  |---|---|
  | 基本の 3 つ | `"lex"`, `"grlex"`, `"grevlex"` (変えない) |
  | 重み | `{"weights": [1, 0, 2], "then": "grevlex"}` |
  | block | `{"block": [{"count": 2, "order": "grevlex"}, {"count": 1, "order": "lex"}]}` |

- 既存の JSON はバイト単位で変わらない.
- `then` は省略できない. 未知の key, 重複した key, `then` に object, 小数や文字列の重み, 配列の形の区画は断る. 同じ順序の書き方を増やさないためである.

**意味.** 変数には `RingSpec.variables` の順に x_1, ..., x_n と番号を付ける. lex と, grlex と grevlex の同点の決め方では x_1 > ... > x_n となる. 重みの順序での変数の大小は, 重みで決まる (例: `{"weights": [0, 1], "then": "lex"}` では x_2 > x_1). a, b ∈ NN^n, |a| = Σ a_i とする.
- lex: a - b の最初の 0 でない成分が正のとき a > b.
- grlex: |a| > |b|. 等しければ lex で比べる.
- grevlex: |a| > |b|. 等しければ, a - b の最後の 0 でない成分が負のとき a > b.
- 重み: w·a > w·b. 等しければ `then` で比べる.
- block: a を先頭から各区画の変数の数ずつに分け, 違いのある最初の区画で, その区画の順序によって比べる (積順序. 消去に使える).
- 重みが 0 以上なら, どれも全順序で積と両立し, 1 が最小である. よって整列順序 (大域的な順序) である.

**大域的な順序に限る.**
- 重みは 0 以上, 2^31 - 1 (`MAX_WEIGHT`) 以下に限る. 上限は, Macaulay2 1.24.11 と OSCAR 1.8.2 がそれを超える重みを断ることによる.
- 負の重みの順序は局所順序か混合順序で, 1 が最小でない. そのとき, 意味のある対象は多項式環 K[x] のイデアルではなく, 局所化 Loc_> K[x] の中のイデアルである. 正規形 (weak normal form) と所属の判定も, 局所化の中の意味になる.
- S 対の lcm representation による判定そのものは, 局所順序でも標準基底 (standard basis) を特徴付ける (Markwig, *Standard Bases in K[[t_1,...,t_m]][x_1,...,x_n]^s*, 2007, Definition 1.1, Remark 1.15, Corollary 3.6. review の指摘 #79). この ADR の初版は「局所順序では判定が成り立たない」ことを除く理由に挙げたが, それは誤りだった.
- 負の重みを断るのは, 範囲を決めたためである. A3 の operation (#66, #67, #68) は K[x] の中の Groebner basis, 正規形, 所属を扱い, certificate も K[x] の多項式で書く. 局所化, 局所順序, 混合順序は, その範囲の外である. そこで, 負の重みを環を読む段階で断る.

**検証.**
- 次のものを, 環を読む段階で断る. どれも backend の parse で `INVALID_ARGUMENTS` になり, CAS は起動しない.
  - 負の重みと, `MAX_WEIGHT` を超える重み.
  - 変数の数と合わない重みの数.
  - 区画のない block と, 変数の数が 0 の区画.
  - 区画の変数の数の和が, 環の変数の数に等しくない block.
- 変数のない環では, 基本の 3 つと空の重みだけが有効である.
- `RingSpec::validate` が順序も検証するので, 多項式を読むたびに確かめる. Weyl 代数の `ordering` は 2n 個の変数について検証する.

**比較.**
- `MonomialOrder::compare` は O(n) で, 次数と重み付きの次数は u128 で求めるので溢れない.
- 先頭項は, 多項式の環の順序で決める. 順序を引数で渡す口は置かない. 検査器は spec の環で多項式を作るので, 環の順序がそのまま宣言した順序になり, 取り違えが起きない.

**driver への渡し方.**
- 各 backend の Rust のコード (`order_items`) が, 順序を CAS の項目の列に変える. driver は項目を並べるだけである. grlex の書き換えなどを, CAS なしの単体試験で確かめられる.

  | 順序 | Macaulay2 の `MonomialOrder => {...}` | OSCAR |
  |---|---|---|
  | lex | `Lex => n` | `lex(V)` |
  | grlex | `Weights => {1, ..., 1}, Lex => n` | `deglex(V)` |
  | grevlex | `GRevLex => n` | `degrevlex(V)` |
  | 重み | `Weights => w` の後に `then` の項目 | `weight_ordering(w, then(V))` |
  | block | 区画ごとの項目を順に | 区画ごとの順序の積 |

- Macaulay2 では, 一覧の中の `GLex` が誤りになるので, grlex を重みと `Lex` で表す. また, block の後の `Weights` は, まだ区画に入っていない最初の変数から当たる.
- Macaulay2 は, 変数の一部しか覆わない区画を誤りにしない. そこで driver が, 区画が変数をちょうど覆うことを確かめる.
- 変数のない環では, driver は今までどおり 1 変数の環を作る.
- 既存の要求 (`polynomial.factor`, `gkz.*`, `matrix.*`) は, 今までどおり環を作る. 変更の前後で, 応答はバイト単位で同じだった.

**照合の試験.**
- 両方の driver に, backend が送らない自己検査の要求 `system.leading_terms` を足す. 宣言した順序の環を順序に依存する operation と同じ関数で作り, 各多項式の先頭単項式の指数を返す.
- `tests/integration/check-driver-orders.sh` (CI の `cas-e2e`) が, Rust の `compare` で求めた先頭単項式と, 2 つの CAS の先頭単項式を比べる. 事例は, 決めた seed から Rust が作る.
- Rust の側では, `compare` が全順序であること, 積と両立すること, 1 が最小であること, 独立に書いた行列による定義と一致することを property test で確かめる. 前の 3 つだけでは, grevlex の同点の決め方を逆にした変異が通ったからである.

## 結果

- 今の Macaulay2 と OSCAR では, 表せない有効な順序はない. 各 backend の `order_items` は, 表せない順序を `INVALID_ARGUMENTS` で断る関門として残す. Asir の順序は #69 で調べる.
- Macaulay2 は, 単項式の重み付きの次数か全次数が 2^31 - 1 を超えると, CAS の誤りで止まる. 誤った値は返さない. 入力を前もって調べて `INVALID_ARGUMENTS` にするかは, 順序に依存する operation を足すとき (#66) に決める.
- 同じ順序の別の書き方 (`{"block": [{"count": n, "order": "lex"}]}` と `"lex"` など) は, 構造的には等しくない. 検査器の環の比較と, W4 の cache は, 両者を別のものとして扱う. 正しい結果を断ることはありうるが, fail-closed である.
- 重みのベクトルを重ねる行列順序と, 局所順序は範囲外である.
- `MonomialOrder` は `Copy` でなくなった. workspace の中で変更の要るコードはなかった.
