# ADR 0021: Raft のログの外に置く, 内容で参照する blob store

- 状態: 採用 (Q9, Q24, Q25, Q26, Q34 は推奨の既定で決めた. RPC, 設定, 表, JSON の key の名前は提案)
- 日付: 2026-11-22
- 関連: Issue #36, #70, #71, #72, #73, #74, [ロードマップ §3](../roadmap.md), [ADR 0017](0017-raft-storage-bounds-and-durability.md), [ADR 0020](0020-cluster-feature-agreement.md), 設計書 §7.3

## 背景

- #20 で, ジョブの spec と結果を 8 MiB までに制限した (ADR 0017, `MAX_SPEC_BYTES` と `MAX_RESULT_BYTES`). 記録は Raft のログの項目に入り, openraft は `AppendEntries` を heartbeat の間隔で打ち切るので, 大きな項目は複製できないからである.
- 単独の scheduler では約 127 MiB の結果を返せたので, 利用者から見える後退になった. また, A2-A4 の certificate は 8 MiB を超えやすい.
- 設計書 §7.3 は「高可用化が必要になった時点で PostgreSQL とオブジェクトストレージへ移行する」と書く. 2026-11-06 に利用者は, scheduler が自前で複製し, `BlobBackend` の境界を置く案を選んだ (ロードマップの Q35).
- この ADR は, ロードマップ §3 の設計を, 実装の前に決定として書く. 段階は B1 (結果, この ADR の主な対象), B2 (引数), B3 (上限, 観測, CLI, backup) である.

## 決定

### 1. 方式: 境界で marker に置き換える

- 大きな値を持つ側 (B1 では agent, B2 では client) が, 先に `PutBlob` で値の本体を送る. 次に, 値を marker に置き換えてから, `ReportFinished` (B2 では `Submit`) を呼ぶ.
- Raft のログと状態機械には, marker だけが入る. 本体は各 scheduler の disk に置き, Raft とは別の接続で複製する.
- 採らない案は次の 2 つである.
  - **`propose()` の中で送る.** proposer は変更を書いた順に batch にし, 確定を待ってから次へ進む (`cluster/mod.rs` の `propose`). 大きな push が 1 つあるだけで, heartbeat の処理と割当てを含むすべての確定が止まり, `COMMIT_WAIT` (10 秒) を越えやすい.
  - **Store 層で透過に外す.** 同じ理由で確定が止まる. そのうえ, 大きな値が scheduler の mutex の中でメモリに載る.

### 2. 外部の object storage との比較と `BlobBackend` の境界

S3 互換の object storage に本体を置く案と比べた.

| 観点 | 自前で複製する | S3 互換の object storage |
|---|---|---|
| 運用で増える依存 | なし. scheduler の binary と disk だけで動く | storage の service, 認証情報, その可用性の監視. 研究室の閉じた network では用意しにくい |
| Raft の過半数と同じ耐久性 | 言える. 投票者の過半数が `fsync` した後でだけ参照を確定させる (§5) | storage の耐久性に依る. scheduler の過半数とは独立で, 片方だけが生き残る障害がありうる |
| 工数 | 大きい. 受信, 複製, 修復, GC を書く (B1 の 5 Issue) | 小さい. ただし, 試験に storage の模型が要る |
| 1 台の構成 | そのまま動く (自分だけが過半数) | 1 台でも storage が要る |

- 推奨は, 自前で複製する案である (Q35 の決定と同じ).
- 後で外部の storage に置き換えられるよう, 境界 `BlobBackend` (trait) を server の中に置く.
  - 境界の上: service 層 (`PutBlob`, `GetBlob`, `StatBlob`, `ensure_blobs`) と, 読出しの展開.
  - 境界の下: 本体の永続化, 過半数への複製, 取得, 修復, GC.
  - 操作は次の 4 つである: 本体を受け取り耐久化して `BlobRef` を返す, offset から読む, 手元と耐久性の状態を返す, 参照のないものを消す.
- 外部の storage の実装は, この ADR の範囲外である.

### 3. 内容アドレスと marker

- id は `sha256:<64 桁の小文字 16 進>` とする. JSON 値の blob の本体は, `canonical_json(value)` (`provenance.rs`) の UTF-8 bytes である.
- marker の形は `{"$blob": "sha256:...", "size": n}` とする. n は本体の byte 数である.
  - 出どころ `"from": {"job_id": ..., "pointer": ...}` は W2 で使う. 欄の名前は, ここで予約する. B1 の reader は `from` を読み飛ばす.
