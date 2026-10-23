//! Cancellable-in-time process activation. Parameters stay alive in the COM callback.
use crate::Result;
use ::windows::{
    core::{implement, IUnknown, Interface, Ref, HRESULT},
    Win32::{
        Media::Audio::*,
        System::{
            Com::{
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
                BLOB,
            },
            Variant::VT_BLOB,
        },
    },
};
use std::{
    mem::{size_of, ManuallyDrop},
    sync::{mpsc, Arc},
    time::Duration,
};

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion {
    complete: mpsc::SyncSender<()>,
    params: Arc<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> ::windows::core::Result<()> {
        let _ = &self.params; // Retain backing memory until the async activation finishes.
        let _ = self.complete.try_send(());
        Ok(())
    }
}

pub fn process_client(pid: u32) -> Result<wasapi::AudioClient> {
    let params = Arc::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    });
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: Arc::as_ptr(&params).cast_mut().cast(),
                    },
                },
            }),
        },
    });
    let (tx, rx) = mpsc::sync_channel(1);
    let callback: IActivateAudioInterfaceCompletionHandler = Completion {
        complete: tx,
        params,
    }
    .into();
    // SAFETY: PROPVARIANT references the correctly sized activation params. Windows reads these
    // parameters; the callback retains their allocation until asynchronous completion.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop),
            &callback,
        )?
    };
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "Windows process-loopback activation timed out")?;
    let mut result = HRESULT(0);
    let mut interface: Option<IUnknown> = None;
    // SAFETY: completion has signalled and outputs are valid writable references.
    unsafe {
        operation.GetActivateResult(&mut result, &mut interface)?;
    }
    result.ok()?;
    let client: IAudioClient = interface
        .ok_or("Windows returned no audio client")?
        .cast()?;
    Ok(wasapi::AudioClient::from_iaudioclient(
        client,
        wasapi::Direction::Render,
    ))
}
