# ADR 0027: syzygy, free resolution, Hilbert 級数

- 状態: 採用 (operation の名前と主張の ID はロードマップ A4 と Issue #97, #98 で提案したもの. 型の名前と JSON の key は提案)
- 日付: 2026-11-22
- 関連: [ロードマップ A4](../roadmap.md), Issue #97, #98, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0022](0022-operation-types-and-signatures.md), [ADR 0024](0024-ideal-groebner-basis-and-its-certificate.md), [主張の一覧](../claims.md)

## 背景

- A3 で, QQ 上の多項式イデアルの Groebner basis, 正規形, 所属の判定を certificate 付きで扱えるようになった. 加群と free resolution はまだない. Betti 表は, 代数幾何と組合せ論の package (K3, K4) の前提である.
- ADR 0019 の区分 (a) のとおり, Rust は syzygy も resolution も計算しない. CAS の答えを, 多項式の行列の積, 項の次数, 整数の演算だけで確かめる.
- 積で確かめられるのは, 値が複体であること (d d = 0), d_1 の像が I であること (両向きの表示), syzygy が F を消すこと (F S = 0) である. 完全性 (ker d_i ⊆ im d_{i+1}) と, syzygy が Syz(F) を生成することは, 積だけでは確かめられない. Syz(F) の Groebner basis とその S 対の表示を certificate にすれば確かめられるが, 大きくなるので後に回す (ロードマップ V7).
- 極小性と Betti 数は, 次数付けの下でだけ不変量として定まる. 斉次でない生成元のイデアルでは, 極小な free resolution も Betti 数も定まらない.

## 決定

**operation と引数.**

| operation | 引数 | 値の型 |
|---|---|---|
| `module.syzygies` | `{"module": <submodule>}` | `syzygies` |
| `complex.free_resolution` | `{"ideal": <ideal>, "certificate": "include" \| "none"}` (既定 `include`) | `free_resolution` |
| `ideal.hilbert_series` | `{"ideal": <ideal>, "betti_table": bool}` (既定 false) | `hilbert_series` |

- 部分加群 (型 `submodule`) は `{"type": "submodule", "ring": R, "rank": m, "generators": <m 行 s 列の polynomial_matrix>}` である. 生成元は列である (ロードマップ Q12). OSCAR の加群は行で扱うので, driver で転置する.
- 3 つとも, 係数域は QQ だけで, 変数のない環も断る. `ideal.hilbert_series` は, 0 でない生成元に斉次でないものがあれば断る. どれも backend の parse (`ModuleRequest::parse`) で `INVALID_ARGUMENTS` とし, CAS は起動しない. 行列の成分の数と変数の数の積は, ADR 0024 と同じ `MAX_GENERATORS_TIMES_VARIABLES` 以下とする.
- 記述子は 3 つとも `order_dependent: false`, `deterministic: true` である. どの値も, 生成元の選び方を除けば項順序に依らない. 検査も順序を使わない.
- 次数は, 各変数の次数を 1 とする標準の次数付けで測る. 重みのある次数付けは扱わない.

**値.**
- `module.syzygies`: `{"type": "syzygies", "syzygies": S}`. S は s 行 r 列で, F S = 0 である.
- `complex.free_resolution`: R/I の free resolution F_0 = R ← F_1 ← ... ← F_k を, 微分の行列で返す.

  ```json
  {"type": "free_resolution", "differentials": [d_1, ..., d_k],
   "degrees": [[0], [deg_1(1), ...], ...], "betti": [{"i": 0, "j": 0, "beta": 1}, ...],
   "certificate": {"differential_in_generators": C, "generators_in_differential": N}}
  ```

  - d_i は n_{i-1} 行 n_i 列で, n_0 = 1 である. I = 0 なら `differentials` は空である.
  - `degrees` は, I の 0 でない生成元がどれも斉次のときだけ置き, そうでなければ置かない. F_0 の次数は 0 である.
  - `betti` は, `degrees` があって, どの d_i にも 0 でない定数の成分がないときだけ置く. backend の Rust のコードが `degrees` から数える (driver は送らない). (i, j) の順に並べ, 0 は載せない.
  - certificate は C (s 行 n_1 列, d_1 = F C) と N (n_1 行 s 列, F = d_1 N) である. k = 0 のときは d_1 を 1 行 0 列の行列と読む. 0 の生成元も位置を保って数える.
- `ideal.hilbert_series`: `{"type": "hilbert_series", "numerator": ["1", "2"], "denominator_exponent": 2, "betti"?: [...]}`. R/I の Hilbert 級数 H(t) = P(t) / (1 - t)^d の, P の整数係数 (次数 0 から, 末尾の 0 なし) と d である. backend の Rust のコードが, P(1) = 0 である限り 1 - t で割って約分する. `betti` は `betti_table` が真のときだけ置く.

**主張.**

| 主張 | 命題 | 方法 | 水準 |
|---|---|---|---|
| `syz.annihilates` | F S = 0 | `self_certifying` | `checked` |
| `syz.generates` | S の列が Syz(F) を生成する | backend の答え | `asserted` |
| `res.complex` | d_i d_{i+1} = 0 | `self_certifying` | `checked` |
| `res.image` | im d_1 = I | `certificate` (d_1 = F C, F = d_1 N) | `checked`. certificate を省けば `asserted` |
| `res.input_homogeneous` | I の 0 でない生成元はどれも斉次 | `recomputation` | `checked` |
| `res.differentials_homogeneous` | 値の次数で, 各 d_i は次数 0 の斉次写像 | `self_certifying` | `checked` |
| `res.entries_in_maximal_ideal` | 0 でない定数の成分がない | `self_certifying` | `checked` |
| `res.exact` | F_1, ..., F_k で完全 | backend の答え | `asserted` |
| `res.minimal` | 極小な graded free resolution | `theorem` (上の 4 つの主張と `res.exact` から) | 自身は `checked`. 実効水準は `asserted` |
| `res.betti` | `betti` は R/I の graded Betti 数 | `theorem` (`res.minimal` から) | 自身は `checked`. 実効水準は `asserted` |
| `hilbert.series` | P / (1 - t)^d は R/I の Hilbert 級数 | backend の答え. Betti 数を求めたときは `consistency` | `asserted` |

- 主張の組は入力と値によって変わる.
  - 斉次でない入力: `res.complex`, `res.image`, `res.exact` だけを載せる. 値が `degrees` か `betti` を持てば断る.
  - 斉次な入力で, 0 でない定数の成分がある値: `res.entries_in_maximal_ideal`, `res.minimal`, `res.betti` を載せない. 値が `betti` を持てば断る.
  - 載せない主張は, 結果の `guarantee.not_claimed` に主張の ID で入れる. このため `OperationVerifier` に `not_claimed_for(spec, value)` を足した. 既定は, 入力に依らない `not_claimed()` を返す.
- `res.betti` を載せる値では, `betti` が `degrees` から数えたものに等しいことを確かめ, 違えば断る.
- `hilbert.series` は, `betti_table` が真なら Σ_{i,j} (-1)^i β_{i,j} t^j と P(t) (1 - t)^{n-d} を整数係数の多項式として比べ, 違えば断る. 分母を払って積で比べるので, 約分した形でも約分していない形でも比べられる. Betti 数も同じ backend の答えなので, 照合 (`cross_checked`) ではなく, 水準は `asserted` のまま method を `consistency` とする. d > n の値と, 係数が整数でない値は断る.
- 命題と 10 項目は `docs/claims.md` の 3 つの節に書いた. 極小性の定理の出典 (Eisenbud §20.1) は要確認である.

**0 のイデアルと全体のイデアル.**
- 0 のイデアル (生成元がないか, 0 だけ) の resolution は, 微分のない F_0 = R である. `degrees` は `[[0]]`, `betti` は β_{0,0} = 1 である. C は s 行 0 列, N は 0 行 s 列である.
- 全体のイデアルの resolution は R ← R で, d_1 は 0 でない定数を成分に持つ. 斉次な入力 (たとえば生成元 1) でも極小ではないので, Betti 数を返さず, `res.minimal` と `res.betti` を `not_claimed` に入れる (R/I = 0 の極小な resolution は 0 の複体である).

**driver との契約.**
- backend の Rust のコードは, ADR 0024 の項の並び (`[coefficient, exponents]`) で次の要求を driver に送る.
  - syzygies: `{"rows": m, "columns": [[{"row": i, "terms": [...]}, ...], ...]}`.
  - resolution: `{"generators": [[term]...], "homogeneous": bool, "certificate": "include" | "none"}`.
  - Hilbert 級数: `{"generators": [[term]...], "betti_table": bool}`.
- 応答は次のとおりである. DM は ADR 0024 の driver の行列である.
  - syzygies: `{"syzygies": DM}`.
  - resolution: `{"differentials": [DM, ...], "degrees"?: [[int]...], "certificate"?: {"differential_in_generators": DM, "generators_in_differential": DM}}`.
  - Hilbert 級数: `{"numerator": ["int", ...], "denominator_exponent": int, "betti"?: [[i, j, beta], ...]}`.
- driver は 0 の生成元を除いてから CAS に渡し, certificate の C には 0 の行を, N には 0 の列を, 元の位置に戻す. OSCAR の `free_resolution` は, 0 の生成元を含む加群で, 完全でない複体 (R ← R, 0 の写像) を返すからである.
- `ModuleRequest::value` は, 各多項式を job の環で読み直して正規形に書き, 行列の形, 次数の形, 次数の有無 (斉次かどうかと合うか), certificate の有無を確かめる. 値が正しいかどうかは検査器が決める.
- Macaulay2 は `syz` と `res` (`C.dd_i`, `degrees C_i`) を使い, C と N は ADR 0024 の C と同じく grevlex の写しで `//` によって持ち上げる. Hilbert 級数は `poincare comodule I` (約分する前の `hilbertSeries` の分子) を (1 - t)^n の上に返し, backend が約分する. Betti 数は `res` の次数から数える.
- OSCAR は `kernel` (syzygy), `free_resolution` (斉次なら標準の次数付けで `algorithm = :mres`, 斉次でなければ次数のない環で既定の algorithm), `hilbert_series_reduced` を使い, C と N は ADR 0024 の `lift_to_generators` で持ち上げる. OSCAR の行列は行ベクトルで写像を表すので, driver で転置する. 全体のイデアルには, `:mres` が R^0 からの 0 の写像を返すので, Macaulay2 と同じ R ← R, d_1 = (1) を返す. 0 のイデアルの `hilbert_series_reduced` は分母を因子の積で返すので, 展開してから (1 - t)^d であることを確かめる.
- どちらの driver の答えも, 実装の時点で image の中で確かめた (driver の注釈). twisted cubic と (x, y, z)^2 で, 次数, Betti 数, Hilbert 級数が 2 つの backend で一致する (backend の試験 `the_betti_tables_agree_with_oscar`).

**型の名前.** ADR 0022 の一覧に, 次のものを足す.

| 名前 | 表すもの |
|---|---|
| `submodule` | 自由加群の部分加群. 生成元は行列の列 |
| `syzygies` | `module.syzygies` の値 |
| `free_resolution` | `complex.free_resolution` の値 |
| `hilbert_series` | `ideal.hilbert_series` の値 |
| `boolean` | 真偽値の引数 (`betti_table`) |

## 結果

- 研究者は Betti 表を得られる. ただし実効水準は `asserted` である. `res.exact` を上げない限り, 極小性と Betti 数は CAS の答えを信じることになる. 値が複体であること, 像が I であること, 次数付けが合うこと, 成分が極大イデアルに入ることは, 毎回 `checked` である.
- 斉次でない入力では, 極小性と Betti 数を主張しない. 結果の `not_claimed` で, 何を主張しないかが分かる.
- `hilbert.series` は Betti 数との整合を確かめても `asserted` である. 同じ CAS の写し間違いと約分の誤りは見つかるが, CAS の計算の誤りは見つからない.
- 検査の時間は, 隣り合う微分の積と, F C, d_1 N の積で決まる. 積は割り込みの checkpoint で止まる.

## 確認

- `crates/algebramesh-ops/src/module.rs` の単体試験: 引数の parse, 応答の読み方と断る規則, 約分.
- `crates/algebramesh-verify/tests/module.rs`: twisted cubic の syzygy, resolution (β_{0,0} = 1, β_{1,2} = 3, β_{2,3} = 2), Hilbert 級数 (1 + 2t)/(1 - t)^2 と約分していない形, 各 `checked` の主張の変異の試験, 斉次でない入力, 定数の成分がある resolution, 0 のイデアルと全体のイデアル.
- `crates/algebramesh-verify/tests/claims.rs`: 主張の一覧の規則と, 3 つの operation の主張.
- backend の側の試験と回帰試験 (`tests/regression/cases/`): Macaulay2 と OSCAR の Betti 表が一致すること.
