// Manos Pitsidianakis <manos.pitsidianakis@linaro.org>
// SPDX-License-Identifier: Apache-2.0 or BSD-3-Clause

#[cfg(feature = "pw-backend")]
mod pipewire;

use std::sync::{Arc, RwLock};

#[cfg(feature = "pw-backend")]
use self::pipewire::PwBackend;
use super::{stream::Stream, BackendType, Result, VirtioSndPcmSetParams};

pub trait AudioBackend {
    fn write(&self, stream_id: u32) -> Result<()>;

    #[allow(dead_code)]
    fn read(&self, stream_id: u32) -> Result<()>;

    fn set_parameters(&self, _stream_id: u32, _: VirtioSndPcmSetParams) -> Result<()> {
        Ok(())
    }

    fn prepare(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    fn release(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    fn start(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    fn stop(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Null audio backend that discards all audio data.
/// Used for testing and platforms without audio support.
pub struct NullBackend;

impl AudioBackend for NullBackend {
    fn write(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    fn read(&self, _stream_id: u32) -> Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn alloc_audio_backend(
    backend: BackendType,
    streams: Arc<RwLock<Vec<Stream>>>,
) -> Result<Box<dyn AudioBackend + Send + Sync>> {
    log::trace!("allocating audio backend {backend:?}");
    match backend {
        BackendType::Null => Ok(Box::new(NullBackend)),
        #[cfg(feature = "pw-backend")]
        BackendType::Pipewire => Ok(Box::new(PwBackend::new(streams))),
        #[cfg(not(feature = "pw-backend"))]
        BackendType::Pipewire => {
            log::warn!("Pipewire backend not available (pw-backend feature not enabled), using Null backend");
            Ok(Box::new(NullBackend))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use super::*;

    #[test]
    fn test_alloc_audio_backend() {
        crate::init_logger();
        {
            let v = BackendType::Null;
            let value = alloc_audio_backend(v, Default::default()).unwrap();
            assert_eq!(TypeId::of::<NullBackend>(), value.as_any().type_id());
        }
        #[cfg(feature = "pw-backend")]
        {
            use pipewire::{test_utils::PipewireTestHarness, *};

            let _test_harness = PipewireTestHarness::new();
            let v = BackendType::Pipewire;
            let value = alloc_audio_backend(v, Default::default()).unwrap();
            assert_eq!(TypeId::of::<PwBackend>(), value.as_any().type_id());
        }
        #[cfg(all(feature = "alsa-backend", target_env = "gnu"))]
        {
            let v = BackendType::Alsa;
            let value = alloc_audio_backend(v, Default::default()).unwrap();
            assert_eq!(TypeId::of::<AlsaBackend>(), value.as_any().type_id());
        }
    }
}
