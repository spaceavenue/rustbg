use wllib::error::{SysError, WireError};
use wllib::io;

// Application-level errors.
//
// Wraps [`wlcore::WireError`] for anything related to the wayland connection/protocol. Each
// variant carries enough context to render a real diagnostic via `write_diagnostic`.
pub enum AppError {
  // A failure from the wayland transport/protocol layer.
  Wire(WireError),
  // A libc syscall outside the wayland socket failed.
  Sys(SysError),
  // dump-bgra/ffmpeg produced fewer bytes than the expected `width * height * 4`.
  ImageDecodeError,
}

impl AppError {
  pub fn write_diagnostic(&self) {
    match self {
      AppError::Wire(e) => e.write_diagnostic(),
      AppError::Sys(e) => WireError::Sys(*e).write_diagnostic(),
      #[cfg(feature = "ffmpeg")]
      AppError::ImageDecodeError => {
        io::write_stderr("[rustbg] image decode error: ffmpeg produced fewer bytes than expected\n")
      }
      #[cfg(not(feature = "ffmpeg"))]
      AppError::ImageDecodeError => io::write_stderr(
        "[rustbg] image decode error: dump-bgra produced fewer bytes than expected\n",
      ),
    }
  }
}

impl From<WireError> for AppError {
  fn from(e: WireError) -> Self {
    AppError::Wire(e)
  }
}
