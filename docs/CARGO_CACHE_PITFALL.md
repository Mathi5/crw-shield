# The Cargo fingerprint cache bug

If you've been working on crw-shield for a while, you've probably hit this:

> "I changed a `.rs` file, ran `docker build --no-cache`, and the resulting
> binary still has the OLD code."

This is a real, recurring bug caused by a combination of Docker's layer
cache and Cargo's incremental compilation cache. This document explains why
it happens and how to work around it reliably.

## What's going on

The `Dockerfile` builds the Rust workspace like this:

```dockerfile
FROM rust:1.88-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.toml
COPY crates/ crates/
RUN cargo build --release --features crw-fetch/tls-fingerprint
```

When Docker runs `cargo build`, Cargo writes its incremental compilation
artifacts to `/build/target/` inside the builder container. The result
(only the `crw-server` binary) is then `COPY`'d to the runtime stage.

The pitfall: **Cargo's incremental compilation is based on crate-level
fingerprints, not on the mtime of individual `.rs` files.** Specifically,
Cargo trusts the on-disk `target/release/.fingerprint/` directory: if the
crate's source hash matches what was compiled last time, it skips
recompilation entirely.

In our case, what tends to happen is:

1. Build #1 succeeds. `target/release/crw-server` is written. The
   fingerprint for `crw-fetch` is stored in `target/release/.fingerprint/`.
2. You edit `crates/fetch/src/ladder.rs`.
3. Build #2 runs. Cargo checks: "Has the source hash of `crw-fetch`
   changed?" The answer is **no** — Cargo's source hash is computed from
   the *crate's* fingerprint, not from a mtime walk. So Cargo skips
   recompilation. The binary in `target/release/crw-server` is identical
   to build #1.
4. Docker sees no change in the file → no rebuild layer → no new image.

The bug is in **step 3**: Cargo's source hash should have changed, but
didn't, because of how it composes the fingerprint. There are several known
upstream issues around this; the cleanest workaround for now is to force
Cargo to think the file has changed.

## The workaround

```bash
# Touch the patched .rs file to invalidate Cargo's source cache
touch crates/fetch/src/ladder.rs

# Then rebuild with --no-cache to force Docker to start from a clean slate
docker rmi -f crw-shield-crw-shield:latest
docker build --no-cache -t crw-shield-crw-shield:latest .
```

The `touch` updates the mtime, which forces Cargo to re-check the source.
Combined with `--no-cache`, the builder container has a fresh `target/`
directory, so the fingerprint is computed from scratch.

## Verifying the new binary is in the image

After a rebuild, **always verify** that the binary actually contains the
new code. A 30-second smoke test:

```bash
docker run --rm --entrypoint /bin/sh crw-shield-crw-shield:latest -c '
  md5sum /usr/local/bin/crw-server
  for s in "your_new_string_1" "your_new_string_2"; do
    c=$(grep -aoc "$s" /usr/local/bin/crw-server 2>/dev/null || echo 0)
    echo "  $s: $c"
  done
'
```

If `grep -aoc` returns `0` for a string you know you added, **the binary
is stale** and you need to repeat the workaround.

A common gotcha: `cargo build --release` may not actually link the final
binary if the linker step is cached. Watch for
`Finished release [optimized] target(s) in X.XXs` in the Docker build log
— if you see that line and the binary hash didn't change, suspect this bug.

## Why we don't fix it properly (yet)

The "real" fix is to make the Docker build layer fingerprint the source
files it depends on. There are several options:

1. **Pass `--mount=type=cache,target=/build/target`** with a stable
   fingerprint key derived from `git rev-parse HEAD` (BuildKit supports
   this).
2. **Add a `RUN echo $GIT_SHA > /build/.git_sha` step** before the build,
   and have the Dockerfile `COPY` it earlier so the layer invalidates
   on every commit.
3. **Always run `cargo clean` before `cargo build`** in the Dockerfile.

Option 1 is the cleanest but requires switching to BuildKit (most CI
runners already use it, but local `docker build` may not). Option 3
negates the value of `--no-cache` because it forces a full recompile
even when nothing changed, ballooning CI time by 5-10 minutes.

For now, the `touch` workaround is reliable, takes 1 second, and doesn't
require any infrastructure change. The decision to migrate to BuildKit
cache mounts is tracked separately.
