//! Pieces shared by the CLAP and VST3 hosts: the window plugin editors are
//! embedded in, timers and file descriptors plugins ask the host to watch
//! (their GUIs' event loops on Linux), and base64 for saved state.

use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Editor window (X11)
// ---------------------------------------------------------------------------

/// A top-level window a plugin editor is embedded into. On Linux this is an
/// X11 window created on its own connection (the plugin draws into a child
/// of it from its own connection). Elsewhere there is no embedding yet and
/// hosts fall back to the plugin's own floating window.
pub struct EditorWindow {
    #[cfg(all(unix, not(target_os = "macos")))]
    x: x11::Win,
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    pub struct Win {
        pub conn: RustConnection,
        pub id: u32,
        wm_delete: u32,
    }

    impl Win {
        pub fn new(w: u32, h: u32, title: &str) -> Result<Self, String> {
            let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("no X11 display: {e}"))?;
            let screen = &conn.setup().roots[screen_num];
            let id = conn.generate_id().map_err(|e| e.to_string())?;
            let aux = CreateWindowAux::new().background_pixel(screen.black_pixel).event_mask(EventMask::STRUCTURE_NOTIFY);
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                screen.root,
                0,
                0,
                w.max(1) as u16,
                h.max(1) as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &aux,
            )
            .map_err(|e| e.to_string())?;
            let intern = |name: &[u8]| conn.intern_atom(false, name).ok().and_then(|c| c.reply().ok()).map(|r| r.atom);
            let protocols = intern(b"WM_PROTOCOLS").unwrap_or(0);
            let wm_delete = intern(b"WM_DELETE_WINDOW").unwrap_or(0);
            let utf8 = intern(b"UTF8_STRING").unwrap_or(AtomEnum::STRING.into());
            let net_name = intern(b"_NET_WM_NAME").unwrap_or(AtomEnum::WM_NAME.into());
            let _ = conn.change_property32(PropMode::REPLACE, id, protocols, AtomEnum::ATOM, &[wm_delete]);
            let _ = conn.change_property8(PropMode::REPLACE, id, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes());
            let _ = conn.change_property8(PropMode::REPLACE, id, net_name, utf8, title.as_bytes());
            let _ = conn.change_window_attributes(id, &ChangeWindowAttributesAux::new());
            conn.map_window(id).map_err(|e| e.to_string())?;
            conn.flush().map_err(|e| e.to_string())?;
            Ok(Self { conn, id, wm_delete })
        }

        pub fn resize(&self, w: u32, h: u32) {
            let _ = self.conn.configure_window(self.id, &ConfigureWindowAux::new().width(w.max(1)).height(h.max(1)));
            let _ = self.conn.flush();
        }

        /// Handle pending events; true if the user closed the window.
        pub fn poll_closed(&self) -> bool {
            let mut closed = false;
            while let Ok(Some(ev)) = self.conn.poll_for_event() {
                if let Event::ClientMessage(ClientMessageEvent { data, .. }) = ev
                    && data.as_data32()[0] == self.wm_delete
                {
                    closed = true;
                }
            }
            closed
        }
    }

    impl Drop for Win {
        fn drop(&mut self) {
            let _ = self.conn.destroy_window(self.id);
            let _ = self.conn.flush();
        }
    }
}

impl EditorWindow {
    #[allow(unused_variables)]
    pub fn new(w: u32, h: u32, title: &str) -> Result<Self, String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if std::env::var_os("DISPLAY").is_none() {
                return Err("no X11 display (DISPLAY is not set)".into());
            }
            Ok(Self { x: x11::Win::new(w, h, title)? })
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            Err("embedded plugin editors are only implemented for X11 so far".into())
        }
    }

    /// The native handle to give the plugin (an X11 window id).
    pub fn handle(&self) -> u64 {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.x.id as u64
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            0
        }
    }

    #[allow(unused_variables)]
    pub fn resize(&self, w: u32, h: u32) {
        #[cfg(all(unix, not(target_os = "macos")))]
        self.x.resize(w, h);
    }

    pub fn poll_closed(&self) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.x.poll_closed()
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Timers and file descriptors
// ---------------------------------------------------------------------------

