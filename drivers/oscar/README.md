# OSCAR driver

`algebramesh.jl` is started once per slot by the oscar backend:

```bash
julia --project=/opt/algebramesh-oscar --startup-file=no algebramesh.jl
```

After loading OSCAR it prints `ALGEBRAMESH_READY {versions}` and then answers one
JSON request per line on stdin with `ALGEBRAMESH_RESULT {json}` or
`ALGEBRAMESH_ERROR message`. `Project.toml` lists the Julia packages (OSCAR 1.x,
JSON3). See [ADR 0005](../../docs/adr/0005-cas-drivers.md).
