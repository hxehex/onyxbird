use super::*;
use crate::{
    driver::{
        tasks::error::{Error, Result},
        Channels,
        DecodeConfig,
        DecodeMode,
    },
    events::context_data::{RtpData, VoiceData},
};
use discortp::{rtp::RtpExtensionPacket, Packet, PacketSize};
use opus2::{Decoder as OpusDecoder, ErrorCode};

#[derive(Debug)]
pub struct SsrcState {
    playout_buffer: PlayoutBuffer,
    crypto_mode: CryptoMode,
    decoder: OpusDecoder,
    decode_size: PacketDecodeSize,
    pub(crate) prune_time: Instant,
    pub(crate) disconnected: bool,
    channels: Channels,
    sample_rate: u32,
}

impl SsrcState {
    pub fn new(pkt: &RtpPacket<'_>, crypto_mode: CryptoMode, config: &Config) -> Self {
        let playout_capacity = usize::from(config.playout_buffer_length.get())
            + usize::from(config.playout_spike_length);
        let (sample_rate, channels) = match config.decode_mode {
            DecodeMode::Decode(config) => (config.sample_rate, config.channels),
            DecodeMode::Decrypt | DecodeMode::Pass => Default::default(),
        };

        Self {
            playout_buffer: PlayoutBuffer::new(playout_capacity, pkt.get_sequence().0),
            crypto_mode,
            decoder: OpusDecoder::new(sample_rate.into(), channels.into())
                .expect("Failed to create new Opus decoder for source."),
            decode_size: PacketDecodeSize::TwentyMillis,
            prune_time: Instant::now() + config.decode_state_timeout.into(),
            disconnected: false,
            channels,
            sample_rate: sample_rate.into(),
        }
    }

    pub fn reconfigure_decoder(&mut self, config: DecodeConfig) {
        self.decoder = OpusDecoder::new(config.sample_rate.into(), config.channels.into())
            .expect("Failed to create new Opus decoder for source.");

        self.channels = config.channels;
        self.sample_rate = config.sample_rate.into();
        self.decode_size = PacketDecodeSize::TwentyMillis;
    }

    pub fn store_packet(&mut self, packet: StoredPacket, config: &Config) {
        self.playout_buffer.store_packet(packet, config);
    }

    pub fn refresh_timer(&mut self, state_timeout: Duration) {
        if !self.disconnected {
            self.prune_time = Instant::now() + state_timeout;
        }
    }

    pub fn get_voice_tick(&mut self, config: &Config) -> Result<Option<VoiceData>> {
        // Acquire a packet from the playout buffer:
        // Update nexts, lasts...
        // different cases: null packet who we want to decode as a miss, and packet who we must ignore temporarily.
        let expected_seq = self.playout_buffer.next_seq();
        let m_pkt = self.playout_buffer.fetch_packet(config);
        let pkt = match m_pkt {
            PacketLookup::Packet(StoredPacket { packet, decrypted }) => Some((packet, decrypted)),
            PacketLookup::MissedPacket => None,
            PacketLookup::Filling => return Ok(None),
        };

        let mut out = VoiceData {
            packet: None,
            decoded_voice: None,
        };

        let should_decode = config.decode_mode.should_decode();
        if let Some((packet, decrypted)) = pkt {
            // `rtp_valid` only checks the version and payload type, so a truncated or otherwise
            // malformed packet can still reach here. Never panic on adversarial rx'd data: treat
            // an unparseable packet as a silent tick instead of killing the receive task.
            let Some(rtp) = RtpPacket::new(&packet) else {
                return Ok(None);
            };
            let extensions = rtp.get_extension() != 0;

            let payload = rtp.payload();
            let payload_offset = self.crypto_mode.payload_prefix_len();
            // Guard the suffix subtraction: `payload` may be shorter than the crypto suffix on a
            // malformed packet, which would otherwise underflow (panic in debug, wrap to an
            // out-of-bounds slice index in release at the `&payload[offset..end]` below).
            let mut payload_end = match payload
                .len()
                .checked_sub(self.crypto_mode.payload_suffix_len())
            {
                Some(end) if end >= payload_offset => end,
                _ => return Ok(None),
            };
            if decrypted && rtp.get_padding() != 0 {
                let count = payload_end
                    .checked_sub(1)
                    .and_then(|i| payload.get(i))
                    .copied()
                    .map_or(0, usize::from);
                if count == 0 || count > payload_end.saturating_sub(payload_offset) {
                    return Ok(None);
                }
                payload_end -= count;
            }

            // We still need to compute missed packets here in case of long loss chains or similar.
            // This occurs due to the fallback in 'store_packet' (i.e., empty buffer and massive seq difference).
            // Normal losses should be handled by the below `else` branch.
            let new_seq: u16 = rtp.get_sequence().into();
            // Wrap-safe gap computation, consistent with `store_packet`'s index math:
            // a plain `u16::saturating_sub` reports 0 missed frames for any loss
            // spanning the 16-bit sequence rollover, skipping packet-loss
            // concealment exactly at the wraparound point.
            let missed_packets = {
                // fetch_packet advances next_seq; compare with its value
                // before the fetch, not with seq + 1.
                let diff = (Wrapping(new_seq) - expected_seq).0;
                if diff >= 0x8000 {
                    0
                } else {
                    diff.min(64)
                }
            };

            // TODO: maybe hand over audio and extension indices alongside packet?
            let (audio, _packet_size) = self.scan_and_decode(
                &payload[payload_offset..payload_end],
                extensions,
                missed_packets,
                should_decode && decrypted,
            )?;

            let payload_end_pad = payload.len() - payload_end;
            let rtp_data = RtpData {
                packet,
                payload_offset,
                payload_end_pad,
            };

            out.packet = Some(rtp_data);
            out.decoded_voice = audio;
        } else if should_decode {
            let mut audio = vec![0; self.sample_rate as usize / 50 * self.channels.channels()];
            let dest_samples = &mut audio[..];
            let len = self.decoder.decode(&[], dest_samples, false)?;
            audio.truncate(self.channels.channels() * len);

            out.decoded_voice = Some(audio);
        }

        Ok(Some(out))
    }

