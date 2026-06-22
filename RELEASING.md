# Release process

This document covers how to cut a new release of crw-shield. Releases are
**fully automated** via GitHub Actions: pushing a semver tag triggers a
build matrix across 4 platforms, attaches the binaries to a GitHub Release,
and publishes a Docker image to GHCR.

## TL;DR

```bash
# 1. Make sure main is green
git checkout main
git pull

# 2. Bump version
# Edit Cargo.toml -> [workspace.package] version = "X.Y.Z"
# Edit crates/*/Cargo.toml if they have a separate version (they shouldn't)
# Update CHANGELOG.md with the new version header

# 3. Commit + tag
git add -A
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"

# 4. Push
git push origin main
git push origin vX.Y.Z

# 5. Wait for the release workflow (~20 min)
# Open https://github.com/Mathi5/crw-shield/releases/tag/vX.Y.Z
```

## Versioning

crw-shield follows [Semantic Versioning](https://semver.org/):

- **MAJOR** — breaking API change (e.g. v1 → v2 of the Firecrawl routes)
- **MINOR** — new feature, backward-compatible (e.g. new tier, new format)
- **PATCH** — bug fix, backward-compatible

The workspace version is in `Cargo.toml` at the root:

```toml
[workspace.package]
version = "0.1.0"
```

All crates inherit this version. There is no per-crate versioning — keep it
simple.

## Build matrix

The release workflow (`.github/workflows/release.yml`) builds the binary
for 4 targets:

| Target | Triple | Binary name |
|--------|--------|-------------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `crw-shield-linux-x86_64.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `crw-shield-linux-aarch64.tar.gz` |
| macOS x86_64 | `x86_64-apple-darwin` | `crw-shield-darwin-x86_64.tar.gz` |
| macOS ARM64 | `aarch64-apple-darwin` | `crw-shield-darwin-aarch64.tar.gz` |

Each archive contains the stripped binary plus a `LICENSE` and a `README.md`
copy, so users have everything they need in one tarball.

Windows is **not** built by default because the CDP fetcher relies on
Linux/macOS-only Chromium paths. If you need Windows support, open an issue.

## What gets published

When you push a tag like `v0.2.0`, the workflow:

1. Runs `cargo test` and `cargo clippy -D warnings` on the source first —
   the release is **aborted** if these fail
2. Builds the binary for all 4 targets in parallel (`cross` for non-native
   triples, native `cargo build` for the runner's host triple)
3. Strips the binary (`strip` on Unix) and tars it up
4. Creates a GitHub Release via `softprops/action-gh-release@v2` with the
   archive as an asset, plus auto-generated release notes
5. Builds the Docker image and pushes it to `ghcr.io/Mathi5/crw-shield:X.Y.Z`
   (and a `:latest` tag for the latest stable)

The release is then visible at:
`https://github.com/Mathi5/crw-shield/releases/tag/vX.Y.Z`

## Manual release (escape hatch)

If the automated workflow fails and you need to publish urgently:

```bash
# Build a single target
cargo build --release --features crw-fetch/tls-fingerprint
strip target/release/crw-server

# Create the archive
mkdir -p crw-shield-X.Y.Z
cp target/release/crw-server crw-shield-X.Y.Z/crw-shield
cp LICENSE README.md crw-shield-X.Y.Z/
tar -czf crw-shield-linux-x86_64.tar.gz crw-shield-X.Y.Z

# Create the GitHub Release (using gh CLI)
gh release create vX.Y.Z \
  crw-shield-linux-x86_64.tar.gz \
  --title "vX.Y.Z" \
  --notes "See CHANGELOG.md for the full list of changes."
```

Repeat for the other 3 targets (ideally on the matching OS, or use `cross`).

## Pre-release versions

For beta / RC tags (`v0.2.0-beta.1`, `v0.2.0-rc.1`), the workflow
**marks the release as pre-release** automatically based on the tag name.
Users will see a warning when downloading from `latest`, so they have to
opt in explicitly.

## Hotfix workflow

If a critical bug is found in a released version:

1. Branch from the affected tag: `git checkout -b hotfix/v0.2.1 v0.2.0`
2. Fix the bug (minimum diff, no refactors)
3. Bump version to `0.2.1` in `Cargo.toml`
4. Tag and push: `git tag -a v0.2.1 -m "v0.2.1 hotfix" && git push origin v0.2.1`
5. Merge back to `main` with a fast-forward or PR

## Skipping CI

If a commit should NOT trigger a release (e.g. you tagged by accident):

```bash
# Delete the tag locally and remotely
git tag -d vX.Y.Z
git push origin --delete vX.Y.Z

# Cancel the in-flight workflow run
gh run list --workflow=release.yml
gh run cancel <run-id>
```

The release (if it was already created) can be deleted via the GitHub UI
or `gh release delete vX.Y.Z`.

## Verification checklist

Before pushing a tag, verify:

- [ ] `cargo test --all --locked` passes
- [ ] `cargo clippy --all --all-targets -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `CHANGELOG.md` has the new version header with the date and a summary
- [ ] `Cargo.toml` version matches the tag (no `-alpha` / `-beta` suffix in
      Cargo; that lives in the git tag only)
- [ ] The bench (if relevant) was run and shows no regression vs the
      previous release

## Troubleshooting

**"release workflow failed: cargo test failed on macOS runner"**
The macOS runner uses a different toolchain version. Check the
`dtolnay/rust-toolchain@stable` step output. Pin to a specific version
(e.g. `1.88`) if needed.

**"Binary is 50MB, expected ~5MB"**
The build didn't strip. Verify the `strip` step in the workflow ran, and
that `--release` was passed to `cargo build`. Debug builds include DWARF
debug info and are 10x larger.

**"Docker image push failed: 401 Unauthorized"**
The workflow's `GITHUB_TOKEN` doesn't have permission to push to GHCR by
default. Add this to repo Settings → Actions → General → Workflow
permissions → "Read and write permissions".
