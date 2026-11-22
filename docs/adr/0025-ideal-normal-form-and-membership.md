# ADR 0025: `ideal.normal_form` と `ideal.membership`

- 状態: 採用 (operation の名前は Issue #67 で決めたもの. 型の名前, JSON の key, 主張 `normal_form.*` と `membership.*` の ID は提案). `certificate: "s_pairs"` と `gb.groebner` の水準は [ADR 0026](0026-s-pair-lcm-representations.md) で改めた
- 日付: 2026-11-09
- 関連: [ロードマップ A3](../roadmap.md), Issue #66, #67, #68, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0022](0022-operation-types-and-signatures.md), [ADR 0023](0023-monomial-orders.md), [ADR 0024](0024-ideal-groebner-basis-and-its-certificate.md), [ADR 0026](0026-s-pair-lcm-representations.md), [主張の一覧](../claims.md)

## 背景

- A3 の残りの 2 つの operation として, 多項式 f の, QQ 上のイデアル I を法とする正規形と, f ∈ I かどうかの判定を足す.
- backend はどちらも I の reduced Groebner basis G を経由して計算する. G とその certificate (C と N) は ADR 0024 の `ideal.groebner_basis` と同じ形で返せる. 足すのは, f を G で割った商と余りの検査である.
- ADR 0019 の区分 (a) のとおり, Rust は割り算 (簡約) をしない. 商と余りは CAS が出し, Rust は f - r = Σ q_k g_k を多項式の積と和で, r が簡約されていることを単項式の整除で確かめる.
- 正規形が一意であることと, 余りが 0 でないことから f ∉ I を導くことは, G が Groebner basis であること (`gb.groebner`) に依る. これは #68 まで `asserted` なので, 2 つの結論は実効水準で `asserted` にとどまる. この依存を `depends_on` で記録する. #68 (ADR 0026) からは, `s_pairs` を求めれば `gb.groebner` が `checked` になり, 2 つの結論も実効水準で `checked` になる.

## 決定

**operation と引数.**
- 名前は `ideal.normal_form` と `ideal.membership` である. 引数はどちらも `{"ideal": <ideal>, "polynomial": <polynomial>, "certificate": "representation" | "s_pairs" | "none"}` で, `certificate` の既定は `representation` である.
- f は I の環で読む. 項順序は ADR 0024 と同じくイデアルの環の順序である. 記述子は, どちらも `order_dependent: true`, `deterministic: true` とする. 余りと certificate が順序に依存するからである (`member` 自体は順序に依存しない).
- 次のものは, ADR 0024 と同じく backend の parse (`IdealRequest::parse`) で `INVALID_ARGUMENTS` として断り, CAS は起動しない.
  - ZZ の環, 変数のない環, `certificate: "s_pairs"` (#68 まで. #68 (ADR 0026) で受けるようにし, certificate は S 対の lcm representation (`s_pairs`) を足して返す. `member: true` の certificate は係数だけのままである).
  - 生成元と f を合わせた s + 1 個の多項式の数と変数の数の積が `MAX_GENERATORS_TIMES_VARIABLES` (2^20) を超えるジョブ.
  - `ideal.groebner_basis` は, これまでどおり `polynomial` の key を断る.

**数学的な根拠.**
- 環を QQ[x_1, ..., x_n], 順序を >, F = (f_1, ..., f_s), I = (F), G = (g_1, ..., g_t) を I の > についての reduced Groebner basis とする.
- f の I を法とする正規形は, f - r ∈ I で, どの項も I の先頭単項式のイデアル ⟨lm(I)⟩ に属さない多項式 r であり, ただ 1 つある (Cox, Little and O'Shea, *Ideals, Varieties, and Algorithms*, 第 2 章 §6, Proposition 1. 番号は版によって異なりうるので要確認).
- f ∈ I であることと, 正規形が 0 であることは同値である (同 §6, Corollary 2. 要確認).
- f ∉ I の証明は (G, N, q, r) で足りる. F = G N から I ⊆ (G) である. f - r = Σ q_k g_k ∈ (G) である. r ≠ 0 で, r のどの項もどの lm(g_k) でも割り切れない. G が (G) の Groebner basis なら, (G) の 0 でない元の先頭単項式はある lm(g_k) で割り切れるので, r ∉ (G) であり, f ∉ (G) ⊇ I である. ここで (G) ⊆ I (C) は使わない.
- 正規形の一意性には, (G) = I (C と N) と, G が (G) の Groebner basis であることの両方が要る.

**値.**
- `ideal.normal_form` の値 (型 `normal_form`) は次のとおりである.

  ```json
  {"type": "normal_form", "remainder": r, "basis": [g_1, ..., g_t],
   "certificate": {"basis_in_generators": C, "generators_in_basis": N, "quotients": [q_1, ..., q_t]}}
  ```

  G は ADR 0024 の値と同じ条件を満たす. C と N も ADR 0024 と同じである. 商はちょうど t 個で, f - r = Σ_k q_k g_k である.
- `ideal.membership` の値 (型 `ideal_membership`) は次のどちらかである.

  ```json
  {"type": "ideal_membership", "member": true, "certificate": {"coefficients": [c_1, ..., c_s]}}
  {"type": "ideal_membership", "member": false,
   "certificate": {"basis": [g_1, ..., g_t], "generators_in_basis": N, "quotients": [q_1, ..., q_t], "remainder": r}}
  ```

  `member` が真なら f = Σ_i c_i f_i で, 係数はちょうど s 個である. 偽なら, G, N, 商, 余りを返し, C は含めない. したがって, (G) ⊆ I と, r が f の I を法とする正規形であることは主張しない (`docs/claims.md` の not_claimed).
- `member` と違う種類の certificate は, `IdealRequest::value` と検査器の両方で断る.
- `certificate: "none"` のときは `certificate` の key を置かない. backend が certificate を省いた値, `omitted` の印を付けた値, 求めていない certificate を持つ値は断る. certificate のせいで値が結果の上限を超えたときは, ADR 0024 と同じく, 検査器が確かめた後で `{"omitted": "certificate_too_large"}` に置き換える.
- I = 0 (0 の生成元だけ) なら G は空で, r = f, 商はない. I が全体なら G = (1), r = 0 である.

**主張.**

| 主張 | 載せる結果 | 命題 | 方法 | 水準 |
|---|---|---|---|---|
| `gb.reduced_form` | normal_form, membership (偽で certificate あり) | ADR 0024 と同じ (G について) | `self_certifying` | `checked`. normal_form では G が値にあるので, certificate を省いても `checked` |
| `gb.contained` | normal_form | (G) ⊆ I. G = F C | `certificate` | `checked`. certificate を省けば `asserted` |
| `gb.contains` | normal_form, membership (偽で certificate あり) | I ⊆ (G). F = G N | `certificate` | `checked`. certificate を省けば `asserted` |
| `gb.groebner` | normal_form, membership (偽で certificate あり) | ADR 0024 と同じ | backend の答え. `s_pairs` を求めたときは `certificate` (ADR 0026) | `asserted`. `s_pairs` を求めたときは `checked` (#68) |
| `normal_form.reduced` | normal_form, membership (偽で certificate あり) | f - r = Σ q_k g_k で, r のどの項もどの lm(g_k) でも割り切れない | `certificate` | `checked`. certificate を省けば `asserted` |
| `normal_form.unique` | normal_form | r は f の I を法とする > についての正規形である | `theorem` (CLO 第 2 章 §6 Prop. 1, 要確認) | 自身は `checked`. depends_on は `normal_form.reduced`, `gb.contained`, `gb.contains`, `gb.groebner` で, 実効水準は `s_pairs` を求めたときだけ `checked` (#68 まではいつも `asserted`) |
| `membership.in` | membership (真) | f ∈ I. f = Σ c_i f_i | `certificate` | `checked`. certificate を省けば `asserted` |
| `membership.not_in` | membership (偽) | f ∉ I | `theorem` (CLO 第 2 章 §6 Cor. 2, 要確認, と上の証明). r ≠ 0 は値から直接確かめる | 自身は `checked`. depends_on は `normal_form.reduced`, `gb.contains`, `gb.groebner` で, 実効水準は `s_pairs` を求めたときだけ `checked` (#68 まではいつも `asserted`). certificate を省けば `asserted` で depends_on を持たない |

- 主張の命題と 10 項目は `docs/claims.md` の `## ideal.normal_form` と `## ideal.membership` に書いた. 同じ主張 ID (`gb.*`, `normal_form.reduced`) を, それを載せる operation の節ごとに置けるように, 一覧の規則を変えた (§書き方). 前提と実効水準は節ごとに読む.
- 検査器は, データが誤っていれば, certificate の求めによらず結果を断る (`UNSUPPORTED_RESULT`). r の項が lm(g_k) で割り切れることは値だけで決まり安いので, certificate を省いた `ideal.normal_form` でも確かめる. `member: false` で r = 0 の値, 形の合わない行列, 個数の合わない商と係数も断る.
- `certificate: "none"` の `ideal.normal_form` では, `gb.reduced_form` と r の簡約性を確かめ, `gb.contained`, `gb.contains`, `normal_form.reduced` を理由とともに `asserted` にする. `normal_form.unique` は自身の水準と depends_on を保つ.
- `member` が偽の値は G と r を certificate の中に持つ. そのため, certificate を省いた値には `gb.*` と `normal_form.*` の主張を載せず, `membership.not_in` だけを理由とともに `asserted` で載せる. r ≠ 0 も確かめられない.

**Issue #67 の本文からの変更.**
- Issue #67 (と roadmap A3 の表) は, `membership.not_in` の前提に `gb.contained` を挙げ, certificate に G, C, N を含めていた.
- 上の証明のとおり, f ∉ I には I ⊆ (G) (N) だけが要り, (G) ⊆ I (C) は要らない. そこで `membership.not_in` の depends_on から `gb.contained` を外し, `member: false` の certificate に C を含めない. C は s × t の多項式の行列で, 大きくなりうるからでもある.
- `depends_on` は「certificate を含む値のとき」の条件付きの前提として一覧に書く. certificate を省いた値の `membership.not_in` は depends_on を持たないからである.
- roadmap A3 の表の `membership.not_in` の certificate にある S 対の h_k は, #68 の `s_pairs` で足す. #68 (ADR 0026) で, `s_pairs` を求めた `member: false` の certificate に足した.

**driver との契約.**
- backend の Rust のコードは, ADR 0024 の要求に `"polynomial": [term, ...]` (項は生成元と同じ並び) を足して driver に送る. `certificate` は `"representation"` か `"none"` である. #68 からは `"s_pairs"` もあり, 応答の certificate (`member: false` の所属では偽の certificate だけ) に `s_pairs` を足す (ADR 0026).
- 応答は次のとおりである. DM は ADR 0024 の driver の行列 (`rows` と, 列ごとの `row` と `terms` の組の列) である.
  - normal_form: `{"basis": [[term]...], "remainder": [term...], "certificate"?: {"basis_in_generators": DM, "generators_in_basis": DM, "quotients": [[term]...]}}`.
  - membership: `{"member": bool, "certificate"?: {"coefficients": [[term]...]} | {"basis": [[term]...], "generators_in_basis": DM, "quotients": [[term]...], "remainder": [term...]}}`.
- `IdealRequest::value` は, 各多項式を job の環で読み直して正規形に書き, 形と個数を確かめ, 知らない key と, 欠けた certificate, 求めていない certificate, driver が省いた certificate, `member` と合わない種類の certificate を断る. データが正しいかどうかは検査器が決める.
- Macaulay2 は, `groebnerRequest` と同じ G を H = `forceGB` G として, r = f % H, q = f // H で商と余りを出す. 係数は C と同じく grevlex の写しで持ち上げる. OSCAR は `reduce_with_quotients(f, G; ordering = o, complete_reduction = true)` で商と余りを, `lift_to_generators` で係数と C を出す. どちらの driver も, 余りの先頭項だけでなくすべての項が簡約されることと, 商の形を, 実装の時点で image の中で確かめた (driver の注釈). 検査器も毎回確かめる.

**型の名前.** ADR 0022 の一覧に, 次の 2 つを足す.

| 名前 | 表すもの |
|---|---|
| `normal_form` | `ideal.normal_form` の値 |
| `ideal_membership` | `ideal.membership` の値 |

## 結果

- `normal_form.unique` と `membership.not_in` は, 自身の水準が `checked` でも, 実効水準は #68 まで `asserted` だった. 利用者が実効水準で `checked` の主張だけを使う限り, r が正規形であることも, f ∉ I であることも保証されなかった. #68 (ADR 0026) からは, `s_pairs` を求めれば実効水準も `checked` である. `representation` を求めたときは, 今も実効水準は `asserted` である. `normal_form.reduced` (f - r ∈ (G) と r の簡約性) と `membership.in` は, 今でも `checked` である.
- `membership.in` は G を使わずに f = Σ c_i f_i だけで確かめるので, `gb.*` の主張に依らない.
- f ∉ I の certificate は G, N, 商, 余りを含み, f ∈ I の係数よりはるかに大きくなりうる. 上限を超えれば省かれ, `membership.not_in` は depends_on のない `asserted` になる.
- 検査の時間は, F C, G N, G q の積で決まる. 積は割り込みの checkpoint で止まり, r の項の整除の検査も 256 回ごとに checkpoint を呼ぶ.

## 確認

- `crates/algebramesh-ops/src/ideal.rs` と `lib.rs` の単体試験: 2 つの operation の引数の parse と断る規則, 記述子, driver への要求と応答の読み方, `member` と合わない certificate の拒否.
- `crates/algebramesh-verify/tests/ideal_reduction.rs`: twisted cubic の正規形と所属, 0 のイデアルと全体のイデアル, 各主張の変異の試験 (商, 余り, 係数, N, C の変更, 簡約されていない余り, 0 の余りで `member: false`, 種類の違う certificate), `normal_form.unique` の実効水準が 4 つの前提の最も弱いものであること, certificate を省いたときと上限を超えたとき, 期限.
- `crates/algebramesh-verify/tests/claims.rs`: 主張の一覧の規則と, 2 つの operation の主張の水準.
- `crates/algebramesh-verify/src/catalogue.rs` の単体試験 `a_claim_may_be_listed_under_several_operations`: 同じ主張 ID を operation ごとに置けて, 前提と実効水準を節ごとに読むこと.
- 回帰試験 (`tests/regression/cases/ideal_normal_form_*.json`, `ideal_membership_*.json`): f ∈ I と f ∉ I (twisted cubic), certificate を省いた所属, 同じ f の lex と grevlex での正規形 (余りが違う), 64 bit を超える係数の正規形, 全体のイデアルの正規形, 0 のイデアルの所属. 期待する値は手で計算し, driver を通さずに Macaulay2 1.24.11 と OSCAR を image の中で直接動かして一致を確かめた. `run.py` は値と, 主張の水準と実効水準を確かめる.
- driver の応答の照合 (Macaulay2 と OSCAR) は, backend の側の試験 (`testdata/ideal.json`) で行う.
