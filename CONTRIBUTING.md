# Contributing

## 変更前の確認

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

OpenXM の通信や Asir backend を変更した場合は, Docker 上の結合試験も通す.

```bash
sh tests/integration/run-e2e.sh
```

## 方針

- 数式処理アルゴリズムを Rust で再実装しない. 検算に必要な最小限の演算 (多項式の積など) に留める.
- CAS に渡す処理は allowlist に登録した operation と型付き引数に限る. 利用者の文字列を CAS の parser に渡さない.
- CAS の結果を文字列表示で比較しない. 共通型に無損失に変換できない結果は, 推測で変換せずエラーにする.
- OpenXM の挙動を新たに確認したら, 根拠 (ソースの場所, 実測した版) とともに `docs/adr/` に記録する.
- `Risa-Asir-container` リポジトリは変更しない. 公開イメージを digest 指定で参照する.
