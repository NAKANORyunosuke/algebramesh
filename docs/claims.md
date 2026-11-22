# 主張の契約

operation ごとに, 結果が主張すること (主張), その命題, 検査の方法と水準を定める ([ADR 0018](adr/0018-vocabulary-of-result-guarantees.md)). これを契約と呼ぶ.

- 契約は, 利用者 (数学者) が命題を確かめたと記すまで「未確認」である. 未確認の operation の要約は, 最大でも `partially_verified` である.
- 主張の ID と命題の言い方は, どれも提案である.
- 検査は, spec の入力と, 結果の値と certificate だけを使う. CAS が写し返した入力は使わない.
- 既存の欄のうち, 名前から読み違えやすいものは [検証の説明](verification.md) で説明する.

## 書き方

`crates/algebramesh-verify/src/catalogue.rs` がこの文書を読み, `crates/algebramesh-verify/tests/claims.rs` が下の規則を CI で確かめる (`cargo test --workspace`).

- operation ごとに節を置き, 見出しを operation の名前にする. 節には次のものを置く.
  - `契約の確認:` の行. 利用者が命題を確かめたら, `済` と日付を書く.
  - 主張ごとの小節. 見出しは主張の ID で, 次の 10 項目を `- 項目: 内容` の形で書く. 内容の中の backquote で囲んだ名前は, 項目に応じて主張の ID, method, 水準, 試験の関数名, 結果の欄の名前として読む.
  - `### not_claimed` の小節. 結果の意味に含まれない性質を並べる.
