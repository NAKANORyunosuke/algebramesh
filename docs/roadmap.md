# AlgebraMesh ロードマップ (v1.1.0 以後)

- 状態: 採用 (2026-11-06). 利用者が決めた問いは §10 にある.
- 由来: 利用者が示した 4 つの Phase (数学的な共通基盤, Workflow Engine, Verification Framework, 研究パッケージ) をもとに, 3 つの独立な案を作って比べ, 数学とシステムの 2 つの観点から審査して直した.
- 新しく出す名前 (operation, 主張の ID, 型, RPC, 欄など) はすべて提案であり, 各 milestone の ADR を書く時点で確かめる (§11).

## 0. 前提と表記

- 【現状】は, HEAD `05720d5` (2026-11-06, workspace の版 1.1.0) の作業ツリーで確かめたことを指す. 【提案】は, この文書で新しく出す案を指す.
- 次に出す版は v1.2 である. `CHANGELOG.md` に「未リリース」の節がある.
- 新しく出す名前はすべて提案であり, §11 で利用者に確かめる. 対象は crate, 型, trait, operation, RPC, proto の欄, JSON の key, 設定, 表, 主張の ID, 機能名, 日本語の言い方である. ADR の番号は作る順に振る仮の番号である.
- 規模は, 既存の commit の追加行数で表す.
  - S: `718bb51` (約 230 行) に近い.
  - M: `be7660e` (約 530 行) に近い.
  - L: `954ad8e`-`96cdb54` (1100-1500 行) に近い.
  - XL: `492acbd` (約 4000 行) に近い.
- 週による見積りはしない (§7.4).
- Issue の題は既存の Issue に合わせて英語で書き, 本文は日本語で書く.
- 旧版の第 1 部 (3 つの草案の採点) と, 2 つの審査への対応 (付録) は外した.

### 0.1 出発点 (【現状】)

**operation と型**
- operation は 4 つある.
  - `polynomial.factor`: asir, macaulay2, oscar にある.
  - `gkz.generate`, `gkz.indicial`, `gkz.series`: macaulay2 だけにある.
- 引数の型は, backend ごとに重複している.
- 積の検算 (`Factorization::expand_with`) は, 3 つの backend の `execute` の中にある (`backend-asir/src/lib.rs:307`, `backend-macaulay2/src/lib.rs:998`, `backend-oscar/src/lib.rs:568`).
- `runner::execute` (`crates/algebramesh-core/src/runner.rs:36`) は `check_result` を呼ばない. `runner.rs:147` の呼出しは, 試験用の backend の中にある.
- `algebramesh-math-types` は core に依存しない.
  - `Polynomial` の係数は `Rational` である. `CoefficientDomain` は ZZ と QQ だけである.
  - 項順序はメタデータにとどまり, driver は順序を CAS に送らない.
  - `Ideal` と `DModuleIdeal` は `Serialize` を持たない.
- `IntegerMatrix` は密な行列で, 演算は `rank` (`matrix.rs:54`) だけである.
  - `rank` は QQ 上の Gauss の消去で, pivot を選ぶ. 大きさに上限はない.
  - 64 bit を超える成分は, 10 進の文字列で受け取る (`matrix.rs:10`).
  - `serde_json` は `arbitrary_precision` を使っていない. そのため, driver が数値で出した大きな整数は f64 になる.
- `Factorization::new` (`polynomial.rs:335`) は, 重複度 0 の因子を断らない. 重複度 0 の非定数の因子は積の検算を通り, 値に残る. 定数の因子は `constant` に畳まれる.
- `algebramesh-gkz` は, 次のものを Rust で計算している.
  - `cone.rs`.
    - `determinant`: pivot を選ぶ消去である.
    - `facet_normals`: 列の部分集合を全列挙する. 上限は `MAX_FACET_CANDIDATES` と `MAX_FACET_WORK` である.
    - `parameter_report`: GKZ の意味の nonresonance を判定する.
    - `is_homogeneous`: `IntegerMatrix::rank` を使う.
  - `series.rs`.
    - `solve` (`:176`): Gauss の消去である.
    - `evaluate`, `in_lattice`, `check_exponent`, `check_series`.
- `checks.*` には, 2 種類の値が混ざっている.
  - Rust が判定した値: `euler_operators_match`, `toric_binomials_valid`, `exponents_are_roots`.
  - CAS の答えを写した値: `generated_by_toric_and_euler`, `expected_in_ideal`.

**大きさの上限**
- 結果と仕様は 8 MiB まで (`MAX_RESULT_BYTES`, `MAX_SPEC_BYTES`).
- Raft の 1 項目は 4 MiB まで.
- gRPC のメッセージは 128 MiB まで (`MAX_MESSAGE_BYTES`, ADR 0017).
- driver の出力は 256 MiB まで (`driver.rs:78`, `backend-oscar/src/lib.rs:67`).
- Issue #36 は OPEN である.

**記録と版の混在**
- `JobRecord`, `BackendInfo`, `Capabilities` は `deny_unknown_fields` を持たない. `JobSpec` は持つ (`job.rs:31`).
- follower の `Store::apply` は, `UpdateJob` の `row.record` を自分の版の `JobRecord` に読み, 書き直して `record_json` に入れる (`store.rs:523-536`).
  - 一方で, `changes` 表には元の JSON が残る (`:671-674`).
  - したがって古い node は, 知らない欄を `record_json` から黙って落とす.
- `Store::apply_batch` は, 知らない `Change` の variant を受け取ると失敗する (`store.rs:669`). その node の Raft はそこで止まる.
- `Scheduler::finished` は `outcome_json` を `CasResult` に読む (`scheduler.rs:799`). leader が古いと, 新しい欄はログに入る前に落ちる.

**cluster**
- `Replication` service は peer token でしか呼べない (`crates/algebramesh-server/src/lib.rs:148-153`). `NodeStatus` を返すのは, この service だけである.
- proto に足りない欄がある (`proto/algebramesh.proto:103-113`).
  - `NodeStatus` には版の欄がない.
  - `BackendOffer` には `driver_version` も driver の hash もない.
- `Node::read_committed` は, 状態機械の mutex を持ったまま closure を実行する (`cluster/mod.rs:479`).
- follower では, `Node::scheduler()` が `UNAVAILABLE` を返す (`:355-374`).
- `propose` (`:1001`) は, 変更を書いた順に batch にまとめ, 確定を待ってから次へ進む.
- 時間の定数は次のとおりである.
  - `COMMIT_WAIT`: 10 秒.
  - `add_member` が追いつきを待つ時間 (`CATCH_UP`): 60 秒.
  - lease: `election_timeout * 4`.
- `SnapshotInbox` (`cluster/snapshot.rs`) には次の性質がある.
  - offset 0 の chunk を受けると, 他の転送の file をすべて消す.
  - `take` は file 全体をメモリに読む.
  - `fsync` しない.
- `changes` 表は直近の 100,000 件を持つ (`store.rs:189`). snapshot は, データベース全体の複製である. `migrate` は, 接続を開くたびに走る.
- 呼出しの期限は次のとおりである.
  - client: 30 秒 (`client/src/lib.rs:104`).
  - agent: 1 回が 30 秒で, 報告の再試行の全体が 120 秒 (`REPORT_PATIENCE`).
- `tests/integration/run-raft-e2e.sh` の手順 3 は, 各 node の `last_seq` が等しいことだけを比べる. 内容は比べない.

**build, CI, 試験, image**
- `.dockerignore` は `.git` を除いている. `deploy/docker/algebramesh.Dockerfile` は commit を渡さない.
- CI は `--all-features` でしか build と試験をしない (`.github/workflows/ci.yml:22-23`).
- `tests/regression/run.py` が出すのは, 合否と版の要約だけである (`:227-230`). 値は保存しない.
- `docs/licenses.md` によると, image には次のものがある.
  - agent-macaulay2 の image: 4ti2, normaliz, libsingular4m4n1 (`:124-126`).
  - agent-oscar の成果物: Singular, polymake, normaliz, TOPCOM, lrslib, cddlib (`:142`).
- 63 個の commit は, 2026-11-03 から 2026-11-06 の間にある.
- GitHub では #37-#41 が OPEN のままだが, 対応する commit は main にある.

## 1. 概要と全体の方針

### 1.1 4 つの Phase と 2 本の track

| Phase | 目的 | 研究者に届く版 |
|---|---|---|
| 1 数学的な共通基盤 | 型付きのイデアル, 加群, 行列. 主張と certificate を返す汎用 operation. #36 | v1.3 (行列), v1.4 (Groebner basis, 8 MiB を超える結果) |
| 2 Workflow Engine | DAG, 再開, 受渡しの型検査, cache | v1.6 から |
| 3 Verification Framework | 計算と検査の分離, 再検証, 照合 | v1.4 (主張の一覧) から段階的に |
| 4 研究パッケージ | GKZ, topology, combinatorics, D-module, algebraic geometry | v1.5 (GKZ の package) から |

- 利用者の Phase の順序と意図は変えない.
- Phase 1 は, 次の 2 本の track に分けて進める.
  - track A (数学): `algebramesh-math-types`, `algebramesh-ops`, `algebramesh-verify`, 各 backend, `drivers/`.
  - track B (基盤): core の `blob.rs`, `algebramesh-server`, `proto/`, `algebramesh-agent`, `algebramesh-client`.
- 実装者は 1 人と仮定する. 「並行」とは, 利用者の確認や Docker の CI を待つ間に, もう一方の track を進めることを指す.
- 2 本の track が同時に触りやすい file は次のとおりである.
  - `proto/algebramesh.proto`.
  - `crates/algebramesh-client/src/lib.rs`.
  - `bindings/python/python/algebramesh/__init__.py`.
  - `crates/algebramesh-agent/src/lib.rs` (`run_assignment`, `deliver`).
  - `crates/algebramesh-core/src/runner.rs`, `provenance.rs`, `job.rs`.
- agent の処理の順 (検算, 正規化, upload, 報告) は, A1 の最初の PR で決める. B1 と B2 はその順に合わせる.
- 次の 3 つを前倒しする (Q1).

| 前倒しするもの | 元の Phase | 置く場所 | 理由 |
|---|---|---|---|
| 版と機能の合意. 記録の新しい欄を守る機能 `record_v2`. `driver_sha256`, `agent_build` | Phase 2 | B0 (最初) | 新しい `Change`, marker, 記録の新しい欄は, どれも古い node と混ざると, 状態が食い違うか Raft が止まる (§1.5) |
| 保証の最小の記録と `ResultVerifier` の hook | Phase 3 | A1 | 新しい operation が最初から主張を返せる. 出荷には B0 の `record_v2` が要る |
| certificate の検査関数 | Phase 1 の「数学的検証 API」 | A1-A6 | operation を足すたびに, その検査関数を足す |

- Phase 3 には次のものが残る.
  - 既存の 4 つの operation の主張の一覧と, その検算の移設.
  - 後からの再検証.
  - process と host の独立 (検証ジョブ).
  - 照合.
  - 確率的な検査.

### 1.2 設計書 §1 の境界 ([ADR 0019](adr/0019-rust-checks-certificates-and-bounded-recomputation.md))

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

**既存のコードと, 予定している検査の分類 (【提案】)**

| 対象 | 区分 | 対応 |
|---|---|---|
| `IntegerMatrix::rank` | (b) | 行と列の数に上限を足す (A2). math-types に残る唯一の消去とし, 新しい消去の API は足さない |
| `cone.rs` の `determinant`, `facet_normals`, `parameter_report`, `is_homogeneous` | (b) | 既存の `MAX_FACET_CANDIDATES` と `MAX_FACET_WORK` を, ADR の上限として記録する. `is_homogeneous` は rank の上限に従う |
| `series.rs` の `solve` (`normalize` が使う) | (b) | math-types には移さない. V2 で (a) に移す予定だったが, (b) のまま残した ([ADR 0028](adr/0028-gkz-package-and-its-checks.md)). 標準形は Rust が定義したもので, 検査器は値の標準形を再計算と比べる (`series.canonical`, `method: recomputation`) |
| `series.rs` の `evaluate`, `in_lattice`, `check_exponent`, `check_series` | (a) の演算だけで済む | 例外として扱わない. `check_series` は CAS が返した toric の生成元を作用させるので, その実効水準は `gkz.toric_generates_IA` に従う (§1.3) |
| K1 の境界行列の組立て | データの構成 | 面の数に上限を置く |
| S 対の lcm representation の検査 (A3) | (a) | 下の論拠により内側とする (Q4 で確認) |
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
- したがってこれは, Buchberger の判定条件 (lcm representation による) を検査しているだけである. 出典は Cox-Little-O'Shea 第 2 章 §9 で, 定理の番号は版によって異なるので要確認である. 第 4 版では Theorem 6 で, 第 1 判定法は Proposition 4 である (どちらも要確認).
- #68 ([ADR 0026](adr/0026-s-pair-lcm-representations.md)) で, この検査を `certificate: "s_pairs"` として実装した.

**規則**
- 主張の命題は, 「spec の入力と, 返した値」についての命題である. 検査器は入力 (F, f, A, b, 順序) を `JobSpec` から取る. CAS が写し返した入力は使わない.
- 次のことはしない.
  - S 多項式を G で割って, 0 になるかを見ること.
  - Rust で Groebner basis, 正規形, HNF, SNF を計算すること.
  - certificate なしに, 「f ∉ I」, 「toric の生成元が I_A を生成する」, 既約性を判定すること.
- `algebramesh-math-types` には, 割り算, 正規形, 新しい消去の API を置かない. review のたびに, この境界が守られているかを確かめる.

### 1.3 保証の語彙 ([ADR 0018](adr/0018-vocabulary-of-result-guarantees.md))

**根拠の水準 (`level`)**

| 値 | 意味 |
|---|---|
| `checked` | AlgebraMesh の検査器が, spec の入力, CAS の出力, certificate だけを使い, 正確な演算で真と判定した. CAS は TCB に入らない. `recomputation` の場合は, Rust の再計算コードが TCB に入る |
| `cross_checked` | 申告した実装の系統 (`lineage`) が重ならない 2 つの producer の結果が, 一意な形か表示の係数で一致した. 証明ではなく, 独立性の仮定に立つ |
| `asserted` | 名前の付いた producer の答えをそのまま記録した. producer は, CAS の名前と版, driver の版, 関数名で特定する |

- `not_claimed` は, 結果の意味に含まれない性質を並べる欄である. 水準の順序には入らない. 例は, 収束, 解空間の基底であること, 重みが generic であることである.

**method ごとの水準の上限**

| method | 水準の上限 |
|---|---|
| `self_certifying`, `certificate` | `checked` |
| `recomputation` | `checked`. Rust の再計算コードが TCB に入ることを記録する |
| `theorem` | 次の 3 つがそろったときだけ `checked`: 定理の出典と番号を記録した. 利用者が命題を確かめた. 仮定の主張がすべて実効水準で `checked` である. そろわなければ, 仮定のうち最も弱い水準 |
| `independent_implementation` | `cross_checked` |
| `consistency` | 水準を与えない. 整合しなければ結果を断る |
| `reference` | 実行時の主張には使わない. 回帰試験だけに使う |

- `scope` は `exact` か `truncated {weight, order}` である.
- 既存の `verified_to_order` は, 次のように写す.
  - 自分の水準は `checked`, `scope` は `truncated` とする.
  - 実効水準は `gkz.toric_generates_IA` に従う. 【現状】ではこれが `asserted` なので, 実効水準も `asserted` になる.
- `depends_on` には前提の主張を書く. 実効水準は, 自分の水準と前提の実効水準のうち最も弱いものとする.
- **契約.**
  - operation ごとに, 主張の集合と `not_claimed` を `docs/claims.md` に書く. これを契約と呼ぶ.
  - `verified` を出せるのは, 利用者 (数学者) が命題を確かめたと記した operation だけである. 確かめるまでは, `partially_verified` が上限である.
  - この規則は, 既存の operation にも新しい operation にも当てる.
- **要約 (`summary`).** すべて実効水準で決める.

  | 要約 | 条件 |
  |---|---|
  | `verified` | 契約が確認済みで, 契約の主張がすべて実効水準で `checked` |
  | `partially_verified` | 実効水準で `checked` の主張と, そうでない主張がある. または, 契約が未確認 |
  | `unverified` | 実効水準で `checked` の主張が 1 つもない |

  - 同じ語を 2 つの層で使わないため, 主張の水準には `verified` を使わない.
  - 要約だけでは区別がつかないので, `claims` は常に返す.
- **fail-closed.**
  - 検査器の誤りや panic が `checked` になることはない. 【現状】の `check_result` は `JoinError` を `UNSUPPORTED_RESULT` にしており, この性質を保つ.
  - backend が自分で返した `guarantee` は捨てる. その中に `asserted` 以外の水準があれば, 結果を `UNSUPPORTED_RESULT` にする.
- **`checked` の行に結び付けるもの.** 次の 2 つを結び付ける.
  - それを支える監査の行 (`docs/audits/2026-11-05-reliability-audit.md` §4) か, 試験.
  - 変異の試験の名前.
- **変異の試験の規則.**
  - 変異は, 「変えた後のものが主張を偽にする」ことを試験の側で別に確かめてから使う.
  - 1 か所を書き換えても正しいままの certificate があるからである. 例は次の 2 つである.
    - ZZ 上で解がないことの y は, y_i に整数を足しても y A ∈ ZZ^n と y b ∉ ZZ を保つ.
    - M の第 i 列が 0 なら, K の第 i 行を変えても M K = 0 を保つ.
  - こうした certificate では, 次のような変異を使う.
    - y b の小数部分を 0 にする.
    - 主張の値そのもの (D の対角成分, H の成分) を変える.
  - 1 か所の書換えを必ず検出できる certificate もある. 多項式の積で効く certificate (C, N, 商, h_k) と, U V = I や B B' = I がそうである.
  - この区別は, V1 の CI の規則に書く.

