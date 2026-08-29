#![no_std]
#![no_main]

mod error;
mod gamma;
mod state;

use state::{Config, State};
use wllib::cli::{LONG_OPTION_TERMINATOR, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT, getopt_long};
use wllib::dispatch::dispatch_once;
use wllib::error::WireError::ConnectionClosed;
use wllib::io::{write_stderr, write_stdout};
use wllib::protocols::zwlr_gamma_control_manager_v1;
use wllib::registry::crawl;
use wllib::transport::Connection;
use wllib::wire::Message;

unsafe extern "C" {
  static optarg: *const libc::c_char;
  static mut optind: libc::c_int;
}
#[link(name = "c", kind = "static")]
unsafe extern "C" {}

const LONGOPTS: [LongOption; 16] = [
  LongOption::new(c"temp", REQUIRED_ARGUMENT, 't'),
  LongOption::new(c"black", REQUIRED_ARGUMENT, 'k'),
  LongOption::new(c"white", REQUIRED_ARGUMENT, 'w'),
  LongOption::new(c"gamma", REQUIRED_ARGUMENT, 'g'),
  LongOption::new(c"gain", REQUIRED_ARGUMENT, 'G'),
  LongOption::new(c"offset", REQUIRED_ARGUMENT, 'O'),
  LongOption::new(c"contrast", REQUIRED_ARGUMENT, 'c'),
  LongOption::new(c"brightness", REQUIRED_ARGUMENT, 'b'),
  LongOption::new(c"posterize", REQUIRED_ARGUMENT, 'p'),
  LongOption::new(c"solarize", REQUIRED_ARGUMENT, 's'),
  LongOption::new(c"invert", NO_ARGUMENT, 'i'),
  LongOption::new(c"no-red", NO_ARGUMENT, 'x'),
  LongOption::new(c"no-green", NO_ARGUMENT, 'y'),
  LongOption::new(c"no-blue", NO_ARGUMENT, 'z'),
  LongOption::new(c"help", NO_ARGUMENT, 'h'),
  LONG_OPTION_TERMINATOR,
];

const USAGE: &str = concat!(
  "Usage: rustemp -t <kelvin>\n",
  "              [--black <0..1>] [--white <0..1>]\n",
  "              [-g|--gamma <exponent>]\n",
  "              [--gain <r>:<g>:<b>] [--offset <r>:<g>:<b>]\n",
  "              [-c|--contrast <-1..1>] [-b|--brightness <multiplier>]\n",
  "              [-p|--posterize <levels>]\n",
  "              [--solarize[=<0..1>]] [-i|--invert]\n",
  "              [--no-red] [--no-green] [--no-blue]\n",
);

fn bad_value() -> ! {
  write_stderr("rustemp: invalid or no value supplied\n");
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
  let optstring = c"t:k:w:g:G:O:c:b:p:s:ixyzh".as_ptr();

  let mut longindex: libc::c_int = 0;
  unsafe { optind = 1 };
  loop {
    let c = unsafe {
      getopt_long(
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
        config.temp = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value());
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
      's' => config.solarize_value = parse_f64(unsafe { optarg }).unwrap_or_else(|| bad_value()),
      'i' => config.invert = true,
      'x' => config.disable_r = true,
      'y' => config.disable_g = true,
      'z' => config.disable_b = true,
      'h' => {
        write_stdout(USAGE);
        unsafe { libc::exit(1) }
      }
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
