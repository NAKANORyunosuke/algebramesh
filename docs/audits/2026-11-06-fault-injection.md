# 障害注入の記録: crash, ディスクの空き不足, 転送中の通信断 (2026-11-06)

- 追跡: Issue #37 ([2026-11-05 の監査](2026-11-05-reliability-audit.md) の残りの課題)
- 対象: commit `a58bf64` に, この記録と同じコミットの修正を加えた作業ツリー
- 区分は 2026-11-05 の監査と同じ (試験, 手で流す試験, Docker, 設計, 未確認).
- この記録は「バグがない」ことの証明ではない. 注入した障害と確かめたこと, 注入していない障害を分ける.

## 1. 注入の方法

| 障害 | 方法 | 何に近いか |
|---|---|---|
| crash | feature `fault-injection` の crash point (`crates/algebramesh-server/src/fault.rs`). 試験が 1 台の data directory に点を仕掛けると, その点を通ったところで panic し, 点の後は何も実行しない. state machine の panic は storage の誤りになり, Raft が止まって scheduler が終わる. 同じ data directory で起動し直す | process の kill (`kill -9`). OS の page cache は残る |
| ディスクの空き不足 | 1 台の data directory を 64 MiB の tmpfs (Docker の `--tmpfs`) に置き, 残りを file で埋める. 空きを戻すときは file を消す | 本当の `ENOSPC`. SQLite は `database or disk is full` を返す |
| 転送中の通信断 | 1 台の手前に TCP の proxy を置き, 他の node はその proxy を経由して接続する. proxy は, 接続を決まったバイト数で切る, 16 KiB ごとに遅らせる, 転送を止める. TLS の record を見るだけで, 中身は変えない | ネットワークの切断, 遅延, 一時的な停止 |
| 古い転送の chunk の遅延 | snapshot の受け口 (`SnapshotInbox`) の単体試験. 新しい転送の途中に, 古い転送の最初の chunk を届ける | 遅れて届いたメッセージ (同じ転送の中の並べ替えは TCP が起こさない) |

試験は `crates/algebramesh-agent/tests/raft_faults.rs` にある. ディスクの試験は `#[ignore]` で, `tests/integration/run-disk-full.sh` (CI の `disk-full`) が Docker で流す.

## 2. 結果

| 障害 | 確かめたこと | 区分 |
|---|---|---|
| follower が自分の snapshot の copy (`VACUUM INTO`) の直後に crash | 起動し直すと, 残った copy は消え, leader と同じジョブ, 同じ最後の変更, 同じログの位置と membership になる | 試験 |
| ログを snapshot まで捨てた直後に crash | 同上 | 試験 |
| 受け取った snapshot を書いて sync した直後 (検証の前) に crash | 同上. 修正の前は, 起動し直すと openraft が panic した (下の 3.1) | 試験 |
| 古いデータベースを checkpoint して閉じ, WAL を消した直後 (rename の前) に crash | 同上. 修正の前は同じく panic した | 試験 |
| rename の直後 (ディレクトリの sync と開き直しの前) に crash | 同上 | 試験 |
| 新しいデータベースに置き換えた後, ログの位置と membership を書く前に crash | 同上. 置き換えたデータベースは leader が snapshot を作ったときの位置と membership を持つ | 試験 |
| follower のディスクが満杯 | follower は `raft stopped: ... database or disk is full` で終わり, 他の 2 台は投入を確定し続ける. 空きを戻して起動し直すと追いつく | Docker |
| leader のディスクが満杯 | leader の作業用の store の書込みが失敗すると, leader は term を手放し, 他の 2 台が leader を選ぶ. 満杯の node はその後 Raft の書込みで止まる (`raft stopped`). 空きを戻して起動し直すと follower として追いつく. 修正の前は leader が降りず, すべての書込みが `INTERNAL` になった (下の 3.3) | Docker |
| snapshot を受け取る follower のディスクが満杯 (1 MiB だけ空き, snapshot は約 4 MiB) | chunk の書込みが断られ, leader は転送をやり直し続ける. follower は止まらない. 空きを戻すと, 起動し直さずに snapshot が届いて追いつき, 受け口に一時ファイルは残らない | Docker |
| snapshot の転送の接続を 300 KiB で 3 回切り, 16 KiB ごとに 2 ms 遅らせ, 途中で election timeout の 3 倍の間止める | 後の転送で snapshot が丸ごと届き, follower は leader と一致する. 受け口に一時ファイルは残らない | 試験 |
| 新しい転送の途中に古い転送の最初の chunk が届く | 新しい転送はその位置から続けられずに失敗し, 古い転送も大きさと SHA-256 が合わずに使われない. 次の転送で正しいデータベースが届く. 誤ったデータベースは install されない | 試験 (単体) |
| leader に選ばれた node の scheduler が始められない (作業用の store が作れない) | その node は term を手放して heartbeat と選挙を止め, 他の node が leader になって投入を受け付ける. 手放した node は follower として追従する | 試験 |
| ログの破棄を求められた follower の state machine が, その位置をまだ適用していない | 破棄は適用を待つ. state machine が止まれば破棄は誤りになる (待ち続けない) | 試験 (単体) |
| 書けなかった artifact, 電源断で失われた artifact | store を開くと, 終わったジョブの欠けた `record.json` を書き直す | 試験 (単体) |

