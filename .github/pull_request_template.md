## What and why

<!-- One or two sentences: the user-visible change and the reason for it. -->

## How it was tested

- [ ] `cargo fmt --all --check` · `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo nextest run --all-targets --locked` · `cargo test --doc --locked` · `cargo deny check`
- [ ] `scripts/check-c.sh` · `scripts/build-fap.sh` (if `flipper/` or `protocol/` changed)
- [ ] A test that fails without this change

## On hardware

<!-- Device, firmware, transport and what you ran from docs/hardware-checklist.md —
     or "not run" and why. Record results in docs/verification.md. -->

## Contracts

- [ ] Physical approval, epochs and grant revocation still hold
- [ ] No missing acknowledgement is reported as success; no mutation is replayed
- [ ] Generated protocol code is regenerated, not hand-edited
