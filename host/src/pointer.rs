use anyhow::Result;
use std::time::Instant;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

pub struct Pointer {
    conn: Connection,
    _queue: EventQueue<State>,
    pointer: ZwlrVirtualPointerV1,
    started: Instant,
}

pub struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);

impl Pointer {
    pub fn new() -> Result<Self> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ())?;
        let seat: Option<WlSeat> = globals.bind(&qh, 1..=1, ()).ok();
        let pointer = manager.create_virtual_pointer(seat.as_ref(), &qh, ());
        conn.flush()?;
        Ok(Self { conn, _queue: queue, pointer, started: Instant::now() })
    }

    pub fn move_to(&self, x: u32, y: u32, width: u32, height: u32) -> Result<()> {
        self.pointer.motion_absolute(self.started.elapsed().as_millis() as u32, x, y, width, height);
        self.pointer.frame();
        self.conn.flush()?;
        Ok(())
    }
}
