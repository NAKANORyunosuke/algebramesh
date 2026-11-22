# ADR 0010: agent ごとの client 証明書

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §11, [ADR 0004](0004-scheduler-agent-protocol.md)

## 背景

agent は共通の `agent.token` で認証していた.
token を持つ者は, 任意の名前で登録できるうえ, 他の agent の id を使って heartbeat を送り, ジョブを受け取り, 結果を報告できた.
1 台の worker から token が漏れると, 全 worker を入れ替えなければならなかった.

## 決定

scheduler の設定に `agent_ca_file` があるとき, agent に client 証明書 (mutual TLS) を求める.

- TLS: scheduler は `agent_ca_file` の CA を client 証明書の検証に使う. client 証明書は TLS の段階では任意とする. client (CLI, Python) の API は同じポートで証明書なしに使うためである.
- agent gateway: すべての呼び出しで, 検証済みの client 証明書を要求する. 無ければ `UNAUTHENTICATED`.
- 名前: 登録時の名前は証明書の common name と一致しなければならない (`PERMISSION_DENIED`).
- 結び付け: 登録した agent id に証明書 (名前と serial number) を記録し, heartbeat, poll, 開始と終了の報告では同じ証明書を要求する. agent id を知っていても, 別の証明書ではその agent として振る舞えない.
- 失効: `revoked_agent_serials` に serial number を書く. CRL や OCSP は使わない. 私設 CA を閉域網で使う前提では, scheduler の設定に書くほうが運用が単純である.
- token: 証明書を使う場合も `agent.token` は引き続き必要とする. 証明書の検証に誤りがあっても token が残る.

証明書は `algebramesh certs agent` で発行する.

- 署名: `certs generate` が作る私設 CA で署名する. `certs generate` はこのために `ca-key.pem` も書く.
- 中身: common name が agent 名で, extended key usage は clientAuth だけ, serial number は 128 bit の乱数, 有効期間は既定で 365 日である.
- 名前: 英数字と `.`, `_`, `-` の 64 文字までに限る.

証明書の解析には x509-parser を使う. 署名と有効期間の検証は rustls が TLS の段階で行い, AgentGateway は検証済みの証明書から名前と serial number を読むだけである.

## 結果

- compose の cluster は worker ごとの証明書を使う. CI の cluster e2e と障害試験はこの構成で動く. `compose.e2e.yml` は証明書を使わない構成のまま残し, 両方を試験する.
- `agent_ca_file` を書かなければ以前と同じ動作になる.
- 同じ証明書を複数の agent で共有することはできる (compose の `bench-worker`). その場合, それらの agent は互いを区別できない.
- `ca-key.pem` を持つ者は任意の agent 証明書を作れる. どのホストにも置かない.
