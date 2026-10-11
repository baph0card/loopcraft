//! Standard MIDI File import and export.

use anyhow::{Result, bail};
use loopcraft_core::{Beats, Clip, Note};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

/// One imported MIDI track.
pub struct ImportedTrack {
    pub name: String,
    pub clip: Clip,
}

pub struct ImportedSong {
    /// First tempo found in the file, if any.
    pub tempo: Option<f64>,
    pub tracks: Vec<ImportedTrack>,
}

/// Parse a `.mid` file into clips (one per track that contains notes).
/// Clip length is rounded up to whole bars of 4/4.
pub fn import_midi(bytes: &[u8]) -> Result<ImportedSong> {
    let smf = Smf::parse(bytes)?;
    let tpb = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(..) => bail!("SMPTE-timed MIDI files are not supported yet"),
    };
    let mut tempo = None;
    let mut tracks = Vec::new();
    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut tick: u64 = 0;
        let mut name = format!("MIDI {}", ti + 1);
        let mut held: [[Option<(u64, u8)>; 128]; 16] = [[None; 128]; 16];
        let mut notes = Vec::new();
        for ev in track {
            tick += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) => name = String::from_utf8_lossy(n).trim().to_string(),
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if tempo.is_none() => tempo = Some(60_000_000.0 / us.as_int() as f64),
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int() as usize;
                    let (key, on, vel) = match message {
                        MidiMessage::NoteOn { key, vel } => (key.as_int(), vel.as_int() > 0, vel.as_int()),
                        MidiMessage::NoteOff { key, .. } => (key.as_int(), false, 0),
                        _ => continue,
                    };
                    let k = key as usize;
                    if on {
                        held[ch][k] = Some((tick, vel));
                    } else if let Some((start, v)) = held[ch][k].take() {
                        notes.push(Note::new(key, start as f64 / tpb, ((tick - start) as f64 / tpb).max(1.0 / 64.0), v));
                    }
                }
                _ => {}
            }
        }
        if notes.is_empty() {
            continue;
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let end = notes.iter().map(|n| n.start + n.length).fold(0.0, f64::max);
        let length = ((end / 4.0).ceil() * 4.0).max(4.0);
        tracks.push(ImportedTrack { clip: Clip::midi(name.clone(), length, notes), name });
    }
    Ok(ImportedSong { tempo, tracks })
}

/// Write a clip's notes as a single-track MIDI file.
pub fn export_midi(notes: &[Note], tempo: f64, name: &str) -> Result<Vec<u8>> {
    const TPB: u16 = 480;
    let to_ticks = |b: Beats| (b * TPB as f64).round().max(0.0) as u64;
    let mut abs: Vec<(u64, bool, u8, u8)> = Vec::with_capacity(notes.len() * 2);
    for n in notes {
        abs.push((to_ticks(n.start), true, n.pitch.min(127), n.velocity.clamp(1, 127)));
        abs.push((to_ticks(n.start + n.length), false, n.pitch.min(127), 0));
    }
    // Offs before ons at equal times.
    abs.sort_by_key(|e| (e.0, e.1));

    let mut events: Vec<TrackEvent> = Vec::with_capacity(abs.len() + 3);
    events.push(TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::TrackName(name.as_bytes())) });
    let us_per_beat = (60_000_000.0 / tempo.max(1.0)).round() as u32;
    events.push(TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_beat))) });
    let mut last = 0u64;
    for (t, on, key, vel) in abs {
        let delta = u28::new((t - last) as u32);
        last = t;
        let message =
            if on { MidiMessage::NoteOn { key: u7::new(key), vel: u7::new(vel) } } else { MidiMessage::NoteOff { key: u7::new(key), vel: u7::new(0) } };
        events.push(TrackEvent { delta, kind: TrackEventKind::Midi { channel: u4::new(0), message } });
    }
    events.push(TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) });

    let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(TPB))));
    smf.tracks.push(events);
    let mut out = Vec::new();
    smf.write_std(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let notes = vec![Note::new(60, 0.0, 1.0, 100), Note::new(64, 1.5, 0.5, 80), Note::new(67, 3.0, 2.0, 127)];
        let bytes = export_midi(&notes, 128.0, "Chords").unwrap();
        let song = import_midi(&bytes).unwrap();
        assert!((song.tempo.unwrap() - 128.0).abs() < 0.01);
        assert_eq!(song.tracks.len(), 1);
        assert_eq!(song.tracks[0].name, "Chords");
        let back = match &song.tracks[0].clip.content {
            loopcraft_core::ClipContent::Midi { notes } => notes.clone(),
            _ => panic!(),
        };
        assert_eq!(back.len(), 3);
        for (a, b) in notes.iter().zip(&back) {
            assert_eq!(a.pitch, b.pitch);
            assert!((a.start - b.start).abs() < 1e-3);
            assert!((a.length - b.length).abs() < 1e-3);
        }
        assert_eq!(song.tracks[0].clip.length, 8.0);
    }
}
