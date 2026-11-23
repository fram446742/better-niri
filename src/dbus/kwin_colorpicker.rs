use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;
use zbus::{fdo, interface};

use crate::dbus::Start;

pub enum KwinColorpickerToNiri {
    PickColor(async_channel::Sender<Option<niri_ipc::PickedColor>>),
}

pub struct KwinColorpicker {
    to_niri: calloop::channel::Sender<KwinColorpickerToNiri>,
}

impl KwinColorpicker {
    pub fn new(to_niri: calloop::channel::Sender<KwinColorpickerToNiri>) -> Self {
        Self { to_niri }
    }
}

#[interface(name = "org.kde.kwin.ColorPicker")]
impl KwinColorpicker {
    #[zbus(name = "pick")]
    async fn pick(&self) -> fdo::Result<u32> {
        let (tx, rx) = async_channel::bounded(1);

        self.to_niri
            .send(KwinColorpickerToNiri::PickColor(tx))
            .map_err(|e| fdo::Error::Failed(format!("failed to request color pick: {e:?}")))?;

        let Some(color) = rx.recv().await.map_err(|e| {
            fdo::Error::Failed(format!("failed to wait for colorpick response: {e:?}"))
        })?
        else {
            return Err(fdo::Error::Failed("no color picked".to_owned()));
        };

        let color: [u8; 4] = [
            255,
            (color.rgb[0] * 255.) as u8,
            (color.rgb[1] * 255.) as u8,
            (color.rgb[2] * 255.) as u8,
        ];

        Ok(u32::from_be_bytes(color))
    }
}

impl Start for KwinColorpicker {
    fn start(self, _monitor: bool) -> anyhow::Result<Connection> {
        let conn = zbus::blocking::Connection::session()?;

        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server().at("/ColorPicker", self)?;
        conn.request_name_with_flags("org.kde.kwin.ColorPicker", flags)?;

        Ok(conn)
    }
}
