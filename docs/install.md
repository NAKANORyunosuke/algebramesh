# インストールと運用手順

この文書は AlgebraMesh を構築し, scheduler と worker を配置する手順をまとめる.
構成要素と責務は [design.md](design.md) の §4 を, 判断の根拠は [adr/](adr/) を参照する.

## 1. 構成

| 構成要素 | 実体 | 置き場所 |
|---|---|---|
| scheduler | `algebramesh server start` | 1台. SQLite (WAL) と artifact をローカルディスクに置く |
| worker agent | `algebramesh worker start` | CAS を持つ各ホスト. scheduler へ外向きに接続する |
| client | `algebramesh` CLI, Python パッケージ `algebramesh` | 任意 |

通信はすべて scheduler の 1 ポート (既定 50051) への gRPC over TLS である.
client と agent は別々の bearer token で認証する.
agent には, agent ごとの client 証明書 (mutual TLS) も要求できる (§3.1).
OpenXM の TCP ポートは worker ホストの中 (127.0.0.1) だけで使い, ネットワークに出さない.

## 2. ビルド

Rust 1.89 以降が必要である (依存する rusqlite が求める).
開発と CI は 1.95.0 に固定している (`rust-toolchain.toml`, image は `rust:1.95.0-bookworm`).

```bash
cargo build --release -p algebramesh-cli
```

生成物は `target/release/algebramesh` の1ファイルである.
コンテナで使う場合は `deploy/docker/algebramesh.Dockerfile` の target を選んでビルドする.

```bash
docker build -f deploy/docker/algebramesh.Dockerfile --target cli -t algebramesh-cli .
docker build -f deploy/docker/algebramesh.Dockerfile --target agent-asir -t algebramesh-agent-asir .
docker build -f deploy/docker/algebramesh.Dockerfile --target agent-macaulay2 -t algebramesh-agent-macaulay2 .
docker build -f deploy/docker/algebramesh.Dockerfile --target agent-oscar -t algebramesh-agent-oscar .
```

| target | CAS | 備考 |
|---|---|---|
| `agent-asir` | Risa/Asir 20260331 | `Risa-Asir-container` の公開イメージを digest 指定で使う ([ADR 0002](adr/0002-asir-runtime-image.md)). Risa/Asir は非営利・非営業目的に限る ([licenses.md](licenses.md)) |
| `agent-macaulay2` | Macaulay2 1.24.11, 4ti2 1.6.10 | Debian trixie のパッケージ |
| `agent-oscar` | OSCAR 1.8.2, Julia 1.11 | ビルド時に OSCAR を precompile する. イメージは約 8 GB |

イメージを配布するときの条件は [licenses.md](licenses.md) にまとめた.
Macaulay2 と OSCAR のイメージは GPL のソフトウェアを含むので, ソースの入手先を示す必要がある.

BuildKit で大きなイメージを並行して export すると snapshot エラーになることがあったので, target は1つずつビルドする.

## 3. 証明書と token

閉域網での運用向けに, 私設 CA, scheduler の証明書, token を生成するコマンドがある.

```bash
algebramesh certs generate --out /etc/algebramesh --host scheduler.internal --host 10.0.0.5
```

| ファイル | 置き場所 | 権限 |
|---|---|---|
| `ca.pem` | scheduler, 全 worker, 全 client | 公開してよい |
| `ca-key.pem` | どのホストにも置かない (証明書を発行するときだけ使う) | 発行する人だけが読める |
| `server.pem`, `server-key.pem` | scheduler のみ | `server-key.pem` は scheduler の実行ユーザーだけが読める |
| `agent.token` | scheduler と全 worker | 実行ユーザーだけが読める |
| `client.token` | scheduler と client | 利用者ごとに配布する |

`--host` には client と worker が接続に使う名前をすべて書く.
既存の CA を使う場合は, `server.pem` と `server-key.pem` をその CA の証明書に置き換える.
token ファイルは1行1 token で, 複数行を書くと複数の token を受け付ける.
token の失効は該当行を消して scheduler を再起動する.
Unix では, `certs generate` と `certs agent` が書く鍵と token の権限は 0600 になる.

### 3.1 agent ごとの証明書 (mutual TLS)

共通の `agent.token` だけでは, token を持つ者はどの agent にもなりすませる.
agent ごとに client 証明書を発行すると, scheduler は次を確かめる.

