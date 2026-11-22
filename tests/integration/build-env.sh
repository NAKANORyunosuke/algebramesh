# Sourced by the integration scripts before they build images: passes the
# commit of the checkout into the images (ADR 0020, `agent_build`), and says
# that the build is clean only if nothing the Rust build reads differs from
# that commit. Without git, nothing is passed and the build is not known.
if command -v git >/dev/null 2>&1 && git rev-parse --verify HEAD >/dev/null 2>&1; then
    ALGEBRAMESH_BUILD_COMMIT=$(git rev-parse HEAD)
    if [ -z "$(git status --porcelain -- Cargo.toml Cargo.lock rust-toolchain.toml proto crates \
        bindings/python/Cargo.toml bindings/python/src)" ]; then
        ALGEBRAMESH_BUILD_CLEAN=1
    else
        ALGEBRAMESH_BUILD_CLEAN=0
    fi
    export ALGEBRAMESH_BUILD_COMMIT ALGEBRAMESH_BUILD_CLEAN
fi
