#![no_std]
#![no_main]

mod error;
mod gamma;
mod state;

use state::{Config, State};
use wllib::cli;
use wllib::dispatch::dispatch_once;
use wllib::error::WireError::ConnectionClosed;
use wllib::io::write_stderr;
use wllib::protocols::zwlr_gamma_control_manager_v1;
use wllib::registry::crawl;
use wllib::transport::Connection;
use wllib::wire::Message;

use crate::error::AppError;

unsafe extern "C" {
  static optarg: *const libc::c_char;
  static mut optind: libc::c_int;
}
#[link(name = "c", kind = "static")]
unsafe extern "C" {}

const LONGOPTS: [cli::LongOption; 14] = [
  cli::LongOption::new(c"temp", cli::REQUIRED_ARGUMENT, 't'),
  cli::LongOption::new(c"black", cli::REQUIRED_ARGUMENT, 'k'),
  cli::LongOption::new(c"white", cli::REQUIRED_ARGUMENT, 'w'),
  cli::LongOption::new(c"gamma", cli::REQUIRED_ARGUMENT, 'g'),
  cli::LongOption::new(c"gain", cli::REQUIRED_ARGUMENT, 'G'),
  cli::LongOption::new(c"offset", cli::REQUIRED_ARGUMENT, 'O'),
  cli::LongOption::new(c"contrast", cli::REQUIRED_ARGUMENT, 'c'),
  cli::LongOption::new(c"brightness", cli::REQUIRED_ARGUMENT, 'b'),
  cli::LongOption::new(c"posterize", cli::REQUIRED_ARGUMENT, 'p'),
  cli::LongOption::new(c"invert", cli::NO_ARGUMENT, 'i'),
  cli::LongOption::new(c"no-red", cli::NO_ARGUMENT, 'x'),
  cli::LongOption::new(c"no-green", cli::NO_ARGUMENT, 'y'),
  cli::LongOption::new(c"no-blue", cli::NO_ARGUMENT, 'z'),
  cli::LONG_OPTION_TERMINATOR,
];

const USAGE: &str = "Usage: rustemp -t <kelvin> [--black <0..1>] [--white <0..1>]\n";

fn bad_value() -> ! {
  write_stderr("rustemp: invalid numeric value\n");
  unsafe { libc::exit(1) };
}

fn parse_f64(s: *const libc::c_char) -> Option<f64> {
  if s.is_null() {
    return None;
  }
  let mut end_ptr = core::ptr::null_mut();
  let val = unsafe { libc::strtod(s, &raw mut end_ptr) };
  if end_ptr == s as *mut libc::c_char {
    None
  } else {
    Some(val)
  }
}

// parses a "<r>:<g>:<b>" string, e.g. a --gain/--offset argument.
fn parse_triplet(s: *const libc::c_char) -> Option<(f64, f64, f64)> {
  if s.is_null() {
    return None;
  }
  let text = unsafe { core::ffi::CStr::from_ptr(s) }.to_str().ok()?;
  let mut parts = text.splitn(3, ':');
  let r = parts.next()?.parse::<f64>().ok()?;
  let g = parts.next()?.parse::<f64>().ok()?;
  let b = parts.next()?.parse::<f64>().ok()?;
  Some((r, g, b))
}

fn parse_u32(s: *const libc::c_char) -> Option<u32> {
  if s.is_null() {
    return None;
  }
  let mut end_ptr = core::ptr::null_mut();
  let val = unsafe { libc::strtoul(s, &raw mut end_ptr, 10) };
  if end_ptr == s as *mut libc::c_char {
    None
  } else {
    u32::try_from(val).ok()
  }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: isize, argv: *const *mut libc::c_char) -> libc::c_int {
  let mut config = Config::default();
  let mut temp_set = false;
  let optstring = c"t:k:w:g:G:O:c:b:p:ixyz".as_ptr();

  let mut longindex: libc::c_int = 0;
  unsafe { optind = 1 };
  loop {
    let c = unsafe {
      cli::getopt_long(
        argc as _,
        argv,
        optstring,
        LONGOPTS.as_ptr(),
        &raw mut longindex,
      )
    };
    if c == -1 {
      break;
    }
    match c as u8 as char {
      't' => {
        let Some(v) = parse_f64(unsafe { optarg }) else {
          AppError::InvalidTemp.write_diagnostic();
          unsafe { libc::exit(1) };
        };
        config.temp = v;
        temp_set = true;
      }
      'k' => config.level_black = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'w' => config.level_white = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'g' => config.gamma = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'G' => {
        let (r, g, b) = parse_triplet(unsafe { optarg }).unwrap_or_else(|| bad_value());
        (config.gain_r, config.gain_g, config.gain_b) = (r, g, b);
      }
      'O' => {
        let (r, g, b) = parse_triplet(unsafe { optarg }).unwrap_or_else(|| bad_value());
        (config.offset_r, config.offset_g, config.offset_b) = (r, g, b);
      }
      'c' => config.contrast = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'b' => config.brightness = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'p' => config.posterize_levels = parse_u32(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'i' => config.invert = true,
      'x' => config.disable_r = true,
      'y' => config.disable_g = true,
      'z' => config.disable_b = true,
      _ => (),
    }
  }

  if !temp_set {
    write_stderr(USAGE);
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