    fn scan_and_decode(
        &mut self,
        data: &[u8],
        extension: bool,
        missed_packets: u16,
        decode: bool,
    ) -> Result<(Option<Vec<i16>>, usize)> {
        let start = if extension {
            let start = RtpExtensionPacket::new(data)
                .map(|pkt| pkt.packet_size())
                .ok_or(Error::IllegalVoicePacket)?;
            // `packet_size()` is derived from the *declared* word count in the
            // extension header, which can exceed the actual packet contents on
            // a malformed packet. Never let that value reach the slice below.
            if start > data.len() {
                return Err(Error::IllegalVoicePacket);
            }
            start
        } else {
            0
        };

        let pkt = if decode {
            let mut out = vec![0; self.decode_buffer_len()];

            for _ in 0..missed_packets {
                let dest_samples = &mut out[..];
                let _ = self.decoder.decode(&[], dest_samples, false);
            }

            loop {
                let slice_to_decode = &data[start..];

                let tried_audio_len = self.decoder.decode(slice_to_decode, &mut out, false);
                match tried_audio_len {
                    Ok(audio_len) => {
                        out.truncate(self.channels.channels() * audio_len);
                        break;
                    },
                    Err(e) if e.code() == ErrorCode::BufferTooSmall => {
                        if self.decode_size.can_bump_up() {
                            self.decode_size = self.decode_size.bump_up();
                            out = vec![0; self.decode_buffer_len()];
                        } else {
                            return Err(Error::IllegalVoicePacket);
                        }
                    },
                    Err(e) => {
                        return Err(e.into());
                    },
                }
            }

            Some(out)
        } else {
            None
        };

        Ok((pkt, data.len() - start))
    }

    fn decode_buffer_len(&self) -> usize {
        self.decode_size.len() * self.channels.channels() * self.sample_rate as usize
            / (2 * SAMPLE_RATE_RAW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::SampleRate;
    use bytes::Bytes;
    use discortp::{rtp::MutableRtpPacket, MutablePacket};

    fn packet(seq: u16) -> StoredPacket {
        let mut bytes = vec![0; 12 + SILENT_FRAME.len() + 20];
        let mut rtp = MutableRtpPacket::new(&mut bytes).unwrap();
        rtp.set_version(2);
        rtp.set_payload_type(RTP_PROFILE_TYPE);
        rtp.set_sequence(seq.into());
        rtp.set_timestamp((u32::from(seq) * 960).into());
        rtp.payload_mut()[..3].copy_from_slice(&SILENT_FRAME);
        StoredPacket {
            packet: Bytes::from(bytes),
            decrypted: true,
        }
    }

    #[test]
    fn mono_packet_loss_keeps_twenty_ms_frame_length() {
        let config = Config {
            decode_mode: DecodeMode::Decode(DecodeConfig::new(Channels::Mono, SampleRate::Hz48000)),
            playout_buffer_length: 1.try_into().unwrap(),
            ..Config::default()
        };
        let first = packet(0);
        let mut state = SsrcState::new(
            &RtpPacket::new(&first.packet).unwrap(),
            CryptoMode::Aes256Gcm,
            &config,
        );
        state.store_packet(first, &config);
        state.store_packet(packet(2), &config);
        for _ in 0..3 {
            let tick = state.get_voice_tick(&config).unwrap().unwrap();
            assert_eq!(tick.decoded_voice.unwrap().len(), 960);
        }
    }

    #[test]
    fn malformed_extensions_error_instead_of_panicking() {
        let pkt = packet(0);
        let config = Config::default();
        let mut state = SsrcState::new(
            &RtpPacket::new(&pkt.packet).unwrap(),
            CryptoMode::Aes256Gcm,
            &config,
        );
        for decode in [false, true] {
            assert!(state
                .scan_and_decode(&[0xbe, 0xde, 0xff, 0xff], true, 0, decode)
                .is_err());
        }
    }
}
