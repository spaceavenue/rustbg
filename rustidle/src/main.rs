#![no_std]
#![no_main]

pub mod config;
pub mod error;
pub mod state;

use wllib::dispatch::dispatch_once;
use wllib::error::WireError::ConnectionClosed;
use wllib::io::write_stderr;
use wllib::protocols::ext_idle_notifier_v1;
use wllib::registry::crawl;
use wllib::transport::Connection;
use wllib::wire::Message;

use crate::config::Config;
use crate::state::State;

#[link(name = "c", kind = "static")]
unsafe extern "C" {}

// flag to track the suspended state
static IS_SUSPENDED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

extern "C" fn handle_sigusr1(_sig: libc::c_int) {
  // fetch_xor with true inverts the current boolean value
  IS_SUSPENDED.fetch_xor(true, core::sync::atomic::Ordering::Relaxed);
}

fn write_usage() -> ! {
  write_stderr("Usage: rustidle <config-file>\n");
  unsafe { libc::exit(1) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: isize, argv: *const *mut libc::c_char) -> libc::c_int {
  if argc != 2 {
    write_usage()
  }
  let config_path = unsafe { *argv.add(1) };
  if config_path.is_null() {
    write_usage()
  }
  if unsafe { libc::access(config_path, libc::F_OK) != 0 } {
    write_stderr("[rustidle] failed to open config file.\n");
    return 1;
  }

  // tells kernel to reap child processes automatically, avoiding watipid
  unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
  // handle SIGUSR1
  unsafe {
    let mut sa: libc::sigaction = core::mem::zeroed();
    sa.sa_sigaction = handle_sigusr1 as *const () as libc::sighandler_t;
    // sa_flags = 0 so that blocking syscalls (recv, etc.) fail with EINTR instead of restarting
    sa.sa_flags = 0;
    libc::sigemptyset(&raw mut sa.sa_mask);
    libc::sigaction(libc::SIGUSR1, &raw const sa, core::ptr::null_mut());
  }

  let mut conn = match Connection::connect() {
    Ok(c) => c,
    Err(e) => {
      e.write_diagnostic();
      unsafe { libc::exit(1) };
    }
  };
  let config = match Config::load(config_path) {
    Ok(c) => c,
    Err(e) => {
      e.write_diagnostic();
      unsafe { libc::exit(1) }
    }
  };
  let mut state = State::init(config);

  if let Err(e) = crawl(&mut conn, &mut state) {
    e.write_diagnostic();
    unsafe { libc::exit(1) };
  }

  let create_notifs = |conn: &mut Connection, state: &mut State| {
    for i in 0..state.config.entry_len {
      let notif = &mut state.notifications[i];
      notif.id = conn.alloc_id();
      let mut msg = Message::new(
        state.global.idle_notifier_id,
        ext_idle_notifier_v1::request::GET_IDLE_NOTIFICATION,
      );
      msg.write_u32(notif.id);
      msg.write_u32(notif.entry.timeout_ms);
      msg.write_u32(state.global.seat_id);

      conn.send_logged(&msg, None);
    }
  };

  create_notifs(&mut conn, &mut state);
  let mut was_suspended = false;

  // main wayland event dispatch loop
  loop {
    if IS_SUSPENDED.load(core::sync::atomic::Ordering::Relaxed) {
      for i in 0..state.config.entry_len {
        let notif = &mut state.notifications[i];
        let msg = Message::new(notif.id, ext_idle_notifier_v1::request::DESTROY);
        conn.send_logged(&msg, None);
      }
      was_suspended = true;
    }

    while IS_SUSPENDED.load(core::sync::atomic::Ordering::Relaxed) {
      unsafe { libc::pause() };
    }

    if was_suspended {
      create_notifs(&mut conn, &mut state);
      was_suspended = false;
    }

    match dispatch_once(&mut conn, &mut state) {
      Ok(()) => (),
      Err(ConnectionClosed) => break,
      Err(e) => {
        if IS_SUSPENDED.load(core::sync::atomic::Ordering::Relaxed) {
          continue;
        }
        e.write_diagnostic();
        break;
      }
    }
  }
  0
}
