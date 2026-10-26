use super::*;
use crate::{
    events::EventData,
    input::{
        codecs::{get_codec_registry, get_probe},
        RawAdapter,
    },
    tracks::Track,
    Event,
    EventContext,
    EventHandler,
};
use std::{io::Cursor, time::Duration};
use uuid::Uuid;

struct Snapshot(Sender<Vec<(Uuid, f32)>>);

#[async_trait::async_trait]
impl EventHandler for Snapshot {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = ctx {
            self.0
                .send(
                    tracks
                        .iter()
                        .map(|(state, handle)| (handle.uuid(), state.volume))
                        .collect(),
                )
                .unwrap();
        }
        None
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_removals_preserve_event_identity() {
    let (mut mixer, listeners) = Mixer::mock(Handle::current(), false);
    let event_task = tokio::spawn(super::super::events::runner(listeners.1.clone()));
    let (tx, rx) = flume::unbounded();
    mixer
        .fire_event(EventMessage::AddGlobalEvent(EventData::new(
            Event::Periodic(TIMESTEP_LENGTH, None),
            Snapshot(tx),
        )))
        .unwrap();

    for _ in 0..4 {
        let (_, context) = Track::from(Vec::<u8>::new()).pause().into_context();
        mixer.add_track(context).unwrap();
    }
    // Natural EOF changes the mixer state before the event task is notified.
    mixer.tracks[0].playing = PlayMode::End;
    mixer.tracks[1].playing = PlayMode::End;
    mixer.audio_commands_events().unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx.recv_async())
        .await
        .unwrap()
        .unwrap();

