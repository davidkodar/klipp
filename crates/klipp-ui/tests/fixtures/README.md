# Fixtures

`old_rows.json` is frozen input, recorded once from the inspector at
BASE-M3 (1e11f285) and edited by hand since when rows went (#1482). The
presentation tests read it as the expected presentation of each row:
`klipp-nodes/tests/presentation.rs`, `klipp-effects/tests/presentation.rs`
and `klipp-ui/tests/generated_rows.rs`. Nothing records it again, and it is
not a parity check against old code: the tabbed page's hand-drawn rows it
holds (`tab/` cases) are gone (#1483).
