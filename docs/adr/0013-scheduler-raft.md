# ADR 0013: scheduler の複製を Raft にする

- 状態: 採用
- 日付: 2026-11-04
- 関連: [ADR 0012](0012-scheduler-high-availability.md) (これに置き換えられた), 設計書 §7

## 背景

ADR 0012 の primary と standby は, 2 台の間の非同期の複製である.
2 台が互いに見えなくなると, 両方が primary になりうる.
また, primary が止まる直前に受け付けた変更は失われうる.
どちらも, 多数決で決める合意がないことによる.

## 決定

3 台以上の scheduler を Raft で複製する.
Raft の実装には openraft 0.9 (MIT または Apache-2.0) を使い, 外部の合意サービスは使わない.
ADR 0012 の primary と standby は廃止する (`[ha]` の設定は受け付けない).

### Raft のログと状態機械

- ログの項目: store への変更 (`Change`) の列と, その変更を書いた作業用の store の term である. 各変更は store の連番を持つ.
- 状態機械: 各 scheduler の SQLite の store (`metadata.sqlite3`) である. 多数派に記録されたログの項目だけを, ログの順に適用する. 適用したログの位置と構成 (membership) は, store の `meta` 表に変更と同じトランザクションで書く.
- ログの保存: Raft のログ, 投票, 確定位置は, 別の SQLite (`raft.sqlite3`) に書く.
- snapshot: store を丸ごと写したもの (`VACUUM INTO`) である. 16 MiB ずつに分けて送り, 受け手は大きさと SHA-256 を確かめてから install する ([ADR 0017](0017-raft-storage-bounds-and-durability.md). 以前は 1 つの gRPC メッセージ (128 MiB) で送っていた). ログが 10,000 項目増えるごとに作り, その前のログを捨てる.

### leader の書き込み

scheduler の処理 (配置, 状態遷移, heartbeat の扱い) は同期の mutex の中で store を読み書きする.
これを「多数派に届いてから適用する」形に書き換える代わりに, leader は作業用の store を持つ.

1. leader になった node は, 自分の term の項目が状態機械に適用されるのを待つ. これで, それまでに確定した変更がすべて状態機械に入る.
2. 状態機械の store を写して, 作業用の store (`working.sqlite3`) を作る. scheduler はこれを使う. 起動時と同じく, 割り当て済みのジョブは `LOST` になる.
3. scheduler が作業用の store に書いた変更は, 書いた順に Raft のログに提案する.
4. client と agent の呼び出しに答える前に, その呼び出しまでに書いた変更を運んだ, この leader の term のログの項目が確定し, 適用されるのを待つ (下の「答えと変更の対応」). 10 秒で確定しなければ `UNAVAILABLE` を返す.
5. leader でなくなったら, 確定を待っている呼び出しに `ABORTED` を返し, scheduler と作業用の store を捨てる. 確定していない変更は, 作業用の store と一緒に消える.

答えを返した変更は多数派に記録されているので, leader が替わっても失われない.

### 答えと変更の対応

