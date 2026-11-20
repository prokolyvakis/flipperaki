# Flipperaki agent rules

Flipperaki = one Rust host (`src/`, one package, one binary with `daemon`, `mcp`,
`hook` and operator modes) + one native C Momentum FAP (`flipper/`), sharing
one Buddy schema (`protocol/buddy.proto`). Read `docs/architecture.md` and
`docs/protocol.md` before changing behavior.

1. Implement the smallest complete user-visible step; no speculative layers.
2. Reuse maintained libraries; document any fork/patch and its removal condition.
3. Keep decisions pure and testable without hardware; side effects at named boundaries.
4. Concrete types, composition, plain functions. Traits only for real I/O variation.
5. Ownership, cancellation, lifetimes and resource bounds are explicit (`src/limits.rs`).
6. Preserve unknown outcomes. Never turn a missing ack into success or replay a mutation.
7. Physical permission, ASCII consumption, connection epochs and grant revocation are contracts.
8. Test behavior and failure modes, not private call sequences.
9. Regenerate protocol code from the schema (`scripts/gen-protocol.sh`); never hand-edit generated code.
10. Conventional Commits (`cog verify`). No formatting churn in behavioral commits.
11. Ordinary tests and bootstrap never pair, operate, flash or publish.
12. Report what was actually tested, on which device/firmware/platform.
13. MCP stdout carries protocol messages only; logs go to stderr, redacted.

Checks (run each separately):

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo nextest run --all-targets --locked
cargo test --doc --locked
cargo deny check
scripts/check-c.sh        # portable C tests with ASan/UBSan
scripts/build-fap.sh      # FAP against the pinned Momentum SDK
```
