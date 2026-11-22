# ADR 0019: Rust の検査の境界 (certificate の検査と上限付きの再計算)

- 状態: 採用
- 日付: 2026-11-06
- 関連: [設計書 §1](../design.md), [ロードマップ §1.2](../roadmap.md), [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0003](0003-result-check-and-deadlines.md), [ADR 0007](0007-gkz-indicial-and-series.md), [ADR 0015](0015-gkz-local-data-guarantees.md)

## 背景

設計書 §1 は「数式処理アルゴリズムを Rust で再実装しない」と定め, Rust の役割に「検証」を含めている. ロードマップの Phase 1 と Phase 3 では, 行列, イデアル, 加群の operation の結果を Rust で検査する. どこまでが検証で, どこからが再実装かの境界を決めておかないと, 検査器が少しずつ CAS の算法を持ち込む.

既存のコードにも境界にかかわるものがある. `IntegerMatrix::rank` は QQ 上の Gauss の消去であり, `crates/algebramesh-gkz/src/cone.rs` は列の部分集合を全列挙して facet を求める. どちらも operation の答えではなく, 入力の小さな不変量を求めている.

## 決定

2026-11-06 に利用者が次を認めた.

Rust の検査は 2 つの区分に分ける. どちらにも入らない判定は CAS に任せ, 主張は `asserted` か `cross_checked` にとどめる.

**区分 (a) certificate の検査.** 次の 5 条件をすべて満たすものである.
1. 新しい数学的対象を探索で作らない.
   - 簡約の相手を選ばない.
   - 結果を左右するような対の選択や打切りをしない.
   - 完備化をしない.
   - pivot を選ぶ消去をしない.
   - 与えられた有限集合の全ての対を決まった順に調べることは, ここでいう選択に当たらない.
2. 使う演算は次のものに限る.
   - 環の加法と乗法. 対象の環は ZZ, QQ, GF(p), 多項式環, Weyl algebra である.
   - 多項式の偏微分.
   - 単項式の比較と整除の判定.
   - 行列の積と, 単位行列との比較.
   - 整数と有理数の大小比較, 整除, gcd.
   - 有理数が整数であるかの判定.
3. 費用は, 入力と certificate の大きさの多項式で抑えられ, checkpoint で止まる. 多項式で抑えられない検査 (K1 の面の列挙, K3 の関数等式の展開) には, `MAX_FACET_WORK` と同じ形で, 仕事量の上限を数で置く.
4. certificate がなければ何も判定しない.
5. 不合格は, CAS か certificate に誤りがあることを示すだけである. Rust が別の答えを出すことはない.

**区分 (b) 上限付きの再計算 (`method: recomputation`).** 次の 4 条件をすべて満たすものである.
1. spec の入力だけを使い, CAS の出力を使わない.
2. 仕事量の上限を ADR に数で書き, 実装で強制する.
3. 決定的である.
4. 整数と有理数の正確な演算を使う.

区分 (b) を「CAS の算法の再実装」と扱わない理由は次の 4 つである.
- 対象は operation の答えではない. 入力の小さな不変量 (A の facet, A の rank, 斉次性, nonresonance) に限る.
- 上限を超える入力は断る.
- operation の答えは, 常に CAS が出す.
- (b) に新しいコードを足すには, ADR の改訂を要する.

(b) のコードは TCB (信頼する計算の範囲) に入る. そのため, 主張には `method: recomputation` を記録する.

**既存のコードと, 予定している検査の分類**

