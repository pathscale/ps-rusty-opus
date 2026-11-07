//! Opus multistream (surround) — port of the core of
//! `src/opus_multistream_{encoder,decoder}.c`. Wraps N mono/coupled Opus
//! coders behind a channel-mapping layout so >2-channel audio (quad, 5.1,
//! 7.1) can be coded as a set of standard Opus streams concatenated with the
//! self-delimited framing.
//!
//! The channel bitrate allocation here is a simple even split across streams
//! (coupled streams get 2x a mono stream's share) — libopus adds a
//! surround-masking analysis on top, a quality refinement, not a conformance
//! requirement. The bitstream layout, mapping, and per-stream Opus coding are
//! standard, so streams interoperate with libopus.

use crate::Error;
use crate::repacketizer::{Repacketizer, parse_frames};
use crate::{Application, Bandwidth, OpusDecoder, OpusEncoder};
use std::borrow::Cow;

/// Vorbis channel layout for mapping family 1, channels 1..=8:
/// (nb_streams, nb_coupled_streams, channel_mapping).
const VORBIS_MAPPINGS: [(usize, usize, &[u8]); 8] = [
    (1, 0, &[0]),                      // mono
    (1, 1, &[0, 1]),                   // stereo
    (2, 1, &[0, 2, 1]),                // 1-d (3.0)
    (2, 2, &[0, 1, 2, 3]),             // quad
    (3, 2, &[0, 4, 1, 2, 3]),          // 5.0
    (4, 2, &[0, 4, 1, 2, 3, 5]),       // 5.1
    (4, 3, &[0, 4, 1, 2, 3, 5, 6]),    // 6.1
    (5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]), // 7.1
];

/// How the channels of a multistream signal map onto mono and stereo
/// (coupled) Opus streams (RFC 7845 channel mapping).
#[derive(Clone)]
pub struct ChannelLayout {
    /// Total output channels.
    pub nb_channels: usize,
    /// Number of Opus streams in each packet.
    pub nb_streams: usize,
    /// How many of those streams are stereo (coupled); they come first.
    pub nb_coupled_streams: usize,
    /// For each output channel, the decoded channel index it takes.
    pub mapping: Vec<u8>,
}

impl ChannelLayout {
    /// Standard layout for a channel count + mapping family (0 = mono/stereo,
    /// 1 = Vorbis surround for 1..=8 channels).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `mapping_family` is not 0 or 1, or `channels` is out
    /// of range for it (1–2 for family 0, 1–8 for family 1).
    pub fn surround(channels: usize, mapping_family: i32) -> Result<Self, Error> {
        match mapping_family {
            0 => {
                if channels == 1 {
                    Ok(Self {
                        nb_channels: 1,
                        nb_streams: 1,
                        nb_coupled_streams: 0,
                        mapping: vec![0],
                    })
                } else if channels == 2 {
                    Ok(Self {
                        nb_channels: 2,
                        nb_streams: 1,
                        nb_coupled_streams: 1,
                        mapping: vec![0, 1],
                    })
                } else {
                    Err(Error::BadArg("family 0 supports only 1-2 channels"))
                }
            }
            1 => {
                if !(1..=8).contains(&channels) {
                    return Err(Error::BadArg("family 1 supports 1-8 channels"));
                }
                let (ns, nc, m) = VORBIS_MAPPINGS[channels - 1];
                Ok(Self {
                    nb_channels: channels,
                    nb_streams: ns,
                    nb_coupled_streams: nc,
                    mapping: m.to_vec(),
                })
            }
            _ => Err(Error::BadArg("unsupported mapping family")),
        }
    }

    fn left_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id * 2 {
                return i as i32;
            }
        }
        -1
    }
    fn right_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id * 2 + 1 {
                return i as i32;
            }
        }
        -1
    }
    fn mono_channel(&self, stream_id: usize, prev: i32) -> i32 {
        let start = if prev < 0 { 0 } else { prev as usize + 1 };
        for (i, &m) in self.mapping.iter().enumerate().skip(start) {
            if m as usize == stream_id + self.nb_coupled_streams {
                return i as i32;
            }
        }
        -1
    }
}

/// Multistream encoder: one Opus encoder per stream (coupled = stereo, the
/// rest mono), coded per the channel layout and concatenated self-delimited.
pub struct OpusMSEncoder {
    layout: ChannelLayout,
    encoders: Vec<OpusEncoder>,
    sample_rate: i32,
    /// Total target bitrate across all streams (split evenly, coupled=2x mono).
    pub bitrate_bps: i32,
}

