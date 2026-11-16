# Changelog

## [1.0.1] - Unreleased

### Safety

- Bound auxiliary and multistream decoder scratch allocation to packet duration; preflight multistream frame lengths, durations, and output capacity before decoding, including padded substream boundaries.
- Validate public band quantization and short LPC SIMD slice windows before dispatch.
- Add a local length guard to the NEON MDCT TDAC kernel.
- `silk::nsq::silk_nsq` keeps its public signature and now panics on out-of-range loop parameters or short coefficient, history, or output windows instead of reading out of bounds.

### Changed

- `OpusEncoder::encode` and `OpusMSEncoder::encode` return `Error::BadArg` when `input` holds fewer samples than the frame requires; trailing samples are ignored.
- `OpusEncoder::encode` returns `Error::BadArg` when `complexity` is outside 0..=10, `bitrate_bps` is negative, or the bitrate is not representable for the frame duration. `packet_loss_perc` is clamped to 0..=100 wherever the encoder uses it.
- `OpusMSDecoder::decode` treats an empty packet as a lost packet and runs packet-loss concealment on every stream for `frame_size` samples, capped at 120 ms per call. Before, only single-stream layouts concealed; other layouts returned an error.

### Compatibility

- Document multistream decoder recovery: recreate the decoder after an error that occurs after decoding begins.
