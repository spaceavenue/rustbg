use core::slice;

use wllib::error::SysError;

use crate::error::AppError;
use crate::image_load;
use crate::state::Config;

// create a memory-backed file, aka memfd, containing the scaled bgra pixel data and return its file
// descriptor to the compositor. the memfd is mmap'ed to our virtual address space, and we pass it
// as a byte slice to our image loading function. we then unmap it and send the fd back. the wayland
// side turns it into a buffer
pub fn get_image_fd(out_width: u32, out_height: u32, config: &Config) -> Result<i32, AppError> {
  // if config.image_path.is_none() {
  //   return Err(AppError::MissingImagePath);
  // }
  let stride = out_width * 4; // 4 bytes per pixel (BGRA)
  let size = (stride * out_height) as usize;

  // create an anonymous, in-memory file.
  let fd = unsafe {
    libc::memfd_create(
      c"rustbg-wayland-shm".as_ptr().cast::<libc::c_char>(),
      libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING | libc::MFD_NOEXEC_SEAL,
    )
  };
  if fd < 0 {
    let err = SysError::last("memfd_create");
    return Err(AppError::Sys(err));
  }

  unsafe {
    // set the file's size, equal to our output size
    if { libc::ftruncate(fd, size as libc::off_t) } < 0 {
      let err = SysError::last("ftruncate");
      libc::close(fd);
      return Err(AppError::Sys(err));
    }

    // map the in-memory file into our address space
    let mmap_ptr = libc::mmap(
      core::ptr::null_mut(),
      size,
      libc::PROT_READ | libc::PROT_WRITE,
      libc::MAP_SHARED,
      fd,
      0,
    );
    if mmap_ptr == libc::MAP_FAILED {
      let err = SysError::last("mmap");
      libc::close(fd);
      return Err(AppError::Sys(err));
    }

    // get a byte slice from the mmap, spawn dump-bgra/ffmpeg, fill the mmap'd region directly,
    // unmap file
    let mmap_slice = slice::from_raw_parts_mut(mmap_ptr.cast::<u8>(), size);
    let result = image_load::load_and_scale(out_width, out_height, mmap_slice, config);
    libc::munmap(mmap_ptr, size);
    result?;
  };

  Ok(fd)
}