Raft の結合試験 (`raft.rs` と `raft_faults.rs`) を Windows と, Windows の Docker (Linux の container) で繰り返し流した結果は 4 節にある.

## 3. 見つけて直した欠陥

### 3.1 snapshot の install の途中の crash で, 起動できなくなる

openraft 0.9.25 は, follower に snapshot の install を頼むと, install の完了を待たずにログの破棄 (`purge`) を進める (`install_full_snapshot` は install の命令と破棄の命令を続けて出し, 破棄には待つ条件がない). そのため, 受け取った snapshot を置き換える前に crash すると, ディスクの上ではログが snapshot の位置まで捨てられ, state machine は古い位置にある. 起動し直すと openraft は `purge_upto <= snapshot_last_log_id` の検査で panic し, その node は二度と起動しなかった.

ログの破棄は, state machine がその位置を適用するまで待つようにした. ディスクの上で, 捨てた位置は適用した位置を超えない. 待つ間, follower の Raft の core は止まるが, install が終われば進む. state machine が止まったときは破棄が誤りになる.

### 3.2 state machine の task の panic で, node が止まりも進みもしなくなる

openraft は, state machine の task が panic しても, 次にその task に命令を送るまで気づかない. その間, node は適用も停止もしない. state machine の処理 (適用, install, snapshot の作成) の panic を storage の誤りに変え, Raft がすぐに止まるようにした.

### 3.3 Raft が止まった後も, 何も確定できない scheduler として動き続ける

storage の誤りで Raft が止まっても (openraft では回復しない), process は gRPC を受け付け続けていた. `serve_node` は Raft が止まると `raft stopped: <原因>` の誤りで終わり, process も終わる. 原因を取り除いて起動し直すと回復する.

### 3.4 書けない leader, 始められない leader が term を握り続ける

- leader の作業用の store の書込みが失敗したとき (leader のディスクが満杯), leader は降りなかった. heartbeat は続くので他の node は leader を選ばず, cluster はすべての書込みを `INTERNAL` で断り続けた.
- leader に選ばれた node の scheduler が始められないとき, その node は term の間ずっと leader のまま, 何も受け付けなかった.

どちらの場合も, 他に投票者がいれば, その node は term を手放し, heartbeat と選挙を止める. 他の node が leader になると heartbeat を戻し, しばらく選挙を始めない (すぐに勝って同じことを繰り返さないように). 投票者が自分だけなら, 間を置いて新しい term の選挙を自分で始め, 状態機械から作業用の store を作り直す.