- marker を置ける場所は, 次の 3 つに限る.
  - 記録の `result` の値そのもの.
  - `result` が object のときの, その top-level の値.
  - spec の `arguments` の top-level の値 (B2).
- **曖昧さの規則.** marker を置ける場所にある object で, `$` で始まる key を 1 つでも持つものは, marker として読む.
  - 正しい marker の形 (`$blob` と `size` が必須, `from` が任意, ほかの key はなし) でなければ, scheduler は報告を断る.
  - agent は, marker を置ける場所に `$` で始まる key を持つ object があれば, その値を必ず blob にする. blob の本体は marker として読み直されないので, 曖昧にならない.
  - `blob_refs` が有効でない cluster では, agent はそのような値を報告できない. 誤り `UNSUPPORTED_RESULT` で失敗させる. この変更を `CHANGELOG.md` に書く.
  - 検査するのは marker を置ける場所だけである. 深い位置にある `$` の key は断らない.
  - B1 より前の記録にも, 形の上では marker に見える値がありうる. 派生の表 (§7) は `blob_refs` を有効にした後に適用した記録からだけ作るので, その値は本体を探されない. ただし, marker を解決する client は, その値を marker と読みうる. 数学の値に `$blob` という key が現れることはまずないので, この危険は受け入れる.
- **正規形の検査は, 値を使う側で行う.**
  - client は取り出すとき, agent は実行の前に (B2), 読んだ bytes の SHA-256 と, それが canonical JSON であることを確かめる. 違えば値として使わない.
  - scheduler は, 大きさと SHA-256 だけを stream のまま確かめる. 値を `Value` に読むことはしない. `canonical_json` は `to_string`, `from_str`, `to_string` の順に変換するので, 数百 MiB の値では元の大きさの数倍のメモリを使うからである.
- agent は, `Value` を `serde_json::to_writer` で一時 file に直接書き, 書きながら hash を計算する.
  - この bytes が `canonical_json` の出力と一致することを, 試験で確かめる.
  - 一致は, `serde_json` の `Map` が key を整列する `BTreeMap` であることに依る. feature の統合で `preserve_order` が有効になると key の順が崩れる. 既存の試験 `canonical_json_sorts_keys` がこれを検出する. 【現状】workspace のどの crate も `preserve_order` を有効にしていない.

### 4. 外に出す規則

- `INLINE_MAX_BYTES` (256 KiB) は, `result` (B2 では `arguments`) について, inline に残す合計の上限とする.
- canonical JSON の大きさで量り, 合計が上限を超えたら, 大きい top-level の値から順に blob にする. 上限以下になったら止める.
- 次のときは, 値全体を 1 つの blob にする.
  - 値が object でない.
  - marker が 1,024 個を超える.
  - 上の手順の後も上限を超える.
- top-level の値ごとの閾値は置かない. 256 KiB 未満の値が多数あると, 合計で約 8 MiB が Raft に入るからである. 記録には `spec` と `input_normalized` の 2 つの写しがあり, `changes` 表は 100,000 件を持ち, snapshot はデータベース全体の複製である. どれにも, その分がすべて残る.

### 5. node の中の置き場所と, 過半数に届けてから参照する手順

