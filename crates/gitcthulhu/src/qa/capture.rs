//! Capture-only screenshots of the app's own window.
//!
//! On Windows the window is captured with `PrintWindow(PW_RENDERFULLCONTENT)` into a memory bitmap: no input is
//! injected, the window is not moved, raised or focused, and the capture also works while the window is partly
//! covered. The capture runs on a background thread (the UI thread keeps pumping messages, which `PrintWindow`
//! needs). Other platforms report that capturing is unsupported.

use std::path::Path;

use gpui_kit::Window;

/// A captured image: tightly packed RGBA, top-down, opaque.
pub struct Frame {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The native handle of the window (an `HWND` on Windows).
#[cfg(windows)]
pub fn window_hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // `Window::window_handle` (gpui's own handle) shadows the trait method: call the trait explicitly.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// The native handle of the window (none off Windows).
#[cfg(not(windows))]
pub fn window_hwnd(_window: &Window) -> Option<isize> {
    None
}

/// Captures the window `hwnd`: its client area, or the whole window (title bar and borders) with `whole_window`.
#[cfg(windows)]
pub fn capture(hwnd: isize, whole_window: bool) -> Result<Frame, String> {
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    };
    use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, IsIconic, PW_RENDERFULLCONTENT,
    };

    let hwnd = hwnd as HWND;
    // SAFETY: plain Win32 calls on a window handle of this process and on GDI objects created and released here;
    // every pointer passed outlives its call.
    unsafe {
        if IsIconic(hwnd) != 0 {
            return Err("the window is minimized, nothing to capture".into());
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let got = if whole_window {
            GetWindowRect(hwnd, &mut rect)
        } else {
            GetClientRect(hwnd, &mut rect)
        };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if got == 0 || width <= 0 || height <= 0 {
            return Err(format!("the window has no size ({width}x{height})"));
        }
        let screen = GetDC(null_mut());
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if screen.is_null() || memory.is_null() || bitmap.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !memory.is_null() {
                DeleteDC(memory);
            }
            if !screen.is_null() {
                ReleaseDC(null_mut(), screen);
            }
            return Err("could not create the capture bitmap".into());
        }
        let previous = SelectObject(memory, bitmap);
        let flags = PW_RENDERFULLCONTENT | if whole_window { 0 } else { PW_CLIENTONLY };
        let printed = PrintWindow(hwnd, memory, flags);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: Default::default(),
        };
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        let lines = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
        if printed == 0 {
            return Err("PrintWindow failed".into());
        }
        if lines == 0 {
            return Err("GetDIBits failed".into());
        }
        Ok(Frame {
            width: width as u32,
            height: height as u32,
            rgba: bgra_to_rgba(bgra),
        })
    }
}

/// Capturing is not supported off Windows.
#[cfg(not(windows))]
pub fn capture(_hwnd: isize, _whole_window: bool) -> Result<Frame, String> {
    Err("screenshots are only supported on Windows".into())
}

/// Swaps the red and blue channels and makes every pixel opaque (the capture bitmap has no alpha).
pub fn bgra_to_rgba(mut pixels: Vec<u8>) -> Vec<u8> {
    let (chunks, _) = pixels.as_chunks_mut::<4>();
    for px in chunks {
        px.swap(0, 2);
        px[3] = 255;
    }
    pixels
}

/// Whether every pixel has the same colour: a real window never does, a failed capture often does.
pub fn is_uniform(frame: &Frame) -> bool {
    let (pixels, _) = frame.rgba.as_chunks::<4>();
    match pixels.split_first() {
        Some((first, rest)) => rest.iter().all(|px| px == first),
        None => true,
    }
}

/// Writes `frame` as a PNG file.
pub fn write_png(path: &Path, frame: &Frame) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png header: {e}"))?;
    writer
        .write_image_data(&frame.rgba)
        .map_err(|e| format!("png data: {e}"))?;
    writer.finish().map_err(|e| format!("png end: {e}"))
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn bgra_becomes_opaque_rgba() {
        assert_eq!(
            bgra_to_rgba(vec![1, 2, 3, 0, 10, 20, 30, 7]),
            vec![3, 2, 1, 255, 30, 20, 10, 255],
            "channels swapped, alpha forced"
        );
    }

    #[test]
    fn a_single_colour_frame_is_flagged() {
        let flat = Frame {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 0, 0, 0, 255],
        };
        assert!(is_uniform(&flat), "all black");
        let varied = Frame {
            rgba: vec![0, 0, 0, 255, 1, 0, 0, 255],
            ..flat
        };
        assert!(!is_uniform(&varied), "two colours");
    }

    #[test]
    fn writes_a_readable_png() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("x.png");
        let frame = Frame {
            width: 2,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 255],
        };
        write_png(&path, &frame).expect("png written");
        let bytes = std::fs::read(&path).expect("file exists");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("decodes");
        let mut buf = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!((info.width, info.height), (2, 2), "dimensions survive");
        assert_eq!(&buf[..4], &[255, 0, 0, 255], "first pixel survives");
    }

    #[cfg(windows)]
    #[test]
    fn capturing_a_missing_window_fails_cleanly() {
        assert!(capture(0x7fff_fff0, false).is_err(), "no such window");
    }
}