### 3.5 その他

- snapshot の copy (`snapshot-<uuid>.sqlite3`) と置き換え用のデータベース (`*.restore`) は, 途中で止まると残り続けた. 起動時に消す.
- ログの破棄は, 破棄した位置の記録と項目の削除の 2 回の COMMIT だった. 1 つのトランザクションにした.
- `record.json` は sync せずに rename していた. 一時ファイルを sync してから rename し, ディレクトリを sync する. 書けなかった artifact の集合はメモリにしかなかったので, store を開くときに欠けたものを探して書き直す.
- 置き換えたデータベースを開き直せないとき, store はスキーマのない一時的なデータベースのまま, 「表がない」という誤りを返していた. 開き直せないことと, 起動し直すことを示す誤りにした.

## 4. 繰り返し流した結果

| 環境 | 試験 | election timeout | 回数 | 結果 |
|---|---|---|---|---|
| Windows の Docker, #41 の修正の前 | `raft.rs` (当時の 9 件) | 300 ms | 10 | 5 回失敗. すべて, 試験が起こしていない選挙による |
| Windows (1 プロセスに試験ごとの cluster) | `raft.rs`, `raft_faults.rs` | 300 ms | 4 | すべて成功 |
| Windows の Docker (Linux の container) | `raft.rs` | 1000 ms | 10 | すべて成功 |
| 同上 | `raft.rs` | 300 ms | 10 | すべて成功 |
| 同上 | `raft_faults.rs` | 1000 ms | 10 | すべて成功 |
| 同上 | `raft_faults.rs` | 300 ms | 10 | すべて成功 |
| 同上, 64 MiB の tmpfs | `disk_full_*` (3 件) | 1000 ms | 2 | すべて成功 |

途中の繰り返しで, 試験の側の次の問題を見つけて直した. どれも実装の欠陥ではない.

- 起動直後の選挙の直後に, 別の node がまだ後の term の候補者でいることがある. 試験は「serving の node が 1 つ」だけで安定した leader とみなしていたので, その leader が直後に降りて `ABORTED` になった. 全 node が同じ term と同じ leader を見るまで待つ.
- 試験用の `compact_log` は, snapshot の位置が呼んだ時点の適用位置と等しいことを待っていた. その間に適用が進むと条件が成り立たない. その位置以上を待つ.
- leader を替える `promote` は, follower が leader の lease の間は投票を断るので, 成り立つとは限らない. leader が term を手放す `hand_over` と, 選挙を止める `hold_elections` (試験用) で, 決まった node に leader を移す.
- leader が替わった直後の投入は `UNAVAILABLE` になりうる. 試験の投入は, client に求めているとおり, 同じ鍵で送り直す.

## 5. 確かめていないこと

- 電源断. crash point は process の kill に近く, OS の page cache は残る. 電源断で違いが出るのは sync していないファイルだけである: snapshot の受け口の部分ファイル (大きさと SHA-256 で確かめる), artifact (今回 sync し, 欠けたものは開くときに書き直す), `VACUUM INTO` の一時ファイル (起動時に消す). SQLite の `synchronous = FULL` の COMMIT と, restore の staged file, rename, ディレクトリの sync の順序は, SQLite と POSIX の仕様にもとづく設計である. 書込みを落とすブロックデバイス (`dm-flakey`) や FUSE の file system (LazyFS) での試験はしていない. どちらも root 権限が要る.
- Raft のログ (`raft.sqlite3`) の書込みの途中の crash. 書込みは SQLite のトランザクションで, crash point は置いていない.
- 書込みの失敗が一部の node にだけ, 断続的に起きる場合 (ディスクの不良).
- 同じ転送の中での chunk の並べ替えと重複. TCP の上では起きないので, 単体試験で断ることだけを確かめた.
