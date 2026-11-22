# ADR 0024: `ideal.groebner_basis` と certificate

- 状態: 採用 (operation, 引数, 主張の名前は Issue #66 で決めたもの. 型の名前は提案). `certificate: "s_pairs"` と `gb.groebner` の水準は [ADR 0026](0026-s-pair-lcm-representations.md) で改めた
- 日付: 2026-11-08
- 関連: [ロードマップ A3](../roadmap.md), Issue #66, #68, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0022](0022-operation-types-and-signatures.md), [ADR 0023](0023-monomial-orders.md), [ADR 0026](0026-s-pair-lcm-representations.md), [主張の一覧](../claims.md)

## 背景

- A3 の最初の operation として, QQ 上の多項式イデアルの reduced Groebner basis を求める. 順序に依存する operation はこれが最初である.
- ADR 0019 の区分 (a) では, Rust は環の演算と単項式の比較と整除だけで certificate を確かめる. 割り算 (簡約) をしないので, 値と certificate の形を, それだけで確かめられるように決める必要がある.
- ADR 0018 の語彙 (`checked`, `asserted`, certificate を省く規則) と, 行列の operation の約束 (`certificate: "none"` と `"omitted": "certificate_too_large"`) に合わせる.

## 決定

**operation と引数.**
- 名前は `ideal.groebner_basis` である. 引数は `{"ideal": <ideal>, "certificate": "representation" | "s_pairs" | "none"}` で, `certificate` の既定は `representation` である.
- 項順序は, イデアルの環の順序 (ADR 0023) である. 引数で別に渡す口は置かない. 記述子は `order_dependent: true`, `deterministic: true` とする.
- 次のものは, backend の parse で `INVALID_ARGUMENTS` として断り, CAS は起動しない. 検査器も, 同じ関数 (`IdealRequest::parse`) で spec を読み, 同じ規則で断る.
  - 係数域が ZZ の環 (「the ideal.* operations are over QQ only」).
  - 変数のない環.
  - `certificate: "s_pairs"` (「s_pairs certificates are not supported yet」). #68 まで断っていた. #68 (ADR 0026) で受けるようにし, certificate は S 対の lcm representation (`s_pairs`) を足して返す.
  - 生成元の数と変数の数の積が `MAX_GENERATORS_TIMES_VARIABLES` (2^20) を超えるイデアル. 各生成元が環の変数の名前を持つので, 数 byte の 0 の生成元でも変数の数だけ文字列を作る. 上限がないと, 8 MiB の spec が数百 GB のメモリを求めうる.

**値.**
- `{"type": "groebner_basis", "basis": [...], "certificate": {"basis_in_generators": C, "generators_in_basis": N}}` である.
- `basis` は I = (f_1, ..., f_s) の reduced Groebner basis G = (g_1, ..., g_t) である. 0 の元を含まず, 各元は monic で, 互いに簡約されていて, 先頭単項式の狭義の昇順に並ぶ. reduced Groebner basis は順序ごとに一意なので (Cox, Little and O'Shea, *Ideals, Varieties, and Algorithms*, 第 2 章 §7. 定理の番号は版によって異なるので要確認), 並びまで決めると値は一意になる.
- I = 0 なら G は空で, I が全体なら G = (1) である. 0 の生成元も位置を保って数える.
- C は s 行 t 列で g_j = Σ_i f_i C_ij, N は t 行 s 列で f_i = Σ_k g_k N_ki である. どちらも行列の operation と同じ `polynomial_matrix` の JSON で書く.
- `certificate: "none"` のときは `certificate` の key を置かない. 行列の operation と同じく, backend が certificate を省いたり `omitted` の印を付けたりした値は断る. certificate のせいで値が結果の上限を超えたときは, 検査器が確かめた後で `{"omitted": "certificate_too_large"}` に置き換える.

**主張.**

| 主張 | 命題 | 方法 | 水準 |
|---|---|---|---|
| `gb.reduced_form` | 0 の元がなく, monic で, 先頭単項式が狭義に増え, どの項も他の元の先頭単項式で割り切れない | `self_certifying` | `checked` (certificate を省いても) |
| `gb.contained` | (G) ⊆ I. G = F C | `certificate` | `checked` |
| `gb.contains` | I ⊆ (G). F = G N | `certificate` | `checked` |
| `gb.groebner` | G は (G) の, 宣言した順序の Groebner basis である | backend の答え. `s_pairs` を求めたときは `certificate` (ADR 0026) | `asserted`. `s_pairs` を求めたときは `checked` (#68) |

- `certificate: "none"` のときと, certificate を省いたときは, `gb.contained` と `gb.contains` を, その理由を `reason` にして `asserted` にする.
- 検査器は, F と順序を spec から取り, CAS が写し返した入力は使わない.
- 両方向の certificate を求めるのは, 片方だけでは別のイデアルの basis が通るからである. C だけなら I の真の部分イデアルの basis が, N だけなら I を含む大きなイデアルの basis が通る.

**順序の取り違えについての限界.**
- representation の検査と `gb.reduced_form` だけでは, driver が宣言と別の順序の basis を返したことを常には見つけられない.
- 断られる例と通る例を `docs/claims.md` に書き, 試験 `an_order_mix_up_that_is_not_monic_is_refused` と `an_order_mix_up_that_is_monic_and_interreduced_passes` で確かめる. 通る例は, grevlex の reduced basis が lex でも monic で互いに簡約されている場合で, lex の昇順に並べ替えると 3 つの主張が `checked` で通る.
- この取り違えを見つけるのは `gb.groebner` の検査 (#68 の `s_pairs`) である. それまでは, driver が ADR 0023 の順序の項目で環を作ることで防ぐ. `gb.groebner` を `asserted` にとどめるのは, このためでもある.
- #68 (ADR 0026) で, 通る例は `s_pairs` を求めると `gb.groebner` で断られるようになった (試験 `an_order_mix_up_is_refused_by_its_s_pairs`). `representation` だけでは, 今も通る.

**driver との契約.**
- backend の Rust のコードが, 順序を ADR 0023 の `order_items` で CAS の項目に変え, 生成元 (係数は文字列, 項は指数の lex の降順), 項目, `certificate` (`"representation"` か `"none"`. #68 からは `"s_pairs"` も. ADR 0026) を driver に送る.
- 応答は `basis` (項の列の列) と, `certificate` (`rows` と, 列ごとの `row` と `terms` の組の列) である. `IdealRequest::value` が, 環で読み直して正規形に書き直し, 行列の形を確かめる. 元の並べ替え, 正規化, 0 の元の除去はしない. それらは検査器が断る.
- driver の誤りは, 他の operation と同じく `ALGEBRAMESH_ERROR` の 1 行で返す.

**型の名前.** ADR 0022 の一覧に, 次の 3 つを足す.

| 名前 | 表すもの |
|---|---|
| `ideal` | 環と生成元の列で与えたイデアル |
| `ideal_certificate_request` | `ideal.*` の `certificate` 引数 (`representation`, `s_pairs`, `none`) |
| `groebner_basis` | `ideal.groebner_basis` の値 |

**多項式行列の積.**
- `PolynomialMatrix::mul_with` を書き換えた. 各列の係数の分母の最小公倍数が, 最も長い分母より 64 bit 以内に収まり, かつ整数に直したどの項も, 元の係数 (分子と分母) の bit 数の 2 倍と 64 bit の和を超えないときは, 列を整数に直して整数で和を取り, 最後に 1 回だけ割る. そうでないときは, 約分しない分数で和を取り, 分母が大きくなりすぎたときだけ約分する.
- 2 つ目の条件は, 1 つの長い分母が同じ列の他の項をすべてその長さに伸ばすのを防ぐ. 整数の項 T 個と 1/2^B の項が 1 つの列では, 整数に直すと T × B bit になり, 和にも同じだけ要る (T = 10^5, B = 3 × 10^6 で約 37 GB ずつ). 条件を項ごとに置くのは, 列全体の和で比べると, 左の因子の 1 つの列にある長い分母が, 右の因子の別の行の項を伸ばし, その項との積がすべて長くなるのを許すからである. 項ごとなら, 整数での項の積の長さは, 分数での積の長さの 2 倍と 128 bit を超えない.
- 最小公倍数は, 分母がすでに割り切るときは取らない. 長い最小公倍数との gcd は 1 bit ごとに 1 歩かかる (Issue #91) が, 短い分母での割り算は 1 語ごとに 1 歩で済む.
- 割り込みの checkpoint は, 各成分の積の前と, 項の積 256 回ごとに置く. 整数に直す前の最小公倍数の計算と, 整数に直す計算では項ごとに, 積の後の約分では項 256 個ごとに置く. ADR 0016 の期限は変わらない.
- 1 行 6 個の多項式と 6 行 40 列の行列 (4 変数, 各成分 40 項, 20 桁の分数, 項の積 384,000 回) の積の release build での時間は次のとおりだった.

  | 分母 | 変更前 | 変更後 |
  |---|---|---|
  | 無作為な 20 桁 | 4.48–4.66 秒 | 1.31–1.36 秒 |
  | 2^20 3^12 5^8 7^6 の約数 | 3.56 秒 | 0.78–0.81 秒 |

- 常に整数に直す方法は, 無作為な分母では最小公倍数が膨らみ 29 秒かかったので採らなかった.

**Macaulay2 の重み付きの次数.**
- ADR 0023 で残した, 単項式の重み付きの次数か全次数が 2^31 - 1 を超える場合は, 入力を前もって調べない.
- 超えるかどうかは計算の途中に現れる単項式で決まり, 入力だけでは判定できない. Macaulay2 は誤りで止まり, 誤った値を返さないので, ジョブは `CasError::Cas` で失敗する (fail-closed).

## 結果

- `gb.groebner` は #68 まで `asserted` だった. 利用者が `checked` の主張だけを使う限り, G が (G) の Groebner basis であることは保証されなかった. #68 (ADR 0026) からは, `s_pairs` を求めれば `checked` である. `representation` と `none` では今も `asserted` である.
- C と N の大きさには上限がない. 結果の上限を超えた certificate は省かれ, `gb.contained` と `gb.contains` は `asserted` になる. 1 本の等式の検査に使うメモリの上限は, まだ置いていない.
- 検査の時間は, 主に F C と G N の積で決まる. 積は割り込みの checkpoint で止まる (試験 `a_long_groebner_check_stops_at_the_deadline`).
- 同じ順序の別の書き方を区別することは, ADR 0023 のとおりである.

## 確認

- `crates/algebramesh-ops/src/ideal.rs` と `lib.rs` の単体試験: 引数の parse と断る規則, 記述子, driver への要求と応答の読み方.
- `crates/algebramesh-verify/tests/ideal.rs`: twisted cubic の basis, 0 のイデアルと全体のイデアル, 各主張の変異の試験, 順序の取り違えの 2 例, 引数の拒否, certificate を省いたときと上限を超えたとき, 期限.
- `crates/algebramesh-verify/tests/claims.rs`: 主張の一覧 (`docs/claims.md`) の規則と, 試験の名前の照合.
- `crates/algebramesh-math-types` の単体試験: 書き換えた積が, 定義どおりの積と一致する. 1 つの長い分母のある列は整数に直さない (`one_long_denominator_does_not_lengthen_the_other_terms`). 積の前後の処理が checkpoint を呼ぶ (`the_work_before_and_after_the_products_calls_the_checkpoint`).
- `crates/algebramesh-ops/src/ideal.rs` の `many_generators_in_many_variables_are_refused`: 生成元の数と変数の数の積の上限.
- driver の照合 (Macaulay2 と OSCAR) は, backend の側の変更で行う.
