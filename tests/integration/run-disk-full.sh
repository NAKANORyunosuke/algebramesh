#!/bin/sh
# A full disk under a scheduler of a Raft cluster (Issue #37), in a Linux
# container whose small file system is a tmpfs of 64 MiB:
#
# 1. A follower's disk fills up: it stops with an explicit error while the
#    other two go on; with space again it starts and catches up.
# 2. The leader's disk fills up: it stops with an explicit error, the other
#    two elect a leader and go on; it comes back as a follower.
# 3. A snapshot does not fit on the receiver's disk: the transfer is refused
#    and sent again until there is space, without a restart.
# 4. A blob does not fit (ADR 0021, Issue #74): a follower refuses it and
#    stays a member without it; as the leader, the job ends with
#    RESOURCE_LIMIT and small results still go through.
#
# The tests are in crates/algebramesh-agent/tests/raft_faults.rs
# (`disk_full_*`) and crates/algebramesh-agent/tests/blobs.rs
# (`disk_full_for_blobs`). The image's toolchain (the same version as
# rust-toolchain.toml) is used as it is, so the components the file lists are
# not downloaded.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
# Docker on Windows needs the Windows form of the path.
SRC=$(cd "$ROOT" && (pwd -W 2>/dev/null || pwd))

docker run --rm \
    -v "$SRC:/src" -w /src \
    -v algebramesh-cargo-registry:/usr/local/cargo/registry \
    -v algebramesh-disk-full-target:/target -e CARGO_TARGET_DIR=/target \
    --tmpfs /small:size=64m,mode=1777 \
    -e ALGEBRAMESH_TEST_SMALL_DIR=/small \
    -e RUSTUP_TOOLCHAIN=1.95.0 \
    -e ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS="${ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS:-1000}" \
    rust:1.95.0-bookworm \
    sh -c 'cargo test --locked -p algebramesh-agent --test raft_faults -- --ignored --test-threads=1 disk_full &&
           cargo test --locked -p algebramesh-agent --test blobs -- --ignored --test-threads=1 disk_full'
