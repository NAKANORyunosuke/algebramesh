# ADR 0012: scheduler の primary と standby

- 状態: [ADR 0013](0013-scheduler-raft.md) (Raft) に置き換えられた
- 日付: 2026-11-03
- 関連: 設計書 §7, [ADR 0004](0004-scheduler-agent-protocol.md)

2026-11-04 に, 3 台以上の scheduler の Raft (ADR 0013) に置き換えた.
この ADR の `[ha]` の設定, `compose.ha.yml`, `run-ha-e2e.sh` は削除した.
以下は置き換える前の記録である.

## 背景

scheduler は 1 プロセスで, SQLite の metadata store をローカルディスクに置いている.
scheduler のホストが止まると, 投入, 状態の問い合わせ, ジョブの割り当てがすべて止まる.
SQLite をネットワークファイルシステムで共有することは, 設計書が禁じている.

## 決定

scheduler を 2 台, primary と standby で動かす.
外部の合意サービス (etcd 等) は使わない.

### 複製

- 変更履歴: store へのすべての書き込みを `Change` (ジョブの挿入と更新, イベント, agent の登録) として表す. 適用と `changes` 表への追記を 1 つのトランザクションで行う. 変更には連番が付く.
- 追従: standby は起動時に primary の store を丸ごと受け取る (`Snapshot`, SQLite の `VACUUM INTO`). その後は連番の続きを long-poll で受け取り (`Pull`), 同じ順に適用する.
- 保持: primary は最新の 100,000 件の変更を残す. それより遅れた standby は snapshot を取り直す.
- 非同期: 複製は非同期である. primary が止まる直前の変更は standby に届かないことがある (通常は 1 秒未満).
- 通信: 複製は scheduler と同じポートの `Replication` サービスで行う. TLS と, peer 用の token (`peer_token_file`) で認証する.

### 役割と epoch

各 store は epoch を持つ. primary になるたびに, 見たことのある epoch より大きい値にする.

- 起動時: peer が primary として動いていれば standby になる. そうでなければ `initial_role` に従う.
- standby: client と agent の呼び出しに `UNAVAILABLE` を返す.
- 自動昇格: standby は primary と `failover_after_ms` の間通信できなければ, epoch を上げて primary になる. 0 にすると自動では昇格せず, 運用者が `algebramesh server promote` で昇格させる.
- 昇格時の扱い: 割り当て済みのジョブは `LOST` になる. scheduler の再起動と同じ扱いで, `retry_policy` に従って再投入される.
- fencing: primary は peer の状態を定期的に確かめる. peer が primary で, epoch が大きい (等しければ node_id が小さい) ときは, 自分を止める (fenced). プロセスは終了コード 1 で終わるので, supervisor が再起動すると standby として戻る.

### client と agent

