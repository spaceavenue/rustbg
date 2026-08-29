use wllib::error::SysError;
use wllib::fmt_lite::StringOnStack;

use crate::error::AppError;
use crate::state::Config;

// this was a bit hard to follow so im documenting for my own reference
// we:
// 1. create a unix pipe
// 2. spawn the dump-bgra child process (fork + execvp)
// 3. connect it's write end (stdout) to the write end of the pipe
// 4. then read the raw bgra pixels from our read end, directly into the mmap_slice
fn exec_and_read<const N: usize>(
  argv: [*const libc::c_char; N],
  buffer: &mut [u8],
) -> Result<(), AppError> {
  unsafe {
    let mut pipe = [0i32; 2];
    // create an unidirectional pipe to read data from child process
    // O_CLOEXEC closes the read/write ends in any other spawned children
    if libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
      return Err(AppError::Sys(SysError::last("pipe2")));
    }

    let pid = libc::fork();
    if pid < 0 {
      let err = SysError::last("fork");
      libc::close(pipe[0]);
      libc::close(pipe[1]);
      return Err(AppError::Sys(err));
    }
    // child process context
    // 1. redirect stdout to the write end of the pipe
    // 2. restore default SIGPIPE handler before exec
    if pid == 0 {
      libc::close(pipe[0]);
      libc::dup2(pipe[1], 1);
      libc::close(pipe[1]);
      libc::signal(libc::SIGPIPE, libc::SIG_DFL);

      // redirect stderr to /dev/null
      let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
      if devnull >= 0 {
        libc::dup2(devnull, 2);
        libc::close(devnull);
      }
      libc::execvp(argv[0], argv.as_ptr());
      // if execvp failed, kill child >:)
      libc::_exit(1);
    }

    // parent process context
    // 1. close the write end in parent
    // 2. read raw bytes from the pipe directly into buffer
    libc::close(pipe[1]);
    let mut offset = 0usize;
    loop {
      let remaining = buffer.len() - offset;
      if remaining == 0 {
        break;
      }
      let num_bytes = libc::read(pipe[0], buffer.as_mut_ptr().add(offset).cast(), remaining);
      match num_bytes {
        num_bytes if num_bytes > 0 => {
          offset += num_bytes as usize;
        }
        0 => break, // EOF reached
        _ => {
          let err = SysError::last("read");
          libc::close(pipe[0]);
          libc::waitpid(pid, core::ptr::null_mut(), 0);
          return Err(AppError::Sys(err));
        }
      }
    }
    libc::close(pipe[0]);
    libc::waitpid(pid, core::ptr::null_mut(), 0);

    if offset != buffer.len() {
      // failed to read complete frame size
      return Err(AppError::ImageDecodeError);
    }
  }
  Ok(())
}

#[cfg(feature = "ffmpeg")]
fn run_ffmpeg(
  out_width: u32,
  out_height: u32,
  buffer: &mut [u8],
  fill: bool,
  path: StringOnStack<256>,
) -> Result<(), AppError> {
  let mut filter = StringOnStack::<96>::new();

  filter
    .push("scale=")
    .push(out_width)
    .push(":")
    .push(out_height);

  match fill {
    true => {
      filter
        .push(":force_original_aspect_ratio=increase,crop=")
        .push(out_width)
        .push(":")
        .push(out_height);
    }
    false => {
      filter
        .push(":force_original_aspect_ratio=decrease,pad=")
        .push(out_width)
        .push(":")
        .push(out_height)
        .push(":(ow-iw)/2:(oh-ih)/2");
    }
  }

  // build ffmpeg argument vector
  let argv: [*const libc::c_char; 11] = [
    c"ffmpeg".as_ptr(),
    c"-i".as_ptr(),
    path.as_ptr(),
    c"-vf".as_ptr(),
    filter.as_ptr(),
    c"-f".as_ptr(),
    c"rawvideo".as_ptr(),
    c"-pix_fmt".as_ptr(),
    c"bgra".as_ptr(),
    c"-".as_ptr(),
    core::ptr::null(),
  ];
  exec_and_read(argv, buffer)
}

#[cfg(not(feature = "ffmpeg"))]
fn run_dump_bgra(
  out_width: u32,
  out_height: u32,
  buffer: &mut [u8],
  fill: bool,
  path: StringOnStack<256>,
) -> Result<(), AppError> {
  let mut w_str = StringOnStack::<10>::new();
  w_str.push(out_width);
  let mut h_str = StringOnStack::<10>::new();
  h_str.push(out_height);
  let mut mode_str = StringOnStack::<5>::new();
  if fill {
    mode_str.push("fill");
  } else {
    mode_str.push("fit");
  }

  // build dump-bgra argument vector: scale image to raw bgra pixels and stream to stdout
  let argv: [*const libc::c_char; 7] = [
    c"dump-bgra".as_ptr(),
    w_str.as_ptr(),
    h_str.as_ptr(),
    mode_str.as_ptr(),
    path.as_ptr(),
    c"-".as_ptr(),
    core::ptr::null(),
  ];
  exec_and_read(argv, buffer)
}

pub fn load_and_scale(
  out_width: u32,
  out_height: u32,
  buffer: &mut [u8],
  config: &Config,
) -> Result<(), AppError> {
  let path = config.image_path;
  let fill = config.fill;

  #[cfg(not(feature = "ffmpeg"))]
  {
    run_dump_bgra(out_width, out_height, buffer, fill, path)
  }

  #[cfg(feature = "ffmpeg")]
  {
    run_ffmpeg(out_width, out_height, buffer, fill, path)
  }
}
