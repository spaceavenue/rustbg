use wllib::dispatch::EventHandler;
use wllib::io::write_stderr;
use wllib::protocols::zwlr_gamma_control_v1;
use wllib::registry::{GlobalHandler, bind, clamp_version};
use wllib::transport::Connection;
use wllib::wire::{Message, read_u32};

use crate::gamma;

pub const MAX_OUTPUTS: usize = 8;

// tracks registered object ids
#[derive(Default)]
pub struct Global {
  // wl_compositor global id
  pub compositor_id: u32,
  // zwlr_layer_shell_v1 global id
  pub gamma_manager_id: u32,
}

// abstration around wl_output
#[derive(Default)]
pub struct Output {
  pub output_id: u32,
  pub gamma_control_id: u32,
}

// config
pub struct Config {
  pub temp: f64,
  // input levels: remaps [level_black, level_white] to [0, 1] before any other ramp op
  pub level_black: f64,
  pub level_white: f64,
  // power-curve exponent. 1.0 is a no-op
  pub gamma: f64,
  // per-channel gain (multiplicative) and offset (additive), applied after the color-temperature
  // factor. also doubles as a manual per-channel black/white point:
  // (gain = 1/(white-black), offset = -black*gain is the same affine transform)
  pub gain_r: f64,
  pub gain_g: f64,
  pub gain_b: f64,
  pub offset_r: f64,
  pub offset_g: f64,
  pub offset_b: f64,
  // sigmoid contrast strength, roughly -1.0..1.0. 0.0 is a no-op
  pub contrast: f64,
  // final multiplicative scale, applied after contrast. 1.0 is a no-op
  pub brightness: f64,
  // quantizes to this many discrete output levels. < 2 disables it
  pub posterize_levels: u32,
  // full inversion
  pub invert: bool,
  // zeros out an entire channel's ramp, bypassing the rest of the pipeline for that channel
  pub disable_r: bool,
  pub disable_g: bool,
  pub disable_b: bool,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      temp: 6500.0,
      level_black: 0.0,
      level_white: 1.0,
      gamma: 1.0,
      contrast: 0.0,
      brightness: 1.0,
      gain_r: 1.0,
      gain_g: 1.0,
      gain_b: 1.0,
      offset_r: 0.0,
      offset_g: 0.0,
      offset_b: 0.0,
      posterize_levels: 0,
      invert: false,
      disable_r: false,
      disable_g: false,
      disable_b: false,
    }
  }
}

pub struct State {
  pub global: Global,
  pub outputs: [Output; 4],
  pub output_len: usize,
  pub config: Config,
}

impl State {
  pub fn init(config: Config) -> Self {
    Self {
      global: Global::default(),
      outputs: core::array::from_fn(|_| Output::default()),
      output_len: 0,
      config,
    }
  }

  // zwlr_gamma_control_v1::gamma_size(size): the compositor telling us how many entries its
  // gamma ramp expects. We generate ramps for the configured color temperature and hand them
  // over via set_gamma.
  fn handle_gamma_size(
    conn: &mut Connection,
    out: &Output,
    config: &Config,
    opcode: u16,
    data: &[u8],
  ) {
    if opcode == zwlr_gamma_control_v1::event::FAILED {
      return;
    }
    let size = read_u32(data, 0) as usize;

    // get the fd containing the scaled image data
    let g_fd = match gamma::get_gamma_table_fd(size, config) {
      Ok(fd) => fd,
      Err(e) => {
        e.write_diagnostic();
        return;
      }
    };
    let gamma_control_id = out.gamma_control_id;
    // set gamma table
    conn.send_logged(
      &Message::new(gamma_control_id, zwlr_gamma_control_v1::request::SET_GAMMA),
      Some(g_fd),
    );
    unsafe { libc::close(g_fd) };
  }
}

impl GlobalHandler for State {
  // bind globals matching interfaces we want. we bind to the minimum of the client's wanted
  // version and the server's advertised version
  fn on_global(&mut self, conn: &mut Connection, name: u32, interface: &str, version: u32) {
    match interface {
      "wl_compositor" => {
        let id = conn.alloc_id();
        match bind(conn, name, interface, clamp_version(7, version), id) {
          Ok(()) => self.global.compositor_id = id,
          Err(e) => e.write_diagnostic(),
        }
      }
      "zwlr_gamma_control_manager_v1" => {
        let id = conn.alloc_id();
        match bind(conn, name, interface, clamp_version(1, version), id) {
          Ok(()) => self.global.gamma_manager_id = id,
          Err(e) => e.write_diagnostic(),
        }
      }
      "wl_output" => {
        if self.output_len >= MAX_OUTPUTS {
          write_stderr("Maximum outputs limit reached\n");
          return;
        }
        let id = conn.alloc_id();
        match bind(conn, name, interface, clamp_version(4, version), id) {
          Ok(()) => {
            self.outputs[self.output_len] = Output {
              output_id: id,
              ..Default::default()
            };
            self.output_len += 1;
          }
          Err(e) => e.write_diagnostic(),
        }
      }
      _ => (),
    }
  }
}

impl EventHandler for State {
  fn handle_event(&mut self, conn: &mut Connection, sender: u32, opcode: u16, data: &[u8]) {
    self.outputs.iter().for_each(|out| {
      if out.gamma_control_id != sender {
        return;
      }
      State::handle_gamma_size(conn, out, &self.config, opcode, data);
    });
  }
}
