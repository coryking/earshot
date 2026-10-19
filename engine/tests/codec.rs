use radio_engine::codec::Decoder;
#[test]
fn noise_and_silence_do_not_unmute_either_codec() {
    for protocol in ["DMR", "NXDN48"] {
        let mut decoder = Decoder::new(protocol, 1).unwrap();
        let mut seed = 7u32;
        let mut emitted = 0;
        for _ in 0..80 {
            let pcm: Vec<i16> = (0..4800)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 16) as i16
                })
                .collect();
            emitted += decoder.push(&pcm).0.len();
        }
        for _ in 0..20 {
            emitted += decoder.push(&[0; 4800]).0.len();
        }
        assert_eq!(
            emitted, 0,
            "{protocol} unmuted on deterministic noise or silence"
        );
    }
}
#[test]
#[ignore = "set RADIO_DMR_DISCRIMINATOR to offline mono48k signed16 fixture"]
fn actual_dmr_fixture_decodes_incrementally() {
    let path = std::env::var("RADIO_DMR_DISCRIMINATOR").expect("set fixture path");
    let bytes = std::fs::read(path).unwrap();
    let mut decoder = Decoder::new("DMR", 2).unwrap();
    let (mut chunks, mut samples) = (0, 0);
    for block in bytes.chunks(9600) {
        let pcm: Vec<_> = block
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let (audio, _) = decoder.push(&pcm);
        if !audio.is_empty() {
            chunks += 1;
            samples += audio.len();
        }
    }
    assert!(
        chunks > 10 && samples > 8000,
        "expected sustained incremental DMR audio, got {chunks} chunks/{samples} samples"
    );
}
