use wllib::error::SysError;

use crate::error::AppError;
use crate::state::Config;

#[link(name = "c")]
unsafe extern "C" {
  fn pow(base: f64, exponent: f64) -> f64;
  fn log(x: f64) -> f64;
  fn exp(x: f64) -> f64;
  fn round(x: f64) -> f64;
}

fn powf(base: f64, exponent: f64) -> f64 {
  unsafe { pow(base, exponent) }
}

fn ln(x: f64) -> f64 {
  unsafe { log(x) }
}

fn expf(x: f64) -> f64 {
  unsafe { exp(x) }
}

fn roundf(x: f64) -> f64 {
  unsafe { round(x) }
}

fn create_memfd(size: usize) -> Result<i32, AppError> {
  let size = size * 3 * 2;
  let fd = unsafe { libc::memfd_create(c"rustemp-memfd".as_ptr(), libc::MFD_ALLOW_SEALING) };
  if fd < 0 {
    return Err(AppError::Sys(SysError::last("memfd_create")));
  }

  // gamma tables contain three ramps (R, G, B), each with `size` elements of 16-bit values (2
  // bytes)
  unsafe {
    if libc::ftruncate(fd, size as libc::off_t) < 0 {
      let err = SysError::last("ftruncate");
      libc::close(fd);
      return Err(AppError::Sys(err));
    }
  }
  Ok(fd)
}

fn mmap_slice<'a>(fd: i32, size: usize) -> Result<(*mut libc::c_void, &'a mut [u16]), AppError> {
  let ptr = unsafe {
    libc::mmap(
      core::ptr::null_mut(),
      size * 3 * 2,
      libc::PROT_WRITE,
      libc::MAP_SHARED,
      fd,
      0,
    )
  };
  if ptr == libc::MAP_FAILED {
    let err = SysError::last("mmap");
    unsafe { libc::close(fd) };
    return Err(AppError::Sys(err));
  }
  let slice = unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u16>(), size * 3) };

  Ok((ptr, slice))
}

// convert temperature in kelvin to rgb data
fn kelvin_to_rgb(kelvin: f64) -> (f64, f64, f64) {
  let kelvin = kelvin.clamp(1000.0, 40000.0);
  let temp = kelvin / 100.0;

  let r = if temp <= 66.0 {
    1.0
  } else {
    let r = (powf(329.698727446 * (temp - 60.0), -0.1332047592)) / 255.0;
    r.clamp(0.0, 1.0)
  };

  let g = if temp <= 66.0 {
    let g = (99.4708025861 * ln(temp) - 161.1195636025) / 255.0;
    g.clamp(0.0, 1.0)
  } else {
    let g = (powf(288.1221695283 * (temp - 60.0), -0.0755148492)) / 255.0;
    g.clamp(0.0, 1.0)
  };

  let b = if temp >= 66.0 {
    1.0
  } else if temp <= 19.0 {
    0.0
  } else {
    let b = (138.5177312231 * ln(temp - 10.0) - 305.0447927307) / 255.0;
    b.clamp(0.0, 1.0)
  };

  (r, g, b)
}

// input levels: remaps [black, white] to [0, 1], clamping outside that range. `black == white`
// is treated as a no-op rather than dividing by zero.
fn apply_levels(t: f64, black: f64, white: f64) -> f64 {
  if (white - black).abs() < f64::EPSILON {
    return t;
  }
  ((t - black) / (white - black)).clamp(0.0, 1.0)
}

// power-curve response: `t ^ (1/exp)`. `exp == 1.0` is a no-op.
fn apply_gamma(t: f64, exp: f64) -> f64 {
  if (exp - 1.0).abs() < f64::EPSILON {
    return t;
  }
  powf(t.max(0.0), 1.0 / exp)
}

// sigmoid contrast: pushes values away from (amount > 0) or toward (amount < 0) the midpoint,
// renormalized so the full [0, 1] input range still maps onto [0, 1]. `amount == 0.0` is a no-op
// (the sigmoid would otherwise be flat, making the renormalization divide by ~0).
fn apply_contrast(t: f64, amount: f64) -> f64 {
  if amount.abs() < 1e-9 {
    return t;
  }
  let k = amount * 10.0;
  let sig = |x: f64| 1.0 / (1.0 + expf(-k * (x - 0.5)));
  let s0 = sig(0.0);
  let s1 = sig(1.0);
  ((sig(t) - s0) / (s1 - s0)).clamp(0.0, 1.0)
}

fn apply_posterize(t: f64, levels: u32) -> f64 {
  if levels < 2 {
    return t;
  }

  let steps = f64::from(levels - 1);
  roundf(t * steps) / steps
}

// runs one channel's normalized input `t` (0..1) through the full ramp-op pipeline. `gain` is
// that channel's color-temperature factor multiplied by its manual gain knob, `offset` is its
// manual offset knob.
fn compute_channel_value(t: f64, gain: f64, offset: f64, config: &Config) -> f64 {
  let v = apply_levels(t, config.level_black, config.level_white);
  let v = apply_gamma(v, config.gamma);
  let v = (v * gain + offset).clamp(0.0, 1.0);
  let v = apply_contrast(v, config.contrast);
  let v = (v * config.brightness).clamp(0.0, 1.0);
  let v = apply_posterize(v, config.posterize_levels);
  let v = (v > config.solarize_value).then_some(1.0 - v).unwrap_or(v);
  if config.invert { 1.0 - v } else { v }
}

pub fn get_gamma_table_fd(size: usize, config: &Config) -> Result<i32, AppError> {
  let (r_factor, g_factor, b_factor) = kelvin_to_rgb(config.temp);
  let fd = create_memfd(size)?;
  let (mmap_ptr, slice) = mmap_slice(fd, size)?;

  // generate gamma curves scaled by the RGB color temperature factors
  for i in 0..size {
    let t = i as f64 / (size.saturating_sub(1).max(1)) as f64;
    // red
    let r = config.disable_r.then_some(0.0).unwrap_or_else(|| {
      compute_channel_value(t, r_factor * config.gain_r, config.offset_r, config)
    });
    slice[i] = (r * 65535.0) as u16;
    // greerg
    let g = config.disable_g.then_some(0.0).unwrap_or_else(|| {
      compute_channel_value(t, g_factor * config.gain_g, config.offset_g, config)
    });
    slice[size + i] = (g * 65535.0) as u16;
    // blue
    let b = config.disable_b.then_some(0.0).unwrap_or_else(|| {
      compute_channel_value(t, b_factor * config.gain_b, config.offset_b, config)
    });
    slice[2 * size + i] = (b * 65535.0) as u16;
  }

  unsafe { libc::munmap(mmap_ptr, size * 3 * 2) };
  Ok(fd)
}
