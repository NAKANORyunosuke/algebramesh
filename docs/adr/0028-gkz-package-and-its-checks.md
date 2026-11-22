# ADR 0028: GKZ の package と, その検査の場所

- 状態: 採用 (crate, module, 型, feature, 値の key `evidence_terms`, 主張 ID `gkz.membership[j]` の名前は提案)
- 日付: 2026-11-23
- 関連: [ロードマップ V2, K0, §6.1](../roadmap.md), Issue #99, #100, #101, [ADR 0007](0007-gkz-indicial-and-series.md), [ADR 0011](0011-gkz-logarithmic-and-canonical-series.md), [ADR 0015](0015-gkz-local-data-guarantees.md), [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [主張の一覧](../claims.md), [GKZ の package](../packages/gkz.md)

## 背景

- GKZ の 3 つの operation (`gkz.generate`, `gkz.indicial`, `gkz.series`) は, 研究分野の operation として最初に入ったもので, Macaulay2 の backend の中に作られた. backend が driver の答えを読み, 値を組み立て, その場で検算し, 主張を記録していた. 検算 (`euler_operators_match`, `toric_binomials_valid`, `check_exponent`, `check_series`, `normalize`) は backend のコードの中にあった.
- 他の operation (A1 以後) では, backend は値を返すだけで, 主張は agent が `OperationVerifier` で spec と値だけから確かめる (ADR 0018, ADR 0019). GKZ だけが, この分け方の外にあった. 検算が backend の中にあると, 計算と検査が同じコードの同じ呼出しの中にあり (ロードマップ Phase 3 の「呼出しの独立」), 値だけから検査し直すこともできない.
- ロードマップ §6.1 は, 研究分野の operation を package として, 共通の基盤 (server, core, rpc, proto) から切り離す形を定めた. 依存は compile 時の Cargo feature `pkg-<name>` で選ぶ (Q16). GKZ をその最初の例として移す (K0). V2 と K0 は同じ一連の変更で行う.

## 決定

**package の形.**

| 部分 | 置き場所 |
|---|---|
| crate | `crates/algebramesh-gkz`. 既存の数学 (`cone`, `operator`, `series`) に, 値を組み立てる `value` と, 検査器の `verify` を足した. IO は持たない |
| backend の glue | `crates/algebramesh-backend-macaulay2/src/packages/gkz.rs`. feature `pkg-gkz` (backend では既定で外れている) |
| agent | feature `pkg-gkz` (既定で入る). backend の `pkg-gkz` を入れ, `algebramesh_agent::registry()` が `algebramesh_gkz::verify::register` を呼ぶ. CLI の `local` も同じ registry を使う |
| driver | `drivers/macaulay2/gkz.m2`. `algebramesh.m2` が, 隣にあれば読み込む. 読み込んだ file は, operation を `packageOperations` に登録する |
| Python | `algebramesh.gkz` の `generate`, `indicial`, `series`. `Client.gkz`, `gkz_indicial`, `gkz_series` はこれを呼ぶ |
| 文書 | [`docs/packages/gkz.md`](../packages/gkz.md) と, この ADR |

- `pkg-gkz` を外した backend は, GKZ の operation と型を `Capabilities` に載せず, その operation を `UNSUPPORTED_OPERATION` で断る. driver の digest にも `gkz.m2` を含めない. 外した agent は GKZ の検査器を登録しない.
- 引数と結果の型の記述子 (`OperationDescriptor`) は, これまでどおり `algebramesh-ops` に置く. ロードマップ §6.1 は crate に置くとしたが, scheduler が投入を受けるときに記述子を引くので (ADR 0022), 移すと server が package に依存する. package を足しても server を変えない, という §6.1 の受入条件と両立しない. 記述子を登録制にするのは, 2 つ目の package の時点で決める.
- server, core, rpc, proto は変えていない.

