# Contributing to Atropos

Thanks for helping. A few rules keep Atropos safe to run on people's machines.

## Ground rules

- **Never act on a bare PID.** Anything that terminates, suspends or signals a process goes through `Platform::pin` (a handle whose create time was re-checked).
- **Tests act only on processes they started** (the `crates/fixtures` binaries, or children a test spawned). Never point a test or a manual run at real system processes; use `--dry-run` when trying the CLI on your machine.
- **Console Ctrl+C reaches every process on that console.** Integration tests give console fixtures their own hidden console (`CREATE_NO_WINDOW`) so a test can never Ctrl+C the test runner.
- **`unsafe` lives only in `crates/atropos-core/src/platform/windows/`**, every block with a `// SAFETY:` comment; handles are RAII.
- No `unwrap()`/`expect()` outside tests. Errors are `thiserror` in the core, `anyhow` in the binaries.
- No new network access or telemetry, ever.

## Workflow

1. Write the failing test first (unit tests run against the fake backend in `platform/fake.rs`; Windows behaviour gets an integration test under `crates/fixtures/tests/`).
2. Make it pass, then run:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p atropos-gui --all-targets --no-default-features --features renderer-wgpu --target-dir target/wgpu -- -D warnings
cargo test --workspace
pwsh scripts/perf.ps1 -Mode all       # if you touched the collector, the GUI loop or the CLI
```

3. Non-obvious Windows API findings go in `docs/notes/windows-apis.md`; design decisions in an ADR under `docs/adr/`.

## Wanted

- A short screen recording (GIF) of a subtree kill in the GUI, made against the `forker` fixture, for the README.
- Manual QA on setups we can't automate: standard (non-admin) users, RDP / VMs without a GPU, non-English Windows and keyboard layouts, Docker Desktop and WSL port forwards. The checklist is `docs/notes/manual-qa.md`.
