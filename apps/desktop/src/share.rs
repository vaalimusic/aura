//! Windows Share UI integration for sharing a track's text metadata.
use std::sync::{Arc, Mutex};
use windows::{
    core::HSTRING,
    ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager},
    Foundation::TypedEventHandler,
    Win32::{
        Foundation::HWND,
        System::WinRT::{
            RoGetActivationFactory, RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED,
        },
        UI::Shell::IDataTransferManagerInterop,
    },
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

struct Apartment;
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}

#[derive(Clone)]
struct Payload {
    title: String,
    text: String,
}

pub struct ShareDialog {
    interop: IDataTransferManagerInterop,
    manager: DataTransferManager,
    token: i64,
    payload: Arc<Mutex<Payload>>,
    _apartment: Apartment,
}

impl ShareDialog {
    fn new(hwnd: HWND, title: &str, text: &str) -> windows::core::Result<Self> {
        let apartment = Apartment::new()?;
        let interop: IDataTransferManagerInterop = unsafe {
            RoGetActivationFactory(&HSTRING::from(
                "Windows.ApplicationModel.DataTransfer.DataTransferManager",
            ))?
        };
        let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };
        let payload = Arc::new(Mutex::new(Payload {
            title: title.into(),
            text: text.into(),
        }));
        let event_payload = payload.clone();
        let handler: TypedEventHandler<DataTransferManager, DataRequestedEventArgs> =
            TypedEventHandler::new(move |_, args: windows::core::Ref<DataRequestedEventArgs>| {
                let request = args.ok()?.Request()?;
                let data = request.Data()?;
                let payload = event_payload.lock().unwrap().clone();
                data.Properties()?.SetTitle(&HSTRING::from(payload.title))?;
                data.SetText(&HSTRING::from(payload.text))?;
                Ok(())
            });
        let token = manager.DataRequested(&handler)?;
        Ok(Self {
            interop,
            manager,
            token,
            payload,
            _apartment: apartment,
        })
    }

    fn show(&mut self, hwnd: HWND, title: &str, text: &str) -> windows::core::Result<()> {
        *self.payload.lock().unwrap() = Payload {
            title: title.into(),
            text: text.into(),
        };
        let _keep_manager_alive = &self.manager;
        unsafe { self.interop.ShowShareUIForWindow(hwnd) }
    }
}

impl Drop for ShareDialog {
    fn drop(&mut self) {
        let _ = self.manager.RemoveDataRequested(self.token);
    }
}

pub fn show(
    dialog: &mut Option<ShareDialog>,
    window: Option<&Window>,
    title: &str,
    text: &str,
) -> Result<(), String> {
    let window = window.ok_or_else(|| "Aura window is unavailable".to_owned())?;
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let hwnd = match handle.as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as *mut _),
        _ => return Err("Aura window is not a Win32 window".into()),
    };
    if dialog.is_none() {
        *dialog = Some(ShareDialog::new(hwnd, title, text).map_err(|error| error.to_string())?);
    }
    dialog
        .as_mut()
        .expect("share dialog created")
        .show(hwnd, title, text)
        .map_err(|error| error.to_string())
}