impl OpusMSEncoder {
    /// A surround encoder for `channels` channels with the standard layout of
    /// `mapping_family` (see [`ChannelLayout::surround`]).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] for an unsupported channel count / mapping family, or any
    /// error from creating the per-stream encoders.
    pub fn new(
        sample_rate: i32,
        channels: usize,
        mapping_family: i32,
        application: Application,
    ) -> Result<Self, Error> {
        let layout = ChannelLayout::surround(channels, mapping_family)?;
        let mut encoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            encoders.push(OpusEncoder::new(sample_rate, ch, application)?);
        }
        let mut enc = Self {
            layout,
            encoders,
            sample_rate,
            bitrate_bps: 64000 * channels as i32,
        };
        enc.set_bitrate(enc.bitrate_bps);
        Ok(enc)
    }

    /// Split the total bitrate across streams (each coupled stream gets 2x a
    /// mono stream's share, matching its 2 channels).
    pub fn set_bitrate(&mut self, total: i32) {
        self.bitrate_bps = total;
        let units = self.layout.nb_coupled_streams * 2
            + (self.layout.nb_streams - self.layout.nb_coupled_streams);
        let per_unit = if units > 0 {
            total / units as i32
        } else {
            total
        };
        for (s, e) in self.encoders.iter_mut().enumerate() {
            e.bitrate_bps = if s < self.layout.nb_coupled_streams {
                per_unit * 2
            } else {
                per_unit
            };
        }
    }

    /// Number of Opus streams in each multistream packet.
    pub fn nb_streams(&self) -> usize {
        self.layout.nb_streams
    }

    /// Encode one frame of interleaved `input` into a multistream packet.
    /// `frame_size` is the number of samples per channel; `input` must contain
    /// at least `frame_size * nb_channels` samples. Any trailing samples are
    /// ignored.
    ///
    /// # Errors
    ///
    /// Returns `BadArg` for an invalid frame duration or an input shorter than
    /// the complete interleaved frame. Also returns errors from per-stream
    /// [`crate::OpusEncoder::encode`] calls or self-delimited packet assembly.
    pub fn encode(&mut self, input: &[f32], frame_size: usize) -> Result<Vec<u8>, Error> {
        // Validate the full request before allocating scratch or mutating any
        // per-stream encoder state. Keep this duration set in sync with the
        // single-stream encoder contract.
        let fs = self.sample_rate as usize;
        let valid = [
            fs / 400,
            fs / 200,
            fs / 100,
            fs / 50,
            fs / 25,
            3 * fs / 50,
            4 * fs / 50,
            5 * fs / 50,
            6 * fs / 50,
        ]
        .contains(&frame_size);
        if !valid || frame_size == 0 {
            return Err(Error::BadArg("Invalid frame size for sampling rate"));
        }
        let nch = self.layout.nb_channels;
        let input_size = frame_size
            .checked_mul(nch)
            .ok_or(Error::BadArg("input size overflow"))?;
        if input.len() < input_size {
            return Err(Error::BadArg("input buffer too small"));
        }

        let mut out: Vec<u8> = Vec::new();
        let mut stream_buf = vec![0f32; frame_size * 2];
        let mut pkt = vec![0u8; 1500 + frame_size];

        for s in 0..self.layout.nb_streams {
            let coupled = s < self.layout.nb_coupled_streams;
            let sch = if coupled { 2 } else { 1 };
            // Gather this stream's channels from the interleaved input.
            if coupled {
                let l = self.layout.left_channel(s, -1);
                let r = self.layout.right_channel(s, -1);
                for i in 0..frame_size {
                    stream_buf[i * 2] = if l >= 0 {
                        input[i * nch + l as usize]
                    } else {
                        0.0
                    };
                    stream_buf[i * 2 + 1] = if r >= 0 {
                        input[i * nch + r as usize]
                    } else {
                        0.0
                    };
                }
            } else {
                let m = self.layout.mono_channel(s, -1);
                for i in 0..frame_size {
                    stream_buf[i] = if m >= 0 {
                        input[i * nch + m as usize]
                    } else {
                        0.0
                    };
                }
            }
            let n =
                self.encoders[s].encode(&stream_buf[..frame_size * sch], frame_size, &mut pkt)?;
            // All streams but the last are self-delimited so the decoder can
            // find each stream's boundary.
            if s != self.layout.nb_streams - 1 {
                let mut rp = Repacketizer::new();
                rp.cat(&pkt[..n])?;
                out.extend_from_slice(&rp.out_self_delimited()?);
            } else {
                out.extend_from_slice(&pkt[..n]);
            }
        }
        Ok(out)
    }

    /// Input sampling rate in Hz.
    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }
}

