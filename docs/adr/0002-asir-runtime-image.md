# ADR 0002: Asir 実行環境として Risa-Asir-container のイメージを使う

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §0.2, §2, §11

## 背景

設計書は既存リポジトリ `NAKANORyunosuke/Risa-Asir-container` を変更しないこと, その GHCR イメージを digest pin した任意の外部依存として扱うことを定めている.
OpenXM を一から build する Dockerfile を新リポジトリに置くかどうかは未定だった.
2026-11-03 にプロジェクト所有者から, 同リポジトリの成果物を使ってよいという判断を得た.

## 決定

- Asir worker には `ghcr.io/nakanoryunosuke/risa-asir-container@sha256:a572b603f710167f708e976900ba883b44c03ecf51378e87a796b7b127bf0aed` を digest で指定して使う. この digest は 2026-11-03 に `latest` から取得したもので, `ox_asir` の version は 20260331 である.
- イメージ内の `ox_launch` は `/OpenXM/lib/asir/ox_launch`, server は `/OpenXM/bin/ox_asir` を使う.
- 既存リポジトリの shell script はコピーしない. worker の起動は新規の `deploy/docker/asir-worker-entrypoint.sh` が行う.
- 使用したイメージは `--asir-build` 経由で各ジョブ記録の `backend.build` に残す.
- 別の build を試す場合は環境変数 `ASIR_IMAGE` で上書きする. `latest` タグだけで再現性を主張しない.

## 影響

- 既存イメージは TeX 等を含み約 1.7 GB と大きい. worker 専用の小さなイメージは, OpenXM と Asir のライセンス・再配布条件を確認してから新リポジトリ側で作る (設計書 §11).
- digest を更新するときは, ADR 0001 の mathcap と CMO の確認, および `tests/integration/run-e2e.sh` をやり直す.