- URL: scheduler の URL をカンマ区切りで複数書ける. client と agent は, 最後に応答したものを使い, `UNAVAILABLE` なら次を試す.
- agent: 登録した scheduler が使えなくなると, 実行中のジョブを止めて他の scheduler に登録し直す. ジョブは新しい primary で `LOST` になっているからである.
- epoch: agent は見た中で最大の epoch を覚え, それより小さい epoch の scheduler には戻らない.
- 接続の期限: 名前の解決と TCP の接続, および TLS の握手は, それぞれ client と agent で 10 秒, scheduler の間で 5 秒までとする. 期限を過ぎた接続は `UNAVAILABLE` になり, 次の scheduler に移る.
  tonic の `connect_timeout` は TCP の接続にしか掛からない. SSH のポート転送やプロキシは, 転送先が止まっていても TCP の接続を受け付ける. そのため, 転送側が諦めるまで TLS の握手が待たされる (2 台での試験では約 2 分).
  `algebramesh_rpc::connect` と `connect_lazy` は, 名前の解決と TCP の接続 (IPv6 と IPv4 の両方の試み) に 1 つの期限を掛け, その後, TLS の握手が終わるまでの読み込みにも同じ長さの期限を掛ける. 接続全体では期限の 2 倍までかかりうる. 握手の終わりは, client が最初の `application_data` の record を書いた時点とする. TLS 1.3 の client が `ServerHello` の後に書く互換用の `change_cipher_spec` (RFC 8446 Appendix D.4) は握手の終わりではない (Issue #87).
  握手の期限を TCP の接続を始めた時点から測らないのは, lazy な channel の接続が, 呼出しが待っている間しか進まないからである (tower の buffer は, 取り消された呼出しを捨てると接続を進めない). Raft の `AppendEntries` は heartbeat の間隔 (election timeout の 3 分の 1. 既定で 500 ms, 試験では 100 ms) で打ち切られるので, scheduler の間の接続は短い区間ずつしか進まない. TCP の接続まで 1 つの期限に入れると, 負荷の高い試験で握手が期限に届かず, leader が過半数に複製できなくなった.

## 結果

- 1 台のホストが止まっても, `failover_after_ms` (既定 15 秒) と agent の再登録の後, 投入と実行が再開する.
- 分断時: primary と standby が互いに見えなくなり, どちらも client や agent から見える場合は, 一時的に primary が 2 つになりうる. 合意サービスを使わない 2 台構成の限界である. 分断が解けると epoch の小さい方が止まる. その間に古い primary が受け付けたジョブは失われる.
- 自動昇格を避ける構成: 分断の恐れが大きいネットワークでは `failover_after_ms = 0` とし, 運用者が primary の停止を確かめてから昇格させる.
- snapshot: 1 つの gRPC メッセージ (128 MiB) に収まる大きさまでに限る.
- 成果物: `artifacts` は standby でも変更から書き出す.

## 確認

- `crates/algebramesh-agent/tests/ha.rs` (1 プロセス内): 複製, primary の停止と standby の昇格, client と agent の切り替え, 古い primary の standby としての復帰, epoch の小さい primary の fencing.
- `tests/integration/run-ha-e2e.sh` (Docker, CI の `ha-e2e`): 実際のプロセスで, primary の強制停止, 昇格, ジョブの継続, 復帰.
- 2 台の物理ホスト (2026-11-04, 下の記録): primary と standby を別のホストに置き, 両方向の切り替えと復帰を確かめた.
- `algebramesh-rpc` の単体試験: 接続を受け付けて何も返さない相手に対して, `connect` が期限で失敗し, `connect_lazy` の呼び出しが `UNAVAILABLE` で返る.

## 2 台の物理ホストでの記録 (2026-11-04)

install.md §8.2 の 2 台 (A: WSL2 の Docker, B: Docker Desktop) に primary と standby を 1 つずつ置き, Tailscale の上の SSH のポート転送で両方向につないだ.

| 段階 | 結果 |
|---|---|
| 開始 | A の scheduler-a が primary, B の scheduler-b が standby になり, standby は primary の store を写した |
| 振り分け | 因数分解 6 件が A と B の worker に 3 件ずつ振り分けられ, 成功した |
| A の primary を強制停止 | 12.2 秒で scheduler-b が primary (epoch 2) になった. それまでの 6 件は残り, 新しい 6 件も成功した |
| scheduler-a を再起動 | epoch 2 の standby として戻った |
| B の primary を強制停止 | 13.0 秒で scheduler-a が primary (epoch 3) になった. それまでの 12 件が残り, 新しい 4 件も成功した |

- 接続の期限: 最初の試験で, 止まった scheduler への client の呼び出しが約 2 分返らなかった. SSH の転送が TCP の接続を受け付け, TLS の握手が待たされたためである. 接続の期限を TLS の握手まで掛けるように直した後は, 同じ状態の呼び出しが 10.4 秒で打ち切られた. この修正は ADR 0013 でもそのまま使う.
- Windows の予約ポート: 49152 番以降の一部は Windows が予約していることがある (`netsh int ipv4 show excludedportrange protocol=tcp`). 50052 番が予約されていたので 50152 番を使った.
