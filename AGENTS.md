# Repository Guidelines

The role of this file is to describe common mistakes andconfusion points that agents might encounter as they work inthis project. If you ever encounter something in the projectthat surprises you, please alert the developer working with youand indicate that this is the case in the AgentMD file to helpprevent future agents from having the same issue.scoped and covered by tests where practical.

## Formatting

- `cargo fmt --check` currently reports pre-existing formatting differences in `src/file_monitor.rs`. Format and check only the files changed for the active task until a repository-wide formatting cleanup is explicitly requested.
  - `rustfmt --check src/main.rs` follows `mod` declarations and will still report `src/file_monitor.rs`; that is the same pre-existing diff, not a new one.

## Building off-Windows

- The crate only compiles for Windows targets (`std::os::windows`, winapi). On Linux/macOS use a cross check instead of `cargo check`: `rustup target add x86_64-pc-windows-gnu`, install `gcc-mingw-w64-x86-64` (needed for `ring`'s C code), then `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc cargo check --tests --target x86_64-pc-windows-gnu`. A full `cargo build --release --target x86_64-pc-windows-gnu` also links successfully (2026-09-27).
- `cargo test` cannot run off-Windows. Platform-independent modules (e.g. `src/git_watch.rs`, which gates its only Windows call behind `#[cfg(windows)]`) can be tested by pointing a scratch crate at them with `#[path = ".../src/git_watch.rs"] mod git_watch;` and the same dependency versions.
