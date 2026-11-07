# Changelog

## [1.0.1] - Unreleased

### Safety

- Bound auxiliary and multistream decoder scratch allocation to packet duration; preflight multistream frame lengths, durations, and output capacity before decoding, including padded substream boundaries.
- Validate public band quantization and short LPC SIMD slice windows before dispatch.
- Add a local length guard to the NEON MDCT TDAC kernel.

### Compatibility

- Preserve the existing `silk_nsq` public API while tracking complete caller-boundary validation as unresolved work; no breaking visibility change was made.
- Document multistream decoder recovery: recreate the decoder after an error that occurs after decoding begins.
