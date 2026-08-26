#![no_std]
#![no_main]

mod error;
mod gamma;
mod state;

use state::{Config, State};
use wllib::dispatch::dispatch_once;
use wllib::error::WireError::ConnectionClosed;
use wllib::io::write_stderr;
use wllib::protocols::zwlr_gamma_control_manager_v1;
use wllib::registry::crawl;
use wllib::transport::Connection;
use wllib::wire::Message;

use crate::error::AppError;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: isize, argv: *const *mut libc::c_char) -> libc::c_int {
  let mut config = Config::default();
  if argc != 2 {
    write_stderr("Usage: rustemp <temp>\n");
    unsafe { libc::exit(1) };
  }

  let arg_ptr = unsafe { *argv.add(1) };
  if arg_ptr.is_null() {
    AppError::InvalidTemp.write_diagnostic();
    unsafe { libc::exit(1) };
  }
  let mut end_ptr = core::ptr::null_mut();
  config.temp = unsafe { libc::strtod(arg_ptr, &raw mut end_ptr) };

  if end_ptr == arg_ptr {
    AppError::InvalidTemp.write_diagnostic();
    unsafe { libc::exit(1) };
  }

  let mut conn = match Connection::connect() {
    Ok(c) => c,
    Err(e) => {
      e.write_diagnostic();
      unsafe { libc::exit(1) };
    }
  };

  let mut state = State::init(config);

  if let Err(e) = crawl(&mut conn, &mut state) {
    e.write_diagnostic();
    unsafe { libc::exit(1) };
  }

  // setup layer surfaces and gamma control for all monitors.
  for i in 0..state.output_len {
    // alloc ids for the new gamma control objects.
    let gamma_ctrl_id = conn.alloc_id();
    let output_id = state.outputs[i].output_id;

    // request gamma control
    let mut gamma_msg = Message::new(
      state.global.gamma_manager_id,
      zwlr_gamma_control_manager_v1::request::GET_GAMMA_CONTROL,
    );
    gamma_msg.write_u32(gamma_ctrl_id);
    gamma_msg.write_u32(output_id);
    conn.send_logged(&gamma_msg, None);

    // store the allocated ids back into our Output instance
    state.outputs[i].gamma_control_id = gamma_ctrl_id;
  }

  // main wayland event dispatch loop
  loop {
    match dispatch_once(&mut conn, &mut state) {
      Ok(()) => (),
      Err(ConnectionClosed) => break,
      Err(e) => {
        e.write_diagnostic();
        break;
      }
    }
  }
  0
}
