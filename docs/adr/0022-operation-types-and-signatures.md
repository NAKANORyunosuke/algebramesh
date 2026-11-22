# ADR 0022: operation の型と signature

- 状態: 採用
- 日付: 2026-11-06
- 関連: [ロードマップ A0, W3](../roadmap.md), [ADR 0014](0014-job-memory-limit.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0020](0020-cluster-feature-agreement.md), Issue #54, #55

## 背景

- operation の名前と引数の型は, backend ごとに書かれていた. `polynomial.factor` の `FactorArguments` は 3 つの backend に同じ形で 3 回あり, `gkz.*` の引数の型は Macaulay2 の backend の中にだけあった.
- `Capabilities` は, operation の名前の一覧 (`operations`) と型の名前の一覧 (`supported_types`) を別々に持つだけである. どの operation がどの型を受けてどの型を返すかは表せない.
- これから足すものは, operation の型を共有する必要がある. A1 の検査器は結果を型で読み, W3 の workflow は, ある operation の結果を別の operation の引数に渡してよいかを型で判定する.
- online な worker のどれも申告していない operation のジョブは, 誤りにならずに `QUEUED` のまま残っていた.

## 決定

**crate `algebramesh-ops`.**
- operation の名前, 引数と結果の型, 記述子 (`OperationDescriptor`) を置く. 依存は `algebramesh-math-types`, `serde`, `serde_json` だけである.
- backend は引数をこの crate の型で読み, client はこの型で投入する (`algebramesh_client::ops`).
- 型はどれも `deny_unknown_fields` を持つ. 結果の型は, 今までの `value` と同じ JSON を書く.
  - `polynomial.factor` の結果は `FactorResult` である. `Factorization` の JSON に `verified_product` を加えた形である.
  - `gkz.*` の結果は `algebramesh-gkz` の報告 (`ParameterReport`, `Subtraction`) を含むので, この crate には置けない. 型は GKZ の package (K0) で定める. それまで, 結果は JSON のまま扱う.

**記述子.** `OperationDescriptor` は次の 5 つを持つ.
- `name`: operation の名前.
- `arguments`: `arguments` の各 key と, その型の名前.
- `result`: 結果の型の名前.
- `order_dependent`: 値が引数の環の項順序に依存するか. 依存する値は, 同じ項順序で計算した値の代わりにしか使えない. 今の 4 つの operation はどれも依存しない.
- `deterministic`: 同じ引数で, backend の同じ build が同じ値を返すか. 今の 4 つの operation はどれもそうである (T0 で, 3 回の値が順序まで一致した).

**型の名前.** signature の型は名前で表す. 結果の型の名前は, その JSON の `type` の値である.

| 名前 | 表すもの |
|---|---|
| `ring` | 多項式環 (係数域, 変数, 項順序) |
| `polynomial` | 傍らの環の多項式 |
| `factorization` | `polynomial.factor` の値 |
| `integer_matrix` | 整数の長方行列 |
| `rational_vector` | 有理数のベクトル |
| `integer_vector` | 整数のベクトル |
| `nonnegative_integer` | 非負整数 |
| `weyl_operator_list` | Weyl 代数の元の列 |
| `gkz_system`, `gkz_indicial`, `gkz_series` | `gkz.generate`, `gkz.indicial`, `gkz.series` の値 |
| `ideal` | 環と生成元の列で与えたイデアル (ADR 0024) |
| `ideal_certificate_request` | `ideal.*` の `certificate` 引数 (`representation`, `s_pairs`, `none`). `ideal.groebner_basis`, `ideal.normal_form`, `ideal.membership` が共有する (ADR 0024, ADR 0025) |
| `groebner_basis` | `ideal.groebner_basis` の値 (ADR 0024) |
| `normal_form` | `ideal.normal_form` の値 (ADR 0025) |
| `ideal_membership` | `ideal.membership` の値 (ADR 0025) |

**signature の申告と集約.**
- backend は, 申告する operation ごとに, 記述子から signature (`{arguments, result}`) を作って `Capabilities.signatures` に入れる. `supported_types` は互換のために残す.
- `ListBackends` は, 同じ backend の online な worker の signature をまとめる.
  - 申告した worker の間で同じ signature は, そのまま返す.
  - 違う signature を申告した operation は `signature_conflicts` に並べ, その signature は返さない.
  - signature を申告しない worker (この ADR より前の agent) は数えない. 「分からない」であって, 食い違いではない.
- `Capabilities` は記録にも Raft のログにも入らない (`ListBackends` は leader のメモリの agent から作る). そのため, ADR 0020 の機能は要らない. 古い scheduler は `signatures` を黙って落とすだけである.

**投入時の検査.**
- `Scheduler::submit` は, その backend の online な worker が 1 台以上いて, どれもその operation を申告していなければ, ジョブを作らずに `FAILED_PRECONDITION` (`Unplaceable`) で断る. 1 台もいなければ, 今までどおり待たせる.
- 要求された上限 (ADR 0014) を強制するかも, その operation を申告する worker の中で見る. 配置 (`can_place`) が両方を同じ worker に求めるからである.
- 既知の `request_id` は, この検査より前に解決する. 同じ投入の再送は, online な worker によらず元のジョブを返す.
- worker の一覧は leader のメモリにある. leader が替わった直後は, agent が登録し直すまで worker がいないので, 投入は待たされる. 同じ投入でも, 答えが時期によって変わりうる.

**名前.** crate, 型, 欄, 型の名前は提案であり, 利用者に確かめる.

## 結果

- 引数の型の定義は `algebramesh-ops` の 1 か所になる. backend の crate には残らない.
- client は, 結果の型を投入の前に `ListBackends` の signature で知る. W3 は, この名前を比べて受渡しを判定する.
- client が `algebramesh-ops` に依存するので, Python の拡張に `num-rational` が入る (`bindings/python/THIRD-PARTY-LICENSES.txt`).
- `gkz.*` の結果の型は K0 まで JSON のままである.

## 確認

- `algebramesh-ops` の単体試験: 記述子の key が引数の型の key と一致する. `FactorResult` が `Factorization` に `verified_product` を加えた JSON を書き, 読み戻せる. 知らない key と別の `type` は断られる.
- 各 backend の単体試験: 申告する operation ごとに, 記述子どおりの signature を申告する.
- `crates/algebramesh-server/tests/limits.rs`: signature の集約と `signature_conflicts`, signature のない worker. 投入時の検査 (Issue #55).
- T0: 回帰試験と `test_cluster.py` の値が, 変更の前後で一致する.
