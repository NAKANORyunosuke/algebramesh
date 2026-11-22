# 信頼性の監査: 不変条件と障害の試験 (2026-11-05)

- 追跡: Issue #25
- 対象: commit `728c965` 以降, Issue #17〜#24 の修正を含む作業ツリー
- この記録は「バグがない」ことの証明ではない. 不変条件ごとに, 何で確かめたか, 何を確かめていないかを分ける.

## 確かめ方の区分

| 区分 | 意味 |
|---|---|
| 試験 | 自動試験があり, CI か `cargo test` で毎回流れる |
| 手で流す試験 | `--ignored` の試験で, 手で流して結果を記録した |
| Docker | `tests/integration/*.sh` (CI の e2e ジョブ) |
| 設計 | コードと文書 (ADR) による議論だけで, 障害注入はしていない |
| 未確認 | 確かめていない. 区別して残す |

## 1. ジョブの一生と結果の配送

| 不変条件 | 確かめ方 | 根拠 |
|---|---|---|
| 受理したジョブは, 再試行の連鎖を含めて終わりの状態に収束するか, `QUEUED` で待つ | 試験 | `server/tests/reconcile.rs` (heartbeat に載らない active なジョブは `LOST` になり再投入される, 他の agent のジョブは変わらない), `agent/tests/cluster.rs` (agent の停止で `LOST` と再試行) |
| `ReportFinished` の応答が失われても, 永続的な `RUNNING` が残らない | 試験 | agent の `deliver` の単体試験 (`UNAVAILABLE` の繰り返し, 答えない呼び出し, 期限で諦める), scheduler の照合 (`reconcile.rs`) |
| 重複した報告, 古い報告は状態を変えない | 試験 | `reconcile.rs` (2 度目の報告は `already SUCCEEDED` で断られる, 遅れた報告は 1 件の小さなイベント) |
| agent の再起動 | Docker | `run-fault-e2e.sh` |
| `request_id` の冪等性: leader 交代, scheduler の再起動, 同時投入, worker の入替えをまたぐ | 試験 | `raft.rs` (leader 交代後の再送, 未確定の投入の後の同じ鍵), `restart.rs`, `limits.rs` (worker の入替え) |
| 同じ鍵の同時投入 | 試験 (#38 で追加) | `server/tests/concurrent_keys.rs` (16 thread が同じ鍵を同時に送る. 一部は別の優先度), `raft.rs` (12 本の接続から 3 台の Raft の leader へ同時に送り, 確定した状態に 1 件). scheduler の lock を投入の途中で外すと一意索引の誤りで落ちることを確かめた |
| 取り消しと完了のすべての順序 | 試験 (#38 で追加) | `server/tests/model.rs`: 投入 (鍵の有無), 取消し, 割当て, 開始と結果の報告 (他の agent, 終わったジョブへの報告を含む), heartbeat, 再起動の操作列を proptest で作り, 実装と小さなモデルの答えと全ジョブの状態を操作ごとに比べる. 取消しを要求したジョブは `CANCELLED` か `LOST` で再投入されない, attempt は `max_attempts` まで, 残りを流すと全ジョブが終わりの状態になる, を確かめる. 取消しと 1 回の実行の順序はすべて列挙する. #26 の 2 つの修正をそれぞれ外すと落ちる. agent の沈黙による `LOST` は再起動で代表させ, 時間には依らない |

## 2. 合意と保存

| 不変条件 | 確かめ方 | 根拠 |
|---|---|---|
| client に成功を返した変更は確定しており, 新しい leader でも消えない | 試験, Docker | `raft.rs` (leader 交代, 別の leader が同じ連番を使っても成功にならない), `run-raft-e2e.sh` (分断と強制停止) |
| 読み出しで未確定の状態が見えない | 試験 | `raft.rs` (提案を止めた取消しが `Status` と `Result` に見えない) |
| SQLite の変更, Raft の適用, artifact の書込みの失敗が, 部分適用や停止を起こさない | 試験 (artifact) / Docker (SQLite, #37 で追加) | artifact: store の単体試験と `raft.rs` (follower の artifact を書けなくしても追従). SQLite の書込み失敗: follower, leader, snapshot の受け手のディスクの空き不足 ([障害注入の記録](2026-11-06-fault-injection.md)) |
| snapshot の作成, 転送, install, ログの破棄, 再起動 | 試験, 手で流す試験 | `raft.rs` (ログを捨てた後の構成員の追加), 手で: 336 MB のデータベースの snapshot 転送 |
| 転送中の通信断, hash の不一致, 中断 | 試験 | `snapshot.rs` の単体試験 (古い転送の chunk の遅延を含む), `raft_faults.rs` (TCP の proxy で接続を切り, 遅らせ, 止める. #37 で追加) |
| 電源断の後の復旧 | 設計 / 試験 (crash) | `synchronous = FULL`, restore の手順 (ADR 0017). snapshot の作成, ログの破棄, install の各段階の直後の crash は試験 (#37). 仮想マシンの電源を切る試験は未実施 |
| メタデータが 128 MiB を超えても追いつける | 手で流す試験 | 336 MB, ADR 0017 |
| ログの compaction の後の learner の追加と復帰 | 試験 | `raft.rs` (`compact_log` の後に 4 台目を追加) |

## 3. 資源と安全の上限

| 対象 | 上限 | 確かめ方 |
|---|---|---|
| agent の RPC | 1 回 30 秒, poll は待ち時間 + 30 秒 | 試験 (agent の単体試験) |
| client の RPC | 1 回 30 秒, `wait` は与えた期限 | 試験 (`client/tests/wait.rs`) |
| 結果の報告 | 全体 2 分 | 試験 |
| ジョブの期限 | 検算まで含む `wall_seconds` | 試験 (core の runner) |
| CAS の出力 (Macaulay2) | 256 MiB, 超えたらプロセスグループを止める | 試験 (Linux) |
| CAS の出力 (OSCAR) | 1 要求 256 MiB, 1 行も上限まで, ログ 64 KiB | 試験 (Linux の偽の driver) |
| OpenXM の受信 | 256 MiB, 入れ子の深さ, list の長さ | 試験 (property test, 規模の計測) |
| 結果, 仕様 | 8 MiB | 試験 |
| イベントの本文 | 4 KiB | 試験 (遅れた報告) |
| Raft の項目, `AppendEntries` | 4 MiB (1 項目は単独で) | 手で流す試験 (80 MiB の追従) |
| snapshot | 16 MiB の chunk, scheduler 間のメッセージ 1 GiB | 手で流す試験 |
| 検算の thread | CPU の数 | 試験 (core) |
| GKZ の facet の列挙 | 部分集合 200,000, 作業量 `200,000 * 4^4` | 試験 |
| 待ち行列の長さ | 既定 10,000 件 (`max_queued_jobs`, #40 で追加) | 試験 |
| 敵対的な入力 (未知の tag, 深い CMO, 巨大な JSON, 整数の桁あふれ, 未対応の operation) で panic しない | 試験 (一部) | OpenXM の property test, 多項式と Weyl 代数の桁あふれ, GKZ の級数の境界. JSON の仕様全体を対象にした fuzz は未実施 |
| agent の身元, token, TLS | 試験 | `cluster.rs` (token と TLS の必須, client 証明書, 失効). ネットワークの境界の再確認は未実施 |

## 4. 数学的な正しさ

| 不変条件 | 確かめ方 | 根拠 |
|---|---|---|
| 多項式の積と冪の指数が正確か, 桁あふれは誤り | 試験 | `math-types/tests/exponent_overflow.rs` (property test) |
| 多項式環の公理 (結合法則, 分配法則) | 試験 (#39 で追加) | `math-types/tests/ring_axioms.rs` (property test: 加法の群, 積の結合法則, 可換性, 単位元, 分配法則, 冪. 整数係数と有理係数の積の経路が一致). 積の整数の経路を壊すと落ちる |
| Weyl 代数の交換関係 | 試験 | `weyl.rs` の単体試験 (`[d_i, x_j] = delta_ij`, `d^2 x^2`) |
| Weyl 代数の結合法則 | 試験 (#39 で追加) | `ring_axioms.rs` (結合法則, 両側の分配法則, 単位元, 交換関係. 積を, 試験の中で項ごとに微分して求めた多項式への作用 `(PQ) f = P (Q f)` と照合する). 積の公式の係数を壊すと落ちる |
| GKZ: Euler 作用素と toric 生成元 | 試験 | `gkz` の単体試験, 回帰試験 (`tests/regression`) |
| indicial の根, 重複度の和と rank | 試験 | ADR 0015 の試験 |
| 共鳴, rank jump, 非斉次の例の分離 | 試験 (#39 で追加) | 回帰試験の局所データの例は generic, resonant, rank_jump, inhomogeneous に分かれ, それぞれ rank を Singular で計算した参照 (`tests/regression/references`) と照合し, `run.py` が A から計算する vol(A) と比べる (rank jump の例では rank が vol(A) を超える). rank jump の例は `A = [[1,1,1,1],[0,1,3,4]]`, `beta = (1,2)` の rank 5 (Sturmfels と Takayama) |
| 級数の係数と対数解 | 試験 | 単体試験と回帰試験 (twisted cubic の重複度 3) |
| `verified_to_order` を収束や完全性の証明と扱わない | 設計 | ADR 0007, 0015 |
| 組合せの列挙の上限が正確 | 試験 | `cone.rs` (n ≤ 70 の全 k で Pascal の三角形と一致) |

## この監査で見つけ, 直したもの

監査の試験を書く中で, 次の欠陥を見つけた. どれもこの作業ツリーで直した.

- 1 回の `AppendEntries` が heartbeat の間隔で打ち切られ, 大きな要求が永遠に再送される (#20 の重い試験で発見). 上限を 4 MiB にし, heartbeat を election timeout の 1/3 にした.
- artifact のディレクトリを作れないと, データベースを開けず scheduler が起動しない (#21 の試験で発見).
- snapshot を 64 KiB の chunk で送ると, 336 MB では往復が多すぎて遅い (既定は 16 MiB にした. 試験だけが小さい chunk を使う).

## 監査で見つかった欠陥と修正 (Issue #26〜#35)

監査の試験 (本体を変えずに production のコードを呼ぶ試験) で, 次の欠陥が再現された. どれも修正し, 監査の試験を回帰試験として残した.

| Issue | 不変条件 | 修正と試験 |
|---|---|---|
| #26 | 取り消したジョブから新しい実行が生じない | `finished` と `declare_lost` が取消しを尊重する. `server/tests/invariants.rs` (開始報告の喪失, 再起動, worker の喪失) |
| #27 | 再試行の履歴が分岐しない | `LOST`, 再試行, 参照を 1 つの変更にした. 変更列の全 prefix で試験 |
| #28 | 壊れた snapshot で既存のデータベースを失わない | 置き換える前の検証. 非 SQLite, 途中で切れたもの, 空のデータベースで試験 |
| #29 | すべての接続の待ちに上限がある | TLS の握手の終わりまで期限を保つ. 0 バイト, 1 バイト, 部分的な header と body で試験 |
| #30 | CAS のプロセスは子孫まで回収する | process group ごと止める. 子と孫を作る偽の launcher で試験 (Linux) |
| #31 | 敵対的な入力で worker が止まらない | 期限の値域と checked な加算, panic の回収. panic する backend で試験 |
| #32 | 入力の変換がプロセスを abort しない | 現れない変数を再帰しない, 現れる変数の上限. 5 万変数で debug と release |
| #33 | schema の移行は原子的 | 列ごとの確認と 1 トランザクション. 4 つの schema で試験 |
| #34 | 入力を黙って変えない | Python の整数の検証. 送る JSON の試験 |
| #35 | 正しい入力の round-trip | 変数のない環の定数. Macaulay2 の実際の応答で試験 |

## 残っている課題 (個別の Issue にする候補)

- 8 MiB を超える結果と入力を, Raft のログの外の store (内容の hash で参照) に置く.
- 電源断, ディスクの空き不足, 転送中の通信断の障害注入 (仮想マシンか, 失敗を注入できるファイルシステム). #37 で crash, ディスクの空き不足, 通信断を注入した. 電源断そのものは残る.
- 同じ鍵の同時投入と, 取消しと完了の全順序のモデルによる試験 (#38 で対応した).
- 多項式環と Weyl 代数の公理の property test, GKZ の rank jump の例を独立の参照 (別の CAS) と照合する試験 (#39 で対応した).
- 待ち行列の長さの上限 (#40 で対応した).
- Windows の Docker で不安定な Raft の結合試験 (構成員の追加と leader の除去). CI では通っている (#41 で対応した. 原因は試験が起こしていない選挙で, election timeout を延ばせるようにした).

## CI が通ることと監査が済むことの違い

CI が通ることは, 上の表の「試験」と「Docker」の行が通ることだけを意味する. 「手で流す試験」はこの記録の日に一度流したもので, CI では流れない. 「設計」と「未確認」の行は, CI が通っても確かめられていない.
