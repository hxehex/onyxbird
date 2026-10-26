# Changelog

All notable changes to this project are documented in this file.

## Unreleased — downstream maintenance

### Fixed
- **Driver/UDP Rx**: Correctly offset variable-length RTP extension headers and
  realign the transport-CC MAC when receiving DAVE-encrypted voice packets,
  restoring audio reception on servers with E2EE enabled. ([6d2b96fe])
- **Driver/UDP Rx**: Guard against truncated RTP packets in `udp_rx`, which
  previously caused a panic. ([57b0d322])
- Full audit pass over the mixer, scheduler, receive, input, and connection
  paths (2026-10): eliminated panic paths reachable from malformed packets
  and hi-res/multichannel/zero-rate sources, fixed a mute-tick accumulation
  bug (unbounded pop on unmute), fixed a mixer/event-thread index desync when
  two or more tracks finish in the same tick (stalled `TrackQueue`), fixed
  RTP-timestamp adjustment after park/reactivate, fixed `RawAdapter` seek
  landing 16 bytes off, recovered DAVE session locks from poisoning instead
  of panicking, and removed a per-tick heap allocation in the resampling
  fast path. ([f9181f13] [3367d52a] [b58cc859])

### Changed
- Docs: switch `doc_auto_cfg` to `doc_cfg` for docs.rs builds. ([5d461855])
- Bump crate version to 0.6.1 to match the upstream patch release — its only
  change (the `doc_cfg` switch above) was already present in this tree.
- Remove project logo and upstream badges/links. ([0031c82d])
- Rename the crate and public API for onyxbird (`Onyxbird`, `OnyxbirdKey`,
  `register_onyxbird*`), updating docs, examples, and benches. ([e0148727])
- Style: rustfmt pass and `is_none_or` lint cleanup. ([4534d815])

### Note
This project is a downstream continuation of an ISC-licensed Discord voice
library. Pre-fork history and changelog entries remain available in this
repository's git history (`git log`).

[6d2b96fe]: https://github.com/hxehex/onyxbird/commit/6d2b96fe
[57b0d322]: https://github.com/hxehex/onyxbird/commit/57b0d322
[f9181f13]: https://github.com/hxehex/onyxbird/commit/f9181f13
[3367d52a]: https://github.com/hxehex/onyxbird/commit/3367d52a
[b58cc859]: https://github.com/hxehex/onyxbird/commit/b58cc859
[5d461855]: https://github.com/hxehex/onyxbird/commit/5d461855
[0031c82d]: https://github.com/hxehex/onyxbird/commit/0031c82d
[e0148727]: https://github.com/hxehex/onyxbird/commit/e0148727
[4534d815]: https://github.com/hxehex/onyxbird/commit/4534d815
