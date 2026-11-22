# ADR 0029: 3 つの主張を安い検査で `checked` にする

- 状態: 採用 (値の key `certificate.membership`, `certificate.independence`, driver の key `membership_coefficients`, `independence` の名前は提案)
- 日付: 2026-11-23
- 関連: [ロードマップ V3](../roadmap.md), Issue #102, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0028](0028-gkz-package-and-its-checks.md), [主張の一覧](../claims.md)

## 背景

- 次の 3 つの主張は `asserted` だった. どれも, 小さな certificate か, 値の中の比較だけで確かめられる.
  - `factor.non_associate`: 因数分解の因子どうしが同伴でないこと.
  - `gkz.membership[j]` の「属する」側: 作用素 P_j が H_A(β) に属すること.
  - `series.log_independent`: 対数解が 1 次独立であること.
- ADR 0019 の区分 (a) に収まる. 検査器は積と比較だけを行い, 割り算, 簡約, 階数の計算はしない.

## 決定

**`factor.non_associate` (`self_certifying`).**
- 各組 i < j について f_i lc(f_j) ≠ f_j lc(f_i) を確かめる. lc は環の項順序での先頭係数である.
- f_i = u f_j (u は K の単元) なら等しく, 等しければ f_i = (lc(f_i)/lc(f_j)) f_j である. ZZ の単元は QQ の単元なので, QQ で比例しない因子は ZZ でも同伴でない.
- 値も CAS も変えない. 同伴な因子を並べた値は断る.

**`gkz.membership[j]` (`certificate`).**
- driver は, CAS が属すると答えた作用素について, toric の生成元と Euler 作用素 (値の `toric_generators`, `euler_operators` の順) の左の係数 Q_k を返す. Macaulay2 の Weyl 代数では `F // G` が G Q = F の Q を返し, Q の成分は左から掛かる (Macaulay2 1.24.11 で確かめた). G Q = F が成り立たなければ `null` を返す.
- 値は `certificate.membership[j]` に `{"toric": [...], "euler": [...]}` か `null` を持つ. 係数を返さない driver からの値は `certificate` を持たない.
- 検査器は P_j = Σ_k Q_k g_k を Weyl 代数の積で確かめる. P_j は spec から読む. g_k は `gkz.toric_in_IA` と `gkz.euler` で H_A(β) に属するので, 主張は `checked` で, 前提はこの 2 つだけである. `gkz.generated_by_toric_and_euler` にも `gkz.toric_generates_IA` にも依らない.
- 等式が成り立たない certificate と, CAS が属さないと答えた作用素の certificate は断る.
- 「属さない」側は `asserted` のまま残す. 左イデアルの Groebner basis の certificate が要り, Weyl 代数の S 対の検査はまだ定めていない (ロードマップ §1.2).

**`series.log_independent` (`certificate`).**
- 対数解を持つ指数ごとに, driver は certificate を返す. B は, 各解の shift 0 の項 x^v l^(k) の係数を並べた行列である. 行は k, 列は解である. driver は階数が上がる行を順に選び, 選んだ s 行の正方行列 B_R の逆行列を返す. s は解の数である. shift 0 の部分が 1 次従属なら `null` を返す.
- 値は, その指数の項目に `certificate.independence` (`log_powers` と `inverse`) を持つ.
- 検査器は値の項から B_R を作り, B_R `inverse` = 1 を有理数の積で確かめる. 解の間の 1 次関係は shift 0 の部分の間でも成り立つので, B_R が正則なら解は 1 次独立である. 項は, `verified_to_order` の解では `terms`, `not_a_solution` の解では `evidence_terms` から読む.
- 主張は 1 つの結果に 1 つである. 対数解を持つ指数がすべて certificate を持ち, そのすべてが確かめられたとき `checked` にする. 1 つでも持たない指数があるとき, および対数解を持つ指数がないときは, これまでどおり `asserted` である.
- 等式が成り立たない certificate は断る.

**導入の段階.** ロードマップ V3 は, 水準を記録するだけの段階を経てから断るとした. 今回は 2 つの段階を 1 つの変更で行った. 断る形で回帰試験 (Docker の 61 例) を流し, 誤って断る例がないことを確かめてから入れた. 3 つの certificate は, どれも等式の検査で, 近似や判断を含まないからである.

## 結果

- `polynomial.factor` の結果は, `factor.irreducible` を除いて `checked` になる.
- `gkz.generate` の属する側の答えは, CAS の Groebner basis にも, 4ti2 の toric の生成元が I_A を生成することにも依らずに確かめられる.
- `gkz.series` の値は, 対数解を持つ指数で `certificate` の欄が増える. T0 の比較の差は, この欄と `gkz.generate` の `certificate` だけである.
- 検査の量は, 作用素の積が係数の数だけ (Weyl 代数), 行列の積が解の数の 3 乗に比例する. どちらも中断の求めを確かめる.

## 採らなかった案

- **階数を Rust で計算する.** ADR 0019 の (b) の範囲を広げることになる. certificate なら (a) の積だけで済む.
- **`gkz.membership[j]` を別の ID に分ける.** 属する側と属さない側で ID を分けると, 結果を読む側は作用素ごとに 2 つの ID を見ることになる. 1 つの ID のまま, 前提を「certificate を持たない主張のとき」の条件付きにした.
