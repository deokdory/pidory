# Release Workflow

## Prerequisites

Before releasing, make sure the following are installed on the target server:

- `sqlite3` CLI (for DB inspection/migration)
- `pidory-delayed-restart.service` (installed via `deploy/install.sh`)

Run `deploy/install.sh` on a fresh server to set up the service files.

## Release Steps

1. **Bump version in `Cargo.toml`**

   Edit `[package].version` in `Cargo.toml`:

   ```toml
   [package]
   version = "0.6.x"
   ```

2. **Build and verify**

   ```bash
   cargo build --release
   ```

   Confirm the binary reports the new version (via `/update` command or `--version`).

3. **Commit**

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "chore: bump version to v0.6.x"
   ```

4. **Tag**

   ```bash
   git tag v0.6.x
   ```

   The tag name must exactly match the `Cargo.toml` version with a `v` prefix.

5. **Push commits and tags**

   ```bash
   git push origin <branch>
   git push --tags
   ```

6. **Create GitHub release**

   Go to the repository releases page and draft a new release targeting the tag `v0.6.x`.
   Add a changelog summary in the release notes.

## Version Consistency Rule

`Cargo.toml` version, git tag, and the running binary version must always be identical.

The `/update` command surfaces the current version via `env!("CARGO_PKG_VERSION")`, which is
baked in at compile time. If the binary is built from a commit where `Cargo.toml` says `0.6.3`
but the tag is `v0.6.4`, the bot will report the wrong version and self-update logic will
misfire. Always bump `Cargo.toml` before tagging.

## Future Automation (TODO)

- GitHub Actions: on `push --tags`, verify that the pushed tag matches `Cargo.toml` version.
  If they differ, fail the workflow early to prevent mismatched releases.
  (Tracked as a separate issue — do not implement here.)