### 1.4 新しい operation の結果の形と certificate

- `value` には, 数学的な値と `certificate` (object) だけを置く. 判定は `guarantee.claims` に置く. `checks` は, 既存の 4 つの operation にだけ互換のために残す (Q3).
- 引数 `certificate: "include" | "none"` を置き, 既定を `include` とする. `none` のときは, その certificate が支える主張を `asserted` にする.
- driver は, 64 bit を超える整数を 10 進の文字列で出す.
  - 数値で出した大きな整数は f64 になり, 検査で落ちるからである.
  - 64 bit を超える成分を含む例を, 回帰試験に置く.
- 大きさの扱いは次のとおりである. どの場合も, 黙って水準を下げることはしない.
  - B1 より前.
    - `value` が 8 MiB を超えるときは, certificate を `{"omitted": "certificate_too_large"}` に置き換える. 関係する主張は `asserted` にし, `reason` を記録する.
    - それでも 8 MiB を超えれば, `RESOURCE_LIMIT` とする.
  - B1 より後.
    - §3.4 の規則で, 値を外に出す.
    - `max_blob_bytes` を超える certificate は, B1 より前と同じく省く.
- `guarantee` の大きさは, 主張の種類の数で抑えられる. `guarantee` も `MAX_RESULT_BYTES` に数える.
- 検算の時間は, ジョブの締切に含める (ADR 0016).

### 1.5 版の混在と機能の合意 (ADR, 提案 0020)

**新しい意味の 3 種類.** 次の 3 つは, cluster が合意した機能に含まれるときだけ使う.

| 種類 | 古い node と混ざると起きること |
|---|---|
| 新しい `Change` の variant (`EnableFeatures`, `requests` 表など) | 古い node の `apply_batch` が失敗し, Raft が止まる |
| marker (`$blob`, `$ref`) | 古い node は本体の取得も修復もできない. 古い leader は marker を値として返す |
| 記録の新しい欄 (`guarantee`, `driver_sha256`, `agent_build`, cache の欄) | follower の `apply` が `record_json` から欄を落とし, `changes` 表とも他の node とも食い違う. leader が古いと, 欄はログに入る前に落ちる. 食い違いは snapshot を通じて広まる |

**合意の手順 (【提案】)**
1. 各 node は, `NodeStatus.version` と `NodeStatus.features` を返す.
2. leader は, 全 member (投票者と learner) に問い合わせる. 全員がある機能を持つと確かめたら, `Change::EnableFeatures` を 1 度だけ propose する.
   - この時点で, 古い node は member にいない. したがって, 適用で止まる node はない.
3. それからは, 有効な機能を状態から読む. 問い合わせ直すことはしない. 1 台が止まっても, 機能は無効にならない (Q23).
4. 機能の有効化, `add_member`, `remove_member` の 3 つは, leader の中の 1 つの lock で順に並べる. `add_member` は, 有効な機能を持たない node を断る.
5. 有効化と同時に, `meta` に `min_binary_version` を記録する.
   - B0 以後の binary は, 自分の版がこれより古ければ起動を断る.
   - B0 より前の binary はこの検査を持たない. そのため, 「機能を有効にした後は B0 より前の版に戻さない」と ADR と `docs/install.md` に書く.

**client と agent への公開**
- 有効な機能は, `RegisterResponse.features` と `ListBackendsResponse.server_features` で client と agent に知らせる.
- agent は, 機能を知るまでは使わない. 遅れて使い始めても安全である.
- 古い scheduler は, 新しい RPC に `UNIMPLEMENTED` を返す. client と agent はこれを「機能なし」と扱い, inline で送るか, はっきり失敗する.
- `record_v2` が有効でなければ, agent は新しい欄を外して報告する.

**その他**
- Phase 1 では `JobSpec` (`deny_unknown_fields` を持つ) を変えない.
- 足す欄には, すべて `#[serde(default, skip_serializing_if = ...)]` を付ける.
- 更新の順序 (scheduler, agent, client) を `docs/install.md` に書く. ただし, 正しさをこの順序に頼ることはせず, 機能の合意で守る.
- `legacy_guarantee` (V4) が同じ水準に写せるのは, 既存の 4 つの operation だけである. 新しい operation の記録で `guarantee` を欠くものは, すべての主張を `asserted` とし, 理由を記録する.

## 2. Phase 1: 数学的な共通基盤

