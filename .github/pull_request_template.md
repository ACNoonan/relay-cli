## Summary

- What changed?
- Why was this needed?

## Test Plan

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D clippy::dbg_macro -D clippy::todo -D clippy::unimplemented`
- [ ] `cargo test --all-targets`
- [ ] Manually tested relevant CLI flow(s)

## Checklist

- [ ] Updated tests for behavioral changes
- [ ] Updated README/config examples if user-facing behavior changed
- [ ] No secrets or machine-specific paths added