/// Multistream decoder: decode each stream and remux to the output channels.
pub struct OpusMSDecoder {
    layout: ChannelLayout,
    decoders: Vec<OpusDecoder>,
    sample_rate: i32,
}

impl OpusMSDecoder {
    /// A surround decoder for `channels` channels with the standard layout of
    /// `mapping_family` (see [`ChannelLayout::surround`]).
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] for an unsupported channel count / mapping family, or any
    /// error from creating the per-stream decoders.
    pub fn new(sample_rate: i32, channels: usize, mapping_family: i32) -> Result<Self, Error> {
        let layout = ChannelLayout::surround(channels, mapping_family)?;
        let mut decoders = Vec::with_capacity(layout.nb_streams);
        for s in 0..layout.nb_streams {
            let ch = if s < layout.nb_coupled_streams { 2 } else { 1 };
            decoders.push(OpusDecoder::new(sample_rate, ch)?);
        }
        Ok(Self {
            layout,
            decoders,
            sample_rate,
        })
    }

    /// Decode a multistream packet into interleaved `output` (nb_channels per
    /// sample). Returns the number of samples per channel.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPacket`] if the multistream framing is malformed, or any
    /// error from the per-stream [`crate::OpusDecoder::decode`] calls. If an
    /// error occurs after decoding begins, output may be partially modified and
    /// some stream decoder states may have advanced; discard this decoder and
    /// create a new one before retrying.
    pub fn decode(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, Error> {
        let nch = self.layout.nb_channels;
        let mut data = packet;

        // Parse every stream and compare packet durations before mutating any
        // decoder state. `frame_size` is capacity, not the packet's duration.
        let mut decoded_samples = None;
        for s in 0..self.layout.nb_streams {
            let last = s == self.layout.nb_streams - 1;
            let frames = parse_frames(data, !last)?;
            if frames.ranges().iter().any(|(_, len)| *len > 1275) {
                return Err(Error::InvalidPacket("frame exceeds 1275 bytes"));
            }
            let samples_per_frame =
                crate::repacketizer::samples_per_frame(frames.toc, self.sample_rate) as usize;
            let stream_samples = frames
                .count
                .checked_mul(samples_per_frame)
                .ok_or(Error::BufferTooSmall("multistream duration overflow"))?;
            if let Some(expected) = decoded_samples {
                if stream_samples != expected {
                    return Err(Error::InvalidPacket("multistream duration mismatch"));
                }
            } else {
                decoded_samples = Some(stream_samples);
            }
            if !last {
                data = &data[frames.end..];
            }
        }

        let decoded_samples = decoded_samples.unwrap_or(0);
        if decoded_samples > frame_size {
            return Err(Error::BufferTooSmall("multistream frame buffer too small"));
        }
        let needed = decoded_samples
            .checked_mul(nch)
            .ok_or(Error::BufferTooSmall("multistream output size overflow"))?;
        if output.len() < needed {
            return Err(Error::BufferTooSmall("multistream output buffer too small"));
        }
        let scratch_len = decoded_samples
            .checked_mul(2)
            .ok_or(Error::BufferTooSmall("multistream scratch size overflow"))?;
        let mut buf = vec![0f32; scratch_len];
        output[..needed].fill(0.0);

        let mut data = packet;
        let mut produced = decoded_samples;

        for s in 0..self.layout.nb_streams {
            let coupled = s < self.layout.nb_coupled_streams;
            let last = s == self.layout.nb_streams - 1;
            // Determine this stream's byte slice.
            let (stream_packet, advance) = if last {
                (Cow::Borrowed(data), data.len())
            } else {
                let (normalized, advance) = normalize_self_delimited_packet(data)?;
                (Cow::Owned(normalized), advance)
            };
            let n = self.decoders[s].decode(stream_packet.as_ref(), frame_size, &mut buf)?;
            if n != decoded_samples {
                return Err(Error::InvalidPacket("multistream duration mismatch"));
            }
            produced = n;
            // Remux this stream's channel(s) to the output.
            if coupled {
                let mut prev = -1;
                loop {
                    let chan = self.layout.left_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i * 2];
                    }
                    prev = chan;
                }
                let mut prev = -1;
                loop {
                    let chan = self.layout.right_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i * 2 + 1];
                    }
                    prev = chan;
                }
            } else {
                let mut prev = -1;
                loop {
                    let chan = self.layout.mono_channel(s, prev);
                    if chan == -1 {
                        break;
                    }
                    for i in 0..n {
                        output[i * nch + chan as usize] = buf[i];
                    }
                    prev = chan;
                }
            }
            if !last {
                data = &data[advance..];
            }
        }
        // Unmapped channels (mapping == 255) are silenced.
        for c in 0..nch {
            if self.layout.mapping.get(c).copied() == Some(255) {
                for i in 0..produced {
                    output[i * nch + c] = 0.0;
                }
            }
        }
        Ok(produced)
    }
}

