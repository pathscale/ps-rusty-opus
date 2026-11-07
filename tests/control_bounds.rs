use rusty_opus::{Application, OpusEncoder};

#[test]
fn invalid_controls_leave_output_and_encoder_usable() {
    for invalid in [0, 1, 2] {
        let mut encoder = OpusEncoder::new(48000, 1, Application::RestrictedLowDelay).unwrap();
        let mut clean = OpusEncoder::new(48000, 1, Application::RestrictedLowDelay).unwrap();
        let complexity = encoder.complexity;
        let bitrate = encoder.bitrate_bps;
        match invalid {
            0 => encoder.complexity = -1,
            1 => encoder.complexity = 11,
            _ => encoder.bitrate_bps = -1,
        }
        let pcm = [0.0; 960];
        let mut output = [0xA5; 1276];
        assert!(encoder.encode(&pcm, 960, &mut output).is_err());
        assert_eq!(output, [0xA5; 1276]);
        encoder.complexity = complexity;
        encoder.bitrate_bps = bitrate;
        let mut expected = [0; 1276];
        let actual_len = encoder.encode(&pcm, 960, &mut output).unwrap();
        let expected_len = clean.encode(&pcm, 960, &mut expected).unwrap();
        assert_eq!(&output[..actual_len], &expected[..expected_len]);
    }
}

#[test]
fn extreme_loss_matches_clamped_loss() {
    for (raw, clamped) in [(i32::MIN, 0), (i32::MAX, 100)] {
        let mut encoder = OpusEncoder::new(48000, 1, Application::RestrictedLowDelay).unwrap();
        let mut reference = OpusEncoder::new(48000, 1, Application::RestrictedLowDelay).unwrap();
        encoder.packet_loss_perc = raw;
        reference.packet_loss_perc = clamped;
        let pcm = [0.0; 960];
        let mut actual = [0; 1276];
        let mut expected = [0; 1276];
        for _ in 0..2 {
            let a = encoder.encode(&pcm, 960, &mut actual).unwrap();
            let b = reference.encode(&pcm, 960, &mut expected).unwrap();
            assert_eq!(&actual[..a], &expected[..b]);
        }
    }
}
