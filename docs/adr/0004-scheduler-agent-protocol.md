# ADR 0004: scheduler と agent の通信・配置・障害時の扱い

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §4, §7.2, §7.3, §11

## 背景

v0.2 では, 異なるホストの worker にジョブを配置して回収する仕組みが要る.
設計書は gRPC/TLS, 認証, `LOST` の扱い, 再試行の限定, SQLite (WAL) を定めているが, agent がジョブを受け取る方式と, 障害時に状態をどう遷移させるかは未定だった.

## 決定

### 通信

- `proto/algebramesh.proto` に, client 向け `Scheduler` と agent 向け `AgentGateway` の2サービスを置き, 同じポートで TLS 越しに提供する.
- 認証は `authorization: Bearer <token>` で行う. client 用と agent 用は別の token 集合とし, client の token で agent の API は呼べない.
- ジョブ仕様と記録は JSON 文字列で運ぶ. スキーマは `algebramesh-core` の `JobSpec` / `JobRecord` に1つだけ定義し, protobuf と二重に定義しない.
- agent は scheduler へ外向きに接続する. worker ホストに受信ポートは要らない.

### 配置

- agent は slot ごとに `PollAssignment` を long-poll する (最長 20 秒). scheduler は投入時に待機中の poll を起こす.
- 候補は `QUEUED` のジョブを優先度の降順, 投入順に見て, 次をすべて満たす最初のものとする.
  - backend が agent の申告に含まれる
  - operation が, その backend について申告された capabilities に含まれる
  - `environment.builds` と `environment.agents` の制約を満たす
