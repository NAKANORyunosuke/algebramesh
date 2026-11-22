# Images of AlgebraMesh. Build one target at a time:
#
#   cli              scheduler, client and `algebramesh run` (no CAS)
#   agent-asir       worker agent on the Risa/Asir image (pinned by digest)
#   agent-macaulay2  worker agent with Macaulay2 from Debian trixie
#   agent-oscar      worker agent with Julia + OSCAR
#   python-client    Python with the `algebramesh` package (for tests)
#
# Agents run as the unprivileged user `algebramesh` (design §11).
# Every image carries the AlgebraMesh license, the notices of the Rust crates and,
# for the agents, a notice on the CAS in the image in /usr/share/doc/algebramesh
# (docs/licenses.md).

ARG ASIR_IMAGE=ghcr.io/nakanoryunosuke/risa-asir-container@sha256:a572b603f710167f708e976900ba883b44c03ecf51378e87a796b7b127bf0aed

FROM rust:1.95.0-bookworm AS build
# The commit the sources are from, and whether they are that commit unchanged
# (1), compiled into the agent as its build (ADR 0020). Left empty, the build
# is not known.
ARG ALGEBRAMESH_BUILD_COMMIT=
ARG ALGEBRAMESH_BUILD_CLEAN=
WORKDIR /src
# Only what the Rust build reads, so edits to docs or tests keep the cache.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY proto proto
COPY crates crates
COPY bindings/python/Cargo.toml bindings/python/Cargo.toml
COPY bindings/python/src bindings/python/src
# The image's toolchain (the same version as rust-toolchain.toml) is used as it
# is, so the components the file lists are not downloaded; an older commit's
# file may also name another channel. The image's python3 provides the
# interpreter configuration PyO3 reads at build time.
RUN RUSTUP_TOOLCHAIN="$(rustup default | cut -d' ' -f1)" \
    cargo build --release --locked -p algebramesh-cli -p algebramesh-python \
        --features algebramesh-python/extension-module

FROM debian:bookworm-slim AS cli
RUN useradd --system --create-home algebramesh \
 && mkdir -p /var/lib/algebramesh \
 && chown algebramesh /var/lib/algebramesh
COPY --from=build /src/target/release/algebramesh /usr/local/bin/algebramesh
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/algebramesh/
USER algebramesh
WORKDIR /home/algebramesh
ENTRYPOINT ["algebramesh"]

FROM ${ASIR_IMAGE} AS agent-asir
RUN useradd --system --create-home algebramesh
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/algebramesh/
COPY deploy/notices/agent-asir.txt /usr/share/doc/algebramesh/NOTICE
COPY --from=build /src/target/release/algebramesh /usr/local/bin/algebramesh
USER algebramesh
WORKDIR /home/algebramesh
ENTRYPOINT ["algebramesh"]

FROM debian:trixie-slim AS agent-macaulay2
RUN apt-get update \
 && apt-get install -y --no-install-recommends macaulay2 4ti2 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home algebramesh
COPY drivers/macaulay2 /opt/algebramesh/drivers/macaulay2
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/algebramesh/
COPY deploy/notices/agent-macaulay2.txt /usr/share/doc/algebramesh/NOTICE
RUN dpkg-query -W -f='${Package} ${Version}\n' > /usr/share/doc/algebramesh/debian-packages.txt
COPY --from=build /src/target/release/algebramesh /usr/local/bin/algebramesh
USER algebramesh
WORKDIR /home/algebramesh
ENTRYPOINT ["algebramesh"]

FROM julia:1.11-bookworm AS agent-oscar
ENV JULIA_PROJECT=/opt/algebramesh-oscar JULIA_DEPOT_PATH=/opt/julia-depot
COPY drivers/oscar/Project.toml /opt/algebramesh-oscar/Project.toml
RUN julia -e 'using Pkg; Pkg.instantiate(); Pkg.precompile(); using Oscar, JSON3' \
 && chmod -R a+rX /opt/julia-depot /opt/algebramesh-oscar \
 && useradd --system --create-home algebramesh
COPY drivers/oscar /opt/algebramesh/drivers/oscar
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/algebramesh/
COPY deploy/notices/agent-oscar.txt /usr/share/doc/algebramesh/NOTICE
RUN cd /opt/julia-depot/artifacts \
 && for a in */; do echo "${a%/} $(ls "${a}share/licenses" 2>/dev/null | tr '\n' ' ')"; done \
      > /usr/share/doc/algebramesh/julia-artifacts.txt
COPY --from=build /src/target/release/algebramesh /usr/local/bin/algebramesh
# A writable user depot first, then the read-only depot with precompiled OSCAR.
ENV JULIA_DEPOT_PATH=/home/algebramesh/.julia:/opt/julia-depot
USER algebramesh
WORKDIR /home/algebramesh
ENTRYPOINT ["algebramesh"]

FROM python:3.11-slim-bookworm AS python-client
RUN useradd --system --create-home algebramesh
COPY bindings/python/python /opt/algebramesh-python
COPY --from=build /src/target/release/libalgebramesh_python.so /opt/algebramesh-python/algebramesh/_native.abi3.so
COPY --from=build /src/target/release/algebramesh /usr/local/bin/algebramesh
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/algebramesh/
ENV PYTHONPATH=/opt/algebramesh-python
USER algebramesh
WORKDIR /home/algebramesh