- agent は, `ca.pem` の CA が署名した client 証明書を提示しなければならない. 提示しない接続は `UNAUTHENTICATED` で拒否する.
- agent は証明書の common name と同じ名前でしか登録できない.
- 登録後の heartbeat, ジョブの受け取り, 開始と終了の報告は, 登録したときと同じ証明書でしか行えない. 他の agent の id を使った呼び出しは `PERMISSION_DENIED` になる.
- `revoked_agent_serials` に載せた証明書は拒否する.

client (CLI, Python) は証明書を使わず, token だけで認証する.

```bash
algebramesh certs agent --ca-dir /secure/pki --name worker-a --out /secure/pki/agents --days 365
```

`worker-a.pem` と `worker-a-key.pem` を書き, 証明書の serial number を標準出力に出す.
2 つのファイルを worker-a のホストだけに置く.
証明書の失効は, serial number を `server.toml` の `revoked_agent_serials` に足して scheduler を再起動する.
同じ名前で新しい証明書を発行すれば, その agent は新しい証明書で再び参加できる.

## 4. scheduler

`server.toml` の例:

```toml
listen = "0.0.0.0:50051"
data_dir = "/var/lib/algebramesh"
tls_cert = "/etc/algebramesh/server.pem"
tls_key = "/etc/algebramesh/server-key.pem"
client_token_file = "/etc/algebramesh/client.token"
agent_token_file = "/etc/algebramesh/agent.token"
agent_ca_file = "/etc/algebramesh/ca.pem"     # 省略すると agent の証明書を求めない (§3.1)
revoked_agent_serials = []                     # 失効させた agent 証明書の serial number
heartbeat_interval_ms = 2000
heartbeat_timeout_ms = 15000
max_attempts = 3
max_queued_jobs = 10000                       # 待機中のジョブの上限
# blob store (ADR 0021). 値は既定値.
# blob_dir = "/var/lib/algebramesh/blobs"     # 既定は <data_dir>/blobs
# max_blob_bytes = 268435456                  # 1 つの blob の上限 (256 MiB)
# max_blob_store_bytes = 0                    # 全 blob の合計の上限. 0 は上限なし
# blob_min_free_bytes = 1073741824            # blob store が残す空き (1 GiB)
# blob_orphan_grace_secs = 86400              # 参照されない blob を残す時間
# blob_replication_bytes_per_second = 0       # 他の scheduler への送信の帯域. 0 は抑えない
# blob_upload_bytes_per_token_per_hour = 0    # client token ごとの 1 時間の upload の量. 0 は上限なし
```

```bash
algebramesh server start --config /etc/algebramesh/server.toml
```

- `data_dir/metadata.sqlite3` にジョブ・イベント・agent を記録する. ネットワークファイルシステムに置かない.
- `data_dir/artifacts/<job-id>/record.json` に終了したジョブの記録を書く.
- `data_dir/blobs/` (`blob_dir`) に, 8 MiB を超えうる大きな値の本体を, SHA-256 を名前として置く ([ADR 0021](adr/0021-content-addressed-blob-store.md)). cluster では, 投票者の過半数が本体を disk に持ってから記録が参照する. 本体を読むたびに SHA-256 を確かめ, 合わないものは `blobs/quarantine/` に移して他の scheduler から取り直す. どの記録も参照せず `blob_orphan_grace_secs` の間使われなかった本体は消す. backup の手順は §4.1 にある.
- `blob_upload_bytes_per_token_per_hour` を 0 より大きくすると, client token ごとに, `PutBlob` で送れる量をその値に抑える. 量は 1 時間で均等に戻る. 上限を超える upload は, 本体を書く前に `RESOURCE_EXHAUSTED` で断り, いつ送れるようになるかを文に含める. 失敗した upload の分は戻し, その scheduler が既に持つ blob の upload は数えない. 量は scheduler ごとにメモリに持つので, 再起動で戻り, leader が替わると新しい leader の量で数える. agent token の upload には掛けない (agent の upload は driver の出力の上限で抑えられる).
- `heartbeat_timeout_ms` の間 heartbeat の無い agent はオフラインとし, そのジョブを `LOST` にする.
- 再起動すると, 実行中だったジョブは `LOST` になり, 待機中のジョブはそのまま残る.
- `retry_policy = "if_deterministic"` のジョブだけが, `LOST` 後に別のジョブとして再投入される (最大 `max_attempts` 回).
- 待機中 (`QUEUED`) のジョブが `max_queued_jobs` 件あるときの新しい投入は `RESOURCE_EXHAUSTED` になる. 同じ `request_id` の送り直しと `LOST` の再投入は断らない.
- その backend の online な worker が 1 台以上いて, どれもジョブの operation を申告していなければ, 投入はジョブを作らずに `FAILED_PRECONDITION` になる. operation を申告する worker のどれも `limits.memory_mb` を強制しないときも同じである. その backend の worker が 1 台もいなければ, ジョブは待たされる. 同じ `request_id` の送り直しは, online な worker によらず元のジョブを返す ([ADR 0022](adr/0022-operation-types-and-signatures.md)).

