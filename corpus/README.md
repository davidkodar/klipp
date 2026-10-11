# Generated project corpus

Run `scripts/corpus.sh generate`. This requires FFmpeg with FFV1 support and
generates two seconds of 640×360 moving charts at 24 fps, stereo 48 kHz sync
audio, and saved edit/effects projects. No downloads or installed plug-ins
are used. Outputs go in a fresh directory below `target/klipp-corpus/generated`;
`--out DIRECTORY` changes the parent directory.

`generated.json` pins stream metadata, decoded video frames, decoded PCM samples and canonical
saved-project bytes. Ordinary runs only compare. An intentional source change
requires `scripts/corpus.sh generate --record "reason"`; recording generates
twice in separate directories and refuses non-reproducible inputs. Review the
manifest change with the source change. Encoded container hashes are deliberately
excluded because FFmpeg versions can change them without changing the samples.
`cargo test -p klipp-corpus` verifies the committed contract and planted failure
cases automatically; FFmpeg and FFprobe are required, with no silent skips.

The current cases cover a cut with an independent audio track, blur/sharpen and
a disabled effect. These are input reproducibility checks, not rendered-pixel
references or proof of coverage of the remaining execution families.
