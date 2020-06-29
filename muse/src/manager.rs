use crate::sampler::{Sample, Sampler};
use cpal::{
    traits::{EventLoopTrait, HostTrait},
    Sample as CpalSample,
};
use crossbeam::{
    channel::{unbounded, Receiver, RecvTimeoutError, Sender},
    sync::ShardedLock,
};
use std::{sync::Arc, time::Duration};

pub(crate) enum ManagerMessage {
    Append {
        sampler: Box<dyn Sampler>,
        callback: Sender<PlayingHandle>,
    },
}

pub type ManagerHandle = Arc<ShardedLock<Manager>>;

#[derive(Clone, Debug)]
pub struct PlayingHandle(Arc<u64>);

#[derive(Debug)]
struct PlayingSound {
    handle: PlayingHandle,
    sampler: Box<dyn Sampler>,
}

#[derive(Debug)]
pub struct Manager {
    playing_sounds: Vec<PlayingSound>,
    last_playing_sound_id: u64,
    pub(crate) sender: Sender<ManagerMessage>,
    stream: cpal::StreamId,
}

impl Manager {
    pub(crate) fn open_output_device(
        host: cpal::Host,
        output_device: cpal::Device,
        format: cpal::Format,
    ) -> Result<ManagerHandle, anyhow::Error> {
        let event_loop = host.event_loop();
        let output_stream_id = event_loop.build_output_stream(&output_device, &format)?;
        event_loop.play_stream(output_stream_id.clone())?;

        let (sender, receiver) = unbounded();

        let manager = Arc::new(ShardedLock::new(Manager::new(sender, output_stream_id)));

        let manager_for_thread = manager.clone();
        std::thread::Builder::new()
            .name("muse::manager".to_owned())
            .spawn(move || {
                ManagerThread::new(manager_for_thread, receiver)
                    .main()
                    .unwrap_or_default()
            })?;

        let manager_for_thread = manager.clone();
        std::thread::Builder::new()
            .name("muse::cpal".to_owned())
            .spawn(move || CpalThread::run(manager_for_thread, event_loop, format))?;

        Ok(manager)
    }

    fn new(sender: Sender<ManagerMessage>, stream: cpal::StreamId) -> Self {
        Self {
            sender,
            stream,
            playing_sounds: Vec::new(),
            last_playing_sound_id: 0,
        }
    }
}

struct ManagerThread {
    receiver: Receiver<ManagerMessage>,
    manager: Arc<ShardedLock<Manager>>,
}

impl ManagerThread {
    fn new(manager: Arc<ShardedLock<Manager>>, receiver: Receiver<ManagerMessage>) -> Self {
        Self { manager, receiver }
    }

    fn main(&mut self) -> Result<(), RecvTimeoutError> {
        loop {
            // Check for new messages
            if let Err(err) = self.handle_incoming_messages() {
                if let RecvTimeoutError::Timeout = err {
                    continue;
                } else {
                    return Err(err);
                }
            }

            // TODO remove sounds that are finished playing
        }
    }

    fn handle_incoming_messages(&mut self) -> Result<(), RecvTimeoutError> {
        match self.receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(ManagerMessage::Append { sampler, callback }) => {
                let handle = {
                    // Scope this write so that sending the handle across the callback doesn't happen while the lock is still held
                    let mut manager = self
                        .manager
                        .write()
                        .expect("Error locking manager to add sampler");
                    manager.last_playing_sound_id = manager.last_playing_sound_id.wrapping_add(1);

                    let handle = PlayingHandle(Arc::new(manager.last_playing_sound_id));
                    manager.playing_sounds.push(PlayingSound {
                        handle: handle.clone(),
                        sampler,
                    });
                    handle
                };

                callback.send(handle).unwrap_or_default();

                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

struct CpalThread {}

impl CpalThread {
    fn run(
        manager: Arc<ShardedLock<Manager>>,
        event_loop: cpal::EventLoop,
        format: cpal::Format,
    ) -> ! {
        event_loop.run(move |id, result| {
            let data = match result {
                Ok(data) => data,
                Err(err) => {
                    eprintln!("an error occurred on stream {:?}: {}", id, err);
                    return;
                }
            };

            match data {
                cpal::StreamData::Output {
                    buffer: cpal::UnknownTypeOutputBuffer::U16(buffer),
                } => {
                    Self::copy_samples(&manager, buffer, &format);
                }
                cpal::StreamData::Output {
                    buffer: cpal::UnknownTypeOutputBuffer::I16(buffer),
                } => {
                    Self::copy_samples(&manager, buffer, &format);
                }
                cpal::StreamData::Output {
                    buffer: cpal::UnknownTypeOutputBuffer::F32(buffer),
                } => {
                    Self::copy_samples(&manager, buffer, &format);
                }
                _ => (),
            }
        });
    }

    fn next_sample(manager: &ManagerHandle, format: &cpal::Format) -> Sample {
        let mut manager = manager.write().expect("Error locking manager for sampling");
        let mut combined_sample = Sample::default();
        for sample in manager
            .playing_sounds
            .iter_mut()
            .filter_map(|s| s.sampler.sample(format.sample_rate.0))
        {
            combined_sample += sample;
        }
        combined_sample
    }

    fn copy_samples<S>(
        manager: &ManagerHandle,
        mut buffer: cpal::OutputBuffer<S>,
        format: &cpal::Format,
    ) where
        S: CpalSample,
    {
        for sample in buffer.chunks_mut(format.channels as usize) {
            let generated_sample = Self::next_sample(manager, format);

            match format.channels {
                1 => {
                    sample[0] = cpal::Sample::from(
                        &((generated_sample.left + generated_sample.right) / 2.0),
                    )
                }
                2 => {
                    sample[0] = cpal::Sample::from(&generated_sample.left);
                    sample[1] = cpal::Sample::from(&generated_sample.right);
                }
                _ => panic!("Unsupported number of channels {}", format.channels),
            }
        }
    }
}
