# ADR 0032: client 側の workflow と, ジョブの出力への参照

- 状態: 採用 (機能 `workflow_refs`, RPC `AgentGateway.GetRecord`, marker `$ref`, 定義の `$output`, 状態 file と報告の JSON の key, CLI の命令の名前は提案)
- 日付: 2026-11-23
- 関連: [ロードマップ W2](../roadmap.md), Issue #104, #105, #106, #107, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0020](0020-cluster-feature-agreement.md), [ADR 0021](0021-content-addressed-blob-store.md)

## 背景

- W1 で, 投入に `request_id` を付けられるようになった. 同じ id の投入は同じジョブを返し, `LookupRequest` と `ListJobs` で id からジョブを引ける.
- B2 で, 引数の大きな値を blob の marker で渡せるようになった (ADR 0021).
- W2 は, この 2 つを使って, あるジョブの結果の値を別のジョブの引数に渡す DAG を client の側で動かす. scheduler に DAG を持たせないのは, Raft の状態機械の形を変えずに済ませるためである (ロードマップ W2).
- 決めることは, 値の渡し方, その来歴の検査, 再開と取消しの方法, 保証の水準の扱いである.

## 決定

### 1. 定義

- workflow の定義は JSON で, `{"workflow": <名前>, "nodes": {<節>: {"spec": <job spec>, "priority": <整数>, "depends_on": {<主張>: [{"node", "claim"}]}}}}` とする. `priority` と `depends_on` は省ける. 知らない key は断る.
- workflow と節の名前は, 英数字と `.`, `_`, `-` だけからなる空でない文字列である.
- 節の spec の `arguments` の top-level の値は, `{"$output": {"node": <節>, "pointer": <pointer>}}` でよい. これは, 節 `node` の結果の値のうち `pointer` の指すものである. `pointer` は `""` (結果の全体) か `"/key"` (top-level の 1 つの値) で, 省けば `""` である. `~0` と `~1` の escape を読む.
- 次の定義は, 何も投入せずに断る.
  - 存在しない節, 自分自身を指す `$output`. 閉路.
  - top-level より下にある `$output`, `$output` と他の key を持つ値, top-level より下を指す pointer.
  - `request_id` が 128 bytes を超える節 (`attempt` を上限の 9999 として数える).
  - job spec として読めない spec.
  - `depends_on` が, その節の producer でない節を指すもの.
- 節は, 出力を取る節の後に並べる. 並べ方は名前の順で決まり, 定義が同じなら同じである.

### 2. 値の渡し方

- orchestrator は, producer が `SUCCEEDED` になった後で, `$output` を次のどれかに置き換えてから投入する.
  - producer の記録で値が marker なら, その marker に `from: {"job_id", "pointer"}` を足したもの.
  - inline の値で, canonical JSON が `inline_max_bytes` (既定は `INLINE_MAX_BYTES`, 256 KiB) を超えるなら, `PutBlob` で送った marker に `from` を足したもの.
  - それ以外は `{"$ref": {"job_id", "pointer", "sha256"}}`. `sha256` は値の canonical JSON の SHA-256 (`sha256:<64 桁>`) である.
- 置き換えた spec が 256 KiB を超えれば, 残りの inline の引数を, B2 と同じく大きいものから blob に移す.
- request の fingerprint の版 2 (ADR 0021 §13) は, `$ref` をその `sha256` に置き換えて作る. 値の id と同じなので, 同じ値は inline でも, blob でも, 参照でも同じ request になる. そのため, `inline_max_bytes` の違う client で再開しても `ALREADY_EXISTS` にならない.
- 小さな値を参照で渡すのは, 辺ごとに `PutBlob` と過半数の `fsync` を行わずに済ませるためである.

### 3. 来歴の検査 (Issue #105)

- scheduler の `Submit` は, 引数の top-level の `from` と `$ref` を, ジョブを作る前に確かめる. 確かめるのは, producer の確定した記録が `SUCCEEDED` で, その結果の pointer の先にある値 (inline の値か marker) が同じ SHA-256 を持つことである. marker の id は, 本体を読まずに比べる.
- 次の参照は `FAILED_PRECONDITION` で断り, ジョブを作らない.
  - 知らないジョブ, `SUCCEEDED` でないジョブを指すもの.
  - pointer の先に値がないもの. 結果の全体が 1 つの blob である記録の key を指すものも, 本体を読まないので, ここに入る.
  - SHA-256 が合わないもの.