### 4.1 backup と復元

scheduler の状態は, `data_dir` の次のものである.

- `metadata.sqlite3`: ジョブ, イベント, agent. cluster では `raft.sqlite3` (Raft のログと投票) と `snapshots/` も.
- `artifacts/`: 終了したジョブの `record.json`.
- `blobs/sha256/` (`blob_dir`): blob の本体. 名前が SHA-256 で, 書き終えた本体は変わらない. `blobs/incoming/` は転送の途中の file で, backup には要らない.

記録は blob を名前で参照するので, metadata を先に取り, blob を後に取る. 逆の順では, 間に終わったジョブの blob が backup に欠けうる.

1 台の scheduler:

1. scheduler を止める.
2. `data_dir` を丸ごと写す (`blob_dir` を別に置いたときは, それも).
3. scheduler を起動する.

cluster (止めずに取る):

1. follower を 1 台選ぶ. `server status` で, その scheduler の `blobs_missing` が 0 であることを確かめておく.
2. その scheduler を止める. 残りの過半数で投入と実行は続く.
3. その `data_dir` から, `blobs/` を除いたもの (`metadata.sqlite3`, `raft.sqlite3`, `snapshots/`, `artifacts/`) を写す.
4. その scheduler を起動する. 止めていた間の変更には, leader から追いつく.
5. 動いている scheduler の `blobs/sha256/` を写す (例: `rsync -a sched-a:/var/lib/algebramesh/blobs/sha256/ backup/blobs/sha256/`). 本体は rename で置かれるので, 動いている間に写しても, 書きかけの file は入らない. 写す元は, 3 で写した記録が参照する blob を持つ scheduler, つまり `blobs_missing` が 0 のものにする.

復元:

- 1 台の scheduler: 写したものを `data_dir` に戻して起動する.
- cluster の 1 台を失ったとき: backup からは戻さない. その scheduler を `server remove-member` で除き, 空の `data_dir` で `server add-member` し直す (§9.1). 記録と blob は他の構成員から届く.
- cluster のすべてを失ったとき: 同じ backup を全構成員の `data_dir` に戻し, 全員を起動する. backup を取った時点の後に確定した変更は失われる. 起動の後, 各 scheduler の `server status` で `blobs_missing` が 0 になることを確かめる. 0 にならなければ, 参照された blob の本体が backup に欠けている. その blob を参照するジョブの結果は `DATA_LOSS` になり, 引数に持つジョブは backend を呼ばずに失敗する.

これらの手順は, `crates/algebramesh-agent/tests/blobs.rs` の `a_cluster_restored_from_a_backup_keeps_its_jobs_and_blobs` で, 3 台の cluster について確かめている. 1 台の follower の記録と leader の本体から取った backup を全構成員に戻すと, ジョブと blob が残り, 新しいジョブも実行される.

## 5. worker

`worker.toml` の例 (1台に複数の backend を置いてよい):

```toml
server = "https://scheduler.internal:50051"
ca_file = "/etc/algebramesh/ca.pem"
token_file = "/etc/algebramesh/agent.token"
tls_cert = "/etc/algebramesh/worker-a.pem"      # §3.1. 省略すると証明書を提示しない
tls_key = "/etc/algebramesh/worker-a-key.pem"
name = "worker-a"                               # 省略すると証明書の名前, それも無ければホスト名

[[backends]]
kind = "asir"
slots = 4
ox_launch = "/OpenXM/lib/asir/ox_launch"
ox_asir = "/OpenXM/bin/ox_asir"
build = "ghcr.io/nakanoryunosuke/risa-asir-container@sha256:a572b603f710167f708e976900ba883b44c03ecf51378e87a796b7b127bf0aed"

[[backends]]
kind = "macaulay2"
slots = 2
driver = "/opt/algebramesh/drivers/macaulay2/algebramesh.m2"
build = "debian:trixie-slim macaulay2 1.24.11+ds-5+b1"

[[backends]]
kind = "oscar"
slots = 1
driver = "/opt/algebramesh/drivers/oscar/algebramesh.jl"
project = "/opt/algebramesh-oscar"
build = "OSCAR 1.8.2"
```