/// Timers and fds a plugin registered; the host fires them from `idle()`
/// on the main thread.
#[derive(Default)]
pub struct RunLoop {
    timers: Mutex<Vec<(u64, Duration, Instant)>>,
    fds: Mutex<Vec<(i32, u32)>>,
    next_id: Mutex<u64>,
}

impl RunLoop {
    pub fn add_timer(&self, period_ms: u32) -> u64 {
        let mut n = self.next_id.lock().expect("timer id");
        *n += 1;
        let id = *n;
        let period = Duration::from_millis(period_ms.max(1) as u64);
        if let Ok(mut t) = self.timers.lock() {
            t.push((id, period, Instant::now() + period));
        }
        id
    }

    pub fn remove_timer(&self, id: u64) -> bool {
        self.timers
            .lock()
            .map(|mut t| {
                let n = t.len();
                t.retain(|x| x.0 != id);
                t.len() != n
            })
            .unwrap_or(false)
    }

    pub fn set_fd(&self, fd: i32, flags: u32) {
        if let Ok(mut f) = self.fds.lock() {
            f.retain(|x| x.0 != fd);
            f.push((fd, flags));
        }
    }

    pub fn remove_fd(&self, fd: i32) -> bool {
        self.fds
            .lock()
            .map(|mut f| {
                let n = f.len();
                f.retain(|x| x.0 != fd);
                f.len() != n
            })
            .unwrap_or(false)
    }

    pub fn clear(&self) {
        if let Ok(mut t) = self.timers.lock() {
            t.clear();
        }
        if let Ok(mut f) = self.fds.lock() {
            f.clear();
        }
    }

    /// Timers that are due now (rescheduled).
    pub fn due_timers(&self) -> Vec<u64> {
        let now = Instant::now();
        let Ok(mut t) = self.timers.lock() else { return Vec::new() };
        let mut due = Vec::new();
        for (id, period, next) in t.iter_mut() {
            if *next <= now {
                due.push(*id);
                *next = now + *period;
            }
        }
        due
    }

    /// Registered fds that are ready: (fd, readiness flags in the
    /// registration's own bit layout: 1 read, 2 write, 4 error).
    pub fn ready_fds(&self) -> Vec<(i32, u32)> {
        let fds: Vec<(i32, u32)> = self.fds.lock().map(|f| f.clone()).unwrap_or_default();
        if fds.is_empty() {
            return Vec::new();
        }
        #[cfg(unix)]
        {
            let mut p: Vec<libc::pollfd> = fds
                .iter()
                .map(|&(fd, fl)| libc::pollfd {
                    fd,
                    events: (if fl & 1 != 0 { libc::POLLIN } else { 0 }) | (if fl & 2 != 0 { libc::POLLOUT } else { 0 }),
                    revents: 0,
                })
                .collect();
            // SAFETY: `p` is a valid array of pollfd for its length; timeout 0.
            let n = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, 0) };
            if n <= 0 {
                return Vec::new();
            }
            p.iter()
                .filter(|x| x.revents != 0)
                .map(|x| {
                    let mut f = 0;
                    if x.revents & libc::POLLIN != 0 {
                        f |= 1;
                    }
                    if x.revents & libc::POLLOUT != 0 {
                        f |= 2;
                    }
                    if x.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                        f |= 4;
                    }
                    (x.fd, f)
                })
                .collect()
        }
        #[cfg(not(unix))]
        {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// base64 (standard alphabet, padded) for plugin state in project files
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> { B64.iter().position(|&x| x == c).map(|p| p as u32) };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for q in bytes.chunks(4) {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for &c in q.iter().take(4 - pad) {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0u8, 255, 128, 7][..]] {
            let e = b64_encode(data);
            assert_eq!(b64_decode(&e).as_deref(), Some(data), "{e}");
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_decode("Zm9vYg=="), Some(b"foob".to_vec()));
        assert_eq!(b64_decode("abc"), None);
    }

    #[test]
    fn timers_fire_when_due() {
        let rl = RunLoop::default();
        let id = rl.add_timer(1);
        std::thread::sleep(Duration::from_millis(3));
        assert_eq!(rl.due_timers(), vec![id]);
        assert!(rl.due_timers().is_empty(), "rescheduled");
        assert!(rl.remove_timer(id));
    }
}
