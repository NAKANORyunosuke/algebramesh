# 既存の検査の欄の説明

既存の 4 つの operation (`polynomial.factor`, `gkz.generate`, `gkz.indicial`, `gkz.series`) は, 互換のために検査の印を値の欄に残している. そのうち名前から読み違えやすいものについて, 何を確かめているかを書く. 主張としての定義と水準は [主張の契約](claims.md) にある.

## 因数分解

- `verified_product`: backend が検査の前に書く定数 true である. この欄自身は何も確かめない.
  - 積の検算は, backend の後で `algebramesh-verify` が行う (`factor.product`). 積が入力と違えば結果を断る. 届いた結果でこの欄が true なのは, 検算に通らない結果は届かないからである.
  - 検算が確かめるのは, 積の一致, 重複度が 1 以上であること, 因子が定数でないこと, 因子が互いに同伴でないこと (`factor.non_associate`, #102) である. 既約性 (`factor.irreducible`) は確かめない.

## GKZ 系 (`gkz.generate`)

- `checks.euler_operators_match`: Euler 作用素の列 `euler_operators` だけを, Rust が spec から作った作用素と, 0 でない定数倍と順序を除いて比べる. `generators` の列は Rust が見ない.
- `checks.toric_binomials_valid`: toric の生成元の各元が I_A に属すること (2 項式で, A u = A v) を確かめる. I_A を生成することは確かめない. 空の列も通る.
- `checks.generated_by_toric_and_euler`: Macaulay2 の答えを写したものである. 下の確認のとおり, 今の Macaulay2 では常に真になる比較であり, toric の生成元が I_A を生成すること (`gkz.toric_generates_IA`) の根拠にならない.
- `checks.expected_in_ideal`: Macaulay2 の `gkz` が作った左イデアル (Euler 作用素と toric の生成元が生成する) への所属である. それが H_A(β) への所属であるのは, toric の生成元が I_A を生成するとき (`gkz.toric_generates_IA`) だけである. 偽の答えは, その仮定なしには P ∉ H_A(β) を意味しない. 真の答えに `certificate.membership[j]` があれば, 検査器が P を toric の生成元と Euler 作用素の左の組合せとして確かめるので, P ∈ H_A(β) はこの仮定に依らない (#102).

### Macaulay2 の `gkz` と `toricIdealPartials` (Issue #61)

2026-11-07 に, image `algebramesh-agent-macaulay2:dev` の中で次のことを確かめた.

- 版は, Macaulay2 1.24.11 (Debian の macaulay2 1.24.11+ds-5+b1), package HolonomicSystems 1.0 と FourTiTwo 1.0, 4ti2 1.6.10 である.
- driver は HolonomicSystems を読み込む. Dmodules 1.4.1.1 は WeylAlgebras, HolonomicSystems, BernsteinSato を読み込んで公開するだけなので, どちらから呼んでも同じ `gkz` である.
- `gkz(Matrix, List, PolynomialRing)` (`/usr/share/Macaulay2/HolonomicSystems/Dsystems.m2` の 42〜57 行) は, toric ideal を `toricIdealPartials(A, W)` で作り, `eulerOperators(A, b, W)` の Euler 作用素と足した ideal を返す. ideal の和は 2 つの生成元の行列を並べるだけで, 生成元を減らさない. `toricIdealPartials` は, 4ti2 の `markov` を呼ぶ.
- driver は, `gkz(A, beta)` で ideal I を作る. 引数が 2 つの `gkz` (59〜67 行) は, 環 W を作って上の `gkz` を呼ぶ. driver は `toricIdealPartials` をもう一度呼んで toric の生成元を作り, Euler 作用素と合わせて ideal J を作って, `I == J` を `generated_by_toric_and_euler` として返す. 同じ決定的な関数 (4ti2 の `markov`) の 2 度の呼出しなので, この比較は常に真である. Rust が確かめる `toric_generators` も, 2 度目の呼出しの出力である.
- 実験. `toricIdealPartials` を置き換え, twisted cubic の A = [[1,1,1,1],[0,1,2,3]] について 2 項式 ∂_1 ∂_3 - ∂_2^2 だけを返すようにした. 正しい toric ideal の生成元は 3 つである.
  - `gkz` は置き換えた関数を使い, `I == J` は真のままだった.
  - 一方, ∂_2 ∂_4 - ∂_3^2 は I に属さなかった.
  - Rust の検査 (Euler 作用素の一致と, 2 項式が I_A に属すること) も, この返答を通す.
- 結論. `checks.generated_by_toric_and_euler` は, `gkz.toric_generates_IA` の独立な根拠にならない. `gkz.series` の同じ欄 (driver の同じ比較) も同じである. `gkz.toric_generates_IA` の水準は `asserted` のままである.

## 局所データ (`gkz.indicial`, `gkz.series`)

- `checks.exponents_are_roots`: 返した各指数 v について, v の長さが n で A v = β であること, v が `indicial_ideal` の各生成元の零点であることを確かめる.
  - 「根」は, CAS が返した生成元の零点という意味である. 真の indicial ideal の零点であることは, 生成元が indicial ideal を生成すること (`local.indicial_ideal`, `asserted`) による.
  - 検査に落ちた結果は断るので, 届いた結果ではこの欄は常に真である.
- `checks.multiplicities_sum_to_rank`: 返した重複度の和が, CAS が返した holonomic rank に等しいことを確かめる. 真の rank との比較ではない. 届いた結果では常に真である.
  - A が斉次 (H_A(β) が regular holonomic) なら, これは重みが generic であることの必要条件にすぎない.
  - A が斉次でなければ, generic な重みでも和が rank より小さいことがある (例: A = (1 2), β = 1/3, w = (1, 3)). そのときも結果は断られる.
- `parameters`: CAS の答えではなく, Rust が spec の A と β から計算する (`local.parameters`).

## 級数 (`gkz.series`)

- `status` の `verified_to_order`: 級数の形 (`series.shape`), Euler 作用素で厳密に消えること (`series.euler_exact`), CAS の toric の生成元で重み N まで消えること (`series.toric_to_order`) を確かめた印である.
  - 収束や, 重み N を超えて解に延びることを意味しない.
  - CAS の toric の生成元が I_A を生成することを仮定するので, 実効水準は `asserted` である.
- `status` の `not_a_solution`: 級数がどの解の重み N までの打切りでもないことの証拠を, Rust が見つけた印である. 証拠は, toric の生成元 g を当てたときの, 計算した項だけで決まる 0 でない係数である. CAS が返した級数は `evidence_terms` に残り, 検査器が値だけから証拠を見つけ直す ([ADR 0028](adr/0028-gkz-package-and-its-checks.md)).
  - g が I_A に属することは Rust が確かめるので, この印は CAS の答えに頼らない.
  - Γ 級数そのものが解でない (共鳴) のか, CAS が返した係数が誤っているのかは区別しない.
- `missing`: 計算しなかった解の数についての driver の説明である. Rust は, 並べた解の数との和が重複度に等しいことだけを確かめる.
- `canonical`: Rust が他の指数の解を引いて作った標準形であることの印である. 偽は, 標準形がないことを意味しない.