store の連番だけでは, どの変更が確定したかは分からない (Issue #5).
leader が確定していない変更に連番 101, 102 を使った後に leader でなくなると, 次の leader は同じ連番を別の変更に使って確定させる.
状態機械の連番が 102 に達したことを待つだけでは, 元の変更が確定していないのに成功を返しうる.

- 提案する task (proposer) は term ごとにあり, 自分の提案した項目が確定して適用されたとき (`client_write` の成功) にだけ, その項目が運んだ最後の連番を報告する. 呼び出しはこの報告を待つ. 別の leader の項目が同じ連番を確定させても, 報告は進まない.
- leader でなくなったとき, または提案が失敗したときは, 待っている呼び出しに `ABORTED` を返す. その変更は確定したかどうか分からない. client は `ABORTED` を自動では送り直さない. 送り直すときは同じ `request_id` を付ける. 確定していれば最初のジョブが返り, 確定していなければ新しく作られる.
- 状態機械は, 項目の term と, その変更を書いた作業用の store の term が違う項目, または連番が最後に適用した変更に続かない項目を, 変更なしとして適用する. ログと状態だけで決めるので, すべての node が同じ項目を飛ばす. 後の term で再び leader になった node に古い term の proposer が残っていても, その変更は適用されない. 連番の続かない項目で状態機械の適用が失敗し, Raft が止まることもない. 飛ばした項目の proposer は, 提案の失敗として扱う.
- 何も書かない呼び出し (Issue #18): 既に使われた `request_id` の投入や, 終わったジョブの取り消しのように作業用の store を変えなかった呼び出しは, 他の呼び出しの変更の確定を待たない. 確定した状態のジョブが, 作業用の store から答える内容と同じなら, それを答える. 違えば (そのジョブ自身の変更がまだ確定していない) 確定を待つので, 確定していないものを答えることはない. 書いたかどうかは, scheduler の変更の処理が, 変更と同じ lock の中で決めて返す (Issue #92). 作業用の store の連番の前後の差では判断しない. 呼び出しの間に他の呼び出しが書いても, 何も書かない呼び出しは, そのジョブが答える内容のとおりに確定していれば, その確定を待たない. 確定していなければ, それまでに書かれた変更のすべて (他の呼び出しのものを含む) の確定を待つ.
- 同じ term のうちに leader の役を降りた node は, その term では再び leader の役を始めない. 1 つの term の作業用の store は 1 つだけである.
- term の欄のない項目 (この変更の前のログ) は, 連番が続く限り適用する.

### leader でない node

- client と agent の呼び出しに `UNAVAILABLE` を返す. client と agent は次の URL を試す (ADR 0012 と同じ).
- 多数派から切り離された leader: 多数派からの応答が election timeout の 2 倍の間なければ, Raft の上では leader のままでも, 呼び出しに `UNAVAILABLE` を返す. 書き込みは多数派に届かないので確定しない.
- epoch: Raft の term を epoch として返す. agent は小さい epoch の scheduler には戻らない (ADR 0012 と同じ).

### client と agent

- client: 呼び出しを, 最後に答えた scheduler, 次にほかの scheduler の順に送る. すべてが `UNAVAILABLE` を返したとき (選挙の間) は, 呼び出しを始めてから 5 秒まで試し直す. 1 回の試みは 30 秒までしか待たない (Issue #17). 読み出しの試みが答えなければ `UNAVAILABLE` として次の scheduler を試す. 投入と取り消しは効果があったか分からないので送り直さず, `DEADLINE_EXCEEDED` を返す.
- `wait` (Issue #17): 期限は待ち始めに 1 回だけ決め, `Status` と `Result` の呼び出しと間の休みのすべてに掛ける. 答えない呼び出しがあっても期限で `WaitTimeout` を返す. `UNAVAILABLE` の間は問い合わせを続ける. 終わったジョブの `Result` が `UNAVAILABLE` を返したときも, 同じ期限と間隔で問い合わせ直す (Issue #93). 期限を与えなければ, 選挙が長引いてもその間待ち続ける.
- 投入の冪等性: 試し直しでも, 確定した後に応答だけが失われた投入は区別できない. そのため投入には client が作る鍵 (`SubmitRequest.request_id`) を付ける. 鍵はジョブの行に記録して Raft で複製するので, 同じ鍵の投入は, leader が替わった後でも最初のジョブを返す. 同じ鍵で spec か優先度が違う投入は `ALREADY_EXISTS` で断る. client ライブラリは投入の呼び出しごとに鍵を作り, CLI (`submit --request-id`) と Python (`submit(request_id=...)`) は呼び出し側の鍵も受け付ける. 鍵はジョブの記録と同じだけ残る. 鍵のない投入 (古い client) は毎回新しいジョブになる.
- agent: heartbeat が `UNAVAILABLE` (または 5 秒の時間切れ) になると, 同じ heartbeat のうちに次の URL を試す. 登録し直すのは, scheduler がその agent を知らないと答えたとき (leader が替わったとき) だけである. 同じ leader が答え始めたときは, 登録と実行中のジョブをそのまま続ける.
- heartbeat の期限: 過半数から切り離されていた leader が答え始めるとき, 各 agent の heartbeat の期限を数え直す. その間 agent が黙っていたのは leader の側の事情だからである.
- 接続: すべての接続で HTTP/2 の ping を 5 秒ごとに送り, 5 秒答えがなければ切る. ネットワークから切り離された相手はパケットを黙って捨てるので, ping がないと, 確立した接続の上の呼び出しが TCP が諦めるまで返らない.

### 構成

- 構成員: 番号, 名前, URL を Raft の構成 (membership) に記録する. 最初の起動で, 各 node が設定の `[[raft.members]]` で Raft を初期化する. その後の構成は Raft のログが決め, 設定の一覧は使わない.
- 構成員を加える: 新しい scheduler を `join = true` で起動すると, 初期化せずに待つ. leader が learner として加え (`add_learner`), ログに追いつくのを待ってから投票者にする (`change_membership`). 追いつくのを 60 秒まで待ち, 追いつかなければ learner のまま失敗を返す.
- 構成員を除く: leader が `change_membership` で除く. leader 自身も除ける. 除かれた leader は, 新しい構成が確定した後に leader をやめ, 残りが leader を選ぶ.
- 構成の変更は joint consensus の 2 段で行う (openraft). 変更の途中で leader が替わると, joint の構成のまま残ることがある. そのときは変更をもう一度行う.
- 除かれた scheduler: 投票者でないことを知った scheduler は選挙を始めない. 最後の構成を受け取らずに選挙を始めても, leader が健在な間は投票を断られる (openraft の leader lease). 除いた後は止める.
- server 証明書: 加える scheduler の名前を含む証明書を, 既存の CA で発行する (`algebramesh certs server`).
- 台数: n 台のうち過半数が動いていれば, 投入と実行を続けられる. 1 台の故障に耐えるには 3 台, 2 台なら 5 台が要る. 2 台の構成では, どちらか 1 台が止まると止まる.
- 通信: Raft のメッセージは scheduler と同じポートの `Replication` サービスで, TLS と peer 用の token で送る. 構成員ごとの接続は, URL が変わらない限り使い回す (openraft は投票のたびに接続を求める).
- client と worker の URL: 構成員を変えても, client と worker の URL の一覧は自動では変わらない. 加えた scheduler の URL は運用者が書き足す.
- 時間: election timeout は `election_timeout_ms` から その 2 倍の間で選ぶ (既定 1,500 ms). heartbeat はその 1/3 とする (ADR 0017. openraft は `AppendEntries` をこの間隔で打ち切る).

## 結果

- 分断: 過半数の側だけが leader を選び, 書き込みを確定できる. 少数の側の leader は確定できず, 呼び出しに `UNAVAILABLE` を返す. primary が 2 つになることはない.
- 確定した変更: client と agent に答えた変更は, leader が替わっても残る.
- 読み出し (Issue #13): `Status` と `Result` は, leader の作業用の store ではなく状態機械 (過半数が確定し, この node が適用した状態) から答える. 作業用の store には確定していない変更が入っており, leader が替わると消えうるからである. 読む前に leader は `ensure_linearizable` で過半数に自分が leader であることを確かめ, それまでに確定した項目の適用を待つ (election timeout の 2 倍まで. 確かめられなければ `UNAVAILABLE`). 答えた変更は leader の状態機械に適用済みなので, 自分の投入や取り消しは直後の読み出しに見える. 読み出しは多数派への往復 1 回分遅くなる. `LookupRequest` と `ListJobs` (Issue #63) も, 同じく状態機械から答える. `LookupRequest` は, 状態機械を持つ間に記録の JSON を読んで写し, 解釈は状態機械を放してから行う. lock を持つ時間は記録の大きさに比例し, 8 MiB の記録で約 4 ms だった (メモリ上の database で測った. disk から読むときは, その時間が加わる). 記録の大きさに依らない時間にするには, 応答に要る欄 (試行の回数, 誤り) を記録の外の列に置く必要がある (#84). `ListJobs` は状態機械を持つ間に記録の外の列だけを写す. 1 回に調べるジョブには, 数 (10,000 件) と, 絞込みで落ちたものも含めた列の大きさ (4 MiB) の上限を置く. 上限は, 調べたジョブの大きさを数える前にそのジョブを読むので, 1 件分を超えうる. page は 1 MiB を超えない. 超えるのは, 1 件だけで 1 MiB を超えるジョブの page だけである (#76). `ListBackends` と `ListWorkers` は leader の記憶にある agent の情報で, store の確定とは関係しない.
- 割り当て済みのジョブ: leader が替わると `LOST` になる (ADR 0012 と同じ).
- 性能: 書き込みを伴う呼び出しは, 多数派への往復 1 回分遅くなる. 変更はまとめて提案する.
- 2 台の構成 (ADR 0012) で物理ホスト 2 台に分けた記録は ADR 0012 に残す.

## 確認

- 単体試験: ログの保存と状態機械 (openraft の `testing::Suite`), 作業用の store の変更と別の store への適用. 別の term の変更や連番の続かない変更を, 変更なしとして適用すること.
- 1 プロセス内の結合試験 (`crates/algebramesh-agent/tests/raft.rs`): 3 node の選挙, leader を止めたときの選び直し, 止めた node の復帰と追従, client と agent の切り替え, 過半数を失った leader が呼び出しを断ること, 4 台めを加えて leader を除くこと. leader が投入を作業用の store に書いたまま提案を止め, 別の node が次の term で同じ連番に別の変更を確定させても, 元の投入が成功にならず (`ABORTED`), 同じ `request_id` で送り直すと 1 つのジョブになること (Issue #5). この試験は, 状態機械の連番を待つ以前の実装では成功を返して失敗する. leader が取り消しを作業用の store に書いたまま提案を止めても, 同じ leader の `Status` と `Result` は確定した `QUEUED` を返し, leader を止めた後の新しい leader でも `QUEUED` のままであること (Issue #13. 作業用の store から読む以前の実装では `CANCELLED` を返して失敗する).
- 遅いホストでの結合試験 (Issue #41): 試験の cluster の election timeout は 300 ms (heartbeat は 100 ms) で, 1 プロセスに 3 から 4 台の scheduler と TLS, `synchronous = FULL` の SQLite を置く. Windows の Docker のように fsync が遅いホストでは heartbeat が遅れ, 試験が起こしていない選挙が起きる. そのとき, 1 つの leader を前提にした試験は `ABORTED` などで落ちる.
  - `ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS` で試験の election timeout を延ばせる. 状態を待つ上限 (既定 15 秒) もそれに比例して延びる.
  - 1 つの leader を前提にした区間では term を見張り, その区間で試験が落ちて term が変わっていれば, 試験が起こしていない選挙が前提を崩したことと, 変数の設定を示す.
  - 同じ鍵の同時投入の試験は, 結果の分からない呼び出し (`ABORTED`, `UNAVAILABLE`) を, client に求めているとおり同じ鍵で送り直す.
- Docker の結合試験 (`tests/integration/run-raft-e2e.sh`, CI の `raft-e2e`): 3 台の scheduler で, ネットワークの分断 (`docker network disconnect`) と leader の強制停止.

Docker の結合試験の結果 (2026-11-04, 3 回とも同じ結果. 分断の直後の投入は後の 2 回で試した):

| 段階 | 結果 |
|---|---|
| 開始 | leader が選ばれ, ジョブが成功した |
| leader を分断 | 残りの 2 台が次の term で leader を選び, ジョブが成功した. 最初のジョブも残った |
| 分断の直後に, 切り離された leader に投入 | 作業用の store には入ったが確定せず, 10 秒後に `UNAVAILABLE` (the change was not committed) で断られた |
| しばらく後に投入 | すぐに `UNAVAILABLE` (has not heard from a majority) で断られた |
| 分断を解く | 切り離されていた scheduler は follower になり, 3 台の store が一致した. 確定しなかった投入は残らなかった |
| leader を強制停止 | 残りの 2 台が leader を選び, ジョブが成功した. 止めた scheduler は戻って追いついた |
| 4 台めを加え, leader を除く | `certs server` の証明書で起動した scheduler-d を `server add-member` で加え, 投票者として追いついた. その後 leader を `server remove-member` で除くと, scheduler-d を含む 3 台が leader を選び, ジョブが成功した (この段は後から加え, 2 回試した) |

試験の途中で, 次の 2 つを直した.

- 分断された leader との確立済みの接続の上で, agent の heartbeat が返らず, agent が新しい leader に移れなかった. HTTP/2 の ping と heartbeat の時間切れを加えた.
- 過半数が戻った leader で, agent が古い登録のまま割り当てを受け, 新しい登録で heartbeat を送ったため, ジョブが `LOST` になった. agent が登録し直すのを, scheduler がその agent を知らないときに限った.
