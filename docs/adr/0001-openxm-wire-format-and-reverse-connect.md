# ADR 0001: OpenXM の通信手順と CMO 表現

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §5, §15「`ox_launch` の細かい接続確立手順」「CMO による多項式の無損失な変換」

## 背景

設計書は `ox_launch` の接続確立手順と CMO の対応範囲を実装開始時に検証し, ADR に記録してから固定すると定めている.
本 ADR は `openxm-org/OpenXM_contrib2` の `asir2018/io/` のソースと, `ghcr.io/nakanoryunosuke/risa-asir-container@sha256:a572b603…` 上の `ox_asir` (version 20260331) での実測に基づく.

## 確認した事実

### ReverseConnect の接続順序 (`io/ox_launch.c`, `io/sio.c`)

- `ox_launch <host> 0 <control_port> <data_port> <server> <display>` の第2引数 `0` は接続モードを表す. 起動側が control, data の順に `try_connect` する.
- `try_connect` は 100 ms 間隔で最大10回だけ再試行する. listener がまだ無い場合に備え, worker 側の entrypoint でも再試行する.
- パスワード等の認証は送られない. 接続元の検証はネットワーク境界と, control と data の接続元ホストの一致確認で行う.
- `ox_launch` は control で byte-order を交渉してから server を fork し, data 接続を server の fd 3/4 に渡す. data 側の交渉は server 起動後に始まる.
- server の exec に失敗すると, `ox_launch` は data 接続にバイト `0x02` を書く. クライアントはこれを `ServerSpawnFailed` として報告する.
- 第6引数 (display) は `"0"` を渡す. 標準出力と標準エラーは `/dev/null` へ送られ, `DISPLAY` は空になる. 既存リポジトリの entrypoint は `worker1:0` を渡しているが, その場合は `-display worker1:0` 付きで起動される.
- control 上の `SM_shutdown` を受けると, launcher は server を `SIGKILL` してから終了する. `SM_control_kill` は server だけを `SIGKILL` する.

### byte-order 交渉 (`get_iofp`, `ox_asir.c`)

- server 側 (launcher, `ox_asir`) が先に1バイト書き, その後クライアントの1バイトを読む. 値は little endian 機なら `0x01`, big endian 機なら `0xFF` である.
- 両者のバイトが異なれば server は network byte order に変換する. AlgebraMesh は常に `0x00` を送るので, 通信路は常に big endian になる.

### CMO 表現 (`io/cio.c`)

- `CMO_ZZ` は符号付きの語数 (符号は整数の符号) に続き, 絶対値を 32 bit 語で最下位語から並べる (`mpz_export(…, -1, …)`).
- `CMO_STRING` と `CMO_DATUM` は 32 bit 長とバイト列からなる. 終端 NUL は無い.
- 多項式は `CMO_RECURSIVE_POLYNOMIAL(CMO_LIST[CMO_INDETERMINATE(CMO_STRING name)…], body)` である. `body` は入れ子の `CMO_UNIVARIATE_POLYNOMIAL(項数, 変数番号, (次数, 係数)…)` で, 次数は降順に並ぶ.
- Asir は 0 を `CMO_NULL` として送る. 係数が有理数なら `CMO_QQ`, 整数なら `CMO_ZZ` である.
- `SM_executeFunction` は関数名, 引数個数 (`CMO_INT32`), 第1引数, 第2引数, … の順に pop する. したがってクライアントは最後の引数から順に push し, 次に個数, 最後に関数名を push する.
- 関数が見つからない等の誤りは `CMO_ERROR2(CMO_LIST[serial, message])` としてスタックに積まれ, `SM_popCMO` で返る.

### 実サーバの mathcap (実測)

```text
[[199901160, "ox_asir"],
 [276,275,258,262,263,277,266,267,268,274,269,272,265,264,273,300,270,271,400,278,279,280,281,282,283],
 [[514,[1,2,3,4,5,2130706433,2130706434,17,19,20,21,22,24,25,26,31,34,27,33,60,61,52,35]],
  [2144202544,[0,1]]]]
```

第1要素の system 名は `"Ox_system=ox_asir"` ではなく `"ox_asir"` である. パーサは両方を受け付ける.

## 決定

- `algebramesh-openxm-protocol` は上記の表現を実装し, golden test (`codec.rs`, `message.rs`) で固定する.
- 第1段階で decode できる CMO は NULL, INT32, DATUM, STRING, MATHCAP, LIST, ZZ, QQ, ZERO, ERROR2, INDETERMINATE, RECURSIVE_POLYNOMIAL, UNIVARIATE_POLYNOMIAL とする. それ以外は `UnsupportedCmo` を返し, 読み飛ばさない. その場合セッションは同期が不明になるので破棄する.
- 1 listener 組で受け付けるセッションは1つとする (Asir の `try_accept` と同じく, accept 後に listener を閉じる).
- 不正な長さ, 深さ, 要素数, 多倍長整数の大きさには上限を設け, 宣言長に応じて事前確保しない.

## 残る課題

- (解決済み, Issue #8) `recv` は不完全なメッセージを受け取るたびに先頭から decode し直していた. 数百 MB 規模の結果では二乗時間になる. 今は `MessageReader` が, 受け取った部分の構造を 1 度だけたどって位置を覚え, メッセージが揃ってから 1 回だけ decode する. 文字列, datum, 整数は長さで読み飛ばし, list と多項式は明示的な stack で 1 要素ずつたどる. 上限 (`DecodeLimits`) は長さを読んだ時点で掛ける. 64 KiB ずつ与えたときの時間 (release, 1 回の計測):

  | メッセージ | 大きさ | `MessageReader` | 先頭から decode し直す方式 |
  |---|---|---|---|
  | 1 MiB の datum の list | 1 KiB / 1 MiB / 16 MiB / 128 MiB | 0.000 / 0.001 / 0.013 / 0.114 秒 | 0.000 / 0.001 / 0.510 秒 / (測らず) |
  | 32 bit 整数の list | 1 KiB / 1 MiB / 16 MiB / 128 MiB | 0.000 / 0.004 / 0.066 / 0.607 秒 | 0.000 / 0.030 / 6.556 秒 / (測らず) |

  1 MiB あたりの時間は大きさによらずほぼ一定である. 受信のバッファは `Vec` の伸長により最大でメッセージの 2 倍の容量になる (128 MiB のメッセージで 256 MiB). 計測は `cargo test --release -p algebramesh-openxm-protocol --test decode_scaling -- --ignored --nocapture` で再現できる. 受信は `read_buf` で読んだ分だけをバッファに足すので, 途中で取り消しても読んだバイトは失われない.
- UNIX domain socket による `ox_launch` (host が `"."` の場合) には未対応.
- little endian での通信は単体テストのみで確認した. 実サーバとは常に network byte order で通信する.