```bash
algebramesh worker start --config /etc/algebramesh/worker.toml
```

- `--tls-cert` と `--tls-key` を付けると, 設定ファイルの `tls_cert` と `tls_key` を置き換える. 同じ設定ファイルを複数の worker で共有するときに使う.
- 起動時に各 backend を準備し (OSCAR はこの時点で読み込む), 準備できた backend だけを CAS の版とともに登録する.
- `slots` は同時に実行するジョブ数である. Asir では1 slot が1 OpenXM セッションに当たる.
- `build` はイメージの digest やパッケージ版を書く. ジョブ記録に残り, ジョブ仕様の `environment.builds` で実行先を絞り込める.
- worker は低権限のユーザーで動かす. CPU, メモリ, ネットワークの制限はコンテナや systemd で掛ける ([ADR 0003](adr/0003-result-check-and-deadlines.md)).

## 6. client

```bash
export ALGEBRAMESH_SERVER=https://scheduler.internal:50051
export ALGEBRAMESH_CA=/etc/algebramesh/ca.pem
export ALGEBRAMESH_TOKEN_FILE=~/.config/algebramesh/client.token

algebramesh backends
algebramesh workers
algebramesh submit examples/factor.json --wait
algebramesh submit examples/gkz_design_9_2.json      # job id を表示
algebramesh status <job-id>
algebramesh wait <job-id> --timeout 600
algebramesh result <job-id> --format json
algebramesh result <job-id> --format claims   # 結果の主張と根拠の水準 (ADR 0018)
algebramesh verify <job-id>                   # 記録と blob の hash を確かめ, 手元の検査器で検査し直す (ADR 0031)
algebramesh cancel <job-id>
algebramesh blob put data.bin                 # blob として送り, marker {"$blob": ..., "size": n} を表示
algebramesh blob put --json rows.json         # 値の canonical JSON を送る. marker を spec の arguments に使える
algebramesh blob stat sha256:<hex>            # present, durable (過半数が持つ), size
algebramesh blob get sha256:<hex> -o out.bin  # SHA-256 を確かめて書く. -o を省くと標準出力
```

`blob put` で送った blob は, どの記録も参照しなければ `blob_orphan_grace_secs` (既定 24 時間) の後に消える.

結果ごとに, 何を確かめたかの記録 (`guarantee`) が付く. 主張ごとに, AlgebraMesh が確かめた (`checked`) か, backend の答えをそのまま記録した (`asserted`) かを示し, 全体を `verified`, `partially_verified`, `unverified` に要約する. cluster の全 scheduler がこの記録を扱える版になるまでは付かない ([ADR 0020](adr/0020-cluster-feature-agreement.md)).

Python からは次のように使う ([bindings/python/README.md](../bindings/python/README.md)).

```bash
pip install maturin
maturin develop --release -m bindings/python/Cargo.toml
```

```python
from algebramesh import Client
client = Client("https://scheduler.internal:50051", ca="ca.pem", token_file="client.token")
system = client.gkz(A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4])
```

## 7. 1台で試す

Docker があれば, scheduler と4つの worker (Asir 2台, Macaulay2, OSCAR) を1台に立てられる.

```bash
docker compose -f deploy/compose/compose.cluster.yml up -d
docker compose -f deploy/compose/compose.cluster.yml run --rm client workers
docker compose -f deploy/compose/compose.cluster.yml run --rm client submit /repo/examples/factor.json --wait
docker compose -f deploy/compose/compose.cluster.yml down -v
```

この構成では鍵と token を同じ volume に置いている. 試用と試験のためのもので, 本番では §3 の権限で配布する.
`certs` サービスは worker ごとの client 証明書も発行し, scheduler は agent に証明書を求める (§3.1).

## 8. 複数ホストへの配置

