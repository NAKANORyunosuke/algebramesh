# ADR 0017: Raft の snapshot, 大きさの上限, 永続性

- 状態: 採用
- 日付: 2026-11-05
- 関連: [ADR 0013](0013-scheduler-raft.md), Issue #20, #21, #22, #23, #37

## 背景

ADR 0013 の Raft には, 大きさと永続性について次の穴があった.

- snapshot はメタデータの SQLite 全体で, 1 つの gRPC メッセージ (128 MiB) で送っていた. データベースが 128 MiB を超えると, ログを捨てた後に加わる, または戻る scheduler が追いつけなかった (#20).
- 1 つのログ項目と 1 回の `AppendEntries` の大きさに上限がなかった. openraft は `AppendEntries` を heartbeat の間隔で打ち切るので, 大きな要求は打ち切りと再送を繰り返して進まない (#20, 試験で確かめた).
- SQLite の COMMIT の後で artifact (`record.json`) の書込みが失敗すると, 確定した変更が失敗として返り, follower では状態機械の適用の失敗として Raft に伝わった (#21).
- 遅れて届いた結果の報告の本文を, そのままイベントに書いて複製していた (#22).
- 状態機械の SQLite は WAL と `synchronous = NORMAL` で, 電源断や OS の停止では最後の COMMIT が失われうる. 一方で Raft は snapshot までのログを捨てる (#23).

## 決定

### snapshot の分割転送 (#20)

- leader は snapshot を 16 MiB ずつ (`ClusterConfig.snapshot_chunk_bytes`) `SendSnapshotChunk` で送り, 最後に `InstallSnapshot` でメタデータと全体の大きさ, SHA-256 を送る.
- follower は chunk を transfer ごとの一時ファイル (`<data_dir>/snapshots/snapshot-in-<uuid>.part`) に順に書く. 位置の飛んだ chunk は断る. 全体の大きさと SHA-256 が合ったときだけ install する. transfer の id は UUID だけを受け付ける. 新しい transfer は中断した transfer のファイルを消す.
- 送受信のメモリ: snapshot は openraft の型 (`Vec<u8>`) なので, 送る側と install する側で一度は全体をメモリに持つ.
- scheduler 同士の `Replication` サービスのメッセージの上限を 1 GiB にする (peer token で認証した相手だけ). client と agent は 128 MiB のまま.

### ログ項目の大きさ (#20)

- ジョブの結果 (`MAX_RESULT_BYTES`) と仕様 (`MAX_SPEC_BYTES`) は 8 MiB まで. 大きな結果は報告の時点でジョブを `RESOURCE_LIMIT` で失敗させ, 大きな仕様は投入の時点で断る. agent も 8 MiB を超える結果は送らずに誤りに置き換える.
- 1 つの Raft 項目に入れる変更は 4 MiB まで (1 つの変更がそれより大きければ単独で). 1 回の `AppendEntries` は 4 MiB まで送り, 残りは `PartialSuccess` で次に回す (1 項目は必ず送る).
- heartbeat の間隔を election timeout の 1/3 (既定 500 ms) にする. openraft はこの時間で `AppendEntries` を打ち切るので, 1 回で送る量はこの時間に送れる量でなければならない.

### artifact はコミットの外 (#21)

- artifact は確定したデータベースから作れる副産物とし, 書込みの失敗は COMMIT の結果にも, 状態機械の適用の結果にも影響させない. 失敗したジョブを覚えておき, 5 秒以上たってからの次の書込みと, leader の sweep で書き直す. データベースを開くときに artifact のディレクトリを作れなくても, データベースは開く.

### イベントの大きさ (#22)

- 遅れて届いた報告は, 送った agent, attempt, 理由, 本文の大きさと SHA-256 だけをイベントに残す. 同じ内容の報告は一度だけ記録する.
- イベントの本文は 4 KiB (`MAX_EVENT_BYTES`) までとし, それより長いものは切る.

### 永続性 (#23)

- 状態機械の SQLite は `synchronous = FULL` とする (Raft のログと同じ). 適用した変更と適用したログの位置は, 電源断の後も残る. したがって snapshot までのログを捨てても, その位置までの状態は失われない.
- leader の作業用の store は term ごとに状態機械から作り直すので, `synchronous = NORMAL` のままとする.
- snapshot の install (`Store::restore`) は, 次の順にする. どこで止まっても, 古いデータベースか新しいデータベースのどちらかが丸ごと残る.
  1. 新しいデータベースを同じディレクトリのファイルに書き, `fsync` する.
  2. 古いデータベースを checkpoint し (WAL を空にする), 閉じる.
  3. 古い WAL と shared memory のファイルを消す.
  4. 新しいファイルを古いファイルの名前に rename し (置き換えは不可分), ディレクトリを `fsync` する (Unix).

### 障害への備え (#37)

[障害注入の記録](../audits/2026-11-06-fault-injection.md) で見つけたことから, 次を加えた.

- ログの破棄は, state machine がその位置まで適用するのを待つ. openraft 0.9.25 は snapshot の install の完了を待たずにログを破棄するので, その間の crash でログだけが先に捨てられ, 起動できなくなっていた. ディスクの上で, 捨てた位置は適用した位置を超えない.
- 破棄した位置の記録と項目の削除は 1 つのトランザクションで行う.
- state machine の処理の panic は storage の誤りとして openraft に返す. Raft が storage の誤りで止まると, `serve_node` は `raft stopped` の誤りで終わり, process も終わる. 原因 (ディスクの空き不足など) を取り除いて起動し直す.
- leader の作業用の store の書込みが失敗したとき, または leader に選ばれた node の scheduler が始められないとき, 他に投票者がいれば, その node は term を手放し, heartbeat と選挙を止めて他の node に leader を任せる. 他の node が leader になると heartbeat を戻し, election timeout の 10 倍の間は選挙を始めない. 投票者が自分だけなら, 間を置いて新しい term の選挙を始める.
- 起動時に, 途中で止まった snapshot の copy (`snapshot-<uuid>.sqlite3`) と置き換え用のデータベース (`*.restore`) を消す.
- `record.json` は一時ファイルを sync してから rename し, ディレクトリを sync する. store を開くとき, 終わったジョブの欠けた artifact を書き直す.

## 結果

- 結果の上限が 8 MiB になった. これまで単独の scheduler では約 127 MiB まで返せた. より大きな結果は, Raft のログの外に置く store (内容の hash で参照する) が要る. これは今後の課題である.
- 大きな項目の複製は遅い. 1 MiB のジョブ 80 件分のログ (約 80 MiB) に止めていた follower が追いつくのに 23 秒掛かった (1 プロセスに 3 台, release, Windows).
- `synchronous = FULL` で, 状態機械への適用ごとに fsync が増える.

## 確認

- 単体試験: chunk の組立て (順序, 欠け, 改ざん, 中断, UUID でない id), 書けない artifact (COMMIT と follower の適用が成功し, 書けるようになると DB と同じ `record.json` が書かれる), restore (WAL のある古いデータベースを置き換える, 途中の `.restore` が残っても古いデータベースが開ける, `synchronous` の値), 遅れた報告のイベント (10 MiB の結果を 20 回送り直しても 1 件, 1 KB 未満), 結果と仕様の上限.
- Raft の結合試験: snapshot までログを捨ててから 4 台目を加え, 64 KiB の chunk の snapshot で追いつく. follower 1 台の artifact のディレクトリを書けなくしても追従し続け, 書けるようになると artifact を書く.
- 手で流す試験 (`--ignored`, release): メタデータのデータベースが 336 MB のとき, 4 台目が 16 MiB の chunk の snapshot で 6.4 秒で追いついた. 止めていた follower が 80 MiB のログに 4 MiB ずつの `AppendEntries` で追いついた (分割する前は 64 MiB の要求が heartbeat の間隔で打ち切られ続け, 追いつかなかった).
- 障害注入 (#37, [記録](../audits/2026-11-06-fault-injection.md)): snapshot の作成, ログの破棄, install の各段階の直後の crash の後に起動し直して他の node と一致すること (`raft_faults.rs`). follower, leader, snapshot の受け手のディスクの空き不足 (64 MiB の tmpfs, `tests/integration/run-disk-full.sh`). 転送中の接続の切断, 遅延, 一時停止 (TCP の proxy).
- 未確認: 実際の電源断 (仮想マシンの電源を切る試験, 書込みを落とすブロックデバイス), OS の停止をまたぐ `fsync` の順序. これらは手順と SQLite の仕様にもとづく設計であり, 障害注入では確かめていない. crash の試験は process の kill に当たり, OS の page cache は残る.