- 候補の選び方 (Issue #19): poll は, その agent の backend の `QUEUED` のジョブを, 配置に要る小さな列 `placement` (backend, operation, limits, environment) だけで優先度と投入順に見て選び, 記録全体を復元するのは割り当てる 1 件だけである. 索引 `jobs_queue (state, backend, priority DESC, seq, job_id, placement)` が問い合わせの列をすべて含むので, 大きな `record_json` のある行は読まない. 既存のデータベースの `placement` は, 開くときに記録から埋める. 100 KiB と 1 MiB のジョブ 1000 件を別の backend に積んだ計測 (release, Windows) では, 候補の読み込みは 0.1 ms 未満 (同じ backend で制約により 1 件も実行できない場合も 0.5 ms) で, 記録の大きさによらない. 全件の記録の復元は 180 ms と 2,400 ms だった. 計測は `cargo test --release -p algebramesh-server --test poll_scaling -- --ignored --nocapture` で再現できる.
- 待ち行列の上限 (Issue #40): `QUEUED` のジョブが `max_queued_jobs` (既定 10,000 件) に達したら, 新しいジョブの投入を `RESOURCE_EXHAUSTED` で断り, 何も記録しない. 件数は投入と同じ lock の中で索引 `jobs_queue` から数える. 既に使われた `request_id` の投入は上限より先に解決し, 元のジョブを返す. `LOST` のジョブの再投入は受け付け済みのジョブの置き換えなので上限に掛けない (その分だけ上限を超えうる). `ListBackends` の `queued_jobs` と `max_queued_jobs` が現在の件数と上限を示す. 1 件の仕様は 8 MiB までなので, 上限は件数を抑えるが, 合計の大きさは `max_queued_jobs * 8 MiB` までありうる.
- ジョブ記録は scheduler が組み立てる. agent は `ReportStarted` と `ReportFinished` (CasResult か CasError の JSON) だけを送る.
- agent は登録の前に各 backend を準備し, CAS が報告する版を申告する. 準備に失敗した backend は申告しない.

### 障害時

| 事象 | 扱い |
|---|---|
| agent の heartbeat が `heartbeat_timeout_ms` 途絶える | agent をオフラインとし, その agent の `ASSIGNED`, `RUNNING`, `CANCELLING` のジョブを `LOST` にする |
| scheduler の再起動 | 起動時に `ASSIGNED`, `RUNNING`, `CANCELLING` のジョブを `LOST` にする. `QUEUED` は残す |
| `LOST` のジョブが `retry_policy = if_deterministic` | 新しい job ID で再投入する (`retry_of` と `retried_as` で相互に参照). `max_attempts` まで |
| `LOST` 後に届いた結果 | 状態を変えずにイベントとして残す (`accepted = false`). 残すのは送った agent, attempt, 理由, 本文の大きさと SHA-256 だけで, 同じ内容は一度だけ記録する (Issue #22, [ADR 0017](0017-raft-storage-bounds-and-durability.md)) |
| scheduler が agent を知らない heartbeat | agent に再登録させる. 実行中のジョブは結果が受理されないので取り消す |
| `ReportFinished` が届かない (Issue #10, #14) | agent は, 結果が受理されるか scheduler が断るまで, 間隔を広げながら最大 2 分送り直す. 2 分は送り直し全体の期限で, 1 回の呼び出しは 30 秒と残り時間の短い方までしか待たない (答えない handler があっても期限に届く). 期限切れの呼び出しの結果は分からないが, 同じ報告をもう一度送っても, 終わったジョブへの報告として状態を変えずに断られる. その間ジョブは heartbeat の `running_job_ids` に残る. 送れない大きさの結果 (gRPC の上限 128 MiB を超える, または scheduler が `RESOURCE_EXHAUSTED` などで断る) は, 小さな `RESOURCE_LIMIT` の誤りに置き換えて送り, ジョブは `FAILED` になる. 送り直しをあきらめた agent はジョブを heartbeat から外す |
| agent の heartbeat に載らない active なジョブ (Issue #10) | heartbeat の届く agent でも, `ASSIGNED`, `RUNNING`, `CANCELLING` のジョブが `heartbeat_timeout_ms` より長く heartbeat に載らなければ `LOST` にする (再投入の扱いは上と同じ). 割り当てた時点から数えるので, 割り当てと agent の次の heartbeat の間の行き違いでは `LOST` にならない. leader が過半数を取り戻したときは数え直す. heartbeat はその agent の active なジョブの ID だけを索引 `(agent_id, state)` で読み, 記録を復元するのは `LOST` にするジョブだけである (Issue #16) |
| agent の呼び出しが答えない (Issue #14) | 登録, `ReportStarted`, `ReportFinished` は 1 回 30 秒, poll は待ち時間に 30 秒を足した時間で打ち切り, `UNAVAILABLE` として扱う. 打ち切った poll で割り当てられていたジョブは heartbeat に載らないので, 上の規則で `LOST` になる |
| client の取消し | `QUEUED` は直ちに `CANCELLED`. `RUNNING` は `CANCELLING` にし, heartbeat の応答で agent に伝える |

`LOST` を `QUEUED` に戻さず別のジョブとして再投入するのは, 状態機械 (設計書 §7.2) で `LOST` を終端状態として保つためである.
同じ入力が2回実行され得ることは記録 (`retry_of`) から分かる. exactly-once は保証しない.

### 永続化

- SQLite は WAL モードで scheduler のローカルディスクに置く. 終了したジョブの記録は `artifacts/<job-id>/record.json` にも書く.
- 1つの scheduler が単独で書く. 高可用構成は扱わない (設計書 §3.3).

## 検証

- 同一プロセス内の試験 (`crates/algebramesh-agent/tests/cluster.rs`) で次を確認した.
  - 2 agent への分散
  - 取消し
  - agent の停止による `LOST`
  - 再試行
  - token と TLS の要求
- 再起動の扱いは `crates/algebramesh-server/tests/restart.rs` で確認した.
- Docker 上の cluster (`tests/integration/run-cluster-e2e.sh`) で, 2台の Asir worker に8件の独立ジョブが分散して成功することを確認した.
- 物理的に異なる2ホストでの確認はまだ行っていない. 同じマシン上の別コンテナで代替した.

## 残る課題

- agent ごとの mTLS 証明書は未対応 (共有 token のみ).
- 長時間の poll と heartbeat は1本の HTTP/2 接続上で多重化される. 大規模な worker 数での負荷は未測定.
