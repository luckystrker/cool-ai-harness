# M5 Rust dependency and security policy

The production Rust workspace is fail-closed at the dependency boundary. This policy applies to
every crate listed by the root `Cargo.toml`; the isolated M0 spike remains evidence, not a release
dependency graph.

## Rules

- Direct workspace dependency versions are exact pins. Upgrades are reviewed changes to both
  `Cargo.toml` and `Cargo.lock`, not implicit resolver movement.
- CI invokes Cargo with `--locked`; a stale lockfile fails instead of resolving a different graph.
- `cargo-deny` rejects known advisories, yanked crates, wildcard versions, unknown registries,
  unknown Git sources and licenses outside the explicit allowlist in `deny.toml`.
- Multiple transitive versions are reported as warnings until consolidation is practical; they are
  not silently ignored.
- Git dependencies require an explicit policy amendment, immutable revision and provenance review.
- Unsafe Rust is forbidden workspace-wide. Exceptions require a narrowly scoped ADR and tests at
  the unsafe boundary before the lint policy can change.
- Dependabot proposes a bounded weekly Cargo update group. CI remains the decision gate; automated
  opening of a pull request is not permission to merge it.

## CI and release evidence

The cross-platform Rust matrix formats, lints, tests, builds debug and release targets, verifies
generated protocol artifacts, type-checks/runs the TypeScript App Server sample and uploads the
platform `cool` binary. A separate Linux job executes `cargo deny check` because advisory data is a
network-backed CI concern.

Local development must still run the repository's full Rust definition-of-done commands. If the
advisory database cannot be reached locally, record the limitation and rely on the required CI job;
do not report the advisory scan as locally passed.
