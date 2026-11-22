# ADR 0026: S 対の lcm representation による `gb.groebner` の certificate

- 状態: 採用 (certificate の種類 `s_pairs` は Issue #66 で決めたもの. JSON の key `pairs` と `representations` は提案)
- 日付: 2026-11-10
- 関連: [ロードマップ A3 と §1.2](../roadmap.md), Issue #66, #67, #68, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0023](0023-monomial-orders.md), [ADR 0024](0024-ideal-groebner-basis-and-its-certificate.md), [ADR 0025](0025-ideal-normal-form-and-membership.md), [主張の一覧](../claims.md)

## 背景

- ADR 0024 と ADR 0025 では, `gb.groebner` (G は (G) の, 宣言した順序の Groebner basis である) を backend の答えとして `asserted` にとどめた. 引数 `certificate: "s_pairs"` は名前だけ決め, CAS を起動する前に断っていた.
- そのため, `normal_form.unique` (r は正規形である) と `membership.not_in` (f ∉ I) は, 自身の水準が `checked` でも, 実効水準は `asserted` だった. `ideal.groebner_basis` の結果の要約も, 契約を確認しても `verified` にならなかった.
- representation の検査 (C と N) と `gb.reduced_form` だけでは, driver が宣言と別の順序の basis を返したことを常には見つけられない (ADR 0024 の「順序の取り違えについての限界」).
- ADR 0019 の区分 (a) のとおり, 検査器は Groebner basis を計算せず, 割り算 (簡約) もしない. Groebner basis であることの certificate は, 環の演算と単項式の比較だけで確かめられる形に決める必要がある.

## 決定

**数学的な根拠.**
- 環を QQ[x_1, ..., x_n], 順序を > (spec の環の順序), G = (g_1, ..., g_t) を 0 でない多項式の列とする. lm, lc, lt = lc lm は > での先頭単項式, 先頭係数, 先頭項である.
- i < j について L_ij = lcm(lm(g_i), lm(g_j)) とし, S(g_i, g_j) = (L_ij / lt(g_i)) g_i - (L_ij / lt(g_j)) g_j とする.
- S(g_i, g_j) = Σ_k h_k g_k で, h_k g_k ≠ 0 となる各 k について lm(h_k g_k) < L_ij (真に小さい) であるとき, この表示を S(g_i, g_j) の lcm representation と呼ぶ (Cox, Little and O'Shea, *Ideals, Varieties, and Algorithms*, 第 2 章 §9).
- 定理: G が (G) の Groebner basis であることは, i < j の全ての S(g_i, g_j) が lcm representation を持つことと同値である (同 §9, 第 4 版の Theorem 6. 番号は版によって異なるので要確認).
- 先頭単項式が互いに素な対は, データなしに lcm representation を持つ (Buchberger の第 1 判定法. 同 §9, 第 4 版の Proposition 4. 要確認). m_i = lm(g_i), c_i = lc(g_i), g_i = c_i m_i + p_i とすると, m_i と m_j が互いに素なら L_ij = m_i m_j で,

  S(g_i, g_j) = (m_j / c_i) g_i - (m_i / c_j) g_j = (p_i g_j - p_j g_i) / (c_i c_j)

  である. 両辺とも m_j p_i / c_i - m_i p_j / c_j に等しい (左辺では m_i m_j の項が, 右辺では p_i p_j の項が打ち消し合う). 右辺の 2 つの積の先頭単項式は lm(p_i) m_j と lm(p_j) m_i で, どちらも m_i m_j より小さい. p_i か p_j が 0 なら, その積はない. 検査器が除くのは, ちょうどこの対である. 除いた対については何も計算しない. 恒等式は `docs/claims.md` にも書き, 利用者が確かめられるようにした.
- lm(h g) = lm(h) lm(g) (単項式の積は指数の和) なので, 不等式は単項式の比較 lm(h_k) + lm(g_k) < L_ij になる.
- S(g_i, g_j) を G で割った商は, lm(q_k g_k) ≤ lm(S(g_i, g_j)) < L_ij を満たすので, lcm representation である. driver はそれを送ってよい. 検査器は割り算をしない.

**certificate.**
- job が `certificate: "s_pairs"` を求めたとき, `ideal.groebner_basis` と `ideal.normal_form` の certificate, および `member: false` の `ideal.membership` の certificate は, これまでの内容 (C と N, 商, 余りなど) に加えて, key `s_pairs` を持つ.

  ```json
  "s_pairs": {"pairs": [[i, j], ...], "representations": H}
  ```

- `pairs` は, 先頭単項式が互いに素でない (ある変数の指数が両方で正である) 対 (i, j) を全て, (i, j) の辞書式の昇順に並べたものである. i と j は 0 から数え, i < j である. P を `pairs` の長さとする. 対がなければ `"pairs": []` で, H は t 行 0 列である.
- `representations` は t 行 P 列の多項式行列 H (`polynomial_matrix`) で, 第 p 列は p 番目の対 (i, j) について S(g_i, g_j) = Σ_k g_k H_kp を満たす.
- `representation` を求めた job の値が `s_pairs` を持てば断る. `s_pairs` を求めた job の値が持たなくても断る. `IdealRequest::value` と検査器の両方で断る.
- `member: true` の `ideal.membership` の certificate は, job が何を求めても係数 `{"coefficients": [...]}` だけである. f ∈ I を示すのに Groebner basis は要らないので, `s_pairs` は当てはまらない.
- `IdealRequest::parse` は `s_pairs` を断らなくなった. `IdealRequest.certificate` は求めた certificate の種類 (`representation`, `s_pairs`, `none`) を持ち, driver と検査器がそれを読む.

**検査.** 検査器 (`algebramesh-verify`) は, G (読んで 0 でないことを確かめた後) と spec の順序と `s_pairs` から, 次の順に確かめる. どれかが成り立たなければ, 結果を断る (`UNSUPPORTED_RESULT`).
1. G の先頭単項式から対の並びを求め, `pairs` と一致する. 欠けた対, 余分な対, 互いに素な対, 重なった対, 順の違いを断る. 並びは前から順に比べ, t^2 個の対の一覧は作らない.
2. H を spec の環で読み, t 行 P 列である.
3. 各対の S(g_i, g_j) を, 単項式と多項式の積と多項式の差だけで作る.
4. G を 1 行の行列として H と掛け, 第 p 列が p 番目の S(g_i, g_j) に等しい (`check_row_times`).
5. 0 でない各 H_kp について, lm(H_kp) + lm(g_k) が spec の順序で L_ij より小さい. 指数が 32 bit に収まらなければ断る.
- 検査器は G に元を足さず, 簡約の相手を選ばず, 割り算をせず, 成功するときも全ての対を調べる. 積は ADR 0024 の checkpoint で止まり, 対の並びの比較と不等式の比較は 256 回ごとに checkpoint を呼ぶ. ADR 0016 の期限は変わらない. 等式と不等式は, 対を並びの順に組に分けて調べる. 組は, その対の |g_i| + |g_j| と H の列の項の数の和が 2^16 (G の項の数がそれより多ければその数) に達したところで閉じる. 同時に持つ S 多項式と G H の列はその組の分だけなので, 応答に並べた対の数が多くてもメモリはそれに比例して増えず, 誤った対はその組で断られ, 後の組の S 多項式は作られない.

**区分 (a) である論拠.**
- [ロードマップ](../roadmap.md) §1.2 の「S 対の検査が Buchberger 法の再実装でない論拠」のとおりである. 検査器が受け取るのは CAS が返した G と H で, 確かめるのは環の演算による等式と単項式の比較による不等式だけである.
- 対は G と順序だけで決まる並びを全て調べ, 第 1 判定法の対だけを除く. Buchberger 法と違い, 0 にならない S 多項式から新しい元を作ることはなく, どの対をどの元で簡約するかも選ばない.
- 費用は, 高々 t(t - 1)/2 個の多項式の等式と, H の 0 でない成分の数の単項式の比較である. どちらも certificate の大きさの多項式で抑えられる.

**主張.**

| 主張 | 載せる結果 | `s_pairs` を求めたとき | `representation` を求めたとき |
|---|---|---|---|
| `gb.groebner` | groebner_basis, normal_form, membership (偽で certificate あり) | `certificate` で `checked` | backend の答えで `asserted` (「the job asked for no certificate of the S-pairs」) |
| `normal_form.unique` | normal_form | 実効水準も `checked` | 実効水準は `asserted` |
| `membership.not_in` | membership (偽) | 実効水準も `checked` | 実効水準は `asserted` |

- `normal_form.unique` と `membership.not_in` の自身の水準と depends_on は ADR 0025 のままである. 前提の `gb.groebner` が `checked` になるので, 実効水準も `checked` になる.
- `certificate: "none"` のときは, ADR 0024 と ADR 0025 のとおり `gb.groebner` を「the job asked for no certificate of the S-pairs」で `asserted` にする. job が `s_pairs` を求め, certificate が結果の上限を超えて省かれたときは, `gb.groebner` を省いた理由 (`certificate_too_large`) で `asserted` にする. `representation` を求めて省かれたときは, 理由はこれまでどおり「the job asked for no certificate of the S-pairs」である.
- `s_pairs` を求めた `ideal.groebner_basis` の値では, 契約の主張がすべて `checked` になる. 利用者が契約を確認すれば, 要約は `verified` になる. 実際の契約はまだ未確認なので, 要約は `partially_verified` のままである.
- 主張の命題, 定理と第 1 判定法の恒等式, 検査の 5 段, 変異の試験の名前は `docs/claims.md` に書いた.

**順序の取り違え.**
- ADR 0024 の通る例 (F = (-xyz + z^2, -xyz - x^2 + xy), lex を宣言したジョブに grevlex の reduced Groebner basis を lex の昇順に並べて返す) は, `representation` では `gb.reduced_form`, `gb.contained`, `gb.contains` が `checked` で通る.
- `s_pairs` では `gb.groebner` で断られる. z (xyz - z^2) - y (yz^3 + xz^2 - yz^2) = -y^2 z^3 + y^2 z^2 - z^3 は I に属し, その lex の先頭単項式 y^2 z^3 は lex の先頭単項式 xz^2, xyz, x^2 のどれでも割り切れない. よってこの基底は lex の Groebner basis でなく, 定理によって lcm representation を持たない S 対がある. driver がどんな `s_pairs` を送っても, 検査のどこかで断られる.
- 試験 `an_order_mix_up_is_refused_by_its_s_pairs` は, 壊れた driver が送りうる 2 つの `s_pairs` を確かめる. grevlex で求めた対と表示を lex の並びに移すと, lex の対 (0, 2) が欠けるので対の並びで断られる. lex の各 S 対を grevlex で割った表示は, 等式は成り立つが, lex の先頭単項式が L_01 に等しい積を含むので不等式で断られる.
- driver は, これまでどおり宣言した順序の項目 (ADR 0023) で環を作り, 対もその順序で求める.

**driver との契約.**
- backend の Rust のコードは, driver に `"certificate": "representation" | "s_pairs" | "none"` を送る.
- 応答は, ADR 0024 と ADR 0025 の `certificate` の中に, `"s_pairs": {"pairs": [[i, j], ...], "representations": DM}` を足したものである. DM は ADR 0024 の driver の行列 (`rows` と, 列ごとの `row` と `terms` の組の列) で, `rows` は t である. 対がなければ `{"pairs": [], "representations": {"rows": t, "columns": []}}` である. `member: false` の所属にだけ置き, `member: true` には置かない.
- `IdealRequest::value` は, H を job の環で読み直し, t 行 P 列であることを確かめる. 対の並びが正しいかどうかは検査器が決める.
- driver は, 検査器と同じ規則 (ある変数の指数が両方で正) で, spec の順序の先頭単項式から対を求める. 各対について, monic の G で S(g_i, g_j) を作り, G で割った商を h_k とし, 余りが 0 であることを driver の中で確かめる (0 でなければ, G は Groebner basis でないので失敗する). Macaulay2 では S // forceGB(G) と S % forceGB(G), OSCAR では `reduce_with_quotients` を使う. 商が lm(h_k g_k) ≤ lm(S) を満たすことは image の中で確かめる (driver の注釈). 満たさない商は, 検査器が不等式で断る.

## 結果

- `s_pairs` を求めれば, `gb.groebner` は `checked` で, `normal_form.unique` と `membership.not_in` は実効水準でも `checked` である. A3 の主張のうち, 実効水準で `asserted` にとどまるのは, `representation` か `none` を求めたときと, certificate を省いたときだけになった.
- H の大きさには上限がない. 対は O(t^2) 個あり, 各列は t 個の多項式である. 結果の上限を超えた certificate は省かれ, `gb.groebner` は `certificate_too_large` で `asserted` になる. 1 本の等式の検査に使うメモリの上限は, ADR 0024 と同じく, まだ置いていない.
- 検査の時間は, 主に G H の積で決まる. 期限で止まる (試験 `a_long_s_pair_check_stops_at_the_deadline`). 試験は期限で止まることを確かめるが, 止まった位置が S 対の積の中であることは, 前の段が構成上安いことに依り, 測ってはいない.
- 既定の certificate は `representation` のままである. `s_pairs` は追加の計算と転送を要するので, 利用者が求めたときだけ返す.
- driver が `s_pairs` を返さないと, `s_pairs` を求めたジョブは `IdealRequest::value` で断られる. CAS を起動する前には断らない.
- Weyl algebra の S 対は, 第 1 判定法が成り立たないので, この検査をそのまま使わない (ロードマップ §1.2).

## 確認

- `crates/algebramesh-ops/src/ideal.rs` の単体試験: `s_pairs` の parse, driver への要求, 求めたときだけ `s_pairs` を受ける規則, JSON の読み書き, normal_form と `member: false` の所属での `s_pairs`.
- `crates/algebramesh-verify/src/certificate.rs` の単体試験: 互いに素な単項式と lcm, 先頭単項式が互いに素でない対の並び.
- `crates/algebramesh-verify/tests/ideal_s_pairs.rs`: twisted cubic と lex の例の basis, 0 のイデアルと全体のイデアルと 1 元の basis (対がない), 1 か所変えた h_k の拒否, 定義どおりの表示 (h_i = L / lt(g_i), h_j = -L / lt(g_j)) が等式を満たすが不等式で断られること, 欠けた対, 互いに素な対, 並べ替えた対, 重なった対の拒否, `s_pairs` が欠けた値と求めていない値の拒否, 指数の溢れ, 順序の取り違え, normal_form と `member: false` の所属, 確認済みの契約の下で `verified` になること (検査器を包む試験用の verifier で), certificate が上限を超えたとき (`ideal.groebner_basis` と `ideal.normal_form`), 期限, 最初の組の誤った対が後の組を作る前に断られること (`a_wrong_s_pair_is_refused_before_the_later_pairs_are_built`).
- `crates/algebramesh-verify/tests/claims.rs`: 3 つの operation の `gb.groebner` が `certificate` で `checked` であること, `normal_form.unique` と `membership.not_in` の水準.
- 回帰試験 (`tests/regression/cases/ideal_*_s_pairs.json`): twisted cubic の grevlex と block order, 小さな例の lex と重み付きの順序, cyclic-4 の grevlex の basis, 小さな例の grevlex の正規形, twisted cubic の外の多項式の所属. 期待する値は, 同じイデアルの既存の例 (#66, #67) のものを使う. `run.py` は値に加え, `gb.groebner` が `checked` であること, `normal_form.unique` と `membership.not_in` の実効水準が `checked` であること, certificate が `s_pairs` を求めたときだけ持つことを確かめる. lex の例は先頭単項式 z^7, y, x が互いに素なので, 対は 0 個である.
- driver の応答の照合 (Macaulay2 と OSCAR) は, backend の側の試験で行う. 順序を取り違えた driver の応答は, `tests/regression` の driver を壊せないので, 各 backend の `testdata/ideal.json` の `mixup_lex_answered_in_grevlex` に記録した. lex のジョブ (`mixup_lex_s_pairs` と同じ引数) を, grevlex の順序の項目で driver に送って得た応答で, `mixup_grevlex_s_pairs` の応答に等しい. この応答が `gb.groebner` で断られることを, 試験 `the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs` (Macaulay2 と OSCAR の両方) で確かめる.
- 利用者に確かめてもらうもの: 定理と第 1 判定法の番号 (Cox, Little and O'Shea 第 2 章 §9), `docs/claims.md` の恒等式.
