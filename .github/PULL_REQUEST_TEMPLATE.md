## What this changes, and why

<!-- The why is the part the diff cannot show. If it fixes an issue,
     link it: "Fixes #123". -->

## How it was tested

<!-- Which suite, on what hardware. If you have no GPU, say so here —
     the GPU parity tests will have skipped on your machine and the
     maintainer will run them before merging. -->

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green
- [ ] Ran on a machine with a GPU  <!-- if not, say what you have -->

## Checklist

- [ ] Every commit is signed off (`git commit -s`)
- [ ] Authored under my own name and email, with no AI agent as author,
      committer or co-author
- [ ] I have read this diff and can explain why each part of it is there
- [ ] New `unsafe` carries a `// SAFETY:` comment listing its preconditions
- [ ] A new dependency is licence-compatible and added to the credits in
      `README.md`  <!-- delete if not applicable -->

<!-- The PR title becomes the commit subject on main, because PRs are
     squash-merged. Imperative mood, <= 70 chars, no trailing full stop. -->
