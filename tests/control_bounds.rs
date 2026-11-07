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

#[test]
fn extreme_positive_audio_rate_is_rejected_without_state_or_output_changes() {
    let mut encoder = OpusEncoder::new(8000, 1, Application::Audio).unwrap();
    let mut fresh = OpusEncoder::new(8000, 1, Application::Audio).unwrap();
    encoder.bitrate_bps = i32::MAX;
    let pcm = [0.0; 160];
    let mut output = [0xA5; 1276];

    assert!(matches!(
        encoder.encode(&pcm, 160, &mut output),
        Err(rusty_opus::Error::BadArg(
            "SILK packet rate budget is not representable"
        ))
    ));
    assert_eq!(output, [0xA5; 1276]);

    encoder.bitrate_bps = fresh.bitrate_bps;
    let mut actual = [0; 1276];
    let mut expected = [0; 1276];
    let actual_len = encoder.encode(&pcm, 160, &mut actual).unwrap();
    let expected_len = fresh.encode(&pcm, 160, &mut expected).unwrap();
    assert_eq!(&actual[..actual_len], &expected[..expected_len]);
}

#[test]
fn audio_silk_ignores_pcm_after_the_validated_frame() {
    let mut exact_encoder = OpusEncoder::new(8000, 1, Application::Audio).unwrap();
    let mut trailing_encoder = OpusEncoder::new(8000, 1, Application::Audio).unwrap();
    let pcm = [0.125; 160];
    let mut trailing_pcm = [0.125; 161];
    trailing_pcm[160] = -1.0;
    let mut exact_output = [0; 1276];
    let mut trailing_output = [0; 1276];

    let exact_len = exact_encoder.encode(&pcm, 160, &mut exact_output).unwrap();
    let trailing_len = trailing_encoder
        .encode(&trailing_pcm, 160, &mut trailing_output)
        .unwrap();
    assert_eq!(trailing_len, exact_len);
    assert_eq!(&trailing_output[..trailing_len], &exact_output[..exact_len]);
}