**目的.**
- 型付きのイデアル, 加群, 行列をそろえる. 行列は密と疎で, 係数域は ZZ, QQ, GF(p) である.
- 汎用 operation が, 主張と certificate を返すようにする.
- 8 MiB を超える結果と入力を, Raft のログの外に置く (#36).

**範囲.**

| | 内容 |
|---|---|
| 含む | 比較の道具 (T0). 機能の合意 (B0). 保証の最小の記録. 検査関数. `algebramesh-ops`. 項順序の拡張と, 順序を CAS に送ること. `matrix.*`, `ideal.*`, `module.syzygies`, `complex.free_resolution`, `ideal.hilbert_series`. GF(p) と疎行列. 消去, イデアル商, 飽和 (需要が出てから). #36 |
| 含まない | Rust で割り算, 正規形, Groebner basis, HNF, SNF を計算すること. 局所順序と負の重み. 代数体と GF(q) (q が素数でないもの). ZZ 上の Groebner basis. GF(p) 上の HNF と SNF. 斉次でない入力の Betti 表. Ext と Tor. 既存の GKZ の検算の移設 (Phase 3). 外部の object storage の実装 |

### T0. 値の比較の道具 [S. 最初]

成果物:
- `tests/regression/run.py` と `bindings/python/tests/test_cluster.py` に, 2 つの mode を足す.
  - 各例の `value` を canonical JSON で保存する mode.
  - 前の版で保存したものと比べる mode. job id, 時刻, host, log は比べない.
- 同じ image で 3 回流し, CAS の出力の順序が安定しているかを確かめる. 安定しない欄は構造的な比較に回し, その一覧を文書に残す.

受入条件:
1. 同じ commit で 2 回流しても, 差が出ない.
2. 値を 1 か所変えた版では, 差が報告される.

Issue: `[P2][test] Save and compare canonical result values across versions`.

### track A (数学)

#### A0. operation の定義の共有 [M]

研究者に届くもの:
- Python の `Client.factor`.
- online な worker が持たない operation を, 投入した時点で断ること.

成果物:
- 新しい crate `crates/algebramesh-ops`.
  - 依存は `algebramesh-math-types`, `serde`, `serde_json` だけとする.
  - 中身は次のとおりである.
    - operation 名の定数.
    - 引数と結果の型 (`deny_unknown_fields`).
    - 記述子 `OperationDescriptor {name, arguments, result, order_dependent, deterministic}`.
  - backend ごとに重複している `polynomial.factor` と `gkz.*` の引数の型を, ここ 1 か所に移す.
- `Capabilities.signatures` (`#[serde(default)]`). `supported_types` は互換のため残す.
- `combine` (`scheduler.rs:138`) が `signatures` もまとめるようにする. 同じ operation に違う signature を申告する worker があれば, `Capabilities.signature_conflicts` に載せる.
- `Scheduler::submit` に, 投入時の検査を `check_limits` と同じ形で足す.
  - その backend の online な worker が 1 台以上いて, どれもその operation を申告していなければ断る.
  - 1 台もいなければ, 今と同じく待たせる.
- `docs/install.md` に次の 2 点を書く.
  - leader が替わった直後は, agent が登録し直すまで待たされる. そのため, 同じ投入でも答えが変わりうる.
  - agent の更新の途中では, 新しい operation が断られうる.
- Python の `Client.factor(ring, polynomial, backend=...)` と, Rust の `algebramesh_client::ops`.

受入条件:
1. T0 で, 回帰試験と `test_cluster.py` の `value` が, 変更の前後で一致する.
2. 引数の型の定義が, `algebramesh-ops` の 1 か所だけになる.
3. 投入時の検査が次のように振る舞う.
   - online な worker が持たない operation は, `check_limits` と同じ種類の誤りで断られる.
   - worker が 1 台もいなければ, 待たされる.
   - leader を替えた直後の投入は待たされ, agent が登録した後は同じ答えになる.
4. `ListBackends` が `signatures` を返す. 食い違う signature を申告する 2 台の試験用 worker では, `signature_conflicts` が出る.

Issue:
- `[P2][api] Share typed operation arguments and results in one crate`.
- `[P2][api] Refuse an operation that no online worker of the backend offers`.
- `[P3][python] Add a factor helper to the Python client`.

#### A1. 検証の土台: `guarantee`, `ResultVerifier`, `algebramesh-verify` [M. 出荷には B0 が要る]

成果物:
- `crates/algebramesh-core/src/guarantee.rs`. serde のデータだけを置く.
  - `EvidenceLevel`.
  - `Claim {id, level, method, by, scope, depends_on, reason}`.
  - `Guarantee {schema, catalog, verifier: {name, version, commit}, summary, claims}`.
- `CasResult.guarantee` と `JobRecord.guarantee` (`serde(default, skip_serializing_if)`).
  - `JobRecord::complete` がこの欄を写す.
  - agent は, `record_v2` が有効なときだけ, この欄を報告に含める.
- core の trait `ResultVerifier: Send + Sync { fn verify(&self, spec: &JobSpec, result: CasResult, interrupt: &Interrupt) -> Result<CasResult, CasError>; }`.
  - この trait は JSON を扱うだけなので, core は math-types に依存しないままである.
  - `runner::execute` に, `backend.execute` の後で `check_result(&ctx, ...)` を呼ぶ処理を新しく足し, その中で verifier を走らせる.
  - verifier は, agent と `run_job` (CLI の local 実行) が渡す.
- 最初の PR で, agent の処理の順 (検算, 正規化, upload, 報告) を決める. upload の位置は B1 まで空けておく.
- 新しい crate `crates/algebramesh-verify`.
  - 依存してよいのは core, math-types, ops だけである. backend, `algebramesh-openxm-*`, `algebramesh-rpc`, `algebramesh-server` には依存しない.
  - 中身は次の 3 つである.
    - operation ごとの trait `OperationVerifier` と, operation 名を鍵にした登録表. package の crate がこの trait を実装し, agent が有効にした feature に応じて登録表を組む.
    - 登録表にない operation の主張を, すべて `asserted` にする規則.
    - 汎用の検査関数を置く `certificate` module.
- `polynomial.factor` の検算をここに移す.
  - 3 か所にあった同じ手順が 1 つになる.
  - 移すときに, 重複度 0 の因子を断るようにする. 【現状】の欠陥の修正である.
- `polynomial.factor` の主張 (ID は提案).

  | 主張 | 命題 | 水準 |
  |---|---|---|
  | `factor.product` | spec の入力 = constant × Π f_i^{m_i}. 各 m_i ≥ 1 で, 各 f_i は定数でない | `checked` |
  | `factor.domain` | 係数が宣言した係数域にある | `checked` |
  | `factor.primitive` | (ZZ のとき) 各 f_i の係数の gcd が 1 である | `checked` |
  | `factor.non_associate` | f_i どうしが同伴でない | V3 までは `asserted` |
  | `factor.irreducible` | 定数でない各 f_i は既約である. `constant` は素因数に分けない. ZZ 上では Gauss の補題によって QQ 上の既約性に帰着するので, `depends_on` に `factor.primitive` を書く | `asserted` |

- math-types の `Polynomial::mul` と `DifferentialOperator::mul` に, 一定の項数ごとに closure を呼ぶ checkpoint を置く. math-types は core に依存しないので, `Interrupt` ではなく closure で受け取る.
- ADR 0018 (語彙) と ADR 0019 (境界). 中身は §1.2 と §1.3 である.
- Python の `Job.guarantee()`. CLI は要約と主張の表を出す.

受入条件:
1. T0 で, `value` が変わらない.
2. 次の偽の backend は, 3 つの backend のどの経路でも `UNSUPPORTED_RESULT` になる.
   - 自分で `checked` を名乗る backend.
   - 積が入力と一致しない因数分解を返す backend.
   - 重複度 0 の因子を返す backend.
3. spec と違う入力を写し返す偽の backend の結果は, 断られる. 検査が spec の入力を使うことの試験である.
4. 登録表にない operation (試験用の backend) の主張は, すべて `asserted` になる.
5. 互換と依存.
   - `guarantee` のない古い `record_json` が読める.
   - proto は変わらない.
   - core の依存に math-types が増えない. `algebramesh-verify` の依存に backend が現れない. どちらも `cargo metadata` を読む試験で確かめる.
6. 締切を越えた検証は, `TIMEOUT` になる. 中断を受けた大きな積は, 100 ms 以内に止まる. 試験の形は `check.rs` の既存の試験と同じにする.
7. `record_v2` が無効な cluster では, 記録に `guarantee` が現れない. 全 node の `record_json` も一致する.

Issue:
- `[P2][design] Fix the vocabulary of what a result claims (ADR 0018)`.
- `[P2][design] ADR: certificate checks and bounded recomputation in Rust (ADR 0019)`.
- `[P2][verify] Record a guarantee with every result through a ResultVerifier hook`.
- `[P2][verify] Refuse factors of multiplicity zero in polynomial.factor`.
- `[P2][resource] Interrupt long products inside a result check`.

#### A2. 整数と有理数の行列 [L]

研究者に届くもの: certificate 付きの SNF, HNF, kernel, rank, solve. 契約を確かめた後は, 要約が `verified` になる.

成果物:
- math-types.
  - `IntegerMatrix::{mul, transpose, identity, is_identity}` と serde.
  - `RationalMatrix` (密).
  - `IntegerMatrix::rank` に, 行と列の数の上限を足す.
  - `algebramesh-gkz` の `determinant` と `solve` は, math-types に移さない (§1.2).
- 検査関数 (`algebramesh-verify::certificate`) と operation. operation の名前は提案である.

| operation | CAS が返す certificate | Rust の検査 | 水準 |
|---|---|---|---|
| `matrix.rank` | r 次の小行列 B の行と列の位置, その逆行列 B'. 列階段形の K (n - r 列) | B B' = I から rank ≥ r. M K = 0 と K の階段形 (列が独立) から nullity ≥ n - r, よって rank ≤ r | `checked` |
| `matrix.kernel` (QQ) | 列階段形の K と, rank の certificate | M K = 0. 列は独立で, 列の数は n - r. rank ≥ r と合わせて, K は kernel の基底 | `checked` |
| `matrix.kernel` (ZZ) | 転置した行列について, U A^T = H と V (U V = I) | H は行階段形で, 0 でない行は独立. H の 0 の行に対応する U の行が, ZZ 上の基底になる. U が単模なので完全 | `checked` |
| `matrix.solve` | 解 x か, 解がないことの certificate y | A x = b を確かめる. QQ 上で解がないとき: y A = 0 かつ y b ≠ 0. ZZ 上で解がないとき: y ∈ QQ^m で, y A ∈ ZZ^n かつ y b ∉ ZZ (整数解 x があれば y b = (y A) x ∈ ZZ となり矛盾する) | `checked` |
| `matrix.hermite_normal_form` (ZZ だけ) | U と V (U V = I) | H = U A. H は行階段形で, pivot は正, pivot の上の成分は 0 以上 pivot 未満, 0 の行は下にある. HNF は一意なので値が決まる | `checked` |
| `matrix.smith_normal_form` (ZZ だけ) | P, Q とそれぞれの逆行列 | D = P A Q. P と Q は単模. D は対角で, d_i ≥ 0 かつ d_1 \| d_2 \| ... | `checked` |

- backend.
  - oscar: `snf_with_transform`, `hnf_with_transform`, `kernel`, `rank`, `can_solve_with_solution` を使う.
  - macaulay2: `smithNormalForm`, `kernel`, `rank`, `solve` を使う. HNF に LLLBases の `hermite` を使えるかは要確認である. 使えなければ, HNF は oscar だけで提供する.
  - 変換行列を掛ける向きは, image の中で確かめてから使う.
  - 手続きは, `drivers/oscar/algebramesh.jl` と `drivers/macaulay2/algebramesh.m2` に足す.
- Python: `smith_normal_form`, `hermite_normal_form`, `kernel`, `rank`, `solve`.
- 回帰試験 `tests/regression/cases/matrix_*.json` に, 次の例を置く.
  - 手で確かめられる例. [[2,4],[6,8]] の SNF は diag(2,4) である (gcd が 2 で, |det| が 8).
  - 64 bit を超える成分を含む例.
- 利用者に確かめてもらうもの: 上の表の 6 行の命題と, `docs/claims.md` の `matrix.*` の契約.

受入条件:
1. 契約を確かめた後は, 5 つの operation の要約が `verified` になる.
2. 主張ごとに, §1.3 の規則に従う変異の試験が少なくとも 1 つある. 記録した CAS の応答を変異させると, `UNSUPPORTED_RESULT` になる.
3. oscar と macaulay2 の H と D が, 構造的に一致する.
4. 解がない入力では, y を返して成功する. y b が整数になるように変えた y は, 断られる.
5. `algebramesh-gkz` の既存の試験が, 変更なしで通る.
6. 大きな結果の扱い.
   - B1 より前: certificate のせいで `value` が 8 MiB を超える例では, certificate が省かれる. `certificate_too_large` が記録され, 関係する主張は `asserted` になる.
   - B1 より前: 値そのものが 8 MiB を超えれば `RESOURCE_LIMIT` になる.
   - B1 より後: 同じ例が `SUCCEEDED` になる.
7. 64 bit を超える成分が, driver から検査まで値を変えずに通る.

Issue:
- `[P2][math] Integer and rational matrix operations with checked certificates`.
- `[P2][verify] Certificate checks for rank, kernel, solve, HNF and SNF`.
- `[P3][math] Bound the size of IntegerMatrix::rank`.
- `[P3][drivers] Print integers beyond 64 bits as strings`.

#### A3. 項順序とイデアル [L]

研究者に届くもの: 指定した項順序での reduced Groebner basis, 正規形, 所属の判定.

成果物:
- math-types.
  - `MonomialOrder` に `Weights {weights, then}` と `Block(Vec<(count, base)>)` を足す. `base` は lex, grlex, grevlex のどれかである.
  - `Weights` の重みは非負に限り, 同点は `then` (大域的な順序) で決める. 負の重みは, CAS を起動する前に `INVALID_ARGUMENTS` で断る. A3 の operation は多項式環 K[x] の中の Groebner basis, 正規形, 所属を扱い, 局所化を要する局所順序と混合順序はその範囲の外だからである (ADR 0023. lcm representation による判定そのものは局所順序でも成り立つ).
  - 比較関数 `compare` を足す. property test で次の 3 つを確かめる.
    - 全順序であること.
    - 積と両立すること (a < b ならば a c < b c).
    - 1 が最小であること.
  - `Polynomial::{sub, neg, total_degree, leading_monomial, leading_term, leading_coefficient}` を足す. 先頭項は環の項順序で決め, 順序を引数には取らない (#65 で決めた). 検査器は spec の環で多項式を作るので, 宣言した順序と別の順序を渡す口をなくして, 取り違えを防ぐ.
  - `IdealData {"type": "ideal", "ring", "generators"}` を足す.
  - `PolynomialMatrix` を足す. 列を生成元とし, 疎な形で持つ.
  - `Factorization` を Deserialize できるようにする.
- driver. 順序に依存する operation では, 項順序を明示して渡す.
  - M2 では `MonomialOrder =>` で渡す.
  - OSCAR では `ordering` で渡す.
  - driver で表せない順序は, CAS を起動する前に断る.
  - `polynomial.factor` は順序に依存しないので変えない.
- operation (名前は提案).
  - `ideal.groebner_basis`. 引数 `certificate` は `"representation" | "s_pairs" | "none"` で, 既定は `representation` である.
  - `ideal.normal_form`. 余り r と商 q_k を返す.
  - `ideal.membership`.
- 係数域は QQ に限り, ZZ は断る. GF(p) は A5 で足す.
- backend.
  - macaulay2: `gb(..., ChangeMatrix => true)`, `getChangeMatrix`, `//`, `%` を使う.
  - oscar: `groebner_basis_with_transformation_matrix` (掛ける向きは要確認), `coordinates`, `reduce_with_quotients` (要確認) を使う.
  - asir: 変換行列を取れるかを調べる. `nd_gr`, `gentrace`, `nd_btog` は要確認である.
    - #69 で確かめた ([記録](audits/2026-11-22-asir-groebner-transformation.md)). C は `nd_gr(F, V, 0, O | gentrace=1)` と `nd_btog`, N は `dp_true_nf_and_quotient` で得られ, ADR 0023 の順序はすべて表せ (重みには Asir の上限がある), 値は CMO の多項式のリストで無損失に受け取れる.
    - Asir に `ideal.groebner_basis` を足すと決め, #94 を起票した. v1.4 の受入条件には含めず, K4 より前に行う.

主張 (ID は提案):

| 主張 | 検査 | 水準 |
|---|---|---|
| `gb.contained`: (G) ⊆ I | G = F C | `checked` |
| `gb.contains`: I ⊆ (G) | F = G N | `checked` |
| `gb.reduced_form`: 各元は monic で, どの項も他の元の先頭単項式で割り切れない (宣言した順序で) | 単項式の比較と整除 | `checked` |
| `gb.groebner`: G は Groebner basis である | `s_pairs` のとき, 先頭単項式が互いに素でない各対の lcm representation を確かめる (§1.2) | `checked` (Q4). `s_pairs` がなければ `asserted` |
| `normal_form.reduced`: f - r = Σ q_k g_k で, r のどの項もどの lm(g_k) でも割り切れない | 積と単項式の整除 | `checked` |
| `normal_form.unique`: r は f の I を法とする正規形である | `depends_on`: `normal_form.reduced`, `gb.contained`, `gb.contains`, `gb.groebner` | 実効水準は前提に従う |
| `membership.in`: f ∈ I | 入力の生成元に対する係数で, f = Σ c_i f_i | `checked` |
| `membership.not_in`: f ∉ I | certificate は G, C, N, S 対の h_k, 商 q_k, 余り r (r ≠ 0). `depends_on`: `normal_form.reduced`, `gb.contained`, `gb.contains`, `gb.groebner` | 実効水準は `gb.groebner` と同じ |

- `membership.not_in` の certificate は G, C, N, h_k を含むので, `membership.in` の certificate よりはるかに大きい.
  - 表示の次数は, 最悪の場合に二重指数的である (Mayr-Meyer).
  - h_k は O(|G|^2) 個ある.
  - §1.4 の大きさの規則に従う.
  - #67 (ADR 0025) で, `membership.not_in` の depends_on から `gb.contained` を外し, certificate から C を外した. f ∉ I を示すには I ⊆ (G) (N) だけが要り, (G) ⊆ I は要らないからである. h_k は #68 で足す.
  - #68 (ADR 0026) で, `s_pairs` を求めたジョブの `ideal.groebner_basis` と `ideal.normal_form` の certificate, および `member: false` の certificate に, S 対の h_k (`s_pairs`) を足した. `gb.groebner` は `checked` になり, `normal_form.unique` と `membership.not_in` は実効水準でも `checked` になる. `representation` を求めたときは, これまでどおり `asserted` である.
- 結合試験を Docker で置く. 各順序についてランダムな多項式を作り, Rust の `compare` で求めた先頭項と, CAS の先頭項 (M2 の `leadTerm`, OSCAR の `leading_term`) が一致することを確かめる.
- 利用者に確かめてもらうもの: 上の表の 8 行の命題と, Cox-Little-O'Shea にある lcm representation の定理の番号.

受入条件:
1. lex と grevlex で reduced Groebner basis が異なる例を含める. 各 backend の結果が期待値と一致する.
2. M2 と OSCAR の reduced Groebner basis が, 回帰試験の例で構造的に一致する.
3. C, N, 係数, 商, h_k のどれかを 1 か所変えた応答は, 断られる.
4. 順序の取り違えの検出. lex を宣言したジョブで, driver が grevlex を送るように壊す. 例は回帰試験に固定する.
   - `s_pairs` 付きでは, `gb.groebner` で落ちる. #68 (ADR 0026) で, 試験 `an_order_mix_up_is_refused_by_its_s_pairs` で確かめた. driver を壊した応答は `tests/regression` では作れないので, 各 backend の `testdata/ideal.json` の `mixup_lex_answered_in_grevlex` に固定し, 試験 `the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs` (Macaulay2 と OSCAR) で断られることを確かめた.
   - `representation` だけでは, `gb.reduced_form` で落ちる.
   - `representation` だけでは落ちない例がありうることを, 文書に書く.
5. 表せない順序と負の重みは, CAS を起動せずに断られる.
6. certificate を省いた例では `certificate_too_large` が記録され, 関係する主張が `asserted` になる.

Issue:
- `[P1][math] Send the declared monomial order to every CAS and refuse orders a driver cannot express`.
- `[P2][math] Serializable ideals, polynomial matrices and leading terms`.
- `[P2][math] ideal.groebner_basis with transformation matrices in both directions`.
- `[P2][math] ideal.normal_form and ideal.membership with checked certificates`.
- `[P2][verify] lcm representations of S-pairs as a Groebner basis certificate`.
- `[P3][asir] Find out whether Asir can return Groebner basis transformation data`.

#### A4. 自由加群, syzygy, free resolution [L]

成果物:
- 型.
  - `FreeModule {ring, rank, degrees, order: position_over_term | term_over_position}`.
  - `Submodule`.
  - `ChainComplexData {differentials}`.
  - `BettiTable`.
  - 生成元の向きは列とする (Q12). OSCAR の加群は行で扱うので, driver で転置する.
- operation (名前は提案): `module.syzygies`, `complex.free_resolution`, `ideal.hilbert_series`.
- Betti 表と Hilbert 級数の対象は S/I とする (Q33). M2 と OSCAR で対象がそろうよう, driver で合わせる.
- 斉次でない入力では Betti 表を返さず, `res.minimal` を `not_claimed` に入れる. 斉次でない ideal では, 極小な free resolution も Betti 数も不変量として定まらないからである.
- backend.
  - macaulay2: `syz`, `res`, `betti`, `hilbertSeries`.
  - oscar: `syzygy_generators`, `free_resolution`, `betti_table`, `hilbert_series`.

主張 (ID は提案):

| 主張 | 検査 | 水準 |
|---|---|---|
| `syz.annihilates`: F S = 0 | 積 | `checked` |
| `syz.generates`: S が syzygy の全体を生成する | なし | `asserted` (V7 で検討する) |
| `res.complex`: d_i d_{i+1} = 0 | 積 | `checked` |
| `res.image`: d_1 の像が I である | 両向きの表示の係数 | `checked` |
| `res.input_homogeneous`: 入力が正の次数付けで斉次である | 次数の計算 | `checked` |
| `res.differentials_homogeneous`: 微分が次数 0 の斉次写像である | 次数の計算 | `checked` |
| `res.entries_in_maximal_ideal`: 微分の成分が斉次極大イデアルに入る (0 でない定数を含まない) | 成分を調べる | `checked` |
| `res.exact`: 完全である | なし | `asserted` |
| `res.minimal`: 極小な free resolution である | `depends_on`: 上の 3 つの斉次性と成分の主張, `res.exact` | 実効水準は `asserted` |
| `res.betti`: S/I の Betti 表 | `depends_on`: `res.minimal` | 実効水準は `asserted` |
| `hilbert.series`: S/I の Hilbert 級数 | Betti 表と同時に求めたときだけ, Σ (-1)^i β_{ij} t^j / (1-t)^n と有理関数として比べる. 分母を払って積で比べるので, CAS が約分した形でも比べられる | `asserted`. 比べることは `consistency` |

受入条件:
1. twisted cubic のイデアルで, S/I の Betti 表 (β_{0,0} = 1, β_{1,2} = 3, β_{2,3} = 2) が両方の backend で得られる.
2. 微分の成分を 1 つ変えた結果は, 断られる.
3. M2 と OSCAR の Betti 表が, 回帰試験の例で一致する.
4. 斉次でない入力では, Betti 表が返らない.
5. 約分した分子を返す Hilbert 級数でも, 整合の検査が通る.

Issue:
- `[P2][math] Free modules, module.syzygies and complex.free_resolution`.
- `[P3][math] ideal.hilbert_series recorded as a CAS answer with a consistency check`.

実装 (#97, #98, [ADR 0027](adr/0027-syzygies-free-resolutions-and-hilbert-series.md)) で, 上の成果物を次のように変えた.
- 型は, 部分加群 `submodule` (環, 階数, 生成元を列とする多項式の行列) と, 3 つの値の型 (`syzygies`, `free_resolution`, `hilbert_series`) だけにした. `FreeModule`, `ChainComplexData`, `BettiTable` は独立した型にせず, resolution の値の中の微分の列, 次数の列, Betti 数の列 (`{i, j, beta}`) で表す. 加群の項順序 (`position_over_term` など) は, 検査が順序を使わないので入れなかった.
- 斉次でない入力では, `res.minimal` と `res.betti` に加えて, 斉次性と成分の 3 つの主張も結果の `not_claimed` に入れる. 斉次な入力で 0 でない定数の成分がある値 (全体のイデアルなど) では, `res.entries_in_maximal_ideal`, `res.minimal`, `res.betti` を入れる.
- OSCAR の syzygy は `syzygy_generators` でなく `kernel` で求める. Macaulay2 の Hilbert 級数は `hilbertSeries` の約分する前の分子 `poincare` で求め, backend が約分する.

#### A5. GF(p) と疎行列 [L. A3 の後. 最初の週は調査に充てる]

成果物:
- `CoefficientDomain` に素体を足す.
  - JSON の形は `"GF(p)"` とし, 2 ≤ p < 2^31 とする (Q11).
  - p が素数であることは, 入力の検証で確かめる. 2^31 未満では, 底を 2, 7, 61 とする Miller-Rabin で決定的に判定できる (要確認).
  - 係数の表し方 (`Rational` との enum にするか, generic にするか) は, 最初の週に決める.
  - `ring_axioms.rs` を GF(p) にも広げる.
- `SparseMatrix` を足す.
  - 座標形式とする. 成分は行と列で整列し, 0 と重複を持たない.
  - 疎のまま検査するのは, M K = 0 と A x = b だけである.
  - 変換行列は密になりやすいことを, 文書に書く.
- GF(p) に広げるのは, `matrix.rank`, `matrix.kernel`, `matrix.solve` と `ideal.*` である.
  - `matrix.hermite_normal_form` と `matrix.smith_normal_form` は ZZ に限る.
  - 体の上の HNF は既約行階段形にすぎず, 体の上の SNF は rank と同じ情報しか持たないからである.
- backend.
  - oscar: `fpMatrix` と, Hecke の `sparse_matrix` を使う.
  - macaulay2: `ZZ/p` を使う.
  - backend ごとの p の上限を, `Capabilities` で申告させる.

受入条件:
1. 素数でない p と, GF(p) 上の HNF と SNF の投入は, CAS を起動せずに断られる.
2. GF(p) 上の rank が QQ 上の rank より小さくなる例を, 回帰試験に置く.
3. GF(p) 上の Groebner basis の例を置く.
4. 疎な入力の検算に使うメモリが nnz に比例することを計測し, `docs/benchmarks.md` に記録する.

Issue:
- `[P3][math] Prime fields GF(p) in math-types and the drivers`.
- `[P3][math] Sparse matrices in coordinate form`.

#### A6. 消去, イデアル商, 飽和 [M. 需要が出てから. K4 で要るときに着手する]

- operation: `ideal.eliminate`, `ideal.quotient`, `ideal.saturation`.
- 健全性は, 表示の係数で検査して `checked` にする. 確かめる内容は次のとおりである.
  - 消去: 各元が指定した変数だけを含み, かつ I に属すること.
  - 商: q g ∈ I であること.
  - 飽和: ある指数 k について q J^k ⊆ I であること.
- 宣言した順序が, 消去する変数についての消去順序であることも `checked` で確かめる. 次のどちらかが成り立てばよい.
  - `Block` の最初の塊が, 消去する変数である.
  - `Weights` が, 消去する変数に正の重みを, 残りの変数に 0 の重みを与える.
- 完全性の扱い.
  - 消去の完全性は, 消去定理で得る (`method: theorem`). `depends_on` には, 消去順序で計算した G 全体の `gb.contained`, `gb.contains`, `gb.groebner` を書く.
  - 商と飽和の完全性は, `asserted` か `cross_checked` とする.
- Issue: `[P3][math] ideal.eliminate, ideal.quotient and ideal.saturation with soundness checks`.

### track B (基盤)

#### B0. 版と機能の合意 [M. 最初]

成果物:
- proto に欄を足す. どれも欄の追加だけである.
  - `NodeStatus.version`, `NodeStatus.features`.
  - `RegisterRequest.features`, `RegisterResponse.features`.
  - `ListBackendsResponse.server_features`.
  - `BackendOffer.driver_sha256`, `BackendOffer.agent_build`, `BackendOffer.driver_version`, `BackendOffer.verifier_version`.
- `crates/algebramesh-server/src/cluster/mod.rs` に, §1.5 の手順を入れる.
  - 全 member の確認.
  - `Change::EnableFeatures`.
  - 有効化と member の変更を並べる 1 つの lock.
  - `add_member` での検査.
  - `meta` の `min_binary_version`.
- 機能を 2 つ定める.
  - `record_v2`: A1 の `guarantee`, B0 の `driver_sha256` と `agent_build`, W4 の cache の欄を守る.
  - `blob_refs`: B1 の marker を守る.
- `BackendInfo` に 2 つの欄を足す.
  - `driver_sha256`: agent が起動時に, 読み込む driver の全 file から計算する.
  - `agent_build`: build したときの git commit と, 作業ツリーが変更されていたかどうか.
- `deploy/docker/algebramesh.Dockerfile` に, commit を build-arg で渡し, compile 時の環境変数にする.
  - image の中では作業ツリーの状態が分からない. そこで, CI が変更のない checkout から build したときだけ「変更なし」を渡す (Q28).
  - `build` が digest か commit かを判定する形式の規則を書く.
- 文書.
  - ADR 0020 を書く.
  - 設計書 §7.3 を更新する.
  - `docs/install.md` に, 更新の順序と, B0 より前に戻さないことを書く.
- CI に, v1.2 の image を使い, 版の混ざった cluster を作る Docker の試験を足す. 今の Raft の試験 (`crates/algebramesh-agent/tests/common/mod.rs`) は 1 つの process の中で今の code を動かすので, 古い版を入れられない.
- 全 node の記録の内容を比べる道具を作る. `run-raft-e2e.sh` の手順 3 は `last_seq` しか比べない. そこで試験の後に, 各 node の状態の SQLite から `record_json` を job id の順に読み, hash を比べる段を足す.

受入条件:
1. v1.2 の scheduler を 1 台含む 3 台の cluster で, 試験用の機能を要する操作が次のようになる.
   - leader が新しい node のときは `FAILED_PRECONDITION` になる.
   - leader が古い node のときは `UNIMPLEMENTED` になる.
   - client はどちらの場合も「機能なし」と報告する.
   - どの node も `raft stopped` で終わらず, 全 node の `record_json` が一致する.
2. 全台を更新すると, `EnableFeatures` が確定し, 同じ操作が通る. その後で 1 台を止めても, 機能は有効のままである.
3. 機能を持たない node の `AddMember` は断られる. 有効化と `AddMember` を同時に呼んでも, 古い node が有効化の後に member になることはない.
4. `min_binary_version` より古い binary (B0 以後のもの) は, 起動を断る.
5. 来歴の欄.
   - `record.backend` に `driver_sha256` と `agent_build` が入る.
   - driver を 1 byte 変えると, `driver_sha256` が変わる.
   - CI で build した image では, `agent_build` に commit が入る.

Issue:
- `[P2][cluster] Agree on cluster features before using a new log format or record field`.
- `[P2][provenance] Record the driver script hash and the agent build with every result`.
- `[P2][deploy] Pass the source commit into the images`.
- `[P2][test] Mixed-version cluster tests with the v1.2 image`.

#### B1-B3. Issue #36

設計は §3 にまとめる. 段階は次の 3 つである.
- B1 [L-XL]: 結果を blob にする. 取得, 修復, 最小限の GC, `add_member` の扱いを含む. v1.4 で出す.
- B2 [M]: 引数を blob にする. v1.5 で出す.
- B3 [M]: 量の上限, 観測, CLI, backup の手順. v1.5 で出す.

### Phase 1 の完了条件

- T0, A0-A5, B0-B3 の受入条件がすべて通る. A6 は需要があるときだけ含める.
- 新しい operation がすべて `docs/claims.md` に契約を持つ. `checked` の各行に, §1.3 の規則に従う変異の試験がある.
- `matrix.*` の 5 つが, 要約 `verified` で返る.
- `ideal.groebner_basis` が, `s_pairs` 付きの小さな例で `verified` になる (Q4 が認められた場合).
- T0 で, 既存の 4 つの operation の `value` が変わらない.
- ADR 0018-0022 が採用され, 設計書 §6 と §7.3 が更新されている.

## 3. Issue #36 の設計 (ADR, 提案 0021)

- #70 で [ADR 0021](adr/0021-content-addressed-blob-store.md) として採用した. 以下は計画の時点の記述で, ADR と違う点は ADR を正とする. 主な違い: fingerprint の変更と `needs_blob_refs` は B2 に回した (B1 では marker が結果にしか現れない). marker を置ける場所で `$` の key を持つ object はすべて marker として読み, agent はそのような値を blob にする.

### 3.1 置き場所

- track B で, B0 の直後に置く. B1 は v1.4 で出す. v1.3 は B1 を待たない.
- B0 の直後に置く理由は次の 4 つである.
  1. B1 だけで, ADR 0017 で生じた後退 (約 127 MiB から 8 MiB) が戻る.
  2. A2-A4 の certificate は 8 MiB を超えやすい.
  3. Phase 2 の辺と Phase 3 の再検証は, どちらも内容の hash による参照を前提にする.
  4. marker を使えるのは全 member が blob を扱えるときだけなので, B0 の合意が要る.

### 3.2 方式

- 境界で marker に置き換える. client library と agent が, 大きな値を先に `PutBlob` で送り, marker に置き換えてから `Submit` と `ReportFinished` を呼ぶ.
- 採らない案とその理由.
  - `propose()` の中で送る案. proposer は変更を書いた順に batch にし, 確定を待ってから次へ進む. 大きな push が 1 つあるだけで, heartbeat の処理と割当てを含むすべての確定が止まり, `COMMIT_WAIT` を越えやすい.
  - Store 層で透過に外す案. 同じ理由で止まるうえ, 大きな値が scheduler の mutex の中でメモリに載る.
- ADR 0021 では, 外部の S3 互換の object storage を使う案とも比べる.
  - 設計書 §7.3 には「高可用化が必要になった時点で PostgreSQL とオブジェクトストレージへ移行する」とある.
  - 比べる観点: 運用で増える依存, Raft の過半数と同じ耐久性を言えるか, 工数, 1 台の構成での扱い.
  - 推奨は自前で複製する案である. `BlobBackend` という trait の境界を置き, 後で置き換えられるようにする (Q35).

### 3.3 内容アドレスと marker

- id は `sha256:<64 桁の小文字 16 進>` とする. JSON 値の blob の本体は, `canonical_json(value)` (`provenance.rs:11`) の UTF-8 bytes である.
- marker の形は `{"$blob": "sha256:...", "size": n}` とする. 出どころ `from: {job_id, pointer}` は W2 で使うが, 欄の名前は最初から ADR で予約しておく.
- marker を置ける場所は, 次の 3 つに限る.
  - `arguments` の top-level の値.
  - `result` の値そのもの.
  - `result` が object のときの, その top-level の値.
- `$` で始まる key の検査は, 投入時の `arguments` の top-level と, 報告時の `result` の top-level だけで行う. 深い位置にある既存の key は断らない. この変更を `CHANGELOG.md` に書く.
- 正規形の検査は, 値を使う側で行う.
  - agent は実行の前に, client は取り出すときに, 読んだ bytes が canonical JSON であることを確かめる. 違えば `INVALID_ARGUMENTS` にする.
  - scheduler は, 大きさと SHA-256 だけを stream のまま確かめる. 値を `Value` に読むことはしない.
  - `canonical_json` は `to_string`, `from_str`, `to_string` の順に変換するので, 数百 MiB の値では元の大きさの数倍のメモリを使うからである.
- agent は, `Value` を `serde_json::to_writer` で一時 file に直接書き, 書きながら hash を計算する.
  - この bytes が `canonical_json` の出力と一致することを, 試験で確かめる.
  - feature の統合で `serde_json` の `preserve_order` が有効になると, key の順が崩れる. 既存の試験 `canonical_json_sorts_keys` がこれを検出することを, ADR に書く.

### 3.4 外に出す規則

- `INLINE_MAX_BYTES` は, spec の `arguments` と `result` のそれぞれについて, inline に残す合計の上限とする.
- 合計が上限を超えたら, 大きい top-level の値から順に blob にし, 上限以下になったら止める.
- marker が 1,024 個を超えるとき, あるいはそれでも上限を超えるときは, 値全体を 1 つの blob にする.
- top-level の値ごとに閾値を置かない理由は次のとおりである.
  - 256 KiB 未満の値が多数あると, 合計で約 8 MiB が Raft に入る.
  - 記録には `spec` と `input_normalized` の 2 つの写しがある. `changes` 表は 100,000 件を持ち, snapshot はデータベース全体の複製である. どれにも, その分がすべて残る.

### 3.5 node の中の置き場所と書込み

- 置き場所.
  - 本体: `<data_dir>/blobs/sha256/<2 桁>/<62 桁>`.
  - 受信中のもの: `incoming/<uuid>.part`.
  - hash の合わないもの: `quarantine/`.
- 受信の部品は, `crates/algebramesh-server/src/blobs.rs` に新しく作る.
  - `SnapshotInbox` から引き継ぐのは, 「UUID だけを受け付ける」と「offset が順でなければ断る」の 2 つの規則だけである.
  - `SnapshotInbox` そのものは使えない. offset 0 で他の転送の file を消し, file 全体をメモリに読み, `fsync` しないからである.
- 新しい部品の性質は次のとおりである.
  - 転送ごとに独立している.
  - 書きながら SHA-256 を計算する.
  - 書込みは `spawn_blocking` の中で行い, 一定の量ごとに同期する.
  - 確定の手順は `record.json` (#37) と同じにする. 大きさと hash を確かめ, `fsync` し, rename し, ディレクトリを `fsync` する.
  - 古い `.part` は, 起動時だけでなく動作中も, 経過時間に応じて消す.
- 同じ hash の本体が既にあれば, 置き換えずに一時 file を消す. Windows で, 読出し中の file を rename で置き換えないためである.
- 設定 `blob_dir` で, blob を別の volume に置けるようにする. 空きが `blob_min_free_bytes` を下回ったら, 受付を `RESOURCE_EXHAUSTED` で断る.

### 3.6 過半数に届けてから参照する

1. 送り手 (client か agent) は, まず `StatBlob` で過半数が既に持っているかを確かめる. 持っていなければ, leader に `PutBlob` を 4 MiB の chunk の stream で送る.
2. leader は自分の disk に書き, 各 member (learner を含む) に `Replication.PushBlob` で送る. 接続は Raft とは別にし, `blob_replication_bytes_per_second` で帯域を抑えられるようにする.
3. 投票者の過半数が `fsync` を済ませたら, leader は `BlobRef` を返す.
   - 過半数は leader を含めて数える.
   - joint consensus の間は, 2 つの構成のそれぞれで過半数を求める.
   - scheduler が 1 台の構成 (`Node::single`) では, 過半数は自分だけである. `PushBlob` はしない.
4. `ClientService::submit` と `AgentService::report_finished` は, scheduler を呼ぶ前に service 層の `ensure_blobs` で marker を検査する. この検査は mutex の外で行う. `ensure_blobs` は検査だけをし, 大きな転送はしない.
   - cluster のどこにも本体がないとき: `FAILED_PRECONDITION` を返す. 送り手は `PutBlob` からやり直す.
   - 本体はあるが過半数に届いていないとき: 複製を後ろで始め, すぐに `UNAVAILABLE` を返す. 送り手は待ってから `StatBlob` でやり直す.
   - どちらの場合も, 何も書かない.
5. agent の `deliver` (`crates/algebramesh-agent/src/lib.rs:612-`) を直す. これらの応答を受けたら, blob を送り直してから報告をやり直す.
   - upload が失敗し続けても, `RESOURCE_LIMIT` にはしない.
   - ジョブは heartbeat の途絶で `LOST` になり, retry policy に任せる.
6. `blob_refs` の機能が有効でないとき, `PutBlob` は `FAILED_PRECONDITION` (B0 以後の scheduler) か `UNIMPLEMENTED` (B0 より前の scheduler) を返す. 送り手は, 8 MiB 以下なら inline で送り, 超えれば `RESOURCE_LIMIT` で失敗させる.
7. upload にかかる時間は `wall_seconds` に含めない. agent は, 報告の再試行の期限 (`REPORT_PATIENCE`) を値の大きさに応じて延ばす (Q25).

### 3.7 表

- 次の 2 つの表を足す.
  - `blobs (sha256, size, first_seq)`.
  - `blob_refs (sha256, job_id, role)`.
- どちらも, `apply` が `StoredRow` の中の marker から導く派生物である. `Change` の variant は増えない.
- `meta` に派生の表の版を記録し, `migrate` は版が合わないときだけ表を作り直す.
  - `migrate` は接続を開くたびに走る. leader の作業用の store を開くときや, `restore` の後にも走る.
  - そのたびに全件を走査すると, 選挙の後の再開と snapshot の install が, データベースの大きさに比例して遅くなる.

### 3.8 読出しと古い client への展開

- `read_committed` の closure の中では, 記録と marker の一覧を複製するだけにする. blob の読出しと展開は, lock を放してから行う.
  - 状態機械の mutex を持ったまま大きな blob を読むと, その間 Raft の適用が止まり, 書込みが `COMMIT_WAIT` を越えるからである.
- 展開を同時に行う数に, 上限 (semaphore) を置く.
- `JobRef.accept_blob_refs` を付けない `Result` には, marker を展開した記録を返す.
  - 上限は, 返す message 全体で 96 MiB とする. `MAX_MESSAGE_BYTES` の 128 MiB より余裕を持たせる.
  - 上限を超えるときは `FAILED_PRECONDITION` を返す. marker を値と取り違えさせないためである.
- Python の `Job.value()` は, Rust の `Value`, JSON の文字列, `json.loads` の順に変換する. 200 MiB の結果では数 GB のメモリを使う. 大きな結果には `Job.download(path)` を使うよう, 文書で勧める.

### 3.9 取得, 修復, 誤り

- follower は, `Node::scheduler()` を通らない経路で `GetBlob` に答える.
  - 返すのは, 手元にあって hash が合う本体だけである.
  - 手元にないときは, leader の場所を添えて `UNAVAILABLE` を返す.
- `NOT_FOUND` と `DATA_LOSS` は, leader が `ensure_linearizable` を済ませた後でだけ返す. follower の派生の表は適用が遅れうるので, follower が返すと誤りうるからである.

  | 誤り | 条件 (leader が判定する) |
  |---|---|
  | `NOT_FOUND` | 登録されていない hash |
  | `UNAVAILABLE` | 登録されているが, 届く node のどれも本体を持っていない |
  | `DATA_LOSS` | 全 member が答え, 誰も本体を持っていない |

- `BlobRequest.offset` を持たせ, 途中で切れた転送を再開できるようにする.
- 修復.
  - `apply` で「参照されているが手元にない hash」を待ち行列に入れ, lock の外で取りに行く. `PushBlob` の間止まっていて, ログで追いついた follower も, これで本体を得る.
  - 投票者の過半数に届いていない blob を定期的に探し, 複製を補う.
  - snapshot を install した後は, `blob_refs` の全 hash を手元の file と比べ, 足りないものを後ろで取る. 状態機械の適用は本体を待たない (#21 と同じ原則).
  - `remove_member` は, その node を外すと過半数を割る blob があれば, 先に複製を補うか, 断る.
  - `algebramesh server status` に, 過半数に届いていない blob の数, quarantine の数, 空き容量を出す.
- `add_member`.
  - blob を参照する記録があるときは, learner として加えた時点で返す. 同期の進み具合は `server status` に出す.
  - 同じ `AddMember` をもう一度呼ぶと, ログと参照される blob の同期が済んでいれば, 投票者にする (Q24).
  - 今の `add_member` はログの追いつきを最大 60 秒待つ. blob の同期は何時間もかかりうるので, 同じ呼出しの中では待たない.
- 本体のない blob を引数に持つジョブでは, agent は CAS を起動せず, ジョブを `FAILED` にする. 誤りの文には hash を書く. 別の値で代えることはしない.

### 3.10 GC (最小限は B1 に含める)

- ジョブは消さないので, 参照された blob も消さない. 消すのは, 参照のない blob だけである.
- 参照のない blob は, B1 の時点から日常的に生じる. 例は次の 3 つである. そのため, 最小限の GC を B1 に含める.
  - LOST の後に届いた報告の blob.
  - 途中で切れた upload.
  - 投入に至らなかった upload.
- 「触れた」とは, その node での `PutBlob`, `PushBlob`, `StatBlob`, `GetBlob`, `ensure_blobs` を指す. 触れた時刻は, file の mtime を更新して残し, 再起動の後も保つ.
- 消す条件は次の 2 つで, 両方を満たしたときに消す.
  - 最後に触れてから, `blob_orphan_grace` が過ぎた. 既定は 24 時間で, `COMMIT_WAIT` より十分長くない設定は断る.
  - leader が `BlobsRegistered` で「登録なし」と答えた. leader は, 確定した状態と, 確定前の変更を含む作業用の store の両方を見る. `ensure_blobs` を通ったがまだ確定していない `Submit` の参照を見落とさないためである.
- 消すときは hash ごとの lock を取り, 触れた時刻をもう一度確かめる.
- B1 に含めるもの: 参照のない blob の削除, `.part` の掃除, `max_blob_store_bytes`.
- B3 に回すもの: token ごとの `PutBlob` の量の上限, 観測の指標, CLI, backup の手順.

### 3.11 上限 (既定値. 計測してから見直す)

| 名前 | 既定 |
|---|---|
| `INLINE_MAX_BYTES` (inline に残す合計) | 256 KiB |
| `max_blob_bytes` | 256 MiB. driver の出力の上限 (`max_output_bytes`) 以下とする. 上げるときは両方を上げ, agent のメモリの見積りを文書に書く |
| chunk | 4 MiB |
| `blob_min_free_bytes` | 1 GiB |
| 1 ジョブあたりの marker | 1,024 個 |
| 古い client に展開して返す message | 96 MiB |
| `blob_orphan_grace` | 24 時間 |

`MAX_SPEC_BYTES` と `MAX_RESULT_BYTES` は 8 MiB のまま残す.

### 3.12 互換

- agent は, `RegisterRequest.features` に `blob_refs` を申告する. marker を含むジョブは, `Placement.needs_blob_refs` によって, 申告した agent にだけ割り当てる.
- 古い client には, §3.8 の展開で答える.

### 3.13 hash の意味

- request の fingerprint と W4 の `input_key` は, 「内容の hash の形」から作る.
  - この形は, spec の `arguments` の各 top-level の値を, その canonical JSON の SHA-256 に置き換えたものである.
  - marker の値は `$blob` の hash に置き換える. `size` と `from` は含めない.
  - したがって, 同じ値は inline でも blob でも同じ fingerprint になる. 送り手の `INLINE_MAX_BYTES` にも左右されない.
- `jobs` の行ごとに `fingerprint_version` を記録する.
  - 既存の行は, 版 1 (今の式) とする.
  - 新しい request は, その行の版の式で比べる.
  - marker を含む request を版 1 の行と比べることになったら, 理由を添えて断る. これが起きるのは, 版の更新をまたぐ場合だけである.
- `input_sha256` は, 記録した spec (marker を含む) の canonical JSON の hash のままとする (Q9).
  - この値が閾値に左右されることを, ADR に書く.
  - ジョブの間の同一性には, fingerprint を使う.
- #36 の本文には「検算は store から読んだ値に対して行う」とある. これを次のように満たす.
  - agent は, 検査した値の canonical bytes をそのまま upload する. そのため, 記録される hash は検査した値と結び付く.
  - store から読み直して検査し直すのは, V4 (`algebramesh verify`) と V5 (検証ジョブ) である.
  - この読み替えでよいかを, 利用者に確かめる (Q26).

### 3.14 段階ごとの成果物

- B1.
  - `crates/algebramesh-core/src/blob.rs`: `BlobRef`, marker の読み書き, `find_markers`, `INLINE_MAX_BYTES`, §3.4 の規則.
  - `crates/algebramesh-server/src/blobs.rs`: 置き場所, 受信, 検証しながらの読出し, quarantine, 最小限の GC.
  - `crates/algebramesh-server/src/cluster/blob_sync.rs`: 過半数への push, 手元にないときの取得, 修復の待ち行列, 定期の補充, `add_member` と `remove_member` の扱い.
  - `store.rs`: 2 つの表, 派生の表の版, `fingerprint_version`.
  - `service.rs`: `ensure_blobs`, follower の `GetBlob`.
  - agent: 結果を外に出す. `deliver` を直す.
  - Rust の client と Python: marker を解決する. `Job.download(path)` を足す.
  - 設計書 §7.3 を更新する.
- B2.
  - client と Python が, 大きな引数を外に出す.
  - agent が, 実行の前に marker を解決する. hash と canonical JSON の両方を確かめる.
  - `features` と `needs_blob_refs` を使う.
- B3.
  - token ごとの量の上限.
  - 観測の指標.
  - CLI `algebramesh blob put|get|stat`.
  - blob を含めた backup と復元の手順 (`docs/install.md`).

### 3.15 受入条件 (#36 の 3 つの受入基準を含む)

1. 3 台の cluster (`crates/algebramesh-agent/tests/raft.rs` と同じ形) で, 試験用の backend が返す 200 MiB の結果が `SUCCEEDED` になる.
   - Rust の client と Python の `Job.download` が取り出した bytes の SHA-256 が, agent が upload した canonical bytes の SHA-256 と一致する.
   - Python の `Job.value()` は, 構造的に一致する.
   - leader を替えた後でも取り出せる.
2. Raft の項目の大きさ.
   - 1 のジョブで書かれる Raft の項目は, どれも 64 KiB 以下である. snapshot の大きさは, 結果の大きさに比例しない.
   - 250 KiB の top-level の値を 32 個持つ結果でも, Raft の項目はどれも 1 MiB 以下になる.
3. 全 node から本体を消すと, leader の `GetBlob` は `DATA_LOSS` を返す. その blob を引数に持つジョブは `FAILED` になり, 誤りの文に hash が出る. backend は呼ばれない.
4. Raft の過半数は保ったまま, blob の過半数だけを欠く構成を作る. follower を 1 台止め, もう 1 台の `blob_dir` を書けなくする.
   - `PutBlob` は `UNAVAILABLE` になる.
   - その marker を含む `Submit` では, ジョブが作られない.
   - `blob_dir` を直すと, 送り直しで成功する.
5. crash point を 3 つ置く (feature `fault-injection`): rename の前, 1 台目の follower に送った後, `BlobRef` を返す前. どこで止めても, 送り直せば成功し, `incoming` に file が残らない.
6. `BlobRef` を返した直後に leader を止めても, 新しい leader に同じ marker で送ると成功する. 必要なら, `UNAVAILABLE` と再送を経る.
7. ログを snapshot まで捨ててから 4 台目を加える. `AddMember` は learner として返る. 参照される blob がそろった後, 2 回目の `AddMember` で投票者になる.
8. leader の本体を 1 byte 変えると, `GetBlob` は peer から取り直した正しい bytes を返す. 壊れた file は quarantine に移る.
9. 空きが足りない node では, `PushBlob` が `RESOURCE_EXHAUSTED` になり, `raft stopped` にはならない. `tests/integration/run-disk-full.sh` に段を足して確かめる. その tmpfs は 64 MiB なので, `blob_min_free_bytes` を設定で下げる.
10. `max_blob_bytes` (256 MiB) の値を push する間, 既定の election timeout のままで term が変わらない.
    - `blob_dir` を同じ volume に置いたとき, Raft の SQLite の `fsync` がどれだけ遅れるかを計測し, `docs/benchmarks.md` に記録する.
    - 結果によっては, 別の volume に置くことを既定にする.
    - release build で, 手で流す試験とする.
11. 古い client への展開.
    - `accept_blob_refs` を付けない `Result` が marker を返すことはない.
    - 展開が 96 MiB を超えるときは `FAILED_PRECONDITION` になる.
    - 96 MiB の展開を行っている間も, 他の投入は `COMMIT_WAIT` 以内に確定する.
12. v1.2 の scheduler を 1 台含む cluster (Docker) では, `PutBlob` が `FAILED_PRECONDITION` か `UNIMPLEMENTED` になり, どの node も止まらない. 全台を更新すると, marker が使えるようになる.
13. GC.
    - 参照のない blob だけが, 猶予の後に消える. 参照された blob は消えない.
    - `ensure_blobs` を通ったがまだ確定していない `Submit` の blob も消えない.
14. 古い agent には, marker を含むジョブが割り当てられない.
15. scheduler が 1 台の構成で, 200 MiB の結果が peer なしで `SUCCEEDED` になる.
16. 適用が遅れている follower は, 確定したばかりの参照に `NOT_FOUND` を返さない.
17. 修復.
    - `PushBlob` の間止めていた follower が, 再起動の後に本体を得る.
    - 過半数を割る `remove_member` は断られる.
18. `crates/algebramesh-server/tests/model.rs` に次の不変条件を足し, 試験が通る.
    - 参照された hash は `blobs` にある.
    - GC は参照された本体を消さない.
    - 修復の後は, 投票者の過半数が本体を持つ.

### 3.16 Issue

- `[P2][design] ADR: content-addressed blob store outside the Raft log (#36)`.
- `[P1][storage] Content-addressed blob storage and majority replication for job results (#36, part 1)`.
- `[P1][client] Offload and resolve large results in the agent, the Rust client and Python`.
- `[P2][storage] Repair blob replicas after missed pushes and before removing members`.
- `[P2][storage] Large job arguments as blobs (#36, part 2)`.
- `[P2][storage] Quotas, metrics, CLI and backup for the blob store (#36, part 3)`.
- `[P3][test] Fault-inject blob transfers, missing blobs and a full blob disk`.

### 3.17 含まないもの

- 外部の object storage の実装. ADR 0021 で比べ, `BlobBackend` の境界だけを置く.
- 参照された blob の削除と, ジョブの保存期間.
- `UpdateJob` の差分化 (W5 で行う).

## 4. Phase 2: Workflow Engine

**目的.**
- 複数のジョブを DAG として繋ぐ. 中間結果は hash で受け渡す.
- 失敗や中断の後は, 終わった節を計算し直さずに再開する.
- 決定的な計算の結果は再利用する.
- 受け渡した値の保証を, 下流で失わないようにする.

**範囲.**

| | 内容 |
|---|---|
| 含む | 照会の RPC. client 側の DAG (Rust, Python, CLI). 出どころの検査. `signatures` と環による型検査. opt-in の cache. ログの重複の削減 |
| 含まない | scheduler 側の workflow object (W6 で条件付き). 実行中の同じ計算への合流. 数学的な正規形を cache の鍵にすること. 無効にした結果の後続を自動で無効にすること |

### W1. 照会の RPC [S-M. いつでもよい]

- `Scheduler.LookupRequest(request_id) returns JobStatus` を足す.
  - 確定した状態を読むだけで, ジョブは作らない.
  - 答えは「この request_id に答えるジョブ」と定める. W4 の後は, cache で再利用したジョブ (`requests` 表) も返す.
- `Scheduler.ListJobs(request_id_prefix, state, backend, operation, page_token, page_size)` を足す.
  - 絞込みは, 索引のある列だけで行う. 一意の索引 `jobs_request` に加えて, `state`, `backend`, `operation` の索引を `migrate` で足す.
  - 走査する行の数と, 応答の大きさに上限を置く.
  - closure の中では, 行を複製するだけにする.
- client は, 古い scheduler の `UNIMPLEMENTED` を「機能なし」と扱う.
- 受入条件.
  - どちらを呼んでも, ジョブの数は変わらない.
  - leader を替えても, 同じ答えが返る.
  - 1 万件のジョブがあっても, 1 page の応答は上限以下である. その間の他の書込みは, `COMMIT_WAIT` 以内に確定する.
- Issue: `[P2][api] Read-only LookupRequest and paged ListJobs over committed state`.

### W2. client 側の workflow と再開 [L. A0, B2, W1 の後]

- 足すもの: Rust の `algebramesh_client::workflow`, Python の `algebramesh.workflow`, CLI の `algebramesh workflow run|resume|status|cancel <file>`.
- 定義の中では, 他の節の出力を `{"$output": {"node", "pointer"}}` で参照する. `pointer` は top-level に限る.
- orchestrator は, 参照を次のように解決する.
  - `INLINE_MAX_BYTES` を超える値: `{"$blob", "size", "from": {"job_id", "pointer"}}`.
  - 小さな値: producer の記録の位置を指す `{"$ref": {"job_id", "pointer", "sha256"}}`. agent は `AgentGateway.GetRecord` で記録を取り, 値を解決して hash を確かめる. 辺ごとに `PutBlob` と過半数の `fsync` を行わずに済ませるためである.
- `ensure_blobs` は `from` と `$ref` を検査する.
  - 確かめるのは, producer が `SUCCEEDED` であることと, その記録の pointer の先が同じ hash を指すことである.
  - pointer は top-level なので, 大きな blob を読む必要はない.
  - agent は `from` を外してから backend に渡す.
- 節の `request_id` は `wf/<workflow>/<node>/<attempt>` とする. 128 文字を超える id は, 定義を読んだ時点で断る.
- 再開はトポロジカル順に進め, 各節で `LookupRequest` を呼んで, 状態に応じて扱う.

  | 状態 | 扱い |
  |---|---|
  | `SUCCEEDED` (cache の再利用を含む) | 完了とする |
  | 終わっていない | 待つ |
  | `LOST` で `retried_as` がある | `retried_as` を辿る |
  | `LOST` で `retried_as` がない. `FAILED`, `TIMED_OUT`, `CANCELLED` | 止まる. 利用者が選んだときだけ, `attempt` を 1 つ進めて投入する |
  | ジョブがない | 投入する |

  `retried_as` があるのは, `if_deterministic` で, 試行の回数が上限に達していないときだけである (`scheduler.rs:850-852`).
- 取消しは, orchestrator の手元の状態 file (lock を含む) に残す. こうすると, process をまたいで効く.
- W3 で固定した build が online にないときは, 止まって報告する (Q27).
- 保証の扱い.
  - consumer の主張の `depends_on` は, producer の主張を `job:<job_id>#<claim>` の形で指せる.
  - workflow の報告は, 節ごとの実効水準を出す.
  - 例: OSCAR の `ideal.membership` が, G に対する `membership.in` を `checked` にしたとする.
    - それを「f ∈ I」と読むには, M2 の `ideal.groebner_basis` の `gb.contained` が要る.
    - 「f ∉ I」と読むには, `gb.contained`, `gb.contains`, `gb.groebner` が要る.
- 受入条件.
  1. 3 節以上で 2 つの backend にまたがる DAG で, orchestrator を途中で kill してから再開する. prefix ごとのジョブの数は節の数を超えず, 最後の結果の SHA-256 は止めずに流した場合と一致する.
  2. leader を替えても完了する.
  3. 取り消した後は, orchestrator を再起動しても, 投入していない節は投入されない.
  4. 定義を変えた節は, `ALREADY_EXISTS` で断られる. 一方で, `INLINE_MAX_BYTES` の違う client で再開しても `ALREADY_EXISTS` にならない.
  5. `from` か `$ref` が producer の結果と食い違う参照は, `FAILED_PRECONDITION` になる.
  6. producer の主張が `asserted` なら, consumer の実効水準も `asserted` と報告される.
- Issue.
  - `[P2][workflow] Client-side DAG with deterministic request ids and resume`.
  - `[P2][workflow] Verify the provenance of job outputs passed by reference`.
  - `[P2][verify] Carry claim dependencies across workflow edges`.
  - `[P2][api] Let agents read a committed job record`.
- 実施 (#104, #105, #106, #107, [ADR 0032](adr/0032-client-side-workflows-and-references-to-job-outputs.md)). 上との違いは次のとおりである.
  - 参照には, 新しい機能 `workflow_refs` と B2 の `blob_arguments` の両方が要る. 辺を持つ workflow は, 機能のない cluster では投入せずに誤りを返す.
  - 結果の全体が 1 つの blob である記録の key を指す pointer は断る. 検査に本体を読まないためである.
  - 取消しは, 状態 file に書くのに加えて, 終わっていないジョブも取り消す.
  - 節をまたぐ主張の依存は, backend の記録ではなく workflow の定義の `depends_on` に書く. 報告がそれを `job:<job_id>#<claim>` として実効水準を求める. 1 つの記録の中の `job:` の依存は `asserted` と数える.
  - 再開では, 見つけたジョブにも同じ投入を送り直し, 定義の変更を `ALREADY_EXISTS` で見つける. `retry_failed` は, 1 度の実行の中で節ごとに 1 度だけ次の試行を投入する.
  - W3 で固定する build の確認は, W3 で足す.

### W3. backend 間の受渡しの型検査 [S-M]

- orchestrator は投入の前に, producer の結果の型名と consumer の引数の型名を, `signatures` で比べる. 環 (変数, 係数域, 順序) の一致も確かめる.
- 受渡しの規則.
  - `groebner_basis` を `ideal` として渡すことは許す. 順序の情報を落とすだけである.
  - 逆向きの受渡しと, 順序の違う `groebner_basis` の受渡しは断る.
  - `signature_conflicts` のある backend には渡さない.
- backend 固有の artifact (設計書 §6 規則 5) は, 同じ backend の同じ build にしか渡さない. orchestrator が consumer の `environment` を固定する.
- 表示の違い (OSCAR の行と列など) は, 各 backend の adapter が共通の JSON から変換する.
- 任意: `gkz.series` が `gkz.indicial` の結果を参照で受け取れるようにする (K0 の後).
- 受入条件.
  - 型の合わない DAG は, どの節も投入されずに断られる.
  - M2 の lex の basis を OSCAR の `ideal.normal_form` に渡す DAG が通り, Rust の検査も通る.
- Issue: `[P2][workflow] Check operation signatures and rings before submitting a DAG`.

### W4. 計算 cache [L. B0, W1, W2 の後]

- 鍵.
  - `input_key`: `{schema_version, backend, operation}` と, §3.13 の内容の hash の形から作る. limits, `retry_policy`, priority は入れない.
  - `env_key`: `build`, `backend_version`, `driver_version`, `driver_sha256`, `agent_build`, `verifier_version` から作る. どれも B0 で `BackendOffer` に載る.
- hit の条件. 次の 3 つをすべて満たすときに hit とする.
  - `input_key` が等しい.
  - `env_key` が, その request を置ける online な worker のどれかの `env_key` と等しい.
  - hit する記録が, 新しい request の `EnvironmentConstraints::allows` を満たす.
- `input_key` と, cache してよいかどうか (`cacheable_operations` に入るか) は, leader が投入と完了の時点で決めて記録に書く. この欄は `record_v2` で守る. `apply` は欄を写すだけで, hash を計算しない (W5 と同じ規則).
- 次の結果は cache しない.
  - build が digest でも commit でもないもの.
  - `agent_build` が不明なものか, 作業ツリーが変更されていたもの (Q28).
- cache を使うのは, 次の 2 つの宣言がそろったときだけである.
  - backend の宣言 `Capabilities.cacheable_operations`.
  - 投入時の選択 `SubmitRequest.cache`. 値は `NEVER` (既定) か `REUSE` である. `NEVER` のときは fingerprint を変えない.
- 表.
  - 派生の表 `cache_entries`. `apply` が記録の欄を写して作る. 既存の記録からは `migrate` で作る.
  - 表 `requests`. 新しい `Change` になるので, B0 の合意の後でだけ使う. `LookupRequest` はこの表も引く.
- hit したときは, 元のジョブを `JobStatus.cache_hit = true` で返し, 元の `guarantee` もそのまま返す.
- 結果の blob が取れない hit は, miss として扱い, 計算し直す.
- CLI: `algebramesh cache invalidate --job|--build|--backend --operation|--verifier-version`.
- 受入条件.
  1. 2 回目の投入は `cache_hit` になり, 元の job id が返る. `LookupRequest` でもそのジョブが見える.
  2. limits だけが違う場合は hit する.
  3. driver を書き換える, verifier の版を変える, 無効にする, のどれをしても miss になる.
  4. digest でない build の結果は hit しない.
  5. `LOST` の後の再試行が成功すると, その再試行が entry になる.
  6. 新しい request の `EnvironmentConstraints` を満たさない記録には hit しない.
  7. `"2/4"` と `"1/2"` のように数学的に同じ値でも, 別の鍵になる. miss が起きるだけで, 誤った hit は起きない. このことを文書に書く.
- Issue.
  - `[P2][cache] Opt-in result cache keyed by input and environment`.
  - `[P3][cache] Invalidate cached results by job, build, operation or verifier version`.

### W5. `UpdateJob` の差分化 [M. B0 の後]

- `UpdateJob` の `StoredRow` から `spec` と `input_normalized` を除き, `apply` で既存の行と合わせる.
- `input_normalized` を `apply` の中で計算し直すことはしない. `serde_json` の版によって bytes が変わり, node の間で状態が食い違うおそれがあるからである.
- 受入条件.
  - 着手するときに, 引数がログに現れる回数を測る (見込みでは 8 回). これを 2 回に減らす.
  - 合意した機能に含まれないときは, この形を使わない.
- Issue: `[P3][storage] Store job updates as differences once all members agree`.

### W6. 条件付き

- scheduler 側の依存 (`after`) と, 永続の取消し `CancelWorkflow(prefix)`.
- client なしで長く進む workflow が必要だと分かったときだけ入れる. どちらも新しい `Change` になるので, B0 の合意を使う.

### Phase 2 の完了条件

- W1-W5 の受入条件が通る.
- ADR 0023 (workflow) と ADR 0024 (cache) が採用されている.

## 5. Phase 3: Verification Framework

**目的.**
- 計算する側と検査する側を, 3 つの面で分ける.
  - コードの独立: A1 で済む.
  - 呼出しの独立: 既存の operation では V2 で済む.
  - process と host の独立: V5 で行う.
- operation ごとの保証の範囲を, 機械で読める形で返す.

**範囲.**

| | 内容 |
|---|---|
| 含む | 既存の operation の主張の一覧と契約. 既存の検算の移設. 安く上げられる主張の格上げ. 古い記録の写像. 再検証の CLI. 検証ジョブ. 照合. 検証用の permit |
| 含まない | Rust による Groebner basis の計算や簡約. 収束と基底であることの証明 (`not_claimed`). 検査器の形式検証. 系統の自動判定 |

### V1. 主張の一覧, 契約, CI [M. A1 の後]

- `docs/claims.md` を作る.
  - 既存の 4 つの operation について, 各主張の次の項目を書く: ID, 命題 (数式), 範囲, 前提, 水準, `depends_on`, 既存の欄との対応, 水準を上げる方法.
  - 新しい operation は, それぞれの milestone で行を足す.
  - 既存の operation の主張の ID は次のとおりである.
    - factor: `factor.product`, `factor.domain`, `factor.primitive`, `factor.non_associate`, `factor.irreducible`.
    - generate: `gkz.euler`, `gkz.toric_in_IA`, `gkz.toric_generates_IA`, `gkz.generated_by_toric_and_euler`, `gkz.membership[j]`.
    - indicial: `local.exponent_equations`, `local.exponent_roots`, `local.indicial_ideal`, `local.roots_complete`, `local.rank`, `local.rank_sum`, `local.parameters`.
    - series: `series.shape`, `series.euler_exact`, `series.toric_to_order`, `series.not_a_solution`, `series.canonical`, `series.log_independent`, `series.missing`.
- `depends_on` を次のように定める.
  - `series.toric_to_order` は `gkz.toric_generates_IA` に依存する. `check_series` は CAS が返した toric の生成元を作用させるので, 「H_A(β) の解である (次数まで)」の実効水準は, 今は `asserted` になる.
  - `local.exponent_roots` は `local.indicial_ideal` に依存する. `exponents_are_roots` が確かめるのは, CAS が返した indicial の生成元の共通零点であることだけである.
  - `series.not_a_solution` は `gkz.toric_in_IA` だけに依存する. I_A に属する 1 つの元で消えないことを示せば足りるので, 実効水準も `checked` になる.
- `generated_by_toric_and_euler` を確かめる. M2 の `gkz` が `toricIdealPartials` から ideal を作るかを, image の中で確かめる. 作るなら, この比較は `gkz.toric_generates_IA` の独立な根拠にならないと `docs/verification.md` に書く.
- `docs/verification.md` に, 名前の誤読を正す説明を書く. 例: `verified_product` が確かめているのは, 積の一致だけである.
- CI.
  - `checked` の各行に, 変異の試験の名前の欄を持たせる. 欄が空なら CI を落とす.
  - 変異は §1.3 の規則に従う.
- 完了の条件に, 利用者が各 operation の契約の命題を確かめることを含める.
- 受入条件: 既存の欄 (`verified_product`, `checks.*`, `status`, `canonical*`, `missing`) が, すべて一覧のどれかの主張に対応する. 一覧を走査する単体試験で確かめる.
- Issue.
  - `[P2][verify] Catalogue the claims and contracts of every operation`.
  - `[P3][test] Require a falsifying mutation test for every checked claim`.

### V2. GKZ の検算を package の crate に移す [M-L. K0 と同じ一連の PR]

- `backend-macaulay2/src/lib.rs` にある GKZ の判定を, `algebramesh-gkz` が実装する `OperationVerifier` に移す. 対象は `euler_operators_match`, `toric_binomials_valid`, `check_exponent`, `check_series`, `normalize` である.
- backend は, CAS の出力を型に変えるところまでを行う.
- `normalize` の `solve` は, §1.2 の (a) に移す. M2 に組合せの係数を出させ, Rust は代入して確かめる. M2 で係数を出せなければ (b) のまま残し, ADR 0019 に記録する.
- `generated_by_toric_and_euler` と `expected_in_ideal` は, `asserted` に写す.
- 受入条件.
  - T0 で, `guarantee` を除いて同じ値が出る.
  - 長い検証は `TIMEOUT` になる.
- Issue: `[P2][verify] Move the GKZ checks out of the Macaulay2 backend`.
- 実施 (#99, [ADR 0028](adr/0028-gkz-package-and-its-checks.md)). 上との違いは次のとおりである.
  - 値を組み立てる側 (`algebramesh_gkz::value`) が各級数の状態と標準形を決め, 検査器が spec と値だけから確かめ直す. backend の glue は, driver の答えを型に変えるだけである.
  - 値だけから `series.not_a_solution` を確かめ直せるように, `not_a_solution` の項目に CAS の級数を `evidence_terms` として置いた. T0 の比較で, 値の差はこれだけである.
  - `normalize` の `solve` は (b) のまま残した (§1.2).
  - `generated_by_toric_and_euler` と所属の判定は, `asserted` の `gkz.generated_by_toric_and_euler` と `gkz.membership[j]` に写した.

### V3. 安く上げられる主張を上げる [M]

- `factor.non_associate`: 比例の判定で確かめる.
- `series.log_independent`.
  - 【現状】では, M2 が係数行列の rank で判定している (ADR 0011).
  - これを A2 の rank の certificate で確かめる. M2 に m 次の正則な小行列の位置と, その逆行列を出させる. B B' = I から rank ≥ m が得られる. 列は m 個なので, rank ≤ m は自明である.
  - 行列の行の数は, 先頭部分に現れる対数の単項式の数で決まる.
- `gkz.membership` の「属する」側.
  - 左からの係数を M2 に出させ, `DifferentialOperator::mul` で確かめる.
  - 係数は, Rust が H_A(β) への所属を確かめた生成元に対して取る. つまり `toric` (`toric_binomials_valid` で確認) と `euler` (`euler_operators_match` で確認) である.
  - こうすれば, `generated_by_toric_and_euler` に依存せずに `checked` になる.
  - M2 の Weyl algebra で係数を取る手続きは要確認である.
- 導入は 2 段で行う.
  1. 水準を記録するだけにする.
  2. 回帰試験で誤って断る例がないことを確かめてから, 偽なら失敗する主張に上げる.
- 受入条件: 主張ごとに変異の試験がある.
- Issue: `[P3][verify] Raise non-associate factors, logarithmic independence and GKZ membership to checked`.
- 実施 (#102, [ADR 0029](adr/0029-cheap-certificates-for-three-claims.md)). 3 つとも `checked` にした. 上との違いは次のとおりである.
  - `series.log_independent` の certificate は, 選んだ行を小行列の位置ではなく, 行の log の冪 (`log_powers`) で示す.
  - 2 つの段階は 1 つの変更で行った. 断る形で回帰試験を流し, 誤って断る例がないことを確かめた.
  - `gkz.membership[j]` の「属さない」側は `asserted` のまま残した.

### V4. 古い記録の写像と再検証 [M. B1 の後]

- `legacy_guarantee(operation, value, driver_version)` を作る.
  - 既存の 4 つの operation の古い記録を, 同じ水準に写す.
  - 新しい operation の記録で `guarantee` を欠くものは, すべての主張を `asserted` とし, 理由を書く.
- CLI `algebramesh verify <job-id>` を作る.
  - 記録と certificate の blob を取り, hash を確かめ, 手元の verifier で検査し直して, 主張の表を出す.
  - certificate を省いた記録には「再検証できない」と答える. 別の値は出さない.
- 受入条件.
  - 古い記録と新しい記録で, 同じ結果に同じ水準が出る.
  - 1 byte 変えた blob は, hash の誤りになる.
  - 本体のない blob は `DATA_LOSS` になる.
  - 元の記録は変わらない.
- Issue: `[P2][verify] Map old records to guarantees and recheck a job from the blob store`.
- 実施 (#108, [ADR 0031](adr/0031-mapping-old-records-and-rechecking-a-job.md)). 上との違いは次のとおりである.
  - `legacy_guarantee` は `Registry` の method とし, 写像は検査器ごとに持たせた (`OperationVerifier::legacy_claims`). GKZ の検査器が package の crate にあるからである.
  - 新しい operation の記録で `guarantee` を欠くものは, 主張 `value` 1 つを `asserted` とした. spec なしでは, 値がどの主張をするかが決まらないからである (例: `solve.solution` か `solve.no_solution`). 主張ごとの水準は `algebramesh verify` が出す.
  - 値だけから確かめられる `factor.domain`, `factor.primitive`, `factor.non_associate` は, 対応する欄がないので, 写像の中で確かめて `checked` とした.
  - certificate と `evidence_terms` を持たない古い値では, それらが支える主張を `asserted` とした. 再検証も同じ水準を出す (`Context::recheck`).
  - hash を確かめる対象に, 記録の `input_sha256` を加えた.

### V5. 検証ジョブと permit の分離 [L. B1, W1, `AgentGateway.GetRecord` の後]

- CAS を持たない backend `verify` を agent に置き, operation `verify.recheck` を受け持たせる.
  - 仕様には, 対象の job id と結果の SHA-256 だけを入れる.
  - agent は `AgentGateway.GetRecord` で記録を取る. 今の `AgentGateway` には, 記録を読む RPC がない.
  - 元の記録は書き換えず, 検証の結果を新しい記録として残す.
- `algebramesh_core::check` の permit を, ジョブ用と検証用に分ける.
- verifier の版で記録を探せるよう, `migrate` で索引付きの派生の列 (`verifier_version`) を足す. W1 の `ListJobs` の引数では, この検索はできない.
- 受入条件.
  - 検証ジョブが `TIMEOUT` になっても, 元の記録は変わらない.
  - 重い検証を流しても, 他のジョブが permit を待って `TIMEOUT` になることはない.
- Issue.
  - `[P2][verify] Verification jobs that recheck stored results`.
  - `[P3][resource] Separate permits for verification`.

### V6. 照合 [L. V5 と W2 の後]

- `Capabilities.lineage` で, 実装の系統を申告させる. 申告は人が保守する. 系統が重なる組は `cross_checked` にしない.
- 共有部分の一覧は, `docs/licenses.md` を起点に作る.
  - Singular (factory を含む): agent-macaulay2 の image の libsingular4m4n1 と, OSCAR の Singular.
  - normaliz: 両方の image にある.
  - 4ti2, TOPCOM, cddlib, lrslib, polymake, FLINT, GMP.
- 比べる形は 2 つである.
  - 一意な形は, 構造的に比べる. 対象は reduced Groebner basis, HNF, SNF, Betti 表, monic な多項式, 数, 同じ順序の reduced Groebner basis で表した付随素イデアルの集合である.
  - それ以外は, 表示の係数で比べる.
- 投入は W2 の workflow で組み立てる. 比較と記録は `verify.cross_check` が行う.
- 最初の対象.

  | 主張 | 組 | 要確認 |
  |---|---|---|
  | `factor.irreducible` | asir と macaulay2. M2 と OSCAR の組は factory を共有しうるので数えない | M2 の `factor` が factory を使うか |
  | `local.rank` | macaulay2 と, OSCAR 経由の Singular | Singular_jll に dmodloc.lib が含まれるか |
  | `gkz.toric_generates_IA` | M2 (4ti2) と, Singular の toric.lib | toric.lib が 4ti2 を使わないか |

- 受入条件.
  - 系統が重なる組は `cross_checked` にならない.
  - 一致しないときは, 両方の job id を添えて `cross_check_failed` を記録する. 元の記録は変えない.
- Issue.
  - `[P2][verify] Declare implementation lineage per operation`.
  - `[P2][verify] Cross-check jobs between independent CAS`.

### V7. 任意 (必要が出てから)

- 確率的な検査 `checked_probabilistic` (Q7).
  - Schwartz-Zippel の方法と Freivalds の方法を使う.
  - 素数と点は, certificate を受け取った後に検査器が選ぶ.
  - 法 p で検査するときは, 差の係数の高さの上界を入力と certificate の大きさから求めないと, 誤る確率を評価できない. 上界の求め方を先に ADR で決める.
  - 素数, 種, 回数, 誤る確率の上界を記録する.
- 別の process で動く検査器 `algebramesh-verifier`. `algebramesh_core::driver` とメモリ上限 (ADR 0014) の下で起動する. certificate が大きくなったときだけ入れる.
- syzygy の完全性を `checked` にすること.
  - G = F C, F = G N のとき, Syz(F) は C·Syz(G) と I - C N の列で生成される.
    - F (I - C N) = F - G N = 0 なので, I - C N の列は Syz(F) に入る.
    - u ∈ Syz(F) なら N u ∈ Syz(G) であり, u = C (N u) + (I - C N) u と書ける.
  - Syz(G) は, G の全ての対の lcm representation から, Schreyer の構成で得る.
    - 先頭単項式が互いに素な対も除けない. その表示は tail(g_j) g_i - tail(g_i) g_j の形の明示の式で, Rust が組める.
  - CAS の S がこれらを生成することは, 表示の係数を certificate として確かめる.
  - 水準は `gb.groebner` の水準に依存する.

### Phase 3 の完了条件

- すべての operation の結果が `guarantee` を持ち, 契約が確認されている.
- backend の crate に判定のコードが残っていない.
- ADR 0025 (検証ジョブと照合) が採用されている.

## 6. Phase 4: 数学分野ごとの研究パッケージ

### 6.1 package の形 ([ADR 0028](adr/0028-gkz-package-and-its-checks.md). 提案の番号 0026 は #68 が使った)

| 部分 | 置き場所 |
|---|---|
| crate `algebramesh-<name>` | 引数と結果の型, `OperationDescriptor`, `OperationVerifier` の実装, 主張の一覧の行. IO は持たない. 依存してよいのは math-types, ops, verify と, より下の package だけ |
| backend の glue | `crates/algebramesh-backend-<cas>/src/packages/<name>.rs`. Cargo feature `pkg-<name>` で入れる. agent も同じ feature で組む |
| driver | `drivers/macaulay2/<name>.m2`, `drivers/oscar/<name>.jl`. 共通の driver から読み込む. `driver_sha256` は読み込む全 file から計算する |
| Python | `algebramesh.<name>` |
| 回帰試験 | `tests/regression/cases/<name>/` |
| 文書 | `docs/packages/<name>.md` (保証と `not_claimed`). package ごとの ADR (適用の条件を書く) |

- 動的な読込みはしない. agent の `BackendConfig` も変えない.
- K0 の前に, feature ごとに build と試験をする job を CI に足す. 今の CI は `--all-features` でしか試していない.
- 全 package に共通の受入条件.
  - package を 1 つ足しても, `algebramesh-server`, `algebramesh-core`, `algebramesh-rpc`, `proto/` の file は 1 つも変わらない.
  - feature を外しても, build と core と server の試験が通る. そのとき, `Capabilities` にその package の operation は現れない. CI の feature ごとの job で確かめる.
  - `checked` の行には変異の試験があり, `cross_checked` の行には独立な参照がある.
  - 検査の仕事量に, 数の上限がある.
  - package は 2-4 個の operation から始める.

### 6.2 順序 (Q15)

K0 GKZ, K1 topology, K2 combinatorics, K3 D-module, K4 algebraic geometry の順とする. 理由は次の 3 つである.
- topology と combinatorics は A2 の SNF だけで済み, 主張の多くを `checked` にできる.
- 正規化体積は, GKZ の holonomic rank を定理で照らし合わせる手段になる.
- D-module は A3 と Weyl algebra の拡張を要する. algebraic geometry は照合に頼る部分が大きい.

### K0. GKZ [M-L. A1, V1, CI の feature ごとの job の後. V2 と同時に行う]

- `crates/algebramesh-backend-macaulay2/src/lib.rs` の GKZ の部分を, `src/packages/gkz.rs` (`pkg-gkz`) に移す.
- `drivers/macaulay2/algebramesh.m2` を, 共通の部分と `gkz.m2` に分ける.
- Python の `Client.gkz`, `gkz_indicial`, `gkz_series` は残し, `algebramesh.gkz` から呼ぶ.
- 受入条件.
  - T0 で, GKZ の値が変わらない.
  - `pkg-gkz` を外した agent は, `gkz.*` を申告しない.
- Issue.
  - `[P2][package] Package boundary for research areas, starting with GKZ`.
  - `[P2][ci] Build and test each package feature on its own`.
- 実施 (#100, #101, [ADR 0028](adr/0028-gkz-package-and-its-checks.md), [GKZ の package](packages/gkz.md)). §6.1 との違いは次のとおりである.
  - 引数と結果の型の記述子は `algebramesh-ops` に残した. scheduler が投入を受けるときに引くので, crate に移すと server が package に依存する.
  - backend の `pkg-gkz` は既定で外れ, agent の `pkg-gkz` は既定で入る. agent が backend の feature を入れる.
  - `gkz.m2` は, `algebramesh.m2` の隣にあるときだけ読み込まれる.
  - CI の job `packages` は, backend と agent を feature なしで clippy にかけ, 試験する.

### K1. topology [M. A2 の後]

- operation `simplicial.homology` を足す. 係数は ZZ で, A5 の後は GF(p) も扱う.
- 境界行列 ∂_k は, package の crate が facet から組み立てる. 面を列挙し, 符号を付ける.
  - k 次元の facet 1 つから, 2^{k+1} - 1 個の面が出る. そのため面の数に上限を置き, 超える入力は断る.
- backend の glue は, A2 で作った SNF の手続きを, 1 つのジョブの中で行列ごとに呼ぶ. 新しい CAS の算法は書かない.
- 1 つのジョブにするのは, ホモロジーを 1 つの記録と `guarantee` にまとめるためである.
- 主張.
  - ∂_k ∂_{k+1} = 0 と, 各 SNF の certificate は `checked` である. これらから H_k (Betti 数と torsion) が決まるので, 契約の確認の後は要約が `verified` になる.
  - Euler 標数の整合は, `consistency` として確かめる.
- 受入条件.
  - 四面体の境界で H_2 = ZZ になる.
  - 7 頂点のトーラスで H_1 = ZZ^2 になる.
  - 6 頂点の実射影平面で H_1 = ZZ/2, H_2 = 0 になる.
  - SNF の成分を 1 つ変えると断られる.
  - 面の数の上限を超える入力は断られる.
- Issue: `[P3][topology] simplicial.homology with Smith normal form certificates`.
- 実施 (#103, [ADR 0030](adr/0030-topology-package-and-simplicial-homology.md), [topology の package](packages/topology.md)). 上の記述との違いは次のとおりである.
  - 引数の型と記述子は, K0 と同じ理由で `algebramesh-ops` に置いた. 値の型は crate に置き, 記述子では JSON とした.
  - glue は Macaulay2 の backend にだけ置いた. OSCAR の glue は, 照合 (V6) を足すときに置く.
  - 面の数の上限は `MAX_FACES` = 512 とし, 列挙の前に facet の部分集合の数を `MAX_FACE_WORK` = 16384 で抑える.
  - Euler 標数は, 主張 `homology.euler_characteristic` (f_k からの数え直し. `recomputation`) とし, ホモロジーの階数の交代和との比較を `consistency` とした. `homology.groups` が成り立てば両者は恒等的に等しい.
  - Smith 標準形の主張は, 行列ごとの家族にせず, 全ての ∂_k についての `homology.snf_form` と `homology.snf_equivalent` の 2 つにした. `homology.groups` の depends_on に名前で書けるようにするためである.

### K2. combinatorics [L. A2, V1 の後]

- 着手の前に確かめること.
  - `docs/licenses.md` によれば, TOPCOM, normaliz, polymake, lrslib, cddlib は agent-oscar の成果物にあり, normaliz は agent-macaulay2 の image にもある.
  - したがって調べるのは, OSCAR からこれらを呼べるかだけである.
  - 新しい package を image に足す場合に限り, ADR 0008 の手順でライセンスを監査し, `deploy/notices/` を更新する.
- vol(A) の定義.
  - vol(A) は, conv({0} ∪ A) の正規化体積を, 格子 ZA に関して測ったものとする. `tests/regression/run.py:64-67` と同じ定義である.
  - 斉次でない A では, 0 を頂点に持つ単体が要る.
  - 格子の指数 [ZZ^d : ZA] は, A の SNF の certificate から, 不変因子の積として得る.

| operation と主張 (ID は提案) | 検査 | 水準 |
|---|---|---|
| `polytope.vertices` | 頂点ごとに, それを厳密に分離する線形汎関数を確かめる. 頂点でない点ごとに, 凸結合の係数を確かめる | `checked` |
| `triangulation.is_triangulation`: 三角形分割である | 各単体が退化していない. 擬多様体の条件 (内部の ridge は反対側にある 2 つの単体に共有され, 境界の ridge はちょうど 1 つの単体に含まれる. 境界の ridge であることは, 支持超平面の法線を certificate にする). 検査器が選んだ generic な 1 点が, ちょうど 1 つの単体に含まれる. 根拠は De Loera-Rambau-Santos, Triangulations §4.5 の特徴づけ (番号は要確認) | 命題の確認の後は `checked`. それまでは `asserted` |
| `triangulation.regular`: 与えた高さで正則である | 内部の各 ridge で, 厳密な折れの条件 (folding condition) が成り立つ. 頂点に使われない点の高さが, 区分線形関数より厳密に上にある | 命題の確認の後は `checked`. それまでは `asserted` |
| `polytope.normalized_volume` | 極大単体の \|det\| の和を, 格子の指数で割る. `depends_on` は `triangulation.is_triangulation` だけで, 正則性には依存しない | 前提に従う |
| `polytope.lattice_points` | 各点が入力の頂点の凸結合で表せることを, 係数を certificate にして確かめる. CAS が返した不等式は使わない | 健全性は `checked`. 完全性は, 系統の違う 2 つの実装で `cross_checked` |
| 正規化体積と holonomic rank | β が GKZ の意味で nonresonant (`parameter_report` で `resonant == false`) なら, rank = vol(A) (Adolphson 1994). 照らし合わせる相手は holonomic rank の主張である. 今の `local.rank` との関係は ADR 0015 に従い, 要確認とする. `depends_on`: facet の完全性 (`recomputation`), `polytope.normalized_volume`, 格子の指数の SNF certificate | `method: theorem`. 命題の確認の後は `checked` |
| 斉次な A での下界 | 全ての β で rank ≥ vol(A) (Saito-Sturmfels-Takayama Theorem 3.5.1. 要確認) | `consistency`. 下回れば結果を断る |

- Adolphson 1994 については, 定理番号, 斉次性が要るか, semi-nonresonant に広げられるかを確かめる (Q31).
- TOPCOM は M2 と OSCAR の両方が使いうる. そのため三角形分割は, 照合ではなく certificate で確かめる.
- 検査の仕事量 (単体の数, ridge の数) に上限を置く.
- 受入条件.
  - A = [[1,1,1],[0,1,2]] で, 正規化体積が 2 になる.
  - 回帰試験の GKZ の A について, vol(A) が `tests/regression/run.py` の計算と一致する.
  - 特定の内部の ridge の折れの条件を破るように高さを変えると, `triangulation.regular` が断る.
    - 正則な三角形分割を誘導する高さの集合は開集合なので, 任意の小さな変更で断られるとは限らない.
    - そのため, 試験では条件を破る変更を固定する.
  - 擬多様体の条件を破る単体の集合 (重なる 2 つの単体など) は, `triangulation.is_triangulation` で断られる.
- Issue.
  - `[P3][deploy] Check that OSCAR can call TOPCOM and normaliz in the agent image`.
  - `[P3][polytope] Triangulations and normalized volume with checked certificates`.
  - `[P3][verify] Check the GKZ rank against vol(A) by theorem`.

### K3. D-module [L-XL. A3 の後]

- `WeylAlgebraSpec` に, 可換なパラメータ s を足す.
- Weyl algebra では, Buchberger の第 1 判定法が成り立たない.
  - 反例: D_1 で f = x, g = ∂ とすると, 先頭単項式は互いに素だが, S(f, g) = ∂x - x∂ = 1 は 0 に簡約されない.
  - したがって, A3 の `s_pairs` の検査はそのまま使わない.
  - 「属さない」を `checked` にする方法は, 別の ADR で定める.

| operation | 検査 | 水準 |
|---|---|---|
| `dmodule.membership` (属する) | 左からの係数 | `checked` |
| `dmodule.annihilator` (Ann f^s) | 各生成元 P について, P · f^s = 0 を形式的に計算して確かめる | 消すことは `checked`. 生成することは `cross_checked` |
| `dmodule.b_function` | 関数等式 P(s) f^{s+1} = b(s) f^s の作用素 P(s) を CAS に出させる. Rust は作用を形式的に計算する. 使うのは多項式の偏微分, 積, 和だけである | 関数等式は `checked`. したがって b_f \| b も `checked` (`method: theorem`) |
| | b = Π (s + r_i)^{m_i} の形. CAS が出した分解を積で確かめる | `checked` |
| | 必要条件: monic であること, f が定数でなければ (s+1) \| b であること, 根が負の有理数であること (柏原) | `consistency`. 満たさなければ断る. 満たしても水準は上げない |
| | b = b_f (最小性) | `cross_checked` (M2, Singular, Asir) |
| `dmodule.holonomic_rank` | なし | `cross_checked` だけ |

- 関数等式の検査の費用.
  - 検査では ∂^α f^{s+1} = f^{s+1-|α|} Q_α を展開する.
  - Q_α の項数は n, |α|, deg f について増えるので, P(s) の大きさの多項式では抑えられない.
  - そこで仕事量の上限を数で置く. 超えたら関数等式の主張を `asserted` にし, 理由を記録する.
- backend.
  - M2 の Dmodules: `globalBFunction`, `AnnFs`, `holonomicRank`. P(s) を返す関数の候補は `globalB` の `Boperator` である (要確認).
  - OSCAR 経由の Singular: dmod.lib と dmodloc.lib. P(s) を返す関数の候補は `operatorBM` である. 呼出しの形と, library が含まれるかは要確認である.
  - asir: `bfct`, `ann`. CMO の分散多項式 (設計書 §5.3 の第 2 段) が前提になる. ox_sm1 が image にあるかも要確認である.
- 受入条件.
  - f = x で b(s) = s + 1 になる.
  - f = x^2 + y^2 で b(s) = (s + 1)^2 になる.
  - f = y^2 - x^3 で b(s) = (s + 1)(s + 5/6)(s + 7/6) になる.
  - P(s) を 1 項変えると断られる.
  - holonomic rank が `tests/regression/references/holonomic_rank.sing` と一致する.
- Issue.
  - `[P2][math] Weyl algebras with a commuting parameter`.
  - `[P3][dmodule] b-functions checked through the functional equation, and Ann f^s`.
  - `[P3][verify] Cross-check holonomic ranks`.

### K4. algebraic geometry [L-XL. A3, A4, A6, V6 の後]

- 分解する体は QQ とし, 契約に書く (Q32). 代数閉包の上の分解 (絶対分解) は範囲外である.
- 埋め込まれた準素成分は一意でない. 例えば (x^2, xy) = (x) ∩ (x^2, y) = (x) ∩ (x^2, xy, y^2) である. そのため, Q_j の一覧は CAS どうしで比べない.

| 主張 | 検査 | 水準 |
|---|---|---|
| I ⊆ Q_j | 表示の係数 | `checked` |
| I ⊆ ∩ Q_j | 上の主張から得る | `checked` |
| P_j の生成元の冪が Q_j に属する (P_j ⊆ √Q_j) | 表示の係数 | `checked` |
| ∩ Q_j ⊆ I | 一方の成分の共通部分を他方の CAS に計算させ, reduced Groebner basis で I と比べる | `cross_checked` |
| 付随素イデアルの集合, 孤立成分 | 同じ順序の reduced Groebner basis で構造的に比べる. 組は M2 と Asir (OSCAR と M2 は Singular を共有しうる). Asir の Groebner basis は #94 で足す | `cross_checked` |
| Q_j が準素であること, P_j が素であること | なし | `asserted` |
| toric 多様体が滑らかであること | 各極大錐について, 原始的な ray を最初の k 列に持つ整数行列 U と, その整数の逆行列 V (U V = I) を certificate にする | `checked` |
| class group | Cl = coker(M → ZZ^{Σ(1)}) を SNF の certificate で確かめる (Cox-Little-Schenck Theorem 4.1.3. 要確認). ray が N_R を張る必要はない | `checked` |
| Picard group | 滑らかなときだけ Pic = Cl とする. それ以外は範囲外 | 前提に従う |
| 交叉数 | 実行時の検査はしない. 既知の値 (3 次曲面上の 27 本の直線) との比較は, 回帰試験として置く | `asserted` |

- operation (名前は提案): `ideal.primary_decomposition`, `ideal.minimal_primes`, `toric.*`.
- 受入条件.
  - (x^2, xy) で, 付随素イデアルが (x) と (x, y) になる.
  - 変異の試験がある.
  - 照合を記録する.
- 範囲外: 一般の層コホモロジー, 特異点解消, 正規化.
- Issue.
  - `[P3][ag] Primary decomposition with checked containments and cross-checked associated primes`.
  - `[P3][ag] Toric variety properties with unimodular and Smith normal form certificates`.

## 7. 依存, 並行, 版, 見積り

### 7.1 依存

```text
T0 -> A0 -> A1 -> A2 -> A3 -> A4 -> (A6)
                        A3 -> A5
B0 -> A1                   (A1 の出荷には record_v2 が要る)
B0 -> B1 -> B2 -> B3
W1                         (いつでもよい)
A0, B2, W1 -> W2 -> W3
B0, W1, W2 -> W4
B0 -> W5 -> (W6)
A1 -> V1 -> V2 = K0 -> V3
B1 -> V4
B1, W1, GetRecord -> V5 -> V6 (W2 の後) -> (V7)
CI の feature ごとの job -> K0
A2 -> K1
A2, V1 -> K2
A3 -> K3
A3, A4, A6, V6 -> K4
```

`AgentGateway.GetRecord` は W2 の Issue で作り, V5 でも使う.

### 7.2 並行の規則

- track A と track B の開発は, 最初から並行して進めてよい. ただし, A1 を出荷するのは B0 の後である.
- 「並行」とは, 利用者の確認や Docker の CI を待つ間に, もう一方の track を進めることを指す.
- 次の組は同時に進めず, 順に入れる.
  - B1 (派生の表) と, W4, W5 (状態機械). `crates/algebramesh-server/tests/invariants.rs` と `model.rs` の不変条件を崩さないためである.
  - proto を変える 2 つの milestone.
  - A1, B1, B2 による agent と runner の変更. A1 の最初の PR で処理の順を決めてから入れる.
- 実装者が 1 人のときの推奨の順は次のとおりである.
  1. T0, B0, A0, A1, A2 (ここで v1.3).
  2. B1, A3, V1, W1 (ここで v1.4).
  3. B2, B3, A4, V2 と K0, V3 (ここで v1.5).
  4. W2, W3, V4, V5, K1 (ここで v1.6).
  5. W4, A5, V6, K2 (ここで v1.7).
  6. 以後は A6, W5, K3, K4.

### 7.3 版の区切り (Q17)

| 版 | 含めるもの | 研究者に届くもの |
|---|---|---|
| v1.2 | `CHANGELOG.md` の「未リリース」 | 信頼性の修正 (#26-#41 など) |
| v1.3 | T0, B0, A0, A1, A2 | certificate 付きの SNF, HNF, kernel, rank, solve. `Client.factor`. `guarantee`. 版の混ざった cluster での安全 |
| v1.4 | B1 (最小限の GC を含む), A3, V1, W1 | 8 MiB を超える結果. 項順序の指定. Groebner basis, 正規形, 所属の判定. 主張の一覧と契約. `LookupRequest` と `ListJobs` |
| v1.5 | B2, B3, A4, V2 と K0, V3 | 大きな引数. Betti 表. GKZ の package. 安い格上げ |
| v1.6 | W2, W3, V4, V5, K1 | workflow と再開. 型を検査する受渡し. `algebramesh verify`. 検証ジョブ. 単体的ホモロジー |
| v1.7 | W4, A5, V6, K2 | cache. GF(p) と疎行列. 照合. 正規化体積と rank の照合 |
| v1.8 以後 | A6, W5, K3, K4 (W6, V7) | D-module, algebraic geometry |

### 7.4 見積りの方法

- 週による見積りはしない. 63 個の commit は 2026-11-03 から 2026-11-06 の 4 日間にあり, 約 4000 行の `492acbd` も 2026-11-04 の commit である. 進み方を決めているのは, 実装の量ではない.
- 進み方を決めるのは次の 3 つである. 版ごとにその数を出しておく.

  | 版 | 利用者が確かめる命題と ADR | Docker の試験 | image の中で確かめること |
  |---|---|---|---|
  | v1.3 | ADR 0018-0020. factor の 5 行. 行列の 6 行 | 回帰試験. v1.2 との混在 | 変換行列を掛ける向き (M2, OSCAR). M2 の `hermite` |
  | v1.4 | ADR 0021. A3 の 8 行. 既存の 4 つの operation の約 25 行 | Raft と blob の障害注入. disk-full. 256 MiB の手動試験 | 項順序の渡し方と先頭項 |
  | v1.5 | A4 の 11 行. V2 と V3 の 3 行 | 回帰試験 | M2 の `gkz` の作り方. Weyl algebra での係数の取り方 |
  | v1.6 | ADR 0023, 0025. K1 の 2 行 | workflow の kill と再開 | なし |
  | v1.7 | ADR 0024. K2 の 7 行と 2 つの定理. lineage | cache. 照合 | OSCAR から TOPCOM と normaliz を呼べるか. Singular の dmodloc.lib |

- v1.3 で, 確認 1 回の往復にかかる時間と, Docker の各 job の実時間を計測する. 以後の版の日程は, それをもとに見積もる.
- 大きく外れうるのは次の 3 つである.
  - A5: 係数の表し方の変更が, math-types 全体に及ぶ.
  - B1: 複製と修復の障害試験.
  - K3: 要確認の事項が多い.

## 8. リスクと対策

| リスク | 対策 | 残るもの |
|---|---|---|
| 版の混在で記録の新しい欄が落ち, node の間で状態が食い違う | `record_v2` で守る. 全 node の `record_json` を比べる試験 | 機能を有効にした後に B0 より前へ戻すと守れない (文書で禁じる) |
| 版の混在で Raft が止まる | 新しい `Change` は `EnableFeatures` の後でだけ使う. v1.2 の image との Docker 試験 | snapshot から始めた B0 より前の binary は検出できない |
| certificate が結果より大きくなり, 8 MiB を超える (表示は Mayr-Meyer の最悪の場合がある. S 対は O(\|G\|^2) 個. 変換行列は密になる) | `certificate_too_large` を明記する. certificate を任意にする. B1 (v1.4) | 大きな例は `asserted` にとどまる |
| 検算が CAS より重く, 締切を越えて permit を持ち続ける | 積の checkpoint (A1). 仕事量の上限. 検証用の permit (V5) | 1 回の小さな積の中では止まれない |
| Rust の項順序の比較と, CAS の先頭項が食い違う | 順序を明示して送る. 先頭項の結合試験. 表せない順序と負の重みは断る | 局所順序は範囲外 |
| 設計書 §1 の境界が少しずつ崩れる | ADR 0019 の 2 つの区分を review の項目にする. math-types に消去の API を足さない. (b) に足すには ADR の改訂を要する | (b) のコードは TCB に残る |
| 検査器の誤りで, 偽の主張を `checked` にする | 主張ごとの変異の試験と CI. fail-closed. verifier の版の記録. 再検証 (V4, V5) | 試験は標本であり, 証明ではない |
| 変異の試験が, 正しい検査器でも落ちる | 変異が主張を偽にすることを試験の側で確かめる規則 (§1.3) | なし |
| 定理の仮定を取り違える (nonresonant と semi-nonresonant, 斉次性, 格子) | `theorem` は利用者の確認の後でだけ `checked` にする. 出典と番号を記録する | 確認を待つ時間 |
| 名前による誤った安心 (`verified_product`, `generated_by_toric_and_euler`) | 主張の一覧, 契約, 要約, `docs/verification.md` | 互換のため古い名前は残る |
| 照合の独立性を過大に見積もる | `lineage` を申告させる. image の中の共有部分 (Singular, normaliz, TOPCOM など) を一覧にする | 申告は人が保守する |
| 状態機械の lock を持ったまま重い読出しをして, 書込みが止まる | closure の中では複製だけを行う. 展開は lock の外で行い, 同時に行う数に上限を置く | なし |
| blob の push が heartbeat を遅らせ, 選挙を起こす. 大きな `fsync` が SQLite の `fsync` を遅らせる | Raft とは別の接続. 4 MiB の chunk. 帯域の上限. 一定量ごとの同期. `fsync` の遅れの計測. 別の volume | 計測の結果しだい |
| blob で disk が埋まり, Raft の書込みが失敗する | `blob_dir`, `blob_min_free_bytes`, `RESOURCE_EXHAUSTED`. B1 の GC と `max_blob_store_bytes`. disk-full の試験 | 電源断のときの rename の順序は未試験 (ADR 0017 と同じ) |
| blob の複製の数が黙って減る | `apply` での修復の待ち行列. 定期の補充. `remove_member` の検査. `server status` の指標 | なし |
| GC が, 確定前の参照を持つ blob を消す | leader の作業用の store も見る. 猶予を `COMMIT_WAIT` より十分長くする | なし |
| scheduler や Python でメモリが足りなくなる | scheduler は値を `Value` に読まない. `Job.download`. `max_blob_bytes` ≤ `max_output_bytes` | agent は値をメモリに持つ |
| 参照先が欠けたとき, 別の値を返す | hash の検査と `DATA_LOSS`. 別の値で代えない | なし |
| cache の誤った hit | 2 つの宣言. opt-in. `env_key`. `EnvironmentConstraints` の検査. 変更のある build は cache しない | 数学的に同じ値は miss になる |
| GF(p) の変更が広く波及する | A5 を A3 の後に置き, 最初の週を調査に充てる | 見積りが外れうる |
| CAS の関数名や image の中身が想定と違う | 各 milestone で, 最初に image の中で確かめる. 版を固定し, 回帰試験で確かめる | 確かめられなかった backend には足さない |
| Phase 4 の範囲が際限なく広がる | 契約のない operation は足さない. package は 2-4 個の operation から始める | なし |
| workflow は orchestrator が動いている間しか進まない | 文書に書く. `request_id` で冪等に再開する. 取消しは手元の file に残す. scheduler 側の仕組みは W6 | なし |
| 確認を待つ間に作業が止まる | 待つ間に他方の track を進める. 版ごとに確認の数を出しておく | なし |

## 9. 範囲外とするもの

- Rust で次のものを計算すること: Groebner basis, 割り算と正規形, 消去 (既存の `IntegerMatrix::rank` と (b) の一覧を除く), HNF と SNF, 準素分解, b-function, holonomic rank, 三角形分割の探索, LLL.
- certificate なしに, Rust で「f ∉ I」, I_A の生成, 既約性を判定すること.
- 収束, 解空間の基底であること, 重みが generic であることの証明. これらは `not_claimed` とする.
- 数学の対象のうち次のもの.
  - GF(p) 上の HNF と SNF.
  - 斉次でない入力の Betti 表と極小性.
  - 絶対準素分解. 埋め込まれた準素成分の照合.
  - 交叉数の実行時の検査.
- cache と workflow のうち次のもの.
  - 数学的な正規形を cache の鍵にすること.
  - 実行中の同じ計算への合流.
  - scheduler 側の workflow object (W6 は条件付き).
- 保存のうち次のもの.
  - 外部の object storage の実装 (ADR 0021 で比べるだけ).
  - ジョブの削除と保存期間. 参照された blob の削除.
- package の動的な読込み. exactly-once の実行. 検査器の形式検証. 1 つのジョブを並列化して速くすること.
- `gkz.numeric` (設計書 §9.3 の第 4 段). 誤差を表す仕組みが語彙にないためである.
- 代数体. GF(q) (q が素数でないもの). 局所順序と負の重み. 有理関数体上の Weyl algebra. Ext と Tor. 一般の層コホモロジー.
- 機能を有効にした後で, B0 より前の版に戻すこと.

## 10. 利用者が決めること (推奨する既定)

2026-11-06 に, 利用者は次の 4 つを推奨のとおりに決めた.

| 問い | 決定 |
|---|---|
| Q2 (保証の語彙) | 主張ごとの水準を `checked`, `cross_checked`, `asserted` とし, 別に `not_claimed` を置く. 要約を `verified`, `partially_verified`, `unverified` とする |
| Q4, Q5 (設計書 §1 の境界) | certificate の検査 (S 対の lcm representation の検査を含む) と, 上限付きの再計算を認める |
| Q15 (Phase 4 の順序) | GKZ, topology, combinatorics, D-module, algebraic geometry |
| Q35 (#36 の置き場所) | 自前で複製する. `BlobBackend` の境界を置く |

他の問いは, 下の表の推奨する既定で進め, それぞれの milestone で見直す. Q21 (この文書の置き場所) は済んだ. Q22 (#37-#41) は, 対応する commit を push した時点で閉じる.


| # | 問い | 推奨する既定 |
|---|---|---|
| Q1 | B0, 保証の最小の記録, 検査関数を Phase 1 に前倒しするか | する. 独立, 再検証, 照合は Phase 3 に残す |
| Q2 | 保証の語彙 | 水準は `checked`, `cross_checked`, `asserted` で, 別に `not_claimed` を置く. 要約は `verified`, `partially_verified`, `unverified` とする. 日本語の「主張」「根拠の水準」「実効水準」「契約」も確かめたい |
| Q3 | 新しい operation の結果の形 | `value` には数学的な値と `certificate` だけを置く. 判定は `guarantee.claims` に置く. `checks` は既存の 4 つだけに残す |
| Q4 | S 対の lcm representation の検査を, 設計書 §1 の内側と認めるか | 認める (§1.2 の論拠). 割り算は認めない. Weyl algebra には当てない |
| Q5 | 区分 (b) の上限付きの再計算を認め, `checked` (TCB を記録する) とするか | 認める. 対象は §1.2 の表に限る. `solve` は V2 で (a) に移す |
| Q6 | 「属さない」, I_A の生成, 既約性の扱い | 当面は `asserted` とする. 「属さない」は, `gb.groebner` が `checked` のときだけ `checked` にする. 照合で `cross_checked` に上げる |
| Q7 | `checked_probabilistic` を入れるか | 当面は入れない. 入れる場合は `checked` より下に置き, `verified` には数えない. 係数の高さの上界の求め方を先に決める |
| Q8 | certificate の置き場所 | `value` の `certificate` 欄に置き, 大きければ blob にする. 上限を超えたら `certificate_too_large` とする |
| Q9 | marker を含む spec の `input_sha256` | 記録した spec (marker を含む) の hash のままとする. ジョブの同一性は, 内容の hash の形による fingerprint で見る |
| Q10 | cache の既定 | `NEVER` (opt-in). 鍵は内容の hash の形から作る. fingerprint は `REUSE` のときだけ変える |
| Q11 | GF(p) の JSON の形と範囲 | `"GF(p)"` とし, 2 ≤ p < 2^31 とする. backend ごとの上限は `Capabilities` で申告させる |
| Q12 | 加群の生成元の向き | 列. OSCAR は driver で転置する |
| Q13 | 新しい型の JSON に `type` の key を持たせるか | 新しい型だけに持たせる. 既存の JSON は変えない |
| Q14 | operation の名前空間 | `matrix.*`, `ideal.*`, `module.*`, `complex.*`, `simplicial.*`, `polytope.*`, `dmodule.*`, `toric.*`, `verify.*`. 主張の ID には `triangulation.*` も使う. 個々の名前はすべて提案である |
| Q15 | Phase 4 の順序 | GKZ, topology, combinatorics, D-module, algebraic geometry の順. 研究の優先度で入れ替えてよい |
| Q16 | package の選び方 | compile 時の Cargo feature `pkg-<name>` で選ぶ. 動的な読込みはしない |
| Q17 | 版の割り当て | §7.3 の表. v1.3 は B1 を待たない |
| Q18 | workflow の置き場所 | client 側. scheduler 側は W6 で条件付きとする |
| Q19 | 「certificate」の日本語 | 英語のまま書く. ADR 0010 の TLS の証明書と区別するためである |
| Q20 | ZZ 上の既約分解を照合するときの正規化 | 因子は原始的とし, 宣言した順序での先頭係数を正とする. content は `constant` にまとめる |
| Q21 | roadmap の置き場所 | `docs/roadmap.md` を作り, 設計書 §13.2 から参照する |
| Q22 | GitHub で OPEN の #37-#41 | 対応する commit (`aeec354`, `1b67ec0`, `718bb51`, `04f1c34`, `a58bf64`, `05720d5`) を添えて閉じる |
| Q23 | 機能の合意をどう残すか | 全 member を確かめた後に, `Change::EnableFeatures` で記録する. 記録は leader が自動で行い, ログと `server status` に出す. 状態から推す方式 (`blobs` 表が空でなければ使用中とみなす) は採らない |
| Q24 | blob があるときの `AddMember` | learner として加えて返す. 2 回目の呼出しで投票者にする |
| Q25 | upload の時間とジョブの締切 | `wall_seconds` に含めない. 報告の再試行の期限を, 値の大きさに応じて延ばす |
| Q26 | #36 の「検算は store から読んだ値に対して行う」の読み替え | 検査した canonical bytes を upload し, store からの再検査は V4 と V5 で行う |
| Q27 | 再開のとき, 固定した build が online にない | 止まって報告する. 利用者が固定を外すか替えてから再開する |
| Q28 | image の中の `agent_build` の「変更なし」 | CI が変更のない checkout から build したときだけ「変更なし」を渡す. それ以外は不明として, cache しない |
| Q29 | 契約の確認を `verified` の条件にするか | する. 既存の operation にも新しい operation にも当てる |
| Q30 | 用語 | 「非共鳴」は nonresonant だけに使う. semi-nonresonant, lcm representation, folding condition は英語のまま書く. 「表示の係数」(英語で cofactor と呼ばれることがある) は確かめたい. 「GF(p) 上の HNF」は使わない |
| Q31 | rank = vol(A) の定理をどこまで適用するか | 当面は nonresonant だけに使う. 斉次でない A と semi-nonresonant へは, Adolphson 1994 の命題を確かめてから広げる. `run.py` の inhomogeneous の分類の根拠と, `local.rank` と holonomic rank の関係も同時に確かめる |
| Q32 | 準素分解をどの体で行うか | QQ だけ |
| Q33 | Betti 表と Hilbert 級数の対象 | S/I |
| Q34 | 古い client に展開して返す上限 | 96 MiB |
| Q35 | #36 の置き場所の方式 | 自前で複製する. 外部の object storage は ADR 0021 で比べ, `BlobBackend` の境界だけを置く |

## 11. 確認を求める名前 (すべて提案)

- crate: `algebramesh-ops`, `algebramesh-verify`, `algebramesh-<name>`, `algebramesh-verifier` (binary).
- 型と trait: `Guarantee`, `Claim`, `EvidenceLevel`, `ResultVerifier`, `OperationVerifier`, `OperationDescriptor`, `RationalMatrix`, `SparseMatrix`, `PolynomialMatrix`, `FreeModule`, `Submodule`, `ChainComplexData`, `BettiTable`, `IdealData`, `CoefficientDomain::Gf`, `MonomialOrder::{Weights, Block}`, `BlobRef`, `BlobBackend`, `Change::EnableFeatures`.
- 語彙.
  - 水準: `checked`, `cross_checked`, `asserted`, `not_claimed`, `checked_probabilistic`.
  - `method` の 7 つの値 (§1.3).
  - 欄と値: `scope`, `exact`, `truncated`, `depends_on`, `summary`, `catalog`, `lineage`, `certificate`, `certificate_too_large`, `cross_check_failed`.
  - 区分の名前: 「certificate の検査」, 「上限付きの再計算」.
  - 主張の ID: A1-A4, V1, K2, K4 の表に挙げたもの.
- 機能名: `record_v2`, `blob_refs`.
- RPC: `PutBlob`, `GetBlob`, `StatBlob`, `PushBlob`, `FetchBlob`, `BlobsRegistered`, `LookupRequest`, `ListJobs`, `AgentGateway.GetRecord`.
- proto の欄: `NodeStatus.version`, `NodeStatus.features`, `RegisterRequest.features`, `RegisterResponse.features`, `ListBackendsResponse.server_features`, `BackendOffer.driver_sha256`, `BackendOffer.agent_build`, `BackendOffer.driver_version`, `BackendOffer.verifier_version`, `JobRef.accept_blob_refs`, `BlobRequest.offset`, `SubmitRequest.cache`, `JobStatus.cache_hit`.
- `Capabilities` の欄: `signatures`, `signature_conflicts`, `cacheable_operations`, `lineage`.
- 設定: `blob_dir`, `max_blob_bytes`, `max_blob_store_bytes`, `blob_min_free_bytes`, `blob_orphan_grace`, `blob_replication_bytes_per_second`.
- 定数と `meta` の key: `INLINE_MAX_BYTES`, `min_binary_version`, 派生の表の版.
- 表と列: `blobs`, `blob_refs`, `cache_entries`, `requests`, `fingerprint_version`, 派生の列 `verifier_version`.
- JSON の key: `$blob`, `$ref`, `size`, `from`, `pointer`, `$output`, `type`.
- その他: `ensure_blobs`, `needs_blob_refs`, `input_key`, `env_key`, 「内容の hash の形」, `legacy_guarantee`, `request_id` の形 `wf/<workflow>/<node>/<attempt>`.
- command: `algebramesh verify <job-id>`, `algebramesh cache invalidate`, `algebramesh blob put|get|stat`, `algebramesh workflow run|resume|status|cancel`.
- 文書: `docs/claims.md`, `docs/verification.md`, `docs/packages/<name>.md`, `docs/roadmap.md`.
- feature: `pkg-<name>`.
- ADR: 0018 (語彙), 0019 (検査の境界), 0020 (機能の合意), 0021 (blob store), 0022 (型と operation の契約), 0023 (workflow), 0024 (cache), 0025 (検証ジョブと照合), 0026 (package).
  - 実際の番号はこの計画とずれた. 0023 は項順序 (#64), 0024 は `ideal.groebner_basis` と certificate (#66), 0025 は `ideal.normal_form` と `ideal.membership` (#67), 0026 は S 対の lcm representation (#68) に使った. workflow, cache, 検証ジョブと照合, package の ADR は, 書く時点で次の空き番号を使う. この文書の他の箇所の番号は計画の時点のものである.
