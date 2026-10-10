# Revision-3 fixtures for the Format family

Real revision-3 projects, each saved by the app's own writer at commit
97188bb5 (main before any picture-revision-4 row landed), from the
synthetic builders in `crates/klipp-readiness/src/format.rs` there. The
`klipp.frei0r` and `klipp.ofx` cases were built beside them in the same
`run`. Only the project files are kept, with no media. They are what the
conversion to revision 4 converts (`klipp_convert::step4`): genuine old
forms, which a writer at revision 4 can no longer make.

| Folder | Case |
|---|---|
| `row1406` | a Transform with Scale Y 0.5 under Scale X 1 (no aspect yet) |
| `row1394` | a fixed Transform moved to x 100 |
| `row1386` | Opacity 0.5 then Opacity 1, both on (the earlier one kept off in the picture) |
| `row1409` | a Speed Ramp with every field revision 4 drops |
| `row1321` | a timeline grade with a Corrector |
| `row1391` | a grade compound two deep, its Corrector at exposure 0.4 (subgraphs as payloads) |
| `klipp.frei0r` | a frei0r effect with a double 0.1 and a bool (a positional list) |
| `klipp.ofx` | an OFX effect with a double 0.1 and an RGB colour (`{"t", "v"}` values) |

`format.rs`'s `the_revision_3_fixtures_are_as_recorded` holds each file to
its hash below, at revision 3, and read by the conversion's revision-3
reader (`unlift`): a fixture rewritten by accident fails loudly. Record
them again only from a commit before the rows, never from a real project.

```
49a9922254356ed4084cbef95b27469f84e2de592c41404dcdedad20814a78f9  ./klipp.frei0r/media/manifest.json
3d1a834dc2f58e60a895e4eff824e9466cfbb560d3a4e192a7d9af31d248954d  ./klipp.frei0r/project.json
5599d3f6abda548719b8ef2b337d3a352d27f148690fed77a9ed0c157320b7de  ./klipp.ofx/media/manifest.json
5eb3c646e91a80b57c750d8136dbee8925a0df2ed4fb76da56923f36f95e3673  ./klipp.ofx/project.json
1e7d6b7e009ca8c21498ba1aba8deba4cdac96d571ba6e8fd0b688b0059e1e95  ./row1321/media/manifest.json
603814fd9f0eaf0d9b8c4a2d8719f8492f345830b3d13f92bcd663cb11dd2aaf  ./row1321/project.json
5ab88e08958ee3075bc9928cef2ff394ab5615c6c3a24bbe988920948a08261d  ./row1386/media/manifest.json
8d696932457a5aea4b3d98a5d6a27db43b4819fbc343c6d42d21bce5f283a7ac  ./row1386/project.json
df6948af6807079f725eb7aa15d0b68ad447f8f280c7a0bf62ee627806120b73  ./row1391/media/manifest.json
de521c6f47ff5a343f250b419f9a23c3c42b92a4097d059d537f5041d2b648a0  ./row1391/project.json
12db99add9b27179146c55693e814147b1d7f3ded2530ac7afee7b01715199bf  ./row1394/media/manifest.json
38b438c8b8026732394a08b89eab5905becba92fc18ed2befd6cbc639a94f4f1  ./row1394/project.json
c4148d0d873638ac3fb4ce5f5c6abdd610223d82556411f25587eb71ad0777be  ./row1406/media/manifest.json
c39b881134dc6e22baf061b45d313499dc633314ce71542263c56179a9e05070  ./row1406/project.json
078be525475d635d4ba73ad8fd664aa9194941a57c00a15429f719e0a75aac1d  ./row1409/media/manifest.json
de1385e6d5214c92e2f4e707a1bbb59f2e3e210d93db8c90e539b985d78858b6  ./row1409/project.json
```