- top-level より下を指す pointer と, 形の崩れた参照は `INVALID_ARGUMENT` で断る.
- 引数の marker の blob の確認 (`ensure_blobs`) は, これまでどおり行う.

### 4. agent による参照の解決 (Issue #107)

- RPC `AgentGateway.GetRecord(RecordRequest {job_id}) returns (JobResult)` を足した. scheduler の確定した記録を, marker をそのままにして返す. 知らないジョブは `NOT_FOUND` である.
- agent は, `$ref` を持つジョブを実行する前に, producer の記録を `GetRecord` で取り, pointer の先の値の SHA-256 が `sha256` と同じことを確かめて, 値に置き換える. 値が marker なら, `GetBlob` で読んでから置き換える. 合わなければ, backend を呼ばずにジョブを `INVALID_ARGUMENTS` で終える. 応答のない間は, B2 と同じく `UNAVAILABLE` で終える.
- marker の `from` は, marker を値に戻すときに消えるので, backend には渡らない.
- `$ref` を持つジョブは, `workflow_refs` を申告した agent にだけ割り当てる (`Placement.needs_record_refs`). そのような agent が online にいなければ, 投入を断る.

### 5. 機能 `workflow_refs`

- ADR 0020 の機能 `workflow_refs` を足した. 意味は, `from` と `$ref` を受けて検査すること, `GetRecord` に答えること, agent が `$ref` を解決することである.
- 機能が有効でない scheduler は, `from` か `$ref` を持つ投入と `GetRecord` を `FAILED_PRECONDITION` で断る. それより古い scheduler は `GetRecord` に `UNIMPLEMENTED` を返す. 引数の marker を読むのは `blob_arguments` が有効なときだけなので, 参照には 2 つの機能の両方が要る.
- orchestrator は, 辺を持つ節を投入する前に, cluster が両方の機能を有効にしていることを確かめる. なければ, 投入せずに誤り (`WorkflowError::Unsupported`) を返す. 辺のない workflow は, 機能がなくても動く.

### 6. 再開 (Issue #104)

- 節の `request_id` は `wf/<workflow>/<node>/<attempt>` で, `attempt` は 1 から数える.
- orchestrator は, 節を並べた順に進める. 節ごとに, `ListJobs` で `wf/<workflow>/<node>/` の下のジョブを引き, 最も大きな `attempt` を `LookupRequest` で確かめる. ジョブがあれば, 同じ投入を 1 度送り直す. 定義が変わっていれば `ALREADY_EXISTS` になるので, その節を投入せずに誤り (`WorkflowError::Changed`) を返す.
- 状態ごとの扱いは, ロードマップの表のとおりである.
  - `SUCCEEDED`: 完了とする.
  - 終わっていない: 待つ.
  - `LOST` で `retried_as` がある: `retried_as` を辿る.
  - それ以外の終わった状態: 止まる. 利用者が `retry_failed` を選んだときだけ, `attempt` を 1 つ進めて投入する. 1 度の実行の中で, 1 つの節について 1 度だけである.
  - ジョブがない: 投入する.
- 送れない間 (`UNAVAILABLE` など) は, 間を空けて同じ節からやり直す. leader が替わっても, request id で同じジョブに行き着く.
- 同じ状態 file で 2 つの orchestrator が同時に走らないよう, 実行の間は `<state>.run` の lock を持つ. 取れなければ断る.

### 7. 取消し

- 状態 file は `<定義の file>.state.json` (Python で mapping の定義を渡すときは必ず指定する) で, `{"workflow", "cancelled"}` を持つ. 書くときは `.part` に書いてから名前を変える.
- 取消しの記録と各投入は, `<state>.lock` の lock の下で順に並べる. orchestrator は, lock の中で `cancelled` を読んでから `Submit` を呼ぶ. そのため, 取消しを書いた後に投入はなく, 取消しは書いた後で `wf/<workflow>/` の下のジョブを並べれば, 投入済みのジョブをすべて見る.
- 取消しは, 終わっていないジョブも取り消す. process をまたいで, どの orchestrator もそれ以上投入しない.