1. scheduler ホストで §3 の証明書と token を作る. `--host` には worker から見た scheduler の名前を入れる.
2. 各 worker の証明書を `certs agent --name <worker 名>` で発行する. `ca.pem`, `agent.token`, その worker の証明書と鍵を, そのホストへ安全な経路でコピーする. `ca-key.pem` はどのホストにもコピーしない.
3. scheduler の 50051 番ポートを worker と client のネットワークにだけ開ける. 公衆インターネットには開けない.
4. 各 worker で §5 の設定を書き, `tls_cert` と `tls_key` をその worker の証明書にして起動する. agent の名前は証明書の名前になる.
5. `algebramesh workers` で全 worker が `online` になり, `backend_version` が入っていることを確かめる.

Tailscale 等の VPN を使う場合も, TLS と token による認証は省略しない (design §11).

### 8.1 Docker で組む例

`deploy/compose/compose.scheduler-host.yml` は scheduler のホスト用で, scheduler, Asir worker 1 台, 証明書の生成を含む.
`deploy/compose/compose.worker-host.yml` は worker のホスト用で, Asir と Macaulay2 の worker を起動する.

1. scheduler のホストで起動する. `REMOTE_AGENTS` に他のホストの worker の名前を並べると, その証明書も作る. `SCHEDULER_BIND` が scheduler のポートを公開するアドレスである.

```bash
REMOTE_AGENTS="worker-c worker-c-m2" SCHEDULER_BIND=127.0.0.1:50051   docker compose -f deploy/compose/compose.scheduler-host.yml up -d scheduler worker-b
```

2. pki volume から `ca.pem`, `agent.token`, `client.token` と `agents/worker-c*.pem` を worker のホストへ移す (`docker cp`). `ca-key.pem` は移さない.
3. worker のホストから scheduler のポートへ届くようにする. VPN のアドレスで公開する代わりに, SSH のポート転送で worker のホストの loopback に持ってくることもできる.

```bash
ssh -N -L 127.0.0.1:50051:127.0.0.1:50051 <scheduler host>
```

4. worker のホストで起動する. worker はコンテナから `host.docker.internal:50051` (転送したポート) に接続する.

```bash
PKI_DIR=/path/to/pki WORKER_PREFIX=worker-c docker compose -f deploy/compose/compose.worker-host.yml up -d
```

image を build するときは, その前に `. tests/integration/build-env.sh` を実行しておくと, agent が checkout の commit を自分の build として結果に記録する ([ADR 0020](adr/0020-cluster-feature-agreement.md)). 実行しなければ build は記録されない.

### 8.2 2 台で確かめたこと (2026-11-04)

| ホスト | OS と Docker | 置いたもの |
|---|---|---|
| A (家の PC) | Windows 11, WSL2 Ubuntu 24.04, Docker 29.3.0, 24 CPU | scheduler, Asir worker `worker-b` |
| B | Windows 11, Docker Desktop | Asir worker `worker-c`, Macaulay2 worker `worker-c-m2` |

2 台は Tailscale の上の SSH ポート転送でつないだ.
- 転送: B の `127.0.0.1:50051` から, A の WSL のアドレスへ転送した.
- WSL の NAT: A の WSL は NAT モードで, Windows の loopback から WSL へのポートの転送が働かなかった. そのため, scheduler を WSL の内部アドレス (`SCHEDULER_BIND=<WSL の eth0 のアドレス>:50051`) で公開し, そのアドレスを転送の先にした.
- 停止: WSL は最後のプロセスが終わると止まり, コンテナも止まる. 試験中は WSL で `sleep` を動かし続けた.

結果は次のとおりである.

- 登録: 3 つの worker が, それぞれの client 証明書 (mutual TLS) で登録した.
- 振り分け: Asir の因数分解 8 件が 2 台の worker に振り分けられ (A で 2 件, B で 6 件), すべて積の検算に通った.
- Macaulay2: B の Macaulay2 で `examples/gkz_series.json` が検算まで通った.
- 障害: A の `worker-b` で動いている `if_deterministic` のジョブの途中で `worker-b` を強制停止した. ジョブは heartbeat の時間切れで `LOST` になり, 再試行が B の `worker-c` で成功した (停止から完了まで 18 秒).

## 9. scheduler を複数台にする (Raft)