- 同じ主張を, それを結果に載せる operation のそれぞれの節に, その operation での 10 項目とともに置いてよい (#67. 例: `gb.reduced_form`). 1 つの節には 1 度だけ置く. depends_on の前提は, 同じ節の主張として読み, その節になければ, 最初にそれを置いた節の主張として読む. 実効水準も節ごとに決まる.
- 主張の 10 項目は次のとおりである.
  - 命題: 主張する数学的な命題.
  - 範囲: `exact` (厳密) か, `truncated` (重み w と次数 N までの打切り) か.
  - 前提: 命題が意味を持つための入力の条件.
  - 方法: ADR 0018 の method.
  - 水準: 主張自身の水準を 1 つ書く. depends_on によって実効水準が下がるときは, その後に「実効水準は `...`」と書く.
  - depends_on: 前提とする主張. 実効水準は, 自身の水準と前提の実効水準のうち最も弱いものである. 走査の試験は, 書いた実効水準がこの規則に合うことを確かめる. 前提がなければ「なし.」と書く. あれば, 主張の ID を backquote で囲み, 「, 」で区切って並べ, 文を「.」で終える. ある場合だけの前提は, 「K = ZZ のとき `factor.primitive`.」のように, 場合と「のとき」で始まる文に書く. 走査器は, この形でない depends_on を読めないものとして断る (#78).
  - 既存の欄: この主張に対応する, 互換のために残す結果の欄 (`verified_product`, `checks.*`, `status` など).
  - 水準を上げる方法.
  - 支え: 検査を支える監査の行 ([信頼性の監査](audits/2026-11-05-reliability-audit.md) §4) か, 試験.
  - 変異の試験: 下の規則に従う試験の関数名.

どの項目も 1 行に書く. 項目を次の行へ続けると (字下げしても, しなくても), 走査器はその行を読めないものとして断る. 空行の後の段落は, 項目に含めない.

verifier を持つ operation (`polynomial.factor`, 行列の operation, `ideal.groebner_basis`, `ideal.normal_form`, `ideal.membership`, `module.syzygies`, `complex.free_resolution`, `ideal.hilbert_series`, GKZ の package の `gkz.generate`, `gkz.indicial`, `gkz.series`, topology の package の `simplicial.homology`) では, 試験の中の各結果の `guarantee` が一覧に合うことも確かめる. 主張は一覧にあり, 水準は一覧の水準を超えず, method は `方法` が名指すもので, depends_on は一覧の前提に一致する (ある場合だけの前提は除いてよい).

## 変異の試験の規則

- 水準が `checked` の主張には, 変異の試験を 1 つ以上書く. 走査の試験は, 書いた名前ごとに次のことを確かめ, 成り立たなければ落ちる (#82).
  - その名前の試験関数 (`#[test]` か `#[tokio::test]` の付いた関数) が, workspace の中にちょうど 1 つある. 同じ名前の別の関数で満たされることはない.
  - その関数は `#[ignore]` で外されておらず, `cfg(test)` と package の feature (`cfg(feature = "pkg-<name>")`) 以外の `cfg` で外されていない. 関数, その上の module, file, その file を `mod` で宣言する親の file のどれについても, そうである.
- 走査の試験が保証するのは, この意味で試験が「あって, 外されていない」ことである. 実際に走ることは, CI が `cargo test --workspace --all-features` を走らせることによる. package の feature はこの実行で有効なので, それで外される試験も走る (#100). 走査の試験は, 試験の結果を読まない.
- 変異の試験は, 正しい結果を書き換えて主張を偽にする. 書き換えたものが主張を偽にすることを, 検査器のコードを使わずに試験の中で確かめる (点での値, 整数の計算, 試験の中で計算した gcd など). その上で, 結果が `UNSUPPORTED_RESULT` で断られることを確かめる. 値の項目ごとの主張 (級数の `status` など) では, その項目が主張を受けないこと (`verified_to_order` にならないこと) を確かめる.
- 書き換えは, できるだけ他の主張を真のまま保つ. そうすれば, この主張の検査だけが結果を断りうる.
- 1 か所の書換えを必ず検出できる certificate がある. 多項式の積で効く certificate (C, N, 商, h_k) と, U V = I や B B' = I がそうである. ただし, 0 に掛かる成分は除く. たとえば f_i = 0 なら, C の第 i 行を何に変えても G = F C は保たれる.
- 1 か所を書き換えても正しいままでありうる certificate もある. 例は次の 2 つである. こうした certificate では, y b の小数部分を 0 にする, 主張の値そのもの (D の対角成分, H の成分) を変える, などの変異を使う.
  - ZZ 上で解がないことの y は, y_i に整数を足しても y A ∈ ZZ^n と y b ∉ ZZ を保つ.
  - M の第 i 列が 0 なら, K の第 i 行を変えても M K = 0 を保つ.
- 方法が `recomputation` で, 値そのものを Rust が spec から計算する主張には, 書き換える CAS の出力がない. このような主張には, 再計算を既知の値と比べる試験を書く.

## GKZ の検査の場所

GKZ の 3 つの operation は, GKZ の package (`crates/algebramesh-gkz`, [package の説明](packages/gkz.md)) にある (#99, #100). backend は CAS の答えを読み, package の `value` が値を組み立てる. 値を組み立てるとき, 各級数の状態 (`verified_to_order` か `not_a_solution`) と標準形を決める. その後, package の `OperationVerifier` (`algebramesh_gkz::verify`) が, spec と値だけから下の節の主張を確かめ, 結果の `guarantee` に載せる. 主張が成り立たない値は `UNSUPPORTED_RESULT` で断る. 下の節の変異の試験の一部は, Macaulay2 の glue の試験 (`crates/algebramesh-backend-macaulay2/src/packages/gkz.rs`. feature `pkg-gkz`) で, driver の答えを書き換え, 値を組み立てて検査器に通す.

記号. 入力の A ∈ ZZ^{d×n} は行の階数が d の行列で, β ∈ QQ^d とする. D_n = QQ⟨x_1, ..., x_n, ∂_1, ..., ∂_n⟩ は Weyl 代数で, 作用素は x を左に置く正規順序で書く. E_i = Σ_j a_ij x_j ∂_j とし, I_A = ⟨∂^u - ∂^v : u, v ∈ NN^n, A u = A v⟩ ⊂ QQ[∂_1, ..., ∂_n] を toric ideal, H_A(β) = D_n I_A + Σ_i D_n (E_i - β_i) を GKZ 系とする. θ_i = x_i ∂_i とする. 重み w ∈ ZZ^n について, H_A(β) の indicial ideal は, R = QQ(x_1, ..., x_n)⟨∂_1, ..., ∂_n⟩ として R · in_{(-w,w)}(H_A(β)) ∩ QQ[θ_1, ..., θ_n] である (SST §2.3). in_{(-w,w)}(H_A(β)) が torus で固定されるとき, これは in_{(-w,w)}(H_A(β)) の distraction に等しい. driver はこの場合だけを扱い, distraction として計算する.

## polynomial.factor

契約の確認: 未.

入力は, 係数域 K (ZZ か QQ) の多項式 f である. 値は, 定数 c と, 因子 f_1, ..., f_s と重複度 m_1, ..., m_s である.

### `factor.product`

- 命題: f = c × Π f_i^{m_i} である. 各 m_i ≥ 1 で, 各 f_i は定数でない (0 も定数に数える). s = 0 なら f = c である.
- 範囲: `exact`.
- 前提: 値の環は spec の環に等しい (等しくなければ断る). f は spec から読む.
- 方法: `self_certifying`. 積を正確な演算で展開し, f と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `verified_product`. これは backend が検査の前に書く定数 true であり, 検査の結果ではない ([検証の説明](verification.md)).
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「多項式の積と冪の指数が正確か」と「多項式環の公理」. 試験 `a_right_factorization_gets_its_claims`.
- 変異の試験: `a_product_other_than_the_input_is_refused`, `a_factor_of_multiplicity_zero_is_refused`, `a_constant_factor_is_refused`.

### `factor.domain`

- 命題: c と, 各 f_i のすべての係数は K に属する.
- 範囲: `exact`.
- 前提: 値の環は spec の環に等しい. K = QQ では, 読める値について常に真である.
- 方法: `self_certifying`. 係数を読む.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_right_factorization_gets_its_claims`.
- 変異の試験: `coefficients_outside_the_domain_are_refused`, `a_factor_with_a_coefficient_outside_the_domain_is_refused`.

### `factor.primitive`

- 命題: K = ZZ のとき, 各 f_i の係数の gcd は 1 である. f_i の符号は定めない.
- 範囲: `exact`.
- 前提: K = ZZ. K = QQ では主張しない.
- 方法: `self_certifying`. 係数の gcd を計算する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `over_the_integers_the_factors_are_primitive`.
- 変異の試験: `over_the_integers_the_factors_are_primitive`.

### `factor.non_associate`

- 命題: i ≠ j について, f_i と f_j は K[x] で同伴でない. つまり, f_i = u f_j となる K の単元 u (ZZ では ±1, QQ では 0 でない有理数) はない.
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 各組 i < j について f_i lc(f_j) ≠ f_j lc(f_i) を確かめる (#102). lc は環の単項式順序での先頭係数である. f_i = u f_j なら lc(f_i) = u lc(f_j) なので両辺は等しく, 逆に等しければ f_i = (lc(f_i)/lc(f_j)) f_j である. ZZ の単元は QQ の単元なので, QQ で比例しない因子は ZZ でも同伴でない.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: なし.
- 変異の試験: `associate_factors_are_refused`.

### `factor.irreducible`

- 命題: 各 f_i は K[x] で既約である. K = ZZ では, f_i は原始的なので, Gauss の補題によって ZZ[x] での既約性は QQ[x] での既約性と同値である.
- 範囲: `exact`.
- 前提: 各 f_i は定数でない (`factor.product` が確かめる).
- 方法: backend の答え.
- 水準: `asserted`.
- depends_on: `factor.product`. K = ZZ のとき `factor.primitive`.
- 既存の欄: なし.
- 水準を上げる方法: Rust は certificate なしに既約性を判定しない ([ADR 0019](adr/0019-rust-checks-certificates-and-bounded-recomputation.md)). 系統の違う 2 つの CAS (asir と macaulay2) の答えの照合で `cross_checked` にする (V6).
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### not_claimed

- 定数 c の素因数分解. c は 1 つの有理数として返し, 分けない.
- 因子の正規形 (符号, 先頭係数, QQ 上でモニックであること). 因子は K の単元倍を除いてしか定まらない.
- 因子の並び順と, 因子の表示の文字列 (`factors[].display`).
- K の拡大体での既約性 (絶対既約性).
- f = 0 の分解の意味. f = 0 では, c = 0 と定数でない任意の因子の組が `factor.product` を満たす.

## gkz.generate

契約の確認: 未.

入力は A と β, および所属を問う作用素の列 `expected` である. 値は, Euler 作用素の列 `euler_operators`, toric の生成元の列 `toric_generators`, Macaulay2 の `gkz(A, β)` が返す生成元の列 `generators`, `expected` の各作用素の所属 `checks.expected_in_ideal` である.

### `gkz.euler`

- 命題: `euler_operators` は d 個の作用素 e_1, ..., e_d からなる. {1, ..., d} の置換 σ と 0 でない有理数 c_i があって, e_{σ(i)} = c_i (E_i - β_i) である.
- 範囲: `exact`.
- 前提: A は行の階数が d で, 大きさの上限 (ADR 0019) の中にある. β の長さは d である. driver の環の変数は x_1, ..., x_n, D_1, ..., D_n の順である (Rust が確かめる).
- 方法: `recomputation`. Rust が spec の A と β から E_i - β_i を作り, CAS の出力と, 0 でない定数倍と順序を除いて比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `checks.euler_operators_match`, `euler_operators`.
- 水準を上げる方法: 最上位である. `generators` に同じ作用素が入っていることは, この主張に含まれない (`gkz.generated_by_toric_and_euler`).
- 支え: 監査 §4 の「GKZ: Euler 作用素と toric 生成元」. 試験 `checks_cas_output_up_to_units`.
- 変異の試験: `a_changed_or_dropped_euler_operator_is_refused`.

### `gkz.toric_in_IA`

- 命題: `toric_generators` の各元は c (∂^u - ∂^v) の形である (c ∈ QQ \ {0}, u ≠ v ∈ NN^n, A u = A v). よってそれは I_A に属し, H_A(β) にも属する. 列が空でも真である.
- 範囲: `exact`.
- 前提: `gkz.euler` と同じ.
- 方法: `self_certifying`. 2 項式の形と A u = A v を, 正確な整数の演算で確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `checks.toric_binomials_valid`, `toric_generators`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「GKZ: Euler 作用素と toric 生成元」. 試験 `rejects_non_toric_binomial`.
- 変異の試験: `a_toric_binomial_with_a_u_unequal_to_a_v_is_refused`.

### `gkz.toric_generates_IA`

- 命題: `toric_generators` が QQ[∂_1, ..., ∂_n] で生成するイデアルは I_A に等しい. 包含 ⊂ は `gkz.toric_in_IA` であり, この主張は逆の包含を加える.
- 範囲: `exact`.
- 前提: `gkz.euler` と同じ.
- 方法: backend の答え (Macaulay2 の `toricIdealPartials`, つまり 4ti2 の `markov`).
- 水準: `asserted`.
- depends_on: `gkz.toric_in_IA`.
- 既存の欄: `toric_generators`. `checks.generated_by_toric_and_euler` は, この主張の根拠にならない ([検証の説明](verification.md)).
- 水準を上げる方法: 4ti2 を使わない実装 (Singular の toric.lib など) の出力と, イデアルとして等しいこと (決めた項順序での簡約 Groebner 基底が一致すること) を確かめれば, `cross_checked` になる (V6). 生成元の組は一意でないので, 列どうしは比べない. Rust は certificate なしに生成を判定しない.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `gkz.generated_by_toric_and_euler`

- 命題: Macaulay2 の `gkz(A, β)` が作る D_n の左イデアル I は, `toric_generators` と E_1 - β_1, ..., E_d - β_d が生成する左イデアルに等しい. `gkz.generate` の値は, I の生成元を `generators` に, Euler 作用素を `euler_operators` に返す. `gkz.series` の値は, どちらも返さない.
- 範囲: `exact`.
- 前提: `gkz.euler` と同じ.
- 方法: backend の答え (Macaulay2 の `==`). driver が偽と答えた結果は断る. 今の Macaulay2 では, 比べる 2 つのイデアルを同じ関数 `toricIdealPartials` の 2 度の呼出しから作るので, 答えは常に真である ([検証の説明](verification.md)). H_A(β) については何も言わない.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `checks.generated_by_toric_and_euler`, `generators`.
- 水準を上げる方法: `generators` が, `toric_generators` と `euler_operators` を合わせたものに (0 でない定数倍と順序を除いて) 等しいことを Rust が読んで確かめれば, `self_certifying` で `checked` になる. 今の Macaulay2 では, 構成からそうなっている.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `gkz.membership[j]`

- 命題: 各 j について, `checks.expected_in_ideal[j].in_ideal` が真であることと, `expected` の j 番目の作用素 P_j が `generators` の生成する D_n の左イデアルに属することは同値である. 真なら, `gkz.generated_by_toric_and_euler`, `gkz.euler`, `gkz.toric_in_IA` のもとで P_j ∈ H_A(β) である. 偽なら, さらに `gkz.toric_generates_IA` のもとで P_j ∉ H_A(β) である.
- 範囲: `exact`.
- 前提: `gkz.euler` と同じ. P_j は spec から読み, 正規順序で送る.
- 方法: `certificate`. 答えが真で, 値が `certificate.membership[j]` を持つとき (#102), それは `toric_generators` と `euler_operators` の元 g_k についての左の係数 Q_k (`toric` と `euler` の 2 つの列) である. 検査器は P_j = Σ_k Q_k g_k を Weyl 代数の積で確かめる. g_k は `gkz.toric_in_IA` と `gkz.euler` により H_A(β) に属するので, P_j ∈ H_A(β) である. 等式が成り立たない certificate と, 答えが偽の作用素の certificate は断る. 答えが偽のとき, および certificate がないとき (`null`. Macaulay2 の割り算が余りを残した場合) は, backend の答え (Macaulay2 の `isSubset`) で, 自身の水準は `asserted` である.
- 水準: `checked`. 実効水準は `asserted` (certificate を持たない主張の前提 `gkz.toric_generates_IA` による). certificate のある主張の実効水準は `checked` である.
- depends_on: `gkz.euler`, `gkz.toric_in_IA`. certificate を持たない主張のとき `gkz.generated_by_toric_and_euler`, `gkz.toric_generates_IA`.
- 既存の欄: `checks.expected_in_ideal`, `certificate.membership`.
- 水準を上げる方法: 「属さない」側は, 左イデアルの Groebner basis と簡約の certificate が要る. Weyl 代数の S 対の検査は別の ADR で定める (ロードマップ §1.2).
- 支え: なし.
- 変異の試験: `a_membership_certificate_that_does_not_give_the_operator_is_refused`, `a_membership_certificate_for_an_operator_not_in_the_ideal_is_refused`.

### not_claimed

- 3 つの列が Groebner 基底であること, 簡約されていること, 極小であること, 決まった順序や定数倍で並ぶこと.
- toric の生成元の組の極小性と一意性.
- H_A(β) の holonomic 性, holonomic rank, 解 (rank は `gkz.indicial` が返す).
- β が generic であること, 非共鳴であること.
- 両側イデアルや右イデアルへの所属.

## gkz.indicial

契約の確認: 未.

入力は, A と β と重み w ∈ ZZ^n (0 でない) である. 値は, 重み (-w, w) についての H_A(β) の indicial ideal の生成元 `indicial_ideal` (θ の多項式), その零点 `exponents` (点と重複度), holonomic rank `holonomic_rank`, β の共鳴の情報 `parameters` である. `gkz.series` の値の局所データも同じ検査を通るので, この節の主張はそちらにも当てはまる.

### `local.exponent_equations`

- 命題: 返した各指数 v ∈ QQ^n について, v の長さは n で, A v = β である.
- 範囲: `exact`.
- 前提: A は行の階数が d で, d ≤ 127, n ≤ 1024 である. β の長さは d, w の長さは n である (入力の検査).
- 方法: `self_certifying`. v を spec の A と β に代入する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `checks.exponents_are_roots`, `exponents[].point`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」. 試験 `checks_exponents`.
- 変異の試験: `an_exponent_with_a_v_unequal_to_beta_is_refused`.

### `local.exponent_roots`

- 命題: 返した各指数 v は, H_A(β) の indicial ideal の零点である. Rust は, `indicial_ideal` の各生成元が v で 0 になることを確かめる. それが indicial ideal の零点であることは, 生成元が indicial ideal を生成すること (`local.indicial_ideal`) による.
- 範囲: `exact`.
- 前提: `local.exponent_equations` と同じ.
- 方法: `self_certifying`. 生成元を v で正確に評価する.
- 水準: `checked`. 実効水準は `asserted` (`local.indicial_ideal` による).
- depends_on: `local.indicial_ideal`.
- 既存の欄: `checks.exponents_are_roots`, `exponents[].point`, `indicial_ideal`.
- 水準を上げる方法: `local.indicial_ideal` を上げる.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」.
- 変異の試験: `an_exponent_that_is_not_a_root_is_refused`.

### `local.indicial_ideal`

- 命題: `indicial_ideal` の多項式は, 重み w についての H_A(β) の indicial ideal を QQ[θ_1, ..., θ_n] で生成する. この ideal は 0 次元である.
- 範囲: `exact`.
- 前提: H_A(β) の重み (-w, w) についての initial ideal が torus で固定される. そうでなければ driver が断る.
- 方法: backend の答え (Macaulay2 の `gkz`, `inw`, `isTorusFixed`, `distraction`).
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `indicial_ideal`.
- 水準を上げる方法: 系統の違う実装の出力と, イデアルとして等しいこと (QQ[θ] の決めた項順序での簡約 Groebner 基底が一致すること) を確かめれば, `cross_checked` になる. Weyl 代数の certificate は, ADR 0019 でまだ決めていない.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `local.roots_complete`

- 命題: 返した組 (v_1, m_1), ..., (v_s, m_s) について, v_i は互いに異なり, indicial ideal の (代数閉包の上の) 零点のすべてであり, どれも有理点である. m_i は v_i での準素成分の次数 (商の QQ 上の次元) で, Σ m_i は QQ[θ] を indicial ideal で割った環の QQ 上の次元に等しい.
- 範囲: `exact`.
- 前提: indicial ideal は 0 次元で, 零点はすべて有理点である. そうでなければ driver が断る.
- 方法: backend の答え (Macaulay2 の `primaryDecomposition`, `radical`, `degree`). v_i が互いに異なることは Rust が確かめ, 同じ点が 2 度あれば断る.
- 水準: `asserted`.
- depends_on: `local.indicial_ideal`.
- 既存の欄: `exponents[].point`, `exponents[].multiplicity`.
- 水準を上げる方法: QQ[θ] の Groebner 基底の certificate (A3 の `gb.*`) で, 商の次元と各点の重複度を確かめる道がある.
- 支え: 同じ点を断る試験 `a_repeated_exponent_is_refused`.
- 変異の試験: なし (水準が `asserted` のため).

### `local.rank`

- 命題: `holonomic_rank` は, その β での H_A(β) の holonomic rank に等しい (rank が跳ぶ β でも).
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え (Macaulay2 の `holonomicRank`).
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `holonomic_rank`.
- 水準を上げる方法: 系統の違う実装 (Singular) との照合で `cross_checked` にする (V6). 半非共鳴 (semi-nonresonant) な β では rank = vol(A) である (Adolphson) ので, これを `theorem` として使える見込みがある. vol(A) は, 0 と A の列の凸包の, 格子 ZZA について正規化した体積 (ZZA = ZZ^d なら d! × Euclid 体積) である. 半非共鳴と `parameters.resonant` の関係は, 使う前に確かめる.
- 支え: 回帰試験は, Singular で計算した参照と vol(A) と比べる (監査 §4 の「共鳴, rank jump, 非斉次の例の分離」).
- 変異の試験: なし (水準が `asserted` のため).

### `local.rank_sum`

- 命題: 重複度の和 Σ m_i は, H_A(β) の holonomic rank に等しい.
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え. Rust は, 返した重複度の和と返した `holonomic_rank` を正確な整数で比べ, 等しくなければ断る (`consistency`. 水準を与えない). A が斉次でなければ, generic な w でも和が rank より小さいことがある. そのときも結果を断る.
- 水準: `asserted`.
- depends_on: `local.rank`, `local.roots_complete`.
- 既存の欄: `checks.multiplicities_sum_to_rank`, `exponents[].multiplicity`, `holonomic_rank`.
- 水準を上げる方法: `local.rank` と `local.roots_complete` を上げる.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」. 整合の検査の試験 `multiplicities_that_miss_the_rank_are_not_a_success`, `multiplicities_are_summed_without_wrapping`.
- 変異の試験: なし (水準が `asserted` のため).

### `local.parameters`

- 命題: `parameters.facets` は, cone(A) の facet の原始的な内向きの法線 h ∈ ZZ^d をすべて, 整数のベクトルとして辞書式順序で並べたものである. 各 facet について `value_at_beta` は h(β) で, `resonant` が真であることと, g = gcd_j h(a_j) として h(β) ∈ g ZZ であることは同値である. `parameters.resonant` は, ある facet が `resonant` のときに限り真である. `parameters.homogeneous` は, (1, ..., 1) が QQ 上で A の行空間に入るときに限り真である.
- 範囲: `exact`.
- 前提: 列の選び方の数 C(n, d-1) は 200,000 以下で, C(n, d-1) d^4 は 51,200,000 以下である (ADR 0019).
- 方法: `recomputation`. Rust が spec の A と β から正確な整数の演算で計算する. 再計算のコード (`algebramesh-gkz` の `cone.rs`) は信頼する範囲 (TCB) に入る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `parameters.facets`, `parameters.resonant`, `parameters.homogeneous`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「組合せの列挙の上限が正確」.
- 変異の試験: 再計算を既知の値と比べる試験 `facets_of_the_rational_normal_curve`, `facets_in_dimension_one_and_three`, `resonance_divides_by_the_index_of_the_lattice` (h(ZZ A) = g ZZ で g > 1 の場合. g で割るのを忘れる誤りを見つける). 値の parameters の欄を書き換えて断られることを確かめる試験 `parameters_other_than_those_of_a_and_beta_are_refused`.

### not_claimed

- 重み w が H_A(β) について generic であること. A が斉次 (H_A(β) が regular holonomic) なら, 重複度の和と rank の一致は w が generic であることの必要条件にすぎない. A が斉次でなければ, generic な w でも和が rank より小さいことがある (例: A = (1 2), β = 1/3, w = (1, 3) では根は 1 つで重複度 1, rank は 2). そのとき結果は断られる.
- rank と indicial ideal が独立に計算されたこと. 同じ Macaulay2 の driver が両方を計算する.
- 形式解の収束. A が斉次でなければ収束しないことがある. `parameters.homogeneous` は報告するだけである.
- 指数と重複度から解空間の基底が得られること.
- 与えた w と β 以外の重みとパラメータについての事柄.

## gkz.series

契約の確認: 未.

入力は, A, β, w と次数 N である. 各指数 v について, 値は打ち切った級数 T = Σ c_{u,k} x^{v+u} l^{(k)} を返す. ここで l^{(k)} = Π_i (log x_i)^{k_i} / k_i! である. 重複度 1 の指数には Γ 級数を 1 つ返す. 重複度が 1 を超える指数 (状態 `logarithmic`) には解を並べ, その列は対数を含まない Γ 級数 (b = 0) で始まることがある. 級数を計算しなかった指数の状態は `unsupported` である. 級数ごとの状態は, `verified_to_order` (下の主張が成り立つ) か `not_a_solution` (解でないことの証拠がある) で, `not_a_solution` の項目は項 (`terms`) を持たず, CAS が返した級数を証拠 `evidence_terms` として持つ (#99). 局所データの主張 (`local.*`) と toric の生成元の主張 (`gkz.toric_in_IA`, `gkz.toric_generates_IA`, `gkz.generated_by_toric_and_euler`) も, この値に当てはまる. 結果はそれらも載せるので, この節にも同じ項目で置く (#99).

### `gkz.toric_in_IA`

- 命題: `toric_generators` の各元は c (∂^u - ∂^v) の形である (c ∈ QQ \ {0}, u ≠ v ∈ NN^n, A u = A v). よってそれは I_A に属し, H_A(β) にも属する. 列が空でも真である.
- 範囲: `exact`.
- 前提: A は行の階数が d で, 大きさの上限 (ADR 0019) の中にある. β の長さは d である. driver の環の変数は x_1, ..., x_n, D_1, ..., D_n の順である (Rust が確かめる).
- 方法: `self_certifying`. 2 項式の形と A u = A v を, 正確な整数の演算で確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `checks.toric_binomials_valid`, `toric_generators`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「GKZ: Euler 作用素と toric 生成元」. 試験 `rejects_non_toric_binomial`.
- 変異の試験: `a_toric_binomial_with_a_u_unequal_to_a_v_is_refused`.

### `gkz.toric_generates_IA`

- 命題: `toric_generators` が QQ[∂_1, ..., ∂_n] で生成するイデアルは I_A に等しい. 包含 ⊂ は `gkz.toric_in_IA` であり, この主張は逆の包含を加える.
- 範囲: `exact`.
- 前提: A は行の階数が d で, 大きさの上限 (ADR 0019) の中にある. β の長さは d である. driver の環の変数は x_1, ..., x_n, D_1, ..., D_n の順である (Rust が確かめる).
- 方法: backend の答え (Macaulay2 の `toricIdealPartials`, つまり 4ti2 の `markov`).
- 水準: `asserted`.
- depends_on: `gkz.toric_in_IA`.
- 既存の欄: `toric_generators`. `checks.generated_by_toric_and_euler` は, この主張の根拠にならない ([検証の説明](verification.md)).
- 水準を上げる方法: 4ti2 を使わない実装 (Singular の toric.lib など) の出力と, イデアルとして等しいこと (決めた項順序での簡約 Groebner 基底が一致すること) を確かめれば, `cross_checked` になる (V6). 生成元の組は一意でないので, 列どうしは比べない. Rust は certificate なしに生成を判定しない.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `gkz.generated_by_toric_and_euler`

- 命題: Macaulay2 の `gkz(A, β)` が作る D_n の左イデアル I は, `toric_generators` と E_1 - β_1, ..., E_d - β_d が生成する左イデアルに等しい. `gkz.generate` の値は, I の生成元を `generators` に, Euler 作用素を `euler_operators` に返す. `gkz.series` の値は, どちらも返さない.
- 範囲: `exact`.
- 前提: A は行の階数が d で, 大きさの上限 (ADR 0019) の中にある. β の長さは d である. driver の環の変数は x_1, ..., x_n, D_1, ..., D_n の順である (Rust が確かめる).
- 方法: backend の答え (Macaulay2 の `==`). driver が偽と答えた結果は断る. 今の Macaulay2 では, 比べる 2 つのイデアルを同じ関数 `toricIdealPartials` の 2 度の呼出しから作るので, 答えは常に真である ([検証の説明](verification.md)). H_A(β) については何も言わない.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `checks.generated_by_toric_and_euler`, `generators`.
- 水準を上げる方法: `generators` が, `toric_generators` と `euler_operators` を合わせたものに (0 でない定数倍と順序を除いて) 等しいことを Rust が読んで確かめれば, `self_certifying` で `checked` になる. 今の Macaulay2 では, 構成からそうなっている.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `local.exponent_equations`

- 命題: 返した各指数 v ∈ QQ^n について, v の長さは n で, A v = β である.
- 範囲: `exact`.
- 前提: A は行の階数が d で, d ≤ 127, n ≤ 1024 である. β の長さは d, w の長さは n である (入力の検査).
- 方法: `self_certifying`. v を spec の A と β に代入する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `checks.exponents_are_roots`, `exponents[].point`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」. 試験 `checks_exponents`.
- 変異の試験: `an_exponent_with_a_v_unequal_to_beta_is_refused`.

### `local.exponent_roots`

- 命題: 返した各指数 v は, H_A(β) の indicial ideal の零点である. Rust は, `indicial_ideal` の各生成元が v で 0 になることを確かめる. それが indicial ideal の零点であることは, 生成元が indicial ideal を生成すること (`local.indicial_ideal`) による.
- 範囲: `exact`.
- 前提: `local.exponent_equations` と同じ.
- 方法: `self_certifying`. 生成元を v で正確に評価する.
- 水準: `checked`. 実効水準は `asserted` (`local.indicial_ideal` による).
- depends_on: `local.indicial_ideal`.
- 既存の欄: `checks.exponents_are_roots`, `exponents[].point`, `indicial_ideal`.
- 水準を上げる方法: `local.indicial_ideal` を上げる.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」.
- 変異の試験: `an_exponent_that_is_not_a_root_is_refused`.

### `local.indicial_ideal`

- 命題: `indicial_ideal` の多項式は, 重み w についての H_A(β) の indicial ideal を QQ[θ_1, ..., θ_n] で生成する. この ideal は 0 次元である.
- 範囲: `exact`.
- 前提: H_A(β) の重み (-w, w) についての initial ideal が torus で固定される. そうでなければ driver が断る.
- 方法: backend の答え (Macaulay2 の `gkz`, `inw`, `isTorusFixed`, `distraction`).
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `indicial_ideal`.
- 水準を上げる方法: 系統の違う実装の出力と, イデアルとして等しいこと (QQ[θ] の決めた項順序での簡約 Groebner 基底が一致すること) を確かめれば, `cross_checked` になる. Weyl 代数の certificate は, ADR 0019 でまだ決めていない.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `local.roots_complete`

- 命題: 返した組 (v_1, m_1), ..., (v_s, m_s) について, v_i は互いに異なり, indicial ideal の (代数閉包の上の) 零点のすべてであり, どれも有理点である. m_i は v_i での準素成分の次数 (商の QQ 上の次元) で, Σ m_i は QQ[θ] を indicial ideal で割った環の QQ 上の次元に等しい.
- 範囲: `exact`.
- 前提: indicial ideal は 0 次元で, 零点はすべて有理点である. そうでなければ driver が断る.
- 方法: backend の答え (Macaulay2 の `primaryDecomposition`, `radical`, `degree`). v_i が互いに異なることは Rust が確かめ, 同じ点が 2 度あれば断る.
- 水準: `asserted`.
- depends_on: `local.indicial_ideal`.
- 既存の欄: `exponents[].point`, `exponents[].multiplicity`.
- 水準を上げる方法: QQ[θ] の Groebner 基底の certificate (A3 の `gb.*`) で, 商の次元と各点の重複度を確かめる道がある.
- 支え: 同じ点を断る試験 `a_repeated_exponent_is_refused`.
- 変異の試験: なし (水準が `asserted` のため).

### `local.rank`

- 命題: `holonomic_rank` は, その β での H_A(β) の holonomic rank に等しい (rank が跳ぶ β でも).
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え (Macaulay2 の `holonomicRank`).
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `holonomic_rank`.
- 水準を上げる方法: 系統の違う実装 (Singular) との照合で `cross_checked` にする (V6). 半非共鳴 (semi-nonresonant) な β では rank = vol(A) である (Adolphson) ので, これを `theorem` として使える見込みがある. vol(A) は, 0 と A の列の凸包の, 格子 ZZA について正規化した体積 (ZZA = ZZ^d なら d! × Euclid 体積) である. 半非共鳴と `parameters.resonant` の関係は, 使う前に確かめる.
- 支え: 回帰試験は, Singular で計算した参照と vol(A) と比べる (監査 §4 の「共鳴, rank jump, 非斉次の例の分離」).
- 変異の試験: なし (水準が `asserted` のため).

### `local.rank_sum`

- 命題: 重複度の和 Σ m_i は, H_A(β) の holonomic rank に等しい.
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え. Rust は, 返した重複度の和と返した `holonomic_rank` を正確な整数で比べ, 等しくなければ断る (`consistency`. 水準を与えない). A が斉次でなければ, generic な w でも和が rank より小さいことがある. そのときも結果を断る.
- 水準: `asserted`.
- depends_on: `local.rank`, `local.roots_complete`.
- 既存の欄: `checks.multiplicities_sum_to_rank`, `exponents[].multiplicity`, `holonomic_rank`.
- 水準を上げる方法: `local.rank` と `local.roots_complete` を上げる.
- 支え: 監査 §4 の「indicial の根, 重複度の和と rank」. 整合の検査の試験 `multiplicities_that_miss_the_rank_are_not_a_success`, `multiplicities_are_summed_without_wrapping`.
- 変異の試験: なし (水準が `asserted` のため).

### `local.parameters`

- 命題: `parameters.facets` は, cone(A) の facet の原始的な内向きの法線 h ∈ ZZ^d をすべて, 整数のベクトルとして辞書式順序で並べたものである. 各 facet について `value_at_beta` は h(β) で, `resonant` が真であることと, g = gcd_j h(a_j) として h(β) ∈ g ZZ であることは同値である. `parameters.resonant` は, ある facet が `resonant` のときに限り真である. `parameters.homogeneous` は, (1, ..., 1) が QQ 上で A の行空間に入るときに限り真である.
- 範囲: `exact`.
- 前提: 列の選び方の数 C(n, d-1) は 200,000 以下で, C(n, d-1) d^4 は 51,200,000 以下である (ADR 0019).
- 方法: `recomputation`. Rust が spec の A と β から正確な整数の演算で計算する. 再計算のコード (`algebramesh-gkz` の `cone.rs`) は信頼する範囲 (TCB) に入る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `parameters.facets`, `parameters.resonant`, `parameters.homogeneous`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「組合せの列挙の上限が正確」.
- 変異の試験: 再計算を既知の値と比べる試験 `facets_of_the_rational_normal_curve`, `facets_in_dimension_one_and_three`, `resonance_divides_by_the_index_of_the_lattice` (h(ZZ A) = g ZZ で g > 1 の場合. g で割るのを忘れる誤りを見つける). 値の parameters の欄を書き換えて断られることを確かめる試験 `parameters_other_than_those_of_a_and_beta_are_refused`.

### `series.shape`

- 命題: 状態が `verified_to_order` か `not_a_solution` の各級数 T について, b を述べた対数の次数とする (重複度 1 の Γ 級数では b = 0). 次の 6 つが成り立つ. (i) 各 u は ZZ^n に属し, A u = 0. (ii) k は空か長さ n で, |k| = Σ_i k_i ≤ b. (iii) 組 (u, k) は互いに異なり, c_{u,k} ≠ 0. (iv) 各項について w·u ≤ N. (v) u ≠ 0 なら w·u > 0 で, x^v は先頭の単項式である. (vi) b = 0 なら (u, k) = (0, 0) の係数は 1 で, b > 0 なら u = 0 かつ |k| = b の項がある.
- 範囲: `exact`. (iv) は打切り (重み w, 次数 N) を表す.
- 前提: v と b は CAS が返したもので, v は `local.exponent_equations` を満たす. A, w, N は spec から読む.
- 方法: `self_certifying`. 返した項を読み, 重みを正確な整数で計算する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `series[].status`, `series[].terms`, `series[].solutions[].status`, `series[].solutions[].log_degree`, `series[].solutions[].terms`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「級数の係数と対数解」. 試験 `rejects_malformed_series`, `rejects_inconsistent_series_replies`, `a_term_of_huge_weight_is_refused_not_wrapped`, `log_degrees_are_summed_without_wrapping` が (iv) 以外の条件も断ることを示す. 変異が偽にすることを試験の中で確かめるのは, (iv) の試験だけである.
- 変異の試験: `a_term_above_the_order_is_refused`.

### `series.euler_exact`

- 命題: `series.shape` を満たす各級数 T と各 i について, (E_i - β_i) T = 0 が厳密に成り立つ. E_i - β_i は Rust が spec の A と β から作る.
- 範囲: `exact`.
- 前提: `series.shape` と同じ.
- 方法: `self_certifying`. T を代入する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `series[].status`, `series[].solutions[].status`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「級数の係数と対数解」. 試験 `interprets_and_checks_logarithmic_solutions`, `accepts_the_logarithmic_solution`.
- 変異の試験: `a_logarithm_the_euler_operators_do_not_annihilate_is_refused`.

### `series.toric_to_order`

- 命題: 状態が `verified_to_order` の級数 T は, 重み N まで H_A(β) の生成元を満たす. つまり, CAS が返した `toric_generators` の各元 g について, g T の係数のうち, 計算した項 (重み N 以下の項) だけで決まるものはすべて 0 である. そのような係数は, 重み N 以下の項が T に一致する任意の形式級数 φ について, g φ の対応する係数に等しい.
- 範囲: `truncated` (重み w, 次数 N).
- 前提: `series.shape` と `series.euler_exact` を満たす. `toric_generators` と Euler 作用素が H_A(β) を左イデアルとして生成する (`gkz.toric_generates_IA`).
- 方法: `self_certifying`. g T の係数を正確に計算する.
- 水準: `checked`. 実効水準は `asserted` (`gkz.toric_generates_IA` による).
- depends_on: `series.shape`, `series.euler_exact`, `gkz.toric_generates_IA`.
- 既存の欄: `series[].status`, `series[].solutions[].status`, `toric_generators`.
- 水準を上げる方法: `gkz.toric_generates_IA` を上げる.
- 支え: 監査 §4 の「級数の係数と対数解」と「`verified_to_order` を収束や完全性の証明と扱わない」. 試験 `rejects_a_wrong_coefficient_and_missing_terms`.
- 変異の試験: `a_series_the_toric_operator_does_not_annihilate_is_not_verified`, `a_verified_series_the_toric_operator_does_not_annihilate_is_refused`.

### `series.not_a_solution`

- 命題: 状態が `not_a_solution` の項目について, CAS が返した級数 T (値の `evidence_terms`) は `series.shape` と `series.euler_exact` を満たす. さらに, ある g ∈ `toric_generators` について, g T の係数のうち計算した項だけで決まるものに 0 でないものがある. g ∈ I_A ⊂ H_A(β) なので, 重み N 以下の項が T に一致する形式級数で H_A(β) に消されるものはない. つまり T は, どの解の重み N までの打切りでもない.
- 範囲: `exact`. 重み N までの項から, 重み N 以下の項が T に一致するすべての形式級数について述べる.
- 前提: g ∈ I_A (`gkz.toric_in_IA`). `toric_generators` が I_A を生成するかどうかは要らない.
- 方法: `self_certifying`. 検査器が `evidence_terms` を読み, 0 でない係数を見つける.
- 水準: `checked`.
- depends_on: `gkz.toric_in_IA`.
- 既存の欄: `series[].status`, `series[].reason`, `series[].evidence_terms`, `series[].solutions[].status`, `series[].solutions[].reason`, `series[].solutions[].evidence_terms`.
- 水準を上げる方法: 最上位である.
- 支え: 監査 §4 の「級数の係数と対数解」. 試験 `integer_parameters_give_a_polynomial_solution_and_a_non_solution`.
- 変異の試験: `a_series_that_solves_the_system_is_not_labelled_not_a_solution`, `a_solution_given_as_the_evidence_of_not_a_solution_is_refused`.

### `series.canonical`

- 命題: 状態が `verified_to_order` の級数 T (指数 v) について, `canonical` が真なら, C を `canonical_terms` (`normalization` が空なら T) として次が成り立つ. (i) C は, T から `normalization` の各組 (j, b, f) について f T_{j,b} を引き, 重みを v から測って N 以下の項を残したものである. つまり, 項 x^a l^{(k)} は w·(a - v) ≤ N のとき残す. T_{j,b} は, `exponents` の j 番目の指数 v_j の b 番目の解である (j と b は 0 から数える). T が x^v を因子に含むのと同じく, T_{j,b} は x^{v_j} を因子に含む. そのため, 引くときに指数を v_j - v だけずらすことはしない. v_j の重複度が 1 なら, その解は `series[j].terms` の Γ 級数で, b = 0 とする. 重複度が 1 を超えるなら, `series[j].solutions` の b 番目の解である. (ii) v_j - v ∈ ker_ZZ A で 0 < w·(v_j - v) ≤ N となる他の指数 v_j について, C は x^{v_j} l^{(k)} の項を持たない. (iii) `normalization` が空でなければ, C は同じ生成元について `series.shape`, `series.euler_exact`, `series.toric_to_order` の検査を通る. `canonical` が偽なら, `canonical_reason` にその理由を記す.
- 範囲: `truncated` (重み w, 次数 N).
- 前提: 考える指数は, 結果に並べたものである. 並んでいない指数は引かない.
- 方法: `recomputation`. 正規化と, その連立 1 次方程式の解は Rust のコードで, 信頼する範囲 (TCB) に入る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `series[].canonical`, `series[].normalization`, `series[].canonical_terms`, `series[].canonical_reason`, `series[].solutions[].canonical`, `series[].solutions[].normalization`, `series[].solutions[].canonical_terms`, `series[].solutions[].canonical_reason`.
- 水準を上げる方法: 最上位である. (ii) を, 正規化のコードを使わずに値から確かめれば, その部分は `self_certifying` になり, 正規化のコードを TCB から外せる.
- 支え: 試験 `normalizes_against_other_exponents`.
- 変異の試験: 再計算を既知の値と比べる試験 `normalizes_against_other_exponents`. 値の標準形を書き換えて断られることを確かめる試験 `a_canonical_form_the_normalization_does_not_give_is_refused`.

### `series.log_independent`

- 命題: 重複度 m > 1 の指数 v (状態 `logarithmic`) について, 状態が `verified_to_order` の解は形式級数として 1 次独立である.
- 範囲: `exact`.
- 前提: 並べた解は CAS が返したものである.
- 方法: `certificate` (#102). 状態 `logarithmic` の各指数が `certificate.independence` を持つとき, 検査器はそれを確かめる. B を, 各解の shift 0 の項 x^v l^(k) の係数を並べた行列とする. 行は k, 列は解で, 列の順は `solutions` の順である. 項は, `verified_to_order` の解では `terms`, `not_a_solution` の解では `evidence_terms` から読む. certificate の `log_powers` は B の s 個の行 (s は解の数) を選び, `inverse` は選んだ行の s 次の正方行列 B_R の逆行列である. 検査器は B_R `inverse` = 1 を有理数の積で確かめる. このとき B の階数は s で, 解の shift 0 の部分は 1 次独立である. 解の間の 1 次関係は shift 0 の部分の間でも成り立つので, 解 (特に `verified_to_order` の解) も 1 次独立である. 等式が成り立たない certificate は断る. certificate を持たない指数がひとつでもあるとき, および `logarithmic` の指数がないときは, backend の答え (Macaulay2 が, 意図した形 (d · log x)^b の係数行列の階数を確かめる) で, 水準は `asserted` である. Macaulay2 は, shift 0 の部分が 1 次従属なら certificate を出さない.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `series[].solutions`, `series[].certificate.independence`.
- 水準を上げる方法: 最上位である.
- 支え: なし.
- 変異の試験: `an_independence_certificate_that_is_not_an_inverse_is_refused`.

### `series.missing`

- 命題: 重複度 m > 1 の指数 (状態 `logarithmic`) について, 並べた解の数と `missing` の和は m に等しい (Rust が確かめ, 等しくなければ断る). `missing` と `reason` は, 計算しなかった解についての driver の説明である. 状態 `unsupported` とその `reason` は, その指数の級数を計算しなかったという driver の説明である.
- 範囲: `exact`.
- 前提: m は CAS の答えである (`local.roots_complete`).
- 方法: backend の答え. 数の和は Rust が整合を確かめる.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: `series[].missing`, `series[].reason`, `series[].status`.
- 水準を上げる方法: `verified_to_order` の解の数を報告して m と比べれば, 意味のある主張になる. その水準は `local.roots_complete` と `series.log_independent` に従う.
- 支え: 試験 `interprets_and_checks_logarithmic_solutions`.
- 変異の試験: なし (水準が `asserted` のため).

### not_claimed

- 返した級数の収束. `parameters.homogeneous` を報告するだけである.
- 解が H_A(β) の解空間の基底をなすこと.
- 重みが generic であること.
- `verified_to_order` の級数が, 重み N を超えて解に延びること. `verified_to_order` は次数までの形式的な検査である.
- 返した項が, Γ 級数 φ_v そのもの (または摂動の係数 φ_b) の打切りであること. `direction` と `log` は driver の答えを写す.
- `not_a_solution` が, Γ 級数 φ_v が解でない (共鳴) ことを意味すること. CAS の係数の誤りでも `not_a_solution` になる.
- 重み N までの検査が H_A(β) のすべての元について成り立つこと. 検査は, CAS の生成元と Euler 作用素についてだけ行う.
- 指数 v の解の空間の次元が, その重複度に等しいこと.
- `canonical` が偽であることが, 標準形がないことを意味すること.
- `canonical` が真の級数が, SST §2.5 の意味での標準級数の打切りであること. それには, 並べた指数がすべてであること (`local.roots_complete`) と, 級数が解の打切りであること (`series.toric_to_order`) が要る.

## matrix.rank

契約の確認: 未.

記号. 入力の行列を A (m 行 n 列), 右辺を b とする. 係数域は ZZ か QQ である. 「単模」は, 整数の正方行列で整数の逆行列を持つことをいう. 行列の operation の certificate は, job が `certificate: "none"` を指定したときと, certificate のせいで値が結果の上限を超えたとき (B1 より前) に省く (下の「certificate を省いたとき」). そのとき, certificate を使う主張は `asserted` になる.

値は rank r である. certificate は, A の r 次の小行列 B (行と列の位置) とその逆行列 B', および n 行 n - r 列の行列 K である.

### `rank.lower`

- 命題: rank A ≥ r.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. B B' = I を確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_rank_is_bounded_from_both_sides`.
- 変異の試験: `a_rank_too_high_is_refused`.

### `rank.upper`

- 命題: rank A ≤ r.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. A K = 0 で, K が列階段形 (各列の最初の 0 でない成分の位置が, 左から右へ真に下がる) であることを確かめる. K の列は独立なので nullity A ≥ n - r である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_rank_is_bounded_from_both_sides`.
- 変異の試験: `a_rank_too_low_is_refused`, `dependent_kernel_vectors_are_refused`, `a_zero_kernel_vector_is_refused`.

### not_claimed

- certificate の B と K の選び方. どちらも一意でない.

## matrix.kernel

契約の確認: 未.

値は n 行 k 列の行列 K で, その列が基底である.

### `kernel.contained`

- 命題: A K = 0.
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 積を確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_kernel_over_qq_is_a_basis`, `a_kernel_over_zz_is_a_basis_of_the_lattice`.
- 変異の試験: `a_vector_outside_the_kernel_is_refused`, `a_kernel_over_qq_is_a_basis`.

### `kernel.independent`

- 命題: K の列は 1 次独立である.
- 範囲: `exact`.
- 前提: なし.
- 方法: QQ では `self_certifying`. 値の K が列階段形であることを読む. certificate を使わないので, certificate を省いても `checked` である. ZZ では `certificate`. 下の U から確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_kernel_over_qq_is_a_basis`, `a_kernel_over_zz_is_a_basis_of_the_lattice`.
- 変異の試験: `dependent_kernel_vectors_are_refused`, `a_zero_kernel_vector_is_refused`.

### `kernel.spanning`

- 命題: QQ では, K の列は {x ∈ QQ^n : A x = 0} を張る. ZZ では, K の列は格子 {x ∈ ZZ^n : A x = 0} を ZZ 上で張る.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. QQ では, A の n - k 次の小行列とその逆行列から rank A ≥ n - k を確かめ, kernel の次元が k 以下であることを導く. K の列は kernel に入り (`kernel.contained`), 独立なので (`kernel.independent`), kernel を張る. ZZ では, 単模な U (U V = I) について, H = U A^T が行階段形であり, K の列が H の 0 の行に対応する U の行であることを確かめる.
- 水準: `checked`.
- depends_on: QQ のとき `kernel.contained`, `kernel.independent`.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_kernel_over_qq_is_a_basis`, `a_kernel_over_zz_is_a_basis_of_the_lattice`.
- 変異の試験: `a_basis_that_spans_too_little_is_refused`, `a_kernel_over_zz_is_a_basis_of_the_lattice`, `a_transform_whose_inverse_is_not_integral_is_refused`.

ZZ の場合の論拠. y A^T = 0 は, z = y V について z H = 0 と同値である. H の 0 でない行は独立なので, z は 0 の行の位置にしか成分を持たない. y = z U で, y が整数であることと z が整数であることは同値である (U と V が整数). よって kernel の格子は, 0 の行に対応する U の行で ZZ 上張られ, それらは独立である.

### not_claimed

- K の正規形 (基底の選び方). K は基底の 1 つにすぎない.

## matrix.solve

契約の確認: 未.

値は解 x か, 解がないことである. 解がないときの certificate は y である. 解のあるときは, 解そのものが certificate である.

### `solve.solution`

- 命題: A x = b. ZZ では x は整数である.
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 積を確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_solution_is_checked`.
- 変異の試験: `a_solution_is_checked`.

### `solve.no_solution`

- 命題: A x = b を満たす x (ZZ では整数の x) はない.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. QQ では y A = 0 かつ y b ≠ 0 を確かめる. ZZ では, y ∈ QQ^m について y A ∈ ZZ^n かつ y b ∉ ZZ を確かめる. 整数解 x があれば y b = (y A) x ∈ ZZ となって矛盾する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `no_solution_over_zz_is_shown_by_y`, `no_solution_over_qq_is_shown_by_y`.
- 変異の試験: `no_solution_over_qq_is_shown_by_y`, `no_solution_over_zz_is_shown_by_y`.

### not_claimed

- 解の一意性と, 解が複数あるときにどの解を返すか.

## matrix.hermite_normal_form

契約の確認: 未. 係数域は ZZ だけである.

値は m 行 n 列の行列 H である. certificate は, 単模な U とその逆行列 V である.

### `hnf.form`

- 命題: H は行の Hermite 標準形である. 行階段形で, pivot は正, pivot の上の成分は 0 以上 pivot 未満で, 0 の行は下にある.
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 値を読む.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_hermite_form_is_checked`.
- 変異の試験: `a_wrong_hermite_form_is_refused`.

### `hnf.equivalent`

- 命題: H = U A で, U は単模である. よって H と A の行は, ZZ 上同じ格子を張る. `hnf.form` と合わせると, H はその格子の Hermite 標準形であり, これは一意に定まる.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. U V = I と U A = H を確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_hermite_form_is_checked`.
- 変異の試験: `a_wrong_hermite_form_is_refused`, `a_transform_whose_inverse_is_not_integral_is_refused`.

### not_claimed

- U の一意性. A の階数が m より小さいとき, U は一意でない.

## matrix.smith_normal_form

契約の確認: 未. 係数域は ZZ だけである.

値は対角成分 d_1, ..., d_min(m,n) である. certificate は, 単模な P (m 次), Q (n 次) とそれぞれの逆行列である.

### `snf.form`

- 命題: d_i ≥ 0 で, d_1 | d_2 | ... である (0 は 0 だけを割る).
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 値を読む.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_smith_form_of_2_4_6_8_is_diag_2_4`.
- 変異の試験: `a_smith_diagonal_that_does_not_divide_is_refused`.

### `snf.equivalent`

- 命題: diag(d) = P A Q で, P と Q は単模である. `snf.form` と合わせると, diag(d) は A の Smith 標準形であり, これは一意に定まる.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. P P' = I, Q Q' = I, P A Q = diag(d) を確かめる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_smith_form_of_2_4_6_8_is_diag_2_4`.
- 変異の試験: `a_smith_form_that_is_not_equivalent_is_refused`, `a_smith_form_of_another_matrix_is_refused`, `a_transform_whose_inverse_is_not_integral_is_refused`.

### not_claimed

- P と Q の一意性.

## ideal.groebner_basis

契約の確認: 未. 係数域は QQ だけである.

記号. 入力は, 変数が 1 つ以上の環 QQ[x_1, ..., x_n] の多項式の列 F = (f_1, ..., f_s) で, I = (f_1, ..., f_s) とする. 0 の生成元も位置を保って数える. 項順序 > は spec の環の順序 (#64 のどれか) である. 検査器は F と順序を spec から取り, CAS が写し返した入力は使わない. lm(g) と lc(g) は, > での g の先頭単項式と先頭係数である. ZZ の環と変数のない環は, CAS を起動する前に `INVALID_ARGUMENTS` で断る. 引数 `certificate` は `representation` (既定), `s_pairs`, `none` のどれかである.

値は多項式の列 G = (g_1, ..., g_t) である. I が 0 なら G は空で, I が全体なら G = (1) である. certificate は, s 行 t 列の多項式行列 C (`basis_in_generators`) と, t 行 s 列の多項式行列 N (`generators_in_basis`) である. job が `certificate: "s_pairs"` を求めたときは, certificate はさらに S 対の lcm representation (`s_pairs`, 下の `gb.groebner` の後) を持つ. `representation` を求めた値が `s_pairs` を持てば断り, `s_pairs` を求めた値が持たなくても断る. job が `certificate: "none"` を指定したときと, certificate のせいで値が結果の上限を超えたとき (#36 より前) は certificate を省き, certificate を使う主張を `asserted` にする (下の「certificate を省いたとき」).

### `gb.reduced_form`

- 命題: G のどの元も 0 でなく, 各 j で lc(g_j) = 1 である. lm(g_1) < lm(g_2) < ... < lm(g_t) である. i ≠ j について, g_i のどの項も lm(g_j) で割り切れない.
- 範囲: `exact`.
- 前提: 値の多項式は spec の環で読む (変数の数が合わない項は断る).
- 方法: `self_certifying`. 値の各元の先頭項を spec の順序で求め, 係数, 単項式の比較, 単項式の整除を調べる. certificate を使わないので, certificate を省いても `checked` である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_twisted_cubic_basis_is_checked`, `the_zero_ideal_and_the_unit_ideal_are_checked`.
- 変異の試験: `a_zero_basis_element_is_refused`, `a_non_monic_basis_element_is_refused`, `basis_elements_out_of_order_are_refused`, `a_basis_not_in_reduced_form_is_refused`, `an_order_mix_up_that_is_not_monic_is_refused`.

### `gb.contained`

- 命題: (G) ⊆ I. 各 j について g_j = Σ_i f_i C_ij である.
- 範囲: `exact`.
- 前提: C は s 行 t 列である (違えば断る).
- 方法: `certificate`. F を 1 行の行列として C と掛け, 各列を g_j と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_twisted_cubic_basis_is_checked`.
- 変異の試験: `a_basis_element_outside_the_ideal_is_refused`, `a_basis_of_another_ideal_is_refused`, `a_changed_entry_of_c_is_refused`.

### `gb.contains`

- 命題: I ⊆ (G). 各 i について f_i = Σ_k g_k N_ki である.
- 範囲: `exact`.
- 前提: N は t 行 s 列である (違えば断る).
- 方法: `certificate`. G を 1 行の行列として N と掛け, 各列を f_i と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_twisted_cubic_basis_is_checked`.
- 変異の試験: `a_basis_of_a_smaller_ideal_is_refused`, `a_changed_entry_of_n_is_refused`.

### `gb.groebner`

- 命題: G は, G が生成するイデアル (G) の, spec の順序についての Groebner basis である. つまり, (G) の 0 でない元の先頭単項式が生成するイデアルは (lm(g_1), ..., lm(g_t)) に等しい. `gb.contained` と `gb.contains` から (G) = I なので, G は I の Groebner basis であり, `gb.reduced_form` と合わせると I の reduced Groebner basis である.
- 範囲: `exact`.
- 前提: job が `certificate: "s_pairs"` を求め, 値に certificate がある. H は t 行 P 列である (違えば断る).
- 方法: `certificate`. 下の「S 対の lcm representation」の検査をする. job が `representation` を求めたときは, 値は S 対を持たないので, backend の答えとして `asserted` にする. その `reason` は「the job asked for no certificate of the S-pairs」である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である. job が `representation` を求めて `asserted` になったときは, `certificate: "s_pairs"` を求める.
- 支え: 試験 `the_twisted_cubic_basis_is_checked_with_its_s_pairs`, `a_lex_basis_is_checked_with_its_s_pairs`, `the_zero_ideal_the_unit_ideal_and_one_element_have_no_s_pairs`.
- 変異の試験: `a_changed_s_pair_representation_is_refused`, `the_s_pair_representation_from_the_definition_is_refused`, `an_s_pair_left_out_is_refused`, `a_coprime_s_pair_listed_is_refused`, `s_pairs_out_of_order_are_refused`, `an_order_mix_up_is_refused_by_its_s_pairs`.

S 対の lcm representation (#68). i < j について L_ij = lcm(lm(g_i), lm(g_j)) とし, S(g_i, g_j) = (L_ij / lt(g_i)) g_i - (L_ij / lt(g_j)) g_j とする. ここで lt(g) = lc(g) lm(g) である. S(g_i, g_j) = Σ_k h_k g_k で, h_k g_k ≠ 0 となる各 k について lm(h_k g_k) < L_ij (真に小さい) であるとき, この表示を S(g_i, g_j) の lcm representation と呼ぶ. G のどの元も 0 でないとき, G が (G) の Groebner basis であることは, i < j の全ての S(g_i, g_j) が lcm representation を持つことと同値である (Cox, Little and O'Shea, Ideals, Varieties, and Algorithms, Chapter 2, §9, Theorem 6. 番号は版によって違いうるので要確認).

lm(g_i) と lm(g_j) が互いに素な対は, データなしに lcm representation を持つ (Buchberger の第 1 判定法. Cox, Little and O'Shea, Chapter 2, §9, Proposition 4. 要確認). 確かめ方を書く. m_i = lm(g_i), c_i = lc(g_i) とし, g_i = c_i m_i + p_i, g_j = c_j m_j + p_j と書く. m_i と m_j が互いに素なら L_ij = m_i m_j であり, S(g_i, g_j) = (m_j / c_i) g_i - (m_i / c_j) g_j = (p_i g_j - p_j g_i) / (c_i c_j) である. 実際, 両辺とも m_j p_i / c_i - m_i p_j / c_j に等しい (左辺では m_i m_j の項が, 右辺では p_i p_j の項が打ち消し合う). 右辺の 2 つの積の先頭単項式は lm(p_i) m_j と lm(p_j) m_i で, lm(p_i) < m_i と lm(p_j) < m_j からどちらも m_i m_j = L_ij より小さい. p_i や p_j が 0 なら, その積はない. 検査器が除くのは, ちょうどこの対である.

certificate の `s_pairs` は `{"pairs": [[i, j], ...], "representations": H}` である. `pairs` は, 先頭単項式が互いに素でない対 (i, j) を全て, (i, j) の辞書式の昇順に並べたものである. i と j は 0 から数え, i < j である. P を `pairs` の長さとする. H は t 行 P 列の多項式行列で, 第 p 列の成分 H_1p, ..., H_tp は, p 番目の対 (i, j) について S(g_i, g_j) = Σ_k g_k H_kp を満たす. 検査器は次の順に確かめ, どれかが成り立たなければ断る.

1. G と spec の順序から対の並びを求め, `pairs` と一致する. 欠けた対, 余分な対, 互いに素な対, 重なった対, 順の違いは断る.
2. H は spec の環で読めて, t 行 P 列である.
3. 各対の S(g_i, g_j) を Rust で作る. 使うのは単項式と多項式の積と, 多項式の差だけである.
4. G を 1 行の行列として H と掛け, 第 p 列が p 番目の対の S(g_i, g_j) に等しい.
5. 0 でない各 H_kp について, lm(H_kp) と lm(g_k) の指数の和が, spec の順序で L_ij より小さい. lm(h g) = lm(h) lm(g) なので, これは lm(H_kp g_k) < L_ij の検査である. 指数が 32 bit に収まらなければ断る.

検査器は G に元を足さず, 簡約の相手を選ばず, 割り算をせず, 途中で打ち切らない. 成功するときも全ての対を調べる. どの段も job の期限で止まる. これは [ロードマップ](roadmap.md) §1.2 の区分 (a) の検査で, Buchberger 法の再実装ではない. driver は, S(g_i, g_j) を G で割った商を h_k として送ってよい. 割り算による表示は lm(h_k g_k) ≤ lm(S(g_i, g_j)) < L_ij を満たすので, lcm representation である.

`representation` だけでは, 順序の取り違えを常には見つけられない. lex を宣言したジョブに, driver が grevlex の reduced Groebner basis を返したとする. 環は QQ[x, y, z] で, x > y > z である.


断られる例. F = (x^2 y - z/2, 2xy^2 - x, 3xz - y) の grevlex の reduced Groebner basis は, 先頭単項式の昇順に (z^2 - x/3, xz - y/3, x^2 - yz, y^2 z - z/2, y^3 - y/2, xy^2 - x/2) である. lex では最初の元の先頭項が -x/3 なので monic でなく, C と N が正しくても `gb.reduced_form` で断られる (試験 `an_order_mix_up_that_is_not_monic_is_refused`).

通る例. F = (-xyz + z^2, -xyz - x^2 + xy) の grevlex の reduced Groebner basis は, 先頭単項式の昇順に (x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 - yz^2) である. lex での先頭単項式は x^2, xyz, xz^2 で, どの元も monic であり, どの項も他の元の先頭単項式で割り切れない. grevlex の並びのまま返すと, lex では先頭単項式が減少するので, 昇順の条件によって `gb.reduced_form` で断られる. lex の昇順 (xz^2 < xyz < x^2) に並べ替えて C と N も合わせると, `gb.reduced_form`, `gb.contained`, `gb.contains` が `checked` で通り, `gb.groebner` だけが `asserted` になる. しかしこれは lex の Groebner basis ではない. z (xyz - z^2) - y (yz^3 + xz^2 - yz^2) = -y^2 z^3 + y^2 z^2 - z^3 は I に属し, その lex の先頭単項式 y^2 z^3 は x^2, xyz, xz^2 のどれでも割り切れないからである (試験 `an_order_mix_up_that_is_monic_and_interreduced_passes`).

このような取り違えを見つけるのは, `gb.groebner` の検査 (`s_pairs`) である. 通る例を lex の昇順に並べた基底を g_0 = yz^3 + xz^2 - yz^2, g_1 = xyz - z^2, g_2 = x^2 - xy + z^2 とする. これは lex の Groebner basis でないので, 上の定理によって, lcm representation を持たない S 対がある. よって, driver がどんな `s_pairs` を送っても, 検査のどこかで断られる. 試験 `an_order_mix_up_is_refused_by_its_s_pairs` は, 壊れた driver が送りうる 2 つの `s_pairs` を確かめる. grevlex で求めた対と表示を lex の並びに移すと, lex での対 (0, 2) が欠けるので, 対の並びで断られる. lex の各 S 対を grevlex で G で割った表示は, S(g_0, g_1) = y g_0 - z g_1, S(g_0, g_2) = x g_0 - z^2 g_2, S(g_1, g_2) = -g_0 + y g_1 である. 等式は成り立つが, y g_0 の lex の先頭単項式 xyz^2 は L_01 = xyz^2 に等しいので, 不等式で断られる. 同じ基底の grevlex のジョブは, grevlex の S 対とともに `checked` で通る. 実際の driver を grevlex の順序の項目で動かした lex のジョブの応答は, 各 backend の `testdata/ideal.json` に `mixup_lex_answered_in_grevlex` として記録してある (`tests/regression` の driver は壊せない). 試験 `the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs` (Macaulay2 と OSCAR の backend にある同名の 2 つ) は, この応答が `gb.groebner` で断られることを確かめる. driver は, 宣言した順序の項目 (#64) で環を作り, 対もその順序で求める.

### not_claimed

- C, N, H の選び方. どれも一意でない.
- spec の順序と別の順序で, G が Groebner basis であること.
- G の元の次数と係数の大きさ, certificate の大きさ.

## ideal.normal_form

契約の確認: 未. 係数域は QQ だけである.

記号. 環 QQ[x_1, ..., x_n], 項順序 >, F = (f_1, ..., f_s), I = (f_1, ..., f_s), lm と lc は `ideal.groebner_basis` の節と同じである. 入力はさらに多項式 f である. 検査器は F, f, 順序を spec から取り, f を I の環で読む. ZZ の環と変数のない環は, CAS を起動する前に `INVALID_ARGUMENTS` で断る. 引数 `certificate` は `ideal.groebner_basis` と同じく `representation` (既定), `s_pairs`, `none` のどれかである. 生成元と f を合わせた s + 1 個の多項式と変数の数の積は, `MAX_GENERATORS_TIMES_VARIABLES` 以下とする.

f の I を法とする > についての正規形とは, f - r ∈ I で, どの項も ⟨lm(I)⟩ に属さない多項式 r である. ここで ⟨lm(I)⟩ は I の 0 でない元の先頭単項式が生成するイデアルである. このような r はただ 1 つある (Cox, Little and O'Shea, Ideals, Varieties, and Algorithms, Chapter 2, §6, Proposition 1. 番号は版によって違いうるので要確認).

値は, 余り r と, I の reduced Groebner basis G = (g_1, ..., g_t) である. G の条件は `ideal.groebner_basis` の値と同じである. certificate は, `ideal.groebner_basis` と同じ C (`basis_in_generators`) と N (`generators_in_basis`), および t 個の多項式 q_1, ..., q_t (`quotients`) である. job が `certificate: "s_pairs"` を求めたときは, certificate はさらに `ideal.groebner_basis` と同じ `s_pairs` を持つ. job が `certificate: "none"` を指定したときと, certificate のせいで値が結果の上限を超えたとき (#36 より前) は certificate を省き, C, N, 商, S 対を使う主張を `asserted` にする (下の「certificate を省いたとき」).

### `gb.reduced_form`

- 命題: G のどの元も 0 でなく, 各 j で lc(g_j) = 1 である. lm(g_1) < lm(g_2) < ... < lm(g_t) である. i ≠ j について, g_i のどの項も lm(g_j) で割り切れない.
- 範囲: `exact`.
- 前提: 値の多項式は spec の環で読む (変数の数が合わない項は断る).
- 方法: `self_certifying`. `ideal.groebner_basis` と同じ検査を値の `basis` に行う. G は certificate の外にあるので, certificate を省いても `checked` である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_normal_form_of_a_polynomial_outside_the_cubic_is_checked`, `the_zero_ideal_and_the_unit_ideal_give_normal_forms_and_memberships`.
- 変異の試験: `a_normal_form_with_a_non_monic_basis_element_is_refused`.

### `gb.contained`

- 命題: (G) ⊆ I. 各 j について g_j = Σ_i f_i C_ij である.
- 範囲: `exact`.
- 前提: C は s 行 t 列である (違えば断る).
- 方法: `certificate`. F を 1 行の行列として C と掛け, 各列を g_j と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_normal_form_of_a_polynomial_outside_the_cubic_is_checked`.
- 変異の試験: `a_normal_form_with_a_changed_entry_of_c_is_refused`.

### `gb.contains`

- 命題: I ⊆ (G). 各 i について f_i = Σ_k g_k N_ki である.
- 範囲: `exact`.
- 前提: N は t 行 s 列である (違えば断る).
- 方法: `certificate`. G を 1 行の行列として N と掛け, 各列を f_i と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_normal_form_of_a_polynomial_outside_the_cubic_is_checked`.
- 変異の試験: `a_normal_form_with_a_changed_entry_of_n_is_refused`.

### `gb.groebner`

- 命題: G は, G が生成するイデアル (G) の, spec の順序についての Groebner basis である. つまり, (G) の 0 でない元の先頭単項式が生成するイデアルは (lm(g_1), ..., lm(g_t)) に等しい.
- 範囲: `exact`.
- 前提: job が `certificate: "s_pairs"` を求め, 値に certificate がある. H は t 行 P 列である (違えば断る).
- 方法: `certificate`. `ideal.groebner_basis` の「S 対の lcm representation」と同じ検査を, 値の `basis` と certificate の `s_pairs` に行う. job が `representation` を求めたときは, backend の答えとして `asserted` にする. その `reason` は「the job asked for no certificate of the S-pairs」である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である. job が `representation` を求めて `asserted` になったときは, `certificate: "s_pairs"` を求める.
- 支え: 試験 `a_normal_form_with_s_pairs_is_checked`.
- 変異の試験: `a_normal_form_with_a_changed_s_pair_representation_is_refused`.

### `normal_form.reduced`

- 命題: f - r = Σ_k q_k g_k であり, r のどの項もどの lm(g_k) でも割り切れない. 前半から f - r ∈ (G) である.
- 範囲: `exact`.
- 前提: 商はちょうど t 個である (違えば断る).
- 方法: `certificate`. G を 1 行の行列として商の列と掛け, f - r と比べる. r の項が lm(g_k) で割り切れないことは値だけで決まるので, certificate を省いても確かめ, 割り切れる項があれば断る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_normal_form_of_a_polynomial_outside_the_cubic_is_checked`, `the_normal_form_of_a_member_of_the_cubic_is_zero`.
- 変異の試験: `a_changed_quotient_is_refused`, `a_changed_remainder_is_refused`, `a_remainder_not_reduced_is_refused`.

### `normal_form.unique`

- 命題: r は f の I を法とする > についての正規形である. つまり f - r ∈ I で, r のどの項も ⟨lm(I)⟩ に属さない. このような r は f, I, > から一意に定まる.
- 範囲: `exact`.
- 前提: なし.
- 方法: `theorem`. `normal_form.reduced` から f - r ∈ (G) で, r のどの項もどの lm(g_k) でも割り切れない. `gb.contained` と `gb.contains` から (G) = I である. `gb.groebner` から ⟨lm(I)⟩ = (lm(g_1), ..., lm(g_t)) であり, 単項式がこのイデアルに属するのは, ある lm(g_k) で割り切れるときに限る. よって r のどの項も ⟨lm(I)⟩ に属さない. 一意性は Cox, Little and O'Shea, Chapter 2, §6, Proposition 1 (要確認) による.
- 水準: `checked`.
- depends_on: `normal_form.reduced`, `gb.contained`, `gb.contains`, `gb.groebner`.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である. ただし, 結果での実効水準は 4 つの前提の実効水準のうち最も弱いものである. job が `representation` を求めたときは `gb.groebner` によって `asserted` になり, certificate を省いたときは `gb.contained`, `gb.contains`, `gb.groebner`, `normal_form.reduced` によって `asserted` になる. `certificate: "s_pairs"` を求めれば, 実効水準も `checked` になる.
- 支え: 試験 `the_unique_normal_form_takes_the_weakest_level_of_its_premises`, `a_normal_form_with_s_pairs_is_checked`.
- 変異の試験: `a_changed_remainder_is_refused`, `a_remainder_not_reduced_is_refused`.

### not_claimed

- C, N, 商, H の選び方. どれも一意でない. r と G は一意である.
- spec の順序と別の順序での正規形と Groebner basis.
- G の元, 商, r の次数と係数の大きさ, certificate の大きさ.

## ideal.membership

契約の確認: 未. 係数域は QQ だけである.

記号と, 断る入力は `ideal.normal_form` の節と同じである.

値は `member` である. f ∈ I なら真で, そうでなければ偽である. certificate は, `member` が真なら s 個の多項式 c_1, ..., c_s (`coefficients`) である. 偽なら, backend が I の reduced Groebner basis として返す G (`basis`), N (`generators_in_basis`), 商 q_1, ..., q_t (`quotients`), 余り r (`remainder`) である. job が `certificate: "s_pairs"` を求めたときは, 偽の certificate はさらに `ideal.groebner_basis` と同じ `s_pairs` を持つ. 真の certificate は, job が何を求めても係数だけで, `s_pairs` は当てはまらない (f ∈ I を示すのに Groebner basis は要らない). G が I を生成することは確かめない (下の not_claimed). `member` と違う種類の certificate は断る.

偽の certificate には C を含めない. f ∉ I を示すのに (G) ⊆ I は要らないからである (`membership.not_in` の方法). Issue #67 の本文は, `membership.not_in` の前提に `gb.contained` を挙げていたが, この契約では外した.

certificate を省いたときは, `membership.in` か `membership.not_in` を `asserted` にする. `member` が偽の値は G を certificate の中に持つので, そのときは `gb.*` と `normal_form.*` の主張を載せない (下の「certificate を省いたとき」).

### `gb.reduced_form`

- 命題: `member` が偽のとき, certificate の G のどの元も 0 でなく, 各 j で lc(g_j) = 1 である. lm(g_1) < lm(g_2) < ... < lm(g_t) である. i ≠ j について, g_i のどの項も lm(g_j) で割り切れない.
- 範囲: `exact`.
- 前提: `member` が偽で, 値に certificate がある. 値の多項式は spec の環で読む.
- 方法: `self_certifying`. `ideal.groebner_basis` と同じ検査を certificate の `basis` に行う.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_polynomial_outside_the_cubic_is_checked_by_its_normal_form`.
- 変異の試験: `a_membership_certificate_with_a_basis_out_of_order_is_refused`.

### `gb.contains`

- 命題: `member` が偽のとき, I ⊆ (G). 各 i について f_i = Σ_k g_k N_ki である.
- 範囲: `exact`.
- 前提: `member` が偽で, 値に certificate がある. N は t 行 s 列である (違えば断る).
- 方法: `certificate`. G を 1 行の行列として N と掛け, 各列を f_i と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_polynomial_outside_the_cubic_is_checked_by_its_normal_form`.
- 変異の試験: `a_membership_certificate_with_a_changed_entry_of_n_is_refused`.

### `gb.groebner`

- 命題: `member` が偽のとき, G は, G が生成するイデアル (G) の, spec の順序についての Groebner basis である.
- 範囲: `exact`.
- 前提: `member` が偽で, 値に certificate があり, job が `certificate: "s_pairs"` を求めた. H は t 行 P 列である (違えば断る).
- 方法: `certificate`. `ideal.groebner_basis` の「S 対の lcm representation」と同じ検査を, certificate の `basis` と `s_pairs` に行う. job が `representation` を求めたときは, backend の答えとして `asserted` にする. その `reason` は「the job asked for no certificate of the S-pairs」である.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である. job が `representation` を求めて `asserted` になったときは, `certificate: "s_pairs"` を求める.
- 支え: 試験 `a_polynomial_outside_the_cubic_is_checked_with_its_s_pairs`.
- 変異の試験: `a_membership_certificate_with_a_changed_s_pair_representation_is_refused`.

### `normal_form.reduced`

- 命題: `member` が偽のとき, f - r = Σ_k q_k g_k であり, r のどの項もどの lm(g_k) でも割り切れない.
- 範囲: `exact`.
- 前提: `member` が偽で, 値に certificate がある. 商はちょうど t 個である (違えば断る).
- 方法: `certificate`. G を 1 行の行列として商の列と掛け, f - r と比べる. r の各項を lm(g_k) で割り切れるか調べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_polynomial_outside_the_cubic_is_checked_by_its_normal_form`.
- 変異の試験: `a_membership_certificate_with_a_changed_quotient_is_refused`, `a_membership_certificate_with_a_changed_remainder_is_refused`, `a_remainder_not_reduced_is_refused`.

### `membership.in`

- 命題: `member` が真のとき, f ∈ I である. f = Σ_i c_i f_i である.
- 範囲: `exact`.
- 前提: 係数はちょうど s 個である (違えば断る).
- 方法: `certificate`. F を 1 行の行列として係数の列と掛け, f と比べる. Groebner basis を使わないので, `gb.*` の主張には依らない.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `a_member_of_the_cubic_is_checked_by_its_coefficients`, `the_zero_ideal_and_the_unit_ideal_give_normal_forms_and_memberships`.
- 変異の試験: `a_changed_coefficient_is_refused`, `a_polynomial_outside_the_ideal_claimed_as_a_member_is_refused`.

### `membership.not_in`

- 命題: `member` が偽のとき, f ∉ I である.
- 範囲: `exact`.
- 前提: なし.
- 方法: `theorem`. r ≠ 0 は値から直接確かめ, r = 0 なら断る. `normal_form.reduced` から f - r ∈ (G) で, r のどの項もどの lm(g_k) でも割り切れない. `gb.groebner` から (G) の 0 でない元の先頭単項式はある lm(g_k) で割り切れるので, r ∉ (G) であり, よって f ∉ (G) である. `gb.contains` から I ⊆ (G) なので, f ∉ I である (Cox, Little and O'Shea, Chapter 2, §6, Corollary 2. 要確認). (G) ⊆ I (`gb.contained`) は使わない.
- 水準: `checked`.
- depends_on: certificate を含む値のとき `normal_form.reduced`, `gb.contains`, `gb.groebner`.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である. ただし, 結果での実効水準は 3 つの前提の実効水準のうち最も弱いものなので, job が `representation` を求めたときは `gb.groebner` によって `asserted` になる. `certificate: "s_pairs"` を求めれば, 実効水準も `checked` になる. certificate を省いた値では, この主張は `asserted` で, depends_on を持たない.
- 支え: 試験 `a_polynomial_outside_the_cubic_is_checked_by_its_normal_form`, `a_polynomial_outside_the_cubic_is_checked_with_its_s_pairs`.
- 変異の試験: `a_zero_remainder_claiming_not_in_is_refused`.

### not_claimed

- `member` が真のときの係数 c_i の選び方. 一意でない.
- `member` が偽のときの N, 商, H の選び方.
- `member` が偽のとき, certificate の G が I の reduced Groebner basis であること. 確かめるのは I ⊆ (G) (`gb.contains`) だけで, C を含めないので (G) ⊆ I は確かめない. よって, r が f の I を法とする正規形であることも主張しない (`normal_form.unique` はこの operation に載せない).
- certificate の大きさ. f ∉ I の certificate は, f ∈ I のものよりはるかに大きくなりうる.

## module.syzygies

契約の確認: 未. 係数域は QQ だけである.

記号. 入力は, 変数が 1 つ以上の環 R = QQ[x_1, ..., x_n] と, 自由加群 R^m の部分加群 M である. M は m 行 s 列の多項式行列 F (`generators`) の列が生成する (生成元の向きは列. ロードマップ Q12). 検査器は F を spec から取る. ZZ の環と変数のない環は, CAS を起動する前に `INVALID_ARGUMENTS` で断る.

値は s 行 r 列の多項式行列 S (`syzygies`) である. S の列は, F の列の syzygy 加群 Syz(F) = {a ∈ R^s : F a = 0} の生成元である. Syz(F) が 0 なら r = 0 である. certificate はない.

### `syz.annihilates`

- 命題: F S = 0. つまり S の各列は F の列の syzygy である.
- 範囲: `exact`.
- 前提: S は s 行である (違えば断る). 値の多項式は spec の環で読む.
- 方法: `self_certifying`. F と S を掛け, 0 と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_syzygies_of_the_cubic_are_checked`.
- 変異の試験: `a_syzygy_that_does_not_annihilate_is_refused`.

### `syz.generates`

- 命題: S の列は Syz(F) を生成する.
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 生成することの certificate (Syz(F) の Groebner basis と, その S 対の表示) を検査する (ロードマップの V7). 系統の違う 2 つの CAS の答えの照合で `cross_checked` にする (V6).
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### not_claimed

- S の列が極小な生成系であること.
- S の選び方. 一意でない.

## complex.free_resolution

契約の確認: 未. 係数域は QQ だけである.

記号. 入力は, 変数が 1 つ以上の環 R = QQ[x_1, ..., x_n] の多項式の列 F = (f_1, ..., f_s) で, I = (f_1, ..., f_s) とする. 0 の生成元も位置を保って数える. ZZ の環と変数のない環は, CAS を起動する前に `INVALID_ARGUMENTS` で断る. 引数 `certificate` は `include` (既定) か `none` である. 次数は, 各変数の次数を 1 とする標準の次数付けで測る. 0 でない多項式が斉次であるとは, どの項も同じ次数を持つことである.

値は, R/I の free resolution F_0 = R ← F_1 ← ... ← F_k の微分 d_1, ..., d_k (`differentials`) である. F_i = R^{n_i} で, d_i は n_{i-1} 行 n_i 列の多項式行列である (n_0 = 1). I が 0 なら k = 0 である. I の生成元がどれも斉次 (0 は除く) なら, 値は各 F_i の基底の次数の列 (`degrees`) を持つ. さらに, どの d_i にも 0 でない定数の成分がなければ, R/I の graded Betti 数 β_{i,j} (`betti`. F_i の基底のうち次数 j のものの数) を持つ. 生成元に斉次でないものがあれば, 値は次数も Betti 数も持たない. 極小な free resolution も Betti 数も, 不変量として定まらないからである. certificate は, s 行 n_1 列の行列 C (`differential_in_generators`) と, n_1 行 s 列の行列 N (`generators_in_differential`) である. F を 1 行の行列として, d_1 = F C かつ F = d_1 N である. k = 0 のときは d_1 を 1 行 0 列の行列と読む. job が `certificate: "none"` を指定したときと, certificate のせいで値が結果の上限を超えたとき (#36 より前) は certificate を省き, `res.image` を `asserted` にする (下の「certificate を省いたとき」).

主張の組は入力と値によって変わる. 斉次でない入力では `res.complex`, `res.image`, `res.exact` だけを載せ, `res.input_homogeneous`, `res.differentials_homogeneous`, `res.entries_in_maximal_ideal`, `res.minimal`, `res.betti` を結果の `not_claimed` に入れる. 斉次な入力で, 0 でない定数の成分がある値では, `res.entries_in_maximal_ideal`, `res.minimal`, `res.betti` を `not_claimed` に入れる.

### `res.complex`

- 命題: 各 i で d_i d_{i+1} = 0. つまり値は複体である.
- 範囲: `exact`.
- 前提: d_1 は 1 行で, d_{i+1} は d_i の列の数だけの行を持つ (違えば断る). 値の多項式は spec の環で読む.
- 方法: `self_certifying`. 隣り合う微分を掛け, 0 と比べる.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`.
- 変異の試験: `a_resolution_that_is_not_a_complex_is_refused`.

### `res.image`

- 命題: d_1 の像は I である. よって H_0 = F_0 / im d_1 = R/I である.
- 範囲: `exact`.
- 前提: 値に certificate がある. C は s 行 n_1 列, N は n_1 行 s 列である (違えば断る).
- 方法: `certificate`. F C と d_1 を比べ (im d_1 ⊆ I), d_1 N と F を比べる (I ⊆ im d_1).
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`, `the_zero_ideal_and_the_unit_ideal_are_resolved`.
- 変異の試験: `a_resolution_of_another_image_is_refused`, `a_resolution_with_a_changed_entry_of_c_is_refused`, `a_resolution_with_a_changed_entry_of_n_is_refused`.

### `res.input_homogeneous`

- 命題: I の 0 でない生成元はどれも斉次である.
- 範囲: `exact`.
- 前提: なし. この主張は, 斉次な入力の結果にだけ載る.
- 方法: `recomputation`. Rust が spec の F の各項の次数を数える. 斉次でない入力の値が次数か Betti 数を持てば断る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`, `an_inhomogeneous_ideal_claims_no_minimality`.
- 変異の試験: `degrees_for_inhomogeneous_generators_are_refused`.

### `res.differentials_homogeneous`

- 命題: 値の次数で F_i を次数付き自由加群 ⊕_c R(-deg_i(c)) とみると, 各 d_i は次数 0 の斉次写像である. つまり d_i の 0 でない成分 (r, c) は, 次数 deg_i(c) - deg_{i-1}(r) の斉次多項式である. F_0 の基底の次数は 0 である.
- 範囲: `exact`.
- 前提: 入力が斉次である. 次数の列は k + 1 個で, F_0 には 1 つ, F_i には d_i の列の数だけの次数がある (違えば断る).
- 方法: `self_certifying`. 各成分の各項の次数を数える.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`.
- 変異の試験: `a_differential_of_the_wrong_degree_is_refused`.

### `res.entries_in_maximal_ideal`

- 命題: どの d_i の成分も, 斉次な極大イデアル m = (x_1, ..., x_n) に属する. つまり 0 でない定数の成分がない.
- 範囲: `exact`.
- 前提: 入力が斉次である. この主張は, 0 でない定数の成分がない値にだけ載る.
- 方法: `self_certifying`. 各成分が 0 でない定数かを調べる. 定数の成分がある値が Betti 数を持てば断る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`, `a_resolution_with_a_constant_entry_does_not_claim_minimality`.
- 変異の試験: `betti_numbers_of_a_resolution_with_a_constant_entry_are_refused`.

### `res.exact`

- 命題: 値は F_1, ..., F_k で完全である. つまり各 i ≥ 1 で ker d_i = im d_{i+1} であり, d_k は単射である.
- 範囲: `exact`.
- 前提: なし.
- 方法: backend の答え.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 各 ker d_i の生成元と, それを d_{i+1} で表す certificate を検査する (ロードマップの V7). 系統の違う 2 つの CAS の答えの照合で `cross_checked` にする (V6).
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `res.minimal`

- 命題: 値は R/I の極小な graded free resolution である.
- 範囲: `exact`.
- 前提: 入力が斉次で, 0 でない定数の成分がない.
- 方法: `theorem`. `res.input_homogeneous`, `res.differentials_homogeneous`, `res.exact` から, 値は R/I の graded free resolution である. `res.entries_in_maximal_ideal` から, 各 d_i ⊗ R/m = 0 である. このような graded free resolution は極小である (Eisenbud, Commutative Algebra with a View Toward Algebraic Geometry, §20.1, 要確認).
- 水準: `checked`. 実効水準は `asserted` (`res.exact` による).
- depends_on: `res.input_homogeneous`, `res.differentials_homogeneous`, `res.entries_in_maximal_ideal`, `res.exact`.
- 既存の欄: なし.
- 水準を上げる方法: `res.exact` を上げる.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`.
- 変異の試験: `betti_numbers_of_a_resolution_with_a_constant_entry_are_refused`.

### `res.betti`

- 命題: 値の `betti` は R/I の graded Betti 数 β_{i,j} = dim_QQ Tor_i(R/I, QQ)_j である. 0 の β_{i,j} は載せない.
- 範囲: `exact`.
- 前提: `res.minimal` の前提.
- 方法: `theorem`. 値の `betti` が, 次数の列から数えた F_i の基底のうち次数 j のものの数に等しいことを確かめ (違えば断る), `res.minimal` から, 極小な graded free resolution の F_i の次数 j の基底の数は β_{i,j} である (Eisenbud, Commutative Algebra with a View Toward Algebraic Geometry, §20.1, 要確認).
- 水準: `checked`. 実効水準は `asserted` (`res.minimal` による).
- depends_on: `res.minimal`.
- 既存の欄: なし.
- 水準を上げる方法: `res.exact` を上げる.
- 支え: 試験 `the_resolution_of_the_cubic_is_checked`.
- 変異の試験: `betti_numbers_other_than_those_of_the_degrees_are_refused`.

### not_claimed

- 斉次でない入力での, 極小性と Betti 数. 結果の `not_claimed` に主張の ID で入る.
- 斉次な入力で 0 でない定数の成分がある値での, 極小性と Betti 数. 結果の `not_claimed` に主張の ID で入る.
- C と N の選び方, 微分の基底の選び方. どれも一意でない.

## ideal.hilbert_series

契約の確認: 未. 係数域は QQ だけである.

記号. 入力は, 変数が n ≥ 1 個の環 R = QQ[x_1, ..., x_n] の多項式の列 F で, I = (F) とする. 0 でない生成元はどれも (標準の次数付けで) 斉次でなければならず, そうでなければ CAS を起動する前に `INVALID_ARGUMENTS` で断る. ZZ の環と変数のない環も同じく断る. 引数 `betti_table` が真なら, 値は R/I の graded Betti 数も持つ.

値は, R/I の Hilbert 級数 H(t) = Σ_j dim_QQ (R/I)_j t^j を P(t) / (1 - t)^d と書いたときの, P の整数係数の列 (`numerator`. 次数 0 から) と d (`denominator_exponent`) である. backend は P と (1 - t) が互いに素な形に約分して返す. 検査器は, 約分していない形も受け取る. `betti_table` が真なら, 値は `complex.free_resolution` と同じ形の `betti` を持つ.

### `hilbert.series`

- 命題: P(t) / (1 - t)^d は R/I の Hilbert 級数である.
- 範囲: `exact`.
- 前提: P の係数はどれも整数で (違えば断る), d ≤ n である (違えば断る).
- 方法: backend の答え. `betti_table` が真なら, Rust は Σ_{i,j} (-1)^i β_{i,j} t^j と P(t) (1 - t)^{n-d} を整数係数の多項式として比べ, 違えば断る (`consistency`. 水準を与えない). これは H(t) = Σ_{i,j} (-1)^i β_{i,j} t^j / (1 - t)^n (Hilbert の syzygy 定理からの帰結) による. Betti 数も同じ backend の答えなので, 照合ではない. `betti_table` が真なのに値が `betti` を持たないとき, 偽なのに持つときは断る.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 系統の違う 2 つの CAS の答えの照合で `cross_checked` にする (V6).
- 支え: 試験 `the_hilbert_series_of_the_cubic_agrees_with_its_betti_numbers`, `a_hilbert_series_with_a_changed_numerator_is_refused`.
- 変異の試験: なし (水準が `asserted` のため).

### not_claimed

- `betti` が R/I の Betti 数であること. Hilbert 級数との整合だけを確かめる.
- Hilbert 多項式, 次元, 次数. 値から読めるが, 主張しない.

## simplicial.homology

契約の確認: 未. 係数域は ZZ だけである.

記号. 入力は, facet の列で与えた単体的複体 X である. 頂点は 0 以上の整数で, facet は頂点の空でない集合である. 頂点が重なる facet, 他の facet に含まれる facet, 面の数が上限 (`MAX_FACES` = 512) を超える複体, 面の列挙で調べる部分集合の数 (facet F についての 2^|F| - 1 の和) が上限 (`MAX_FACE_WORK` = 16384) を超える複体は, CAS を起動する前に `INVALID_ARGUMENTS` で断る. f_k は X の k 次元の面の数で, d は X の次元である. 面は, 頂点を小さい順に並べた列として, 次元ごとに辞書式順序で並べる. 境界行列 ∂_k (1 ≤ k ≤ d) は f_{k-1} 行 f_k 列で, 面 [v_0, ..., v_k] の列は, v_i を除いた面の行に (-1)^i を持ち, 他の成分は 0 である. 境界行列は topology の package (`crates/algebramesh-topology`, [package の説明](packages/topology.md)) が組み立て, CAS は各 ∂_k の Smith 標準形とその certificate だけを返す. 検査器は複体を spec から取り, ∂_k を組み立て直す.

値は, `face_counts` (f_0, ..., f_d), `smith_diagonals` (∂_1, ..., ∂_d の Smith 標準形の対角成分 d^(1), ..., d^(d)), `homology` (H_0, ..., H_d のそれぞれの `rank` と `torsion`), `euler_characteristic` と, `certificate` である. certificate は, 各 ∂_k について単模な P_k (f_{k-1} 次) と Q_k (f_k 次) とそれぞれの逆行列である (`certificate.boundaries`). r_k を d^(k) の 0 でない成分の数とし, r_0 = r_{d+1} = 0 とする.

### `homology.boundary`

- 命題: `face_counts` は X の各次元の面の数であり, 上の ∂_k について ∂_k ∂_{k+1} = 0 (1 ≤ k < d) である. したがって (C_*(X), ∂) は鎖複体である.
- 範囲: `exact`.
- 前提: なし.
- 方法: `recomputation`. 検査器は spec の facet から面を列挙し, ∂_k を組み立て直し, 積を正確な整数で計算する. 面の列挙と ∂_k の組立てのコードは TCB に入る.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `face_counts`.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_boundary_matrices_of_a_triangle`, `faces_are_shared_between_facets`.
- 変異の試験: `the_boundary_matrices_of_a_triangle`, `a_value_with_other_face_counts_is_refused`.

### `homology.snf_form`

- 命題: 各 k について, d^(k) は min(f_{k-1}, f_k) 個の成分を持ち, その成分は 0 以上の整数で, d^(k)_1 | d^(k)_2 | ... である (0 は 0 だけを割る).
- 範囲: `exact`.
- 前提: なし.
- 方法: `self_certifying`. 値を読む.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `smith_diagonals`.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_recorded_homology_values_pass_with_the_claims_of_the_catalogue`.
- 変異の試験: `a_smith_diagonal_of_a_boundary_that_does_not_divide_is_refused`.

### `homology.snf_equivalent`

- 命題: 各 k について, diag(d^(k)) = P_k ∂_k Q_k であり, P_k と Q_k は単模である. `homology.snf_form` と合わせると, d^(k) は ∂_k の Smith 標準形の対角成分である.
- 範囲: `exact`.
- 前提: なし.
- 方法: `certificate`. P_k P_k' = I, Q_k Q_k' = I, P_k ∂_k Q_k = diag(d^(k)) を確かめる. `matrix.smith_normal_form` の `snf.equivalent` と同じ検査である. job が certificate を求めなかったとき, および検査器が certificate を大きさのために省いたときは, backend の答えとして記録する.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `certificate.boundaries`.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_recorded_homology_values_pass_with_the_claims_of_the_catalogue`.
- 変異の試験: `a_changed_smith_entry_is_refused`, `a_changed_transform_of_a_boundary_is_refused`, `a_certificate_left_out_by_the_backend_is_refused`.

### `homology.groups`

- 命題: 各 k (0 ≤ k ≤ d) について, H_k(X; ZZ) は ZZ^{b_k} と, d^(k+1) の 1 より大きい成分 t のそれぞれについての ZZ/t との直和に同型である. ここで b_k = f_k - r_k - r_{k+1} は `homology[k].rank` で, t の列は `homology[k].torsion` である (d^(d+1) は空とする).
- 範囲: `exact`.
- 前提: なし.
- 方法: `theorem`. ∂_k ∂_{k+1} = 0 のとき, H_k は ∂_k の階数と ∂_{k+1} の Smith 標準形の対角成分で決まる (Munkres, Elements of Algebraic Topology, §11. 定理の番号は要確認). 検査器は, 値の `homology` が f_k と d^(k), d^(k+1) から決まるものに等しいことを確かめ, 違えば断る.
- 水準: `checked`.
- depends_on: `homology.boundary`, `homology.snf_form`, `homology.snf_equivalent`.
- 既存の欄: `homology`.
- 水準を上げる方法: 最上位である. 契約の確認の後, certificate のある結果の要約は `verified` になる.
- 支え: 試験 `the_boundary_of_the_tetrahedron_has_h2_zz`, `the_torus_with_seven_vertices_has_h1_zz2`, `the_projective_plane_with_six_vertices_has_h1_z2_and_h2_zero`.
- 変異の試験: `homology_groups_other_than_the_forms_give_are_refused`, `a_torsion_coefficient_that_the_forms_do_not_give_is_refused`.

### `homology.euler_characteristic`

- 命題: `euler_characteristic` は X の Euler 標数 Σ_k (-1)^k f_k に等しく, Σ_k (-1)^k b_k にも等しい.
- 範囲: `exact`.
- 前提: なし.
- 方法: `recomputation`. 検査器は f_k を spec の複体から数え直す. さらに `homology` の `rank` の交代和と比べ, 違えば断る (`consistency`. 水準を与えない). b_k = f_k - r_k - r_{k+1} なので, 両者は `homology.groups` が成り立てば恒等的に等しい. この比較は, 検査器自身の階数の計算を見張るものである.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `euler_characteristic`.
- 水準を上げる方法: 最上位である.
- 支え: 試験 `the_euler_characteristics_of_the_recorded_complexes`.
- 変異の試験: `the_euler_characteristics_of_the_recorded_complexes`, `a_changed_euler_characteristic_is_refused`.

### not_claimed

- 各ホモロジー群の生成元を表す輪体.
- P_k と Q_k の一意性.
- 複体が三角形分割として表すことを意図した空間についての性質.

## certificate を省いたとき

- job が `certificate: "none"` を指定したときと, certificate のせいで値が結果の上限を超えたとき (#36 より前) は, certificate を使う主張を `asserted` にし, `reason` に理由を記録する. 上限を超えたときは, certificate を `{"omitted": "certificate_too_large"}` に置き換える.
- certificate を省いても値がまだ上限を超えるときは, `RESOURCE_LIMIT` とする.
- job が certificate を求めたのに返答にない場合と, backend が certificate を省いた場合は, 結果を断る (`UNSUPPORTED_RESULT`). 水準を黙って下げることはしない. 解のある `matrix.solve` では, 解そのものが certificate である. `ideal.*` では, job が `certificate: "s_pairs"` を求めたのに certificate に `s_pairs` がない場合と, 求めていないのに `s_pairs` がある場合も断る.
- `ideal.groebner_basis` では, certificate を省くと `gb.contained`, `gb.contains`, `gb.groebner` が `asserted` になり, `gb.reduced_form` は値だけで決まるので `checked` のまま残る. `gb.groebner` の `reason` は, job が `s_pairs` を求めていたときは省いた理由 (`certificate_too_large`) であり, そうでなければ「the job asked for no certificate of the S-pairs」である. job が `representation` を求めたときは, certificate を省かなくても `gb.groebner` は `asserted` である.
- `ideal.normal_form` では, certificate を省くと `gb.contained`, `gb.contains`, `gb.groebner`, `normal_form.reduced` が `asserted` になる. `gb.groebner` の `reason` は `ideal.groebner_basis` と同じである. G と r は certificate の外にあるので, `gb.reduced_form` は `checked` のまま残り, r の項が lm(g_k) で割り切れないことも確かめ続ける (割り切れる項があれば断る). `normal_form.unique` は自身の水準 `checked` と depends_on を保ち, 実効水準は `asserted` である.
- `ideal.membership` では, certificate を省くと `membership.in` か `membership.not_in` が `asserted` になる. `member` が偽の値は G と r を certificate の中に持つので, 省いた値には `gb.*` と `normal_form.*` の主張を載せず, `membership.not_in` は depends_on を持たない. r ≠ 0 も確かめられない.
- `complex.free_resolution` では, certificate を省くと `res.image` が `asserted` になる. 他の主張は値だけで決まるので, 水準を変えない.
- `simplicial.homology` では, certificate を省くと `homology.snf_equivalent` が `asserted` になり, それに依る `homology.groups` の実効水準も `asserted` になる. 他の主張は spec と値だけで決まるので, 水準を変えない.