fn normalize_self_delimited_packet(data: &[u8]) -> Result<(Vec<u8>, usize), Error> {
    let frames = parse_frames(data, true)?;
    let ranges = frames.ranges();
    if ranges.iter().any(|(_, len)| *len > 1275) {
        return Err(Error::InvalidPacket("frame exceeds 1275 bytes"));
    }

    // Each stream's self-delimited prefix is replaced with a standard code-3
    // VBR header before handing the packet to the single-stream decoder.
    let mut normalized = Vec::new();
    normalized.push((frames.toc & 0xfc) | 3);
    normalized.push(0x80 | frames.count as u8);
    for &(_, len) in ranges.iter().take(frames.count.saturating_sub(1)) {
        if len < 252 {
            normalized.push(len as u8);
        } else {
            let first = 252 + (len & 3);
            normalized.push(first as u8);
            normalized.push(((len - first) / 4) as u8);
        }
    }
    for &(offset, len) in ranges {
        normalized.extend_from_slice(&data[offset..offset + len]);
    }
    Ok((normalized, frames.end))
}

impl OpusMSEncoder {
    /// Cap the audio bandwidth of every stream at once.
    pub fn set_max_bandwidth(&mut self, bw: Bandwidth) {
        for e in &mut self.encoders {
            e.max_bandwidth = bw;
        }
    }
}


#[cfg(test)]
mod multistream_encoder_preflight_tests {
    use super::OpusMSEncoder;
    use crate::{Application, Error};

    fn encoder(channels: usize, family: i32) -> OpusMSEncoder {
        OpusMSEncoder::new(48000, channels, family, Application::RestrictedLowDelay).unwrap()
    }

    fn pcm(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| ((i * 17 % 97) as f32 + 1.0) / 100.0)
            .collect()
    }

    fn assert_input_too_small(result: Result<Vec<u8>, Error>) {
        assert!(matches!(result, Err(Error::BadArg("input buffer too small"))));
    }

    #[test]
    fn family_zero_mono_and_stereo_reject_empty_and_short_input() {
        for channels in [1, 2] {
            for input_len in [0, 120 * channels - 1] {
                let mut enc = encoder(channels, 0);
                assert_input_too_small(enc.encode(&pcm(input_len), 120));
            }
        }
    }

    #[test]
    fn family_one_three_channel_mapping_rejects_short_input() {
        let mut enc = encoder(3, 1);
        assert_input_too_small(enc.encode(&pcm(359), 120));
    }

    #[test]
    fn family_one_quad_rejects_before_any_stream_analysis_or_encoding() {
        let mut after_rejection = encoder(4, 1);
        let mut fresh = encoder(4, 1);
        assert_input_too_small(after_rejection.encode(&pcm(479), 120));
        assert!(after_rejection
            .encoders
            .iter()
            .all(|encoder| !encoder.tonality.initialized()));

        let full = pcm(480);
        let after_packet = after_rejection.encode(&full, 120).unwrap();
        let fresh_packet = fresh.encode(&full, 120).unwrap();
        assert_eq!(after_packet, fresh_packet);
    }

    #[test]
    fn family_one_eight_channel_layout_rejects_short_input() {
        let mut enc = encoder(8, 1);
        assert_input_too_small(enc.encode(&pcm(120 * 8 - 1), 120));
    }

    #[test]
    fn invalid_frame_sizes_precede_empty_input_validation() {
        let mut enc = encoder(8, 1);
        for frame_size in [0, 48, usize::MAX] {
            assert!(matches!(
                enc.encode(&[], frame_size),
                Err(Error::BadArg("Invalid frame size for sampling rate"))
            ));
        }
    }

    #[test]
    fn family_one_three_channel_encoding_ignores_trailing_samples() {
        let prefix = pcm(360);
        let mut with_trailing = prefix.clone();
        with_trailing.extend(std::iter::repeat(0.91f32).take(11));

        let mut exact_encoder = encoder(3, 1);
        let mut trailing_encoder = encoder(3, 1);
        let exact_packet = exact_encoder.encode(&prefix, 120).unwrap();
        let trailing_packet = trailing_encoder.encode(&with_trailing, 120).unwrap();
        assert!(!exact_packet.is_empty());
        assert_eq!(exact_packet, trailing_packet);
    }
}
