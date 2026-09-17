# Klipp's patch to wgpu-core 22.1.0

One change, in `src/device/queue.rs`: `queue_write_buffer`,
`queue_write_staging_buffer` and `queue_write_texture` take the device's
snatch lock (read) **before** `pending_writes`, and hold that one guard
for the whole write.

## Why

`queue_submit` takes the snatch lock (read) and then `pending_writes`.
The write paths took `pending_writes` and then the snatch lock. With the
UI, the engine and the decoder threads on one device (the shared device
the zero-copy viewer needs), three threads met like this, found in a
core dump on 2026-09-17:

- the engine in `queue_submit`: holds snatch-read, waits for `pending_writes`;
- a decoder in `queue_write_buffer` (the NV12 convert's params): holds
  `pending_writes`, waits for snatch-read, which parking_lot does not
  grant while a writer waits;
- the UI in `surface_present`: waits for snatch-write, which waits for
  the engine's read.

Nobody moves again. It took two decoders feeding the GPU compositor at a
cut to hit it: playback froze at the first cut under an overlay.

`queue_write_texture` also took the snatch lock twice, the second time
while the first was held, which deadlocks on its own when a writer
arrives between the two.

## Dropping it

Later wgpu releases reworked these locks. Drop this directory and the
`[patch.crates-io]` entry when wgpu is upgraded, and play a timeline
with an overlay across a cut a few hundred times before believing it.