scheduler を 3 台以上用意し, Raft で複製できる ([ADR 0013](adr/0013-scheduler-raft.md)).
過半数の scheduler が動いていれば, 投入と実行を続けられる.
1 台の故障に耐えるには 3 台, 2 台の故障に耐えるには 5 台が要る.
台数の過半数が同じ物理ホストに載っていると, そのホストが止まったときに全体が止まる.

1. 証明書: `certs generate` の `--host` に全員の名前を入れ, 同じ `server.pem` を全員に置く. 全員で同じ `ca.pem`, `client.token`, `agent.token` を使う.
2. peer の token: scheduler の間の通信に使う token を作り, `peer.token` として全員に置く. 例: `od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > peer.token`.
3. `server.toml` に `[raft]` を書く. 構成員の一覧は全員で同じにし, `node_id` だけを変える.

```toml
[raft]
node_id = 1                    # sched-b では 2, sched-c では 3
peer_token_file = "/etc/algebramesh/peer.token"
ca_file = "/etc/algebramesh/ca.pem"
election_timeout_ms = 1500     # この値から 2 倍までの間で選挙を始める

[[raft.members]]
id = 1
name = "sched-a"
url = "https://sched-a.internal:50051"

[[raft.members]]
id = 2
name = "sched-b"
url = "https://sched-b.internal:50051"

[[raft.members]]
id = 3
name = "sched-c"
url = "https://sched-c.internal:50051"
```

4. client と worker には全員の URL をカンマ区切りで書く (`ALGEBRAMESH_SERVER`, `worker.toml` の `server`). leader でない scheduler は `UNAVAILABLE` を返すので, client と worker は次の URL を試す.

```bash
export ALGEBRAMESH_SERVER=https://sched-a.internal:50051,https://sched-b.internal:50051,https://sched-c.internal:50051
```

最初の起動で, 各 scheduler が構成員の一覧で Raft を初期化する.
その後の構成員は Raft が記録し, 設定の `[[raft.members]]` は使わない.

状態の確認と選挙の開始は peer の token で行う.

```bash
algebramesh server status --server https://sched-b.internal:50051 --ca ca.pem --token-file peer.token
algebramesh server promote --server https://sched-b.internal:50051 --ca ca.pem --token-file peer.token
```

`server status` は, 役割 (`leader`, `follower`, `candidate`), term (`epoch`), 適用した最後の変更の番号 (`last_seq`), leader の名前, 呼び出しに答えているか (`serving`) を返す. blob store については, 最後の補充の時点の次の値を返す ([ADR 0021](adr/0021-content-addressed-blob-store.md)).

- `blobs_under_replicated`: leader で, 記録が参照する blob のうち, 投票者の過半数に届いていないものの数. 0 でない間は, leader が送っている.
- `blobs_missing`: その scheduler が持たない, 記録が参照する blob の数. learner では, 投票者になれるまでの残りである.
- `blobs_quarantined`: SHA-256 が合わず `blobs/quarantine/` に移した本体の数.
- `blob_free_bytes`: `blob_dir` の volume の空き.
- `blobs_stored`, `blob_bytes_stored`: その scheduler が持つ本体の数と, その合計の bytes.
- `blob_uploads_refused`: その scheduler が起動してから, `blob_upload_bytes_per_token_per_hour` によって断った upload の数.

- 書き込み: 書き込みを伴う呼び出しは, その変更が過半数に記録されてから答える. 答えた変更は, leader が替わっても残る.
- 分断: 過半数から切り離された leader は, 呼び出しに `UNAVAILABLE` を返す.
- leader の交代: leader が替わると, 割り当て済みのジョブは `LOST` になり, `retry_policy` に従って再投入される.
- leader が替わった直後は, agent が登録し直すまで, 新しい leader は worker を知らない. その間の投入は, 断られるはずのものも待たされる. 同じ投入でも, 替わる前と後で答えが変わりうる.

`deploy/compose/compose.raft.yml` は, 1 台の Docker で 3 台の scheduler と Asir worker を動かす例で, `tests/integration/run-raft-e2e.sh` の試験に使う.

### 9.1 動かしながら構成員を変える

scheduler を加える手順は次のとおりである.

1. 新しい scheduler の名前を含む server 証明書を, 既存の CA で発行する.

```bash
algebramesh certs server --ca-dir pki --host sched-d.internal --out pki-d
```

2. `ca.pem`, トークン, `peer.token`, 発行した証明書を新しいホストに置く. `server.toml` の `[raft]` に `join = true` と自分だけの `[[raft.members]]` を書いて起動する. この scheduler は初期化せずに待つ.

