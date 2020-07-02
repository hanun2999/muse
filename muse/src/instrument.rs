use crate::{
    envelope::PlayingState,
    instrument::loaded::Instantiatable,
    manager::{Device, PlayingHandle},
    note::Note,
    sampler::PreparedSampler,
};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

pub mod loaded;

#[cfg(feature = "serialization")]
pub mod serialization;

pub struct GeneratedTone<T> {
    pub source: T,
    pub control: Arc<RwLock<PlayingState>>,
}

pub type ControlHandles = Vec<Arc<RwLock<PlayingState>>>;

#[derive(Debug)]
pub struct InstrumentController<T> {
    pub control_handles: ControlHandles,
    _tone_generator: std::marker::PhantomData<T>,
}

impl<T> Default for InstrumentController<T> {
    fn default() -> Self {
        Self {
            control_handles: ControlHandles::new(),
            _tone_generator: std::marker::PhantomData::default(),
        }
    }
}

impl<T> InstrumentController<T>
where
    T: ToneGenerator,
    T::CustomNodes: Instantiatable,
{
    pub fn instantiate(
        &mut self,
        sampler: &loaded::LoadedInstrument<T::CustomNodes>,
        note: Note,
    ) -> Result<PreparedSampler, serialization::Error> {
        Ok(sampler.instantiate(&note, &mut self.control_handles))
    }
}

pub trait ToneGenerator: Sized {
    type CustomNodes;

    fn generate_tone(
        &mut self,
        note: Note,
        control: &mut InstrumentController<Self>,
    ) -> Result<PreparedSampler, anyhow::Error>;
}

pub struct PlayingNote<T> {
    note: Note,
    handle: Option<PlayingHandle>,
    controller: InstrumentController<T>,
}

impl<T> PlayingNote<T> {
    fn is_playing(&self) -> bool {
        for control in &self.controller.control_handles {
            let value = control.read().unwrap();
            if let PlayingState::Playing = *value {
                return true;
            }
        }

        false
    }

    fn stop(&self) {
        for control in &self.controller.control_handles {
            let mut value = control.write().unwrap();
            *value = PlayingState::Stopping;
        }
    }

    fn sustain(&self) {
        for control in &self.controller.control_handles {
            let mut value = control.write().unwrap();
            *value = PlayingState::Sustaining;
        }
    }
}

impl<T> Drop for PlayingNote<T> {
    fn drop(&mut self) {
        self.stop();

        let handle = std::mem::take(&mut self.handle);
        let control_handles = std::mem::take(&mut self.controller.control_handles);

        std::thread::spawn(move || loop {
            {
                let all_stopped = control_handles
                    .iter()
                    .map(|control| {
                        let value = control.read().unwrap();
                        *value
                    })
                    .all(|state| state == PlayingState::Stopped);
                if all_stopped {
                    println!("Sound stopping");
                    drop(handle);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        });
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Loudness {
    Fortissimo,
    MezzoForte,
    Pianissimo,
}

pub struct VirtualInstrument<T> {
    playing_notes: Vec<PlayingNote<T>>,
    device: Device,
    sustain: bool,
    tone_generator: T,
}

impl<T> VirtualInstrument<T>
where
    T: ToneGenerator,
{
    pub fn new(device: Device, tone_generator: T) -> Self {
        Self {
            device,
            tone_generator,
            playing_notes: Vec::new(),
            sustain: false,
        }
    }

    pub fn new_with_default_output(tone_generator: T) -> Result<Self, anyhow::Error> {
        let device = Device::default_output()?;
        Ok(Self::new(device, tone_generator))
    }

    pub fn play_note(&mut self, note: Note) -> Result<(), anyhow::Error> {
        // We need to re-tone the note, so we'll get rid of the existing notes
        self.playing_notes.retain(|n| n.note.step != note.step);

        let mut controller = InstrumentController::default();
        let source = self.tone_generator.generate_tone(note, &mut controller)?;
        let handle = Some(self.device.play(source, note)?);

        self.playing_notes.push(PlayingNote {
            note,
            handle,
            controller,
        });

        Ok(())
    }

    pub fn stop_note(&mut self, step: u8) {
        if self.sustain {
            // For sustain, we need ot keep the notes playing, but mark that the key isn't pressed
            // so that when the pedal is released, the note isn't filtered out.
            if let Some(existing_note) = self
                .playing_notes
                .iter_mut()
                .find(|pn| pn.note.step == step)
            {
                existing_note.sustain();
            }
        } else {
            self.playing_notes.retain(|pn| pn.note.step != step);
        }
    }

    pub fn set_sustain(&mut self, active: bool) {
        self.sustain = active;

        if !active {
            self.playing_notes.retain(|n| n.is_playing());
        }
    }
}