**値を組み立てる場所と検査する場所.**
- glue は driver の JSON を, crate の型付きの答え (`LocalAnswer`, `SeriesAnswer`, `ExponentAnswer`, `LogSolutionAnswer`) に変えるだけである. 形の崩れた答えは, ここで `UNSUPPORTED_RESULT` にする.
- crate の `value::generate_value` と `value::local_value` が値を組み立てる. 各級数の状態 (`verified_to_order` か `not_a_solution`) と標準形は, ここで決める. 値の検査の欄 (`checks.*`) は, 互換のために残し, 偽でも断らずに書く.
- crate の `verify` の検査器 (`GenerateVerifier`, `IndicialVerifier`, `SeriesVerifier`) が, spec と値だけから主張を確かめ直す. 主張が成り立たない値と, 検査の欄が真でない値は, `UNSUPPORTED_RESULT` で断る. 主張の ID, 水準, 依存は, これまで backend が記録していたものと同じである. ただし次の 2 点が変わる.
  - `gkz.series` の結果は, `gkz.generate` の toric の主張 3 つと, `gkz.indicial` の局所の主張 7 つも載せる. 級数の主張がそれらに依るからである. 主張の一覧では, `gkz.series` の節にそれらの写しを置いた.
  - `gkz.generate` の所属の判定は, operator ごとに `gkz.membership[j]` (j = 0, 1, ...) の主張とした. 主張の一覧の検査は, `name[k]` の形の ID を `name[j]` の項目として読む (`catalogue::family`).

**値に加えたもの: `evidence_terms`.**
- `not_a_solution` の級数と対数解の項目に, CAS が返した級数を `evidence_terms` として置く. 形は `terms` と同じである.
- これまでの `series.not_a_solution` は, backend がその場で CAS の級数を調べ, 証拠を値に残さなかった. 値だけから確かめ直すには, 証拠が値になければならない.
- 検査器は, `evidence_terms` が `series.shape` と `series.euler_exact` を満たし, 確かめた toric の生成元のどれかで重み N までに 0 でない係数を持つことを確かめる. 解である級数を `not_a_solution` として返した値は断る.
- 既存の値の欄は変えていない. T0 の比較で, GKZ の値の差は `evidence_terms` の追加だけである.

**`normalize` の `solve` は (b) のまま残す.**
- ロードマップ V2 は, 標準形 (ADR 0011) を作る線形方程式の解を CAS に出させ, Rust は代入して確かめる ((a) に移す) とした. 今回は移さず, ADR 0019 の (b) (上限付きの再計算) のまま残した.
- 理由. 標準形は, CAS ではなく Rust が定義したものである (ADR 0011). 係数を CAS に出させるには, driver に同じ正規化を書き直すことになる. 検査器は, 値の標準形の欄を, spec と値から再計算した標準形と比べる (`series.canonical`, `method: recomputation`). 方程式の大きさは指数の数で抑えられている.
- ロードマップ §1.2 と ADR 0019 の表には, この決定を書き足した.

**主張の一覧の走査.**
- 変異の試験の走査 (`crates/algebramesh-verify/tests/claims.rs`) は, `cfg(feature = "pkg-<name>")` で外れる試験も「外されていない」と数える. CI の `cargo test --workspace --all-features` で, package の feature は入るからである.
- 親の file が `cfg` の下で `mod name;` と宣言する module の file は, 外れているものとして扱う. これまでは file の中の属性だけを読み, 親の宣言を読まなかった.

**CI.** job `packages` で, backend と agent を feature なしで clippy にかけ, 試験する. 既存の `rust` の job は全 feature で行う.

## 結果

- GKZ の主張は, 他の operation と同じく, agent が値だけから確かめる. backend の誤りで検査が飛ぶことはない. 記録した値は, 後から検査し直せる (V4).
- Macaulay2 の backend は GKZ を知らずに build できる. 新しい package は, 同じ形 (crate, `packages/<name>.rs`, `<name>.m2`, `algebramesh.<name>`, 文書) で足す.
- 値に `evidence_terms` が増えた. `not_a_solution` の項目は, その分だけ大きくなる. 大きさは `MAX_TERMS` で抑えられている.
- 値を組み立てるときの判定と, 検査器の検査とで, 級数への作用素の作用を 2 度計算する. どちらも中断の求めを確かめ, `wall_seconds` の期限に従う.

## 採らなかった案

- **値の組立ても検査器に任せる.** 状態の判定は検査とほぼ同じ計算だが, 検査器は値を変えない (ADR 0018). 状態を決めるのは値を作る側で, 検査器はそれを確かめる側とした.
- **記述子を crate に移す.** 上に書いたとおり, server が package に依存する.
- **driver を常に `gkz.m2` と組にする.** 隣に file がなければ読み込まないことで, feature なしの image と, 共通の driver だけを差し替える自己検査 (`check-driver-orders.sh`) がそのまま動く.