**置き場所.**
- 本体: `<blob_dir>/sha256/<先頭 2 桁>/<残り 62 桁>`. `blob_dir` の既定は `<data_dir>/blobs` で, 設定で別の volume に置ける.
- 受信中のもの: `<blob_dir>/incoming/<uuid>.part`. hash の合わないもの: `<blob_dir>/quarantine/`.
- 受信は, 新しい部品 (`crates/algebramesh-server/src/blobs.rs`) で行う. 既存の `SnapshotInbox` は使わない. offset 0 の chunk で他の転送の file を消し, file 全体をメモリに読み, `fsync` しないからである.
- 新しい部品の性質.
  - 転送ごとに独立している. 書きながら SHA-256 を数える.
  - 書込みは `spawn_blocking` の中で行い, 一定の量 (64 MiB) ごとに同期する.
  - 確定の手順は `record.json` (#37) と同じである. 大きさと hash を確かめ, `fsync` し, rename し, ディレクトリを `fsync` する.
  - 同じ hash の本体が既にあれば, 置き換えずに一時 file を消す. Windows で, 読出し中の file を rename で置き換えないためである.
  - 古い `.part` は, 起動時と動作中の両方で, 経過時間に応じて消す.
  - 空きが `blob_min_free_bytes` を下回ったら, 受付を `RESOURCE_EXHAUSTED` で断る. Raft は止めない.

**手順.**
1. 送り手は, `StatBlob` で過半数が既に持っているかを確かめる. 持っていなければ, leader に `PutBlob` を 4 MiB の chunk の stream で送る.
2. leader は自分の disk に書き, 各 member (learner を含む) に `Replication.PushBlob` で送る. 接続は Raft とは別にし, `blob_replication_bytes_per_second` で帯域を抑えられるようにする.
3. 投票者の過半数が `fsync` を済ませたら, leader は `BlobRef` を返す.
   - 過半数は leader を含めて数える.
   - joint consensus の間は, 2 つの構成のそれぞれで過半数を求める.
   - scheduler が 1 台の構成 (`Node::single`) では, 過半数は自分だけである. push はしない.
4. `AgentGateway.ReportFinished` (B2 では `Scheduler.Submit`) は, scheduler を呼ぶ前に service 層の `ensure_blobs` で marker を検査する. 検査は mutex の外で行い, 大きな転送はしない.
   - cluster のどこにも本体がないとき: `FAILED_PRECONDITION` を返す. 送り手は `PutBlob` からやり直す.
   - 本体はあるが過半数に届いていないとき: 複製を後ろで始め, すぐに `UNAVAILABLE` を返す. 送り手は待ってから `StatBlob` でやり直す.
   - どちらの場合も, 何も書かない.
5. agent の `deliver` は, これらの応答を受けたら, blob を送り直してから報告をやり直す. upload が失敗し続けても `RESOURCE_LIMIT` にはしない. ジョブは heartbeat の途絶で `LOST` になり, retry policy に任せる.
6. `blob_refs` の機能が有効でないとき, `PutBlob` は `FAILED_PRECONDITION` (B1 以後の scheduler) か `UNIMPLEMENTED` (それより前の scheduler) を返す. 送り手は, 8 MiB 以下なら inline で送り, 超えれば `RESOURCE_LIMIT` で失敗させる.
7. upload にかかる時間は `wall_seconds` に含めない. agent は, 報告の再試行の期限 (`REPORT_PATIENCE`) を値の大きさに応じて延ばす (Q25).

**耐久性の状態.** leader は, blob ごとに本体を `fsync` したと確かめた member の集合をメモリに持つ. leader が替わった後や再起動の後は, member に `Replication.HasBlobs` で問い合わせて作り直す.

### 6. RPC

| service | RPC | 意味 |
|---|---|---|
| `Scheduler`, `AgentGateway` | `PutBlob(stream BlobUpload) returns BlobRef` | 最初の message に `sha256` と `size` を置く. leader だけが受ける |
| 同上 | `GetBlob(BlobRequest) returns (stream BlobChunk)` | `BlobRequest{sha256, offset}`. offset から読む |
| 同上 | `StatBlob(BlobRequest) returns BlobStat` | `BlobStat{present, durable, size}` |
| `Replication` | `PushBlob(stream BlobUpload) returns BlobRef` | follower が書いて `fsync` してから答える |
| `Replication` | `FetchBlob(BlobRequest) returns (stream BlobChunk)` | 手元の本体だけを返す |
| `Replication` | `HasBlobs(BlobList) returns BlobList` | 手元にある hash を返す |
| `Replication` | `BlobsRegistered(BlobList) returns BlobList` | leader が, 参照されている hash を返す (GC 用) |

- `JobRef.accept_blob_refs` を足す (§8).
- B1 で agent と client が使うのは, `Scheduler` の `GetBlob` と `StatBlob`, `AgentGateway` の `PutBlob` と `StatBlob` である. 他は B2 と B3 で使う.

### 7. 派生の表

- 状態機械の SQLite に, 次の 2 つの表を足す.
  - `blobs (sha256 PRIMARY KEY, size, first_seq)`.
  - `blob_refs (sha256, job_id, role)`. `role` は B1 では `result` だけで, B2 で `arguments` を足した.
- B2 では, `arguments` の行は機能 `blob_arguments` (§12) が有効になった後に適用した変更からだけ導く. 表の版を 2 に上げたので, B2 の binary は開くときに表を作り直す.
- どちらも, `apply` が `StoredRow` の記録の中の marker から導く派生物である. `Change` の variant は増えない.
- 派生は, `blob_refs` の機能が有効になった後に適用した変更からだけ行う. それより前の記録は marker を持たない (§3).
- `meta` に派生の表の版 (`blob_tables_version`) を記録する. `migrate` は版が合わないときだけ表を作り直す. `migrate` は接続を開くたびに走るので, そのたびに全件を走査すると, 選挙の後の再開と snapshot の install が, データベースの大きさに比例して遅くなるからである.

### 8. 読出しと古い client への展開

- `read_committed` の closure の中では, 記録の文字列を複製するだけにする. 記録を読むことと blob の展開は, lock を放してから行う. 状態機械の mutex を持ったまま大きな blob を読むと, その間 Raft の適用が止まるからである.
- 展開を同時に行う数に, 上限 (2) を置く.
- `JobRef.accept_blob_refs` を付けない `Result` には, marker を展開した記録を返す.
  - 上限は, 返す message 全体で 96 MiB とする (Q34). `MAX_MESSAGE_BYTES` の 128 MiB より余裕を持たせる.
  - 上限を超えるときは `FAILED_PRECONDITION` を返す. marker を値と取り違えさせないためである.
- Rust の client は `accept_blob_refs` を付け, marker を `GetBlob` で解決する. 途中で切れた転送は `offset` で再開する.
- Python の `Job.value()` は, Rust の `Value`, JSON の文字列, `json.loads` の順に変換するので, 200 MiB の結果では数 GB のメモリを使う. 大きな結果のために `Job.download(path)` を足し, それを勧める. `download` は, 値の canonical JSON を file に書く. marker の部分は本体の bytes をそのまま流す. canonical JSON は部分の canonical JSON をつないだものなので, 全体も canonical JSON になる.

### 9. 取得, 修復, 誤り

- follower は, `Node::scheduler()` を通らない経路で `GetBlob` に答える. 返すのは, 手元にあって hash が合う本体だけである. 手元にないときは, leader の場所を添えて `UNAVAILABLE` を返す.
- leader は, 手元になければ peer から `FetchBlob` で取り, 手元に保存しながら返す. `NOT_FOUND` と `DATA_LOSS` は, leader が `ensure_linearizable` を済ませた後でだけ返す. follower の派生の表は適用が遅れうるので, follower が返すと誤りうるからである.

  | 誤り | 条件 (leader が判定する) |
  |---|---|
  | `NOT_FOUND` | 手元になく, 派生の表に登録されていない hash |
  | `UNAVAILABLE` | 登録されているが, 届く node のどれも本体を持っていない. 答えない member がいる |
  | `DATA_LOSS` | 全 member が答え, 誰も本体を持っていない |

- 読むたびに SHA-256 を確かめる. 合わない file は quarantine に移し, peer から取り直す. 別の値を返すことはない.
- 修復 (#73).
  - `apply` で「参照されているが手元にない hash」を待ち行列に入れ, lock の外で取りに行く. `PushBlob` の間止まっていて, ログで追いついた follower も, これで本体を得る.
  - 投票者の過半数に届いていない blob を定期的に探し, 複製を補う.
  - snapshot を install した後は, `blob_refs` の全 hash を手元の file と比べ, 足りないものを後ろで取る. 状態機械の適用は本体を待たない (#21 と同じ原則).
  - `remove_member` は, その node を外すと過半数を割る blob があれば, 断る.
  - `algebramesh server status` に, 過半数に届いていない blob の数, quarantine の数, 空き容量, learner の同期の進み具合を出す.
- `add_member` (Q24).
  - blob を参照する記録があるときは, learner として加えた時点で返す.
  - 同じ `AddMember` をもう一度呼ぶと, ログと参照される blob の同期が済んでいれば, 投票者にする.
  - 今の `add_member` はログの追いつきを最大 60 秒待つ. blob の同期は何時間もかかりうるので, 同じ呼出しの中では待たない.
- 本体のない blob を引数に持つジョブでは, agent は CAS を起動せず, ジョブを `FAILED` にする. 誤りの文には hash を書く (B2).

### 10. GC (最小限は B1 に含める)

- ジョブは消さないので, 参照された blob も消さない. 消すのは, 参照のない blob だけである.
- 参照のない blob は B1 の時点から日常的に生じる. 例: `LOST` の後に届いた報告の blob, 途中で切れた upload, 報告に至らなかった upload.
- 「触れた」とは, その node での `PutBlob`, `PushBlob`, `StatBlob`, `GetBlob`, `ensure_blobs` を指す. 触れた時刻は file の mtime に残し, 再起動の後も保つ.
- 消す条件は次の 2 つで, 両方を満たしたときに消す.
  - 最後に触れてから, `blob_orphan_grace` が過ぎた. 既定は 24 時間である. `COMMIT_WAIT` の 10 倍より短い設定は断る.
  - leader が `BlobsRegistered` で「登録なし」と答えた. leader は, 確定した状態と, 確定前の変更を含む作業用の store の両方を見る. `ensure_blobs` を通ったがまだ確定していない報告の参照を見落とさないためである.
- 消すときは hash ごとの lock を取り, 触れた時刻をもう一度確かめる.
- B1 に含めるもの: 参照のない blob の削除, `.part` の掃除, `max_blob_store_bytes` (超えたら `PutBlob` と `PushBlob` を `RESOURCE_EXHAUSTED` で断る).
- B3 に回すもの: token ごとの `PutBlob` の量の上限, 観測の指標, CLI, backup の手順.
- **B3 で決めたこと** (#96).
  - 量の上限 `blob_upload_bytes_per_token_per_hour` (既定 0 = 上限なし). client token ごとの token bucket で, 1 時間で均等に戻る. `PutBlob` の最初の message の大きさを, 本体を書く前に差し引く. 足りなければ `RESOURCE_EXHAUSTED`. 失敗した upload の分は戻し, 手元にある blob は数えない.
  - token の区別には, `TokenCheck` が request の extension に入れる `TokenId` (token の SHA-256 の先頭 8 bytes の 16 進) を使う. token そのものは保持しない.
  - 量は scheduler ごとのメモリに持つ. Raft に載せないのは, upload のたびに変更を確定させると, 量の上限のために書込みが増えるからである. 再起動と leader の交代で量が戻ることは, 上限を緩める向きにだけ働く.
  - agent token には掛けない. agent の upload は driver の出力の上限 (`max_output_bytes`) で抑えられ, 上限で結果の報告が止まると, ジョブが失われる.
  - 指標は `NodeStatus` に足した (`blobs_stored`, `blob_bytes_stored`, `blob_uploads_refused`). metrics の endpoint は置かない.
  - CLI `algebramesh blob put|get|stat`. `put --json` は値の canonical JSON を送る. agent は引数の blob が canonical JSON であることを求める (B2) ので, 引数の marker にはこちらを使う.
  - backup と復元の手順は `docs/install.md` §4.1 に書いた. metadata を先に, blob を後に取る.

### 11. 上限 (既定値. 計測してから見直す)

| 名前 | 既定 |
|---|---|
| `INLINE_MAX_BYTES` (inline に残す合計) | 256 KiB |
| `max_blob_bytes` | 256 MiB. driver の出力の上限 (`max_output_bytes`) 以下とする. 上げるときは両方を上げる |
| chunk | 4 MiB |
| `blob_min_free_bytes` | 1 GiB |
| `max_blob_store_bytes` | 0 (上限なし) |
| 1 つの値あたりの marker | 1,024 個 |
| 古い client に展開して返す message | 96 MiB |
| `blob_orphan_grace_secs` | 86400 (24 時間) |
| `blob_replication_bytes_per_second` | 0 (抑えない) |
| `blob_upload_bytes_per_token_per_hour` | 0 (上限なし. B3) |
| `blob_dir` | `<data_dir>/blobs` |

- `MAX_SPEC_BYTES` と `MAX_RESULT_BYTES` は 8 MiB のまま残す. marker に置き換えた後の大きさに当てる.
- agent のメモリの見積り: 256 MiB の値では, backend の出力の文字列, `Value`, 検査器の作業, 一時 file への書出しの順に処理し, 同時に持つのは値の大きさの数倍 (概ね 1 GiB) である. 一時 file から送るので, upload 自体はメモリを増やさない.

### 12. 互換

- 機能 `blob_refs` を B1 で定める (ADR 0020). B1 の binary だけが名乗る.
  - `blob_refs` が有効になるまで, scheduler は `PutBlob` を `FAILED_PRECONDITION` で断り, 報告の中の marker を断る. 古い scheduler は `UNIMPLEMENTED` を返す.
  - 1 台の構成では, 起動時に有効になる.
- agent は, `RegisterResponse.features` で `blob_refs` を知るまで marker を使わない.
- scheduler は, agent が `RegisterRequest.features` で申告した機能を記録する (【現状】は読み捨てている). marker を引数に持つジョブを, `Placement.needs_blob_refs` によって, 申告した agent にだけ割り当てるのは B2 で行う. B1 では marker は結果にしか現れず, 結果の marker は agent 自身が作るからである.
- 古い client には, §8 の展開で答える.
- **B2 で決めたこと.** 引数の marker には, 新しい機能 `blob_arguments` を定めた. B2 の binary だけが名乗る.
  - `blob_refs` だけで引数の marker を使うと, B1 と B2 の混ざった cluster で B1 の leader が marker を検査せずにジョブを作り, 派生の表にも載せない. GC がその blob を消しうる.
  - `blob_arguments` が有効になるまで, scheduler は引数の marker を普通の値として扱う. これまでと同じである. client は `ListBackendsResponse.server_features` でこの機能を確かめてから引数を外に出す.
  - agent は `RegisterRequest.features` に `blob_arguments` を含める. scheduler はそれを記録し, `Placement.needs_blob_refs` のジョブを, それを申告した agent にだけ割り当てる. 申告した agent が online にいなければ, 投入を `Unplaceable` で断る (online の worker が 1 台もいなければ待たせる. 既存の規則と同じ).
- 古い client への展開 (§8) は, `spec.arguments` の marker も展開する. `input_normalized` と `input_sha256` は記録した spec のものを返す (§13).

### 13. hash の意味

- request の fingerprint は, B2 で「内容の hash の形」から作る.
  - この形は, spec の `arguments` の各 top-level の値を, その canonical JSON の SHA-256 に置き換えたものである. marker の値は `$blob` の hash に置き換える. `size` と `from` は含めない.
  - したがって, 同じ値は inline でも blob でも同じ fingerprint になり, 送り手の `INLINE_MAX_BYTES` にも左右されない.
  - `jobs` の行ごとに fingerprint の版を記録する. 既存の行は版 1 (今の式) とし, 新しい request はその行の版の式で比べる. marker を含む request を版 1 の行と比べることになったら, 理由を添えて断る.
  - B2 では, 版を別の列にせず, 既存の列 `request_fingerprint` の値の接頭辞 `v2:` で表した. 版 1 の値は接頭辞を持たない 16 進である. 列を足すと, 状態機械の schema を変える migration が要るからである.
  - 版 2 で記録するのは, `blob_arguments` が有効になった後の request だけである. 有効になる前は, B1 の leader と同じ式で比べられるように版 1 のままにする.
  - B1 では引数に marker が現れないので, fingerprint は変えない.
- `input_sha256` は, 記録した spec (marker を含む) の canonical JSON の hash のままとする (Q9).
  - したがって B2 以後, 同じ入力でも, 送り手の閾値によって `input_sha256` が変わりうる. ジョブの間の同一性には fingerprint を使う.
- #36 の本文は「検算は store から読んだ値に対して行う」とする. これを次のように満たす (Q26).
  - agent は, 検査した値の canonical bytes をそのまま upload する. 記録される hash は, 検査した値と結び付く.
  - store から読み直して検査し直すのは, V4 (`algebramesh verify`) と V5 (検証ジョブ) である.

### 14. 確かめた決定 (ロードマップ §10)

| 問い | 決定 |
|---|---|
| Q9 | marker を含む spec の `input_sha256` は, 記録した spec の hash のままとする |
| Q24 | blob があるときの `AddMember` は, learner として加えて返し, 2 回目の呼出しで投票者にする |
| Q25 | upload の時間は `wall_seconds` に含めず, 報告の再試行の期限を値の大きさに応じて延ばす |
| Q26 | 検査した canonical bytes を upload し, store からの再検査は V4 と V5 で行う |
| Q34 | 古い client に展開して返す上限は 96 MiB |

## 含まないもの

- 外部の object storage の実装. 境界 `BlobBackend` だけを置く.
- 参照された blob の削除と, ジョブの保存期間.
- `UpdateJob` の差分化 (W5 で行う).

## 結果

- 8 MiB を超える結果を, Raft のログと snapshot を大きくせずに返せる. 本体は投票者の過半数の disk にあり, leader が替わっても取り出せる.
- scheduler の disk の使用量は, 結果の大きさに比例して増える. 参照された blob は消さないので, 量の上限と backup の手順を B3 で整える.
- 本体が失われたときは, 黙って別の値を返さず, `DATA_LOSS` になる.