| 対象 | 区分 | 対応 |
|---|---|---|
| `IntegerMatrix::rank` | (b) | 上限は `MAX_RANK_ROWS` = 128 行と `MAX_RANK_COLS` = 1024 列である (#59). 超える行列は消去を始めずに断る. math-types に残る唯一の消去とし, 新しい消去の API は足さない |
| `cone.rs` の `determinant`, `facet_normals`, `parameter_report`, `is_homogeneous` | (b) | 上限は `MAX_FACET_CANDIDATES` = 200000 (調べる列の部分集合の数) と `MAX_FACET_WORK` = 51200000 (部分集合の数と d^4 の積) である. `is_homogeneous` は rank の上限に従う. A に 1 行を足すので, GKZ の入力の A は 127 行までとし, それを超える A は大きすぎる入力として断る |
| `series.rs` の `solve` (`normalize` が使う) | (b) | math-types には移さない. V2 で (a) に移す予定だったが, (b) のまま残した ([ADR 0028](0028-gkz-package-and-its-checks.md)) |
| `series.rs` の `evaluate`, `in_lattice`, `check_exponent`, `check_series` | (a) の演算だけで済む | 例外として扱わない. `check_series` は CAS が返した toric の生成元を作用させるので, その実効水準は `gkz.toric_generates_IA` に従う (§1.3) |
| K1 の境界行列の組立て | データの構成 | 面の数に上限を置く |
| S 対の lcm representation の検査 (A3) | (a) | 下の論拠により内側とする (2026-11-06 に利用者が認めた) |
| Weyl algebra の S 対 | 未定 | 第 1 判定法が成り立たないので, A3 の検査はそのまま使わない. 別の ADR で定める |
| p が素数であることの判定 (A5) | 入力の検証 | 主張の検査ではない. ADR では両者を分けて書く |

**S 対の検査が Buchberger 法の再実装でない論拠.**
- 検査器が受け取るのは, CAS が返した G と, 各対 (g_i, g_j) について CAS が返した h_k である.
- 検査器が確かめるのは次の 2 つである.
  - S(g_i, g_j) - Σ h_k g_k = 0. 環の演算で確かめる.
  - lm(h_k g_k) < lcm(lm(g_i), lm(g_j)). 単項式の比較で確かめる.
- G の全ての対を決まった順に調べる. 先頭単項式が互いに素な対は, Buchberger の第 1 判定法で除く.
- 検査器は G に元を足さず, 簡約の相手を選ばず, 途中で打ち切らない.
- 費用は O(|G|^2) 個の多項式の等式の検査なので, certificate の大きさの多項式で抑えられる.
- したがってこれは, Buchberger の判定条件 (lcm representation による) を検査しているだけである. 出典は Cox-Little-O'Shea 第 2 章 §9 で, 定理の番号は版によって異なるので要確認である.

**規則**
- 主張の命題は, 「spec の入力と, 返した値」についての命題である. 検査器は入力 (F, f, A, b, 順序) を `JobSpec` から取る. CAS が写し返した入力は使わない.
- 次のことはしない.
  - S 多項式を G で割って, 0 になるかを見ること.
  - Rust で Groebner basis, 正規形, HNF, SNF を計算すること.
  - certificate なしに, 「f ∉ I」, 「toric の生成元が I_A を生成する」, 既約性を判定すること.
- `algebramesh-math-types` には, 割り算, 正規形, 新しい消去の API を置かない. review のたびに, この境界が守られているかを確かめる.

## 結果

- Groebner basis であることは, S 対の certificate (各対の lcm representation) を CAS が返したときだけ `checked` にできる. 返さなければ `asserted` である.
- 割り算, 正規形, Groebner basis, HNF, SNF は CAS が計算する. Rust は CAS が返した変換行列や表示の係数を, 環の演算で確かめるだけである.
- 区分 (b) のコードは, 信頼する計算の範囲に入る. 主張に `method: recomputation` を記録するので, 利用者は区別できる.

## 確認

- 区分 (a) の各検査には, 主張を偽にする変異を検出する試験を付ける (ADR 0018).
- 区分 (b) の上限は, 実装の定数と試験で強制する (`MAX_FACET_CANDIDATES`, `MAX_FACET_WORK`, `MAX_RANK_ROWS`, `MAX_RANK_COLS`). 試験は, この ADR の数と定数が一致することも確かめる.