    let expected: Vec<_> = mixer
        .track_handles
        .iter()
        .enumerate()
        .map(|(i, handle)| {
            let volume = 0.25 * (i + 1) as f32;
            handle.set_volume(volume).unwrap();
            (handle.uuid(), volume)
        })
        .collect();
    mixer.audio_commands_events().unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(2), rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    mixer.fire_event(EventMessage::Poison).unwrap();
    event_task.await.unwrap();
    assert_eq!(
        observed, expected,
        "event states must still belong to the correct handles"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn muted_ticks_do_not_accumulate_pcm() {
    let (mut mixer, _listeners) = Mixer::mock(Handle::current(), false);
    let (output, _rx) = flume::unbounded();
    Arc::make_mut(&mut mixer.config).override_connection = Some(OutputMode::Raw(output));
    let pcm: Vec<_> = (0..STEREO_FRAME_SIZE * 16)
        .flat_map(|_| 0.125f32.to_le_bytes())
        .collect();
    let input: Input = RawAdapter::new(Cursor::new(pcm), SAMPLE_RATE, 2).into();
    let Input::Live(live, _) = input else {
        unreachable!()
    };
    let input = live.promote(get_codec_registry(), get_probe()).unwrap();
    let (_, context) = Track::from(Input::Live(input, None)).into_context();
    mixer.add_track(context).unwrap();
    let mut packet = [0; VOICE_PACKET_MAX];
    mixer.mix_and_build_packet(&mut packet).unwrap();
    mixer.muted = true;
    // Cover both the five silence packets and the no-packet early return.
    for _ in 0..8 {
        mixer.mix_and_build_packet(&mut packet).unwrap();
    }
    mixer.muted = false;
    mixer.mix_and_build_packet(&mut packet).unwrap();
    let Some(OutputMessage::Mixed(samples)) = &mixer.raw_msg else {
        panic!("expected PCM")
    };
    assert!(samples.iter().all(|sample| (*sample - 0.125).abs() < 1e-6));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_passthrough_returns_error_without_panicking() {
    let (mut mixer, _listeners) = Mixer::mock(Handle::current(), false);
    mixer
        .conn_active
        .as_ref()
        .unwrap()
        .dave_protocol_version
        .store(0, Ordering::Relaxed);
    let mut packet = [0; VOICE_PACKET_MAX];
    let max_audio = VOICE_PACKET_MAX
        - RtpPacket::minimum_packet_size()
        - mixer.crypto_mode().payload_suffix_len();
    assert!(mixer
        .prep_packet(MixType::Passthrough(max_audio + 1), &mut packet)
        .is_err());
    assert_eq!(
        mixer
            .prep_packet(MixType::Passthrough(max_audio), &mut packet)
            .unwrap(),
        VOICE_PACKET_MAX
    );
}

#[test]
fn dave_ciphertext_leaves_transport_suffix_intact() {
    let mut payload = [0xa5; 1448];
    let ciphertext = vec![7; 1428];
    assert_eq!(
        copy_dave_frame(&mut payload, 0, 20, &ciphertext).unwrap(),
        1428
    );
    assert_eq!(&payload[1428..], &[0xa5; 20]);
    let before = payload;
    assert!(copy_dave_frame(&mut payload, 0, 20, &vec![7; 1429]).is_err());
    assert_eq!(payload, before, "reject overflow before writing any bytes");
}

#[test]
fn opus_toc_does_not_bypass_passthrough_size_limit() {
    use crate::input::LiveInput;
    let metadata = serde_json::json!({
        "dca": { "version": 1, "tool": { "name": "regression", "version": "1" } },
        "opus": { "mode": "audio", "sample_rate": 48000, "frame_size": 960, "vbr": false, "channels": 2 }
    }).to_string();
    let mut dca = b"DCA1".to_vec();
    dca.extend((metadata.len() as u32).to_le_bytes());
    dca.extend(metadata.as_bytes());
    let mut opus = vec![0; 1430];
    opus[..3].copy_from_slice(&SILENT_FRAME);
    assert_eq!(
        opus2::packet::get_nb_samples(&opus, SAMPLE_RATE).unwrap(),
        960
    );
    dca.extend((opus.len() as u16).to_le_bytes());
    dca.extend(opus);
    let Input::Live(live, _) = Input::from(dca) else {
        unreachable!()
    };
    let LiveInput::Parsed(mut input) = live.promote(get_codec_registry(), get_probe()).unwrap()
    else {
        unreachable!()
    };
    let mut output =
        AudioBuffer::<f32>::new(960, SignalSpec::new_with_layout(48_000, Layout::Stereo));
    output.render_reserved(Some(960));
    let mut scratch = AudioBuffer::<f32>::new(960, *output.spec());
    let mut state = DecodeState::default();
    let mut slot = [0; 1428];
    let (kind, _) = mix_logic::mix_symph_indiv(
        &mut output,
        &mut scratch,
        &mut input,
        &mut state,
        1.0,
        Some(&mut slot),
    );
    assert!(matches!(kind, MixType::MixedPcm(_)));
    assert!(state.passthrough == Passthrough::Block);
}

fn pcm_wav(rate: u32, channels: u16) -> Vec<u8> {
    let samples = rate / 5;
    let data_size = samples * u32::from(channels) * 2;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + data_size).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend((channels * 2).to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_size.to_le_bytes());
    for _ in 0..samples {
        for channel in 0..channels {
            // Only the last channel has signal: a stereo clamp must not
            // silently drop it when mixing multichannel audio down to mono.
            let value: i16 = if channel + 1 == channels { 8192 } else { 0 };
            bytes.extend(value.to_le_bytes());
        }
    }
    bytes
}

#[test]
fn high_rate_multichannel_sources_play_instead_of_ending_or_losing_channels() {
    use crate::input::LiveInput;
    for rate in [44_100, 88_200, 96_000, 176_400, 192_000] {
        for channels in [1, 2, 6] {
            let Input::Live(live, _) = Input::from(pcm_wav(rate, channels)) else {
                unreachable!()
            };
            let LiveInput::Parsed(mut input) =
                live.promote(get_codec_registry(), get_probe()).unwrap()
            else {
                unreachable!()
            };
            let mut state = DecodeState::default();
            let mut output =
                AudioBuffer::<f32>::new(960, SignalSpec::new_with_layout(48_000, Layout::Mono));
            let mut scratch =
                AudioBuffer::<f32>::new(960, SignalSpec::new_with_layout(48_000, Layout::Stereo));
            for _ in 0..3 {
                output.clear();
                output.render_reserved(Some(960));
                output.chan_mut(0).fill(0.0);
                let (kind, status) = mix_logic::mix_symph_indiv(
                    &mut output,
                    &mut scratch,
                    &mut input,
                    &mut state,
                    1.0,
                    None,
                );
                assert_eq!(
                    kind,
                    MixType::MixedPcm(960),
                    "rate={rate}, channels={channels}"
                );
                assert!(
                    matches!(status, MixStatus::Live),
                    "source ended early at {rate} Hz"
                );
            }
            let mean = output.chan(0).iter().sum::<f32>() / 960.0;
            let expected = 0.25 / f32::from(channels);
            assert!(
                (mean - expected).abs() < 0.005,
                "rate={rate}, channels={channels}: {mean} != {expected}"
            );
        }
    }
}