### 8. 保証の水準 (Issue #106)

- 主張の `depends_on` は, 他のジョブの結果の主張を `job:<job_id>#<claim>` で指せる (`guarantee::job_claim`). 1 つの記録の中では, そのような依存は `asserted` と数える. 記録だけからは, 他のジョブの水準が分からないからである.
- `Guarantee::effective_level_with` と `summary_with` は, 他のジョブの主張の水準を引数の関数から取る.
- workflow の定義の `depends_on` は, 節の結果の主張が, producer の結果のどの主張に依るかを書く. 報告では, それを `job:<producer の job_id>#<claim>` として主張の `depends_on` に足し, producer の実効水準で consumer の実効水準を求める. backend が返す記録の `depends_on` は変えない.
- 例: `ideal.membership` が G に対する `membership.in` を `checked` にしても, G が `ideal.groebner_basis` の結果で, その `gb.contained` が `asserted` なら, 報告の `membership.in` の実効水準は `asserted` である.

### 9. 報告と入口

- 報告は `{"workflow", "progress", "nodes"}` で, `progress` は `SUCCEEDED`, `RUNNING` (`status` で終わっていない節があるとき), `STOPPED`, `CANCELLED` のどれかである.
- 節ごとに `state` (投入していなければ `NOT_SUBMITTED`), `attempt`, `request_id`, `job_id`, `error`, `result_sha256` (結果の canonical JSON の SHA-256. blob の本体を含めて求める), `summary`, `claims` (`id`, `level`, `depends_on`, `effective_level`) を返す.
- 入口は 3 つで, どれも同じ orchestrator を呼ぶ.
  - Rust: `algebramesh_client::workflow::{run, status, cancel}`.
  - CLI: `algebramesh workflow run|resume|status|cancel <file>`. `--state`, `--retry-failed`, `--inline-max-bytes`, `--poll-secs` を受ける. `resume` は `run` と同じである. 報告を JSON で出し, `STOPPED` と `CANCELLED` では 1 で終わる (`cancel` は 0).
  - Python: `algebramesh.workflow.{run, resume, status, cancel, output}`.

## 結果

- 値は Raft のログを通らずに節の間を渡る. 小さな値は記録を指すだけで, 大きな値は blob を 1 度だけ送る.
- 参照の食い違いは, ジョブを作る前に scheduler が, 実行の前に agent が, それぞれ断る.
- orchestrator の process が止まっても, cluster の request id から再開できる. 何度再開しても, 節ごとのジョブは 1 つ (`retry_failed` を選べば試行ごとに 1 つ) である.
- 試験は, 2 つの backend にまたがる 4 節の DAG で, 受入条件の 1 から 6 を確かめる (`crates/algebramesh-agent/tests/workflow.rs`, `references.rs`, `crates/algebramesh-cli/tests/workflow.rs`). Docker の cluster は使わない.

## 採らなかった案

- **DAG を scheduler に持たせる.** 状態機械に新しい表が要り, 版の混ざった cluster の扱いが増える. W2 では, 既存の request id と検査だけで足りる.
- **すべての辺を blob で渡す.** 辺ごとに過半数の `fsync` が要る. 小さな値では, 記録を指す方が安い.
- **pointer を任意の深さにする.** 結果の全体が blob のとき, 検査に本体を読む必要が出る. W2 では top-level に限り, 深い値は producer の側で top-level に出す.
- **機能のない cluster で, 値を inline に写して渡す.** 来歴を確かめられないので, 辺を持つ workflow は機能を要する.

## ロードマップとの違い

- 結果の全体が 1 つの blob である記録の key を指す pointer は断る. 本体を読まないためである.
- 取消しは, 状態 file に書くのに加えて, 終わっていないジョブも取り消す.
- 節をまたぐ主張の依存は, backend の記録ではなく workflow の定義に書き, 報告で `job:<job_id>#<claim>` として扱う.
- W3 で固定する build の確認は, W3 で足す.