```toml
[raft]
node_id = 4
peer_token_file = "/etc/algebramesh/peer.token"
ca_file = "/etc/algebramesh/ca.pem"
join = true

[[raft.members]]
id = 4
name = "sched-d"
url = "https://sched-d.internal:50051"
```

3. leader に加えさせる. `--server` には今の構成員の URL を並べる. その中の leader が, 新しい scheduler を learner として加え, ログに追いつかせてから投票者にする.

```bash
algebramesh server add-member --server https://sched-a.internal:50051,https://sched-b.internal:50051,https://sched-c.internal:50051 \
  --ca ca.pem --token-file peer.token --id 4 --name sched-d --url https://sched-d.internal:50051
```

記録が blob を参照していれば, 新しい scheduler はその本体を持つまで learner のまま返る (`server members` で投票者でない). 新しい scheduler の `server status` の `blobs_missing` が 0 になってから, 同じ `add-member` をもう一度送ると投票者になる.

4. client と worker の URL の一覧に, 新しい scheduler を書き足す.

scheduler を除くには, `server remove-member --id <番号>` を leader に送る. leader 自身も除ける. 除いた後の投票者の過半数が持たない blob があれば, leader は `FAILED_PRECONDITION` で断り, 裏でその blob を送る. leader の `blobs_under_replicated` が 0 になってから送り直す. 除いた scheduler は止め, client と worker の URL の一覧からも消す.
`server members` は, 構成員の番号, 名前, URL, 投票者かどうか, leader を返す.

- 構成の変更は 2 段 (joint consensus) で行う. 途中で leader が替わると, 変更が途中で止まることがある. そのときは同じ変更をもう一度行う.
- 投票者の数は奇数にしておく. 4 台では, 3 台と同じく 1 台の故障にしか耐えない.

### 9.2 版を上げる

新しい版の scheduler は, 新しいログの形や記録の欄を, cluster の全員が扱えると分かるまで使わない ([ADR 0020](adr/0020-cluster-feature-agreement.md)).

- leader は数秒ごとに全構成員に問い合わせ, 全員が扱える機能を有効にする. 有効にした機能と各 scheduler の版は `server status` の `version`, `features`, `enabled_features` に出る.
- 版を上げる順序は, scheduler (1 台ずつ), agent, client とする. ただし正しさはこの順序に頼らない. 古い scheduler が 1 台でも残っている間は, 新しい機能は有効にならない.
- 機能を有効にした後は, 機能の合意を持たない版 (`server status` が `features` を返さない版) の scheduler に戻さない. 戻した scheduler は, 機能を有効にした変更を適用できずに止まる. 機能の合意を持つ版は, cluster が記録した最低の版 (`min_binary_version`) より古ければ起動を断る.
- 機能を有効にした cluster に加える scheduler は, その機能を扱える版でなければならない. `server add-member` は, 扱えない scheduler を断る.
- agent の更新の途中では, 新しい版で足した operation の投入が断られうる. 新しい版の agent が登録するまで, その operation を申告する worker がいないからである.

## 10. 試験

```bash
cargo test --workspace --all-features         # 単体試験と同一プロセス内の結合試験
sh tests/integration/run-e2e.sh               # 実 ox_asir との OpenXM 結合試験
sh tests/integration/run-cluster-e2e.sh       # scheduler + 4 worker + Python binding
sh tests/integration/run-regression.sh        # 数学的回帰試験 (全 backend)
sh tests/integration/run-fault-e2e.sh         # worker 停止・scheduler 再起動の障害試験
sh tests/integration/run-raft-e2e.sh          # 3 台の scheduler の Raft (leader の停止, 分断)
sh tests/integration/run-mixed-version.sh     # 版の混ざった Raft の cluster (機能の合意, ADR 0020)
sh tests/bench/run-bench.sh "1 2 4" 16 28     # worker 数ごとの所要時間 (docs/benchmarks.md)
cargo deny check licenses bans sources        # Rust 依存のライセンス (deny.toml)
cargo about generate --workspace --locked --all-features --fail -o THIRD-PARTY-LICENSES.txt about.hbs   # THIRD-PARTY-LICENSES.txt の再生成
sh tests/licenses/image-licenses.sh           # イメージに含まれるライセンスの一覧
```
