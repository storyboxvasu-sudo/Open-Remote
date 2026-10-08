use app_common::{DisplayInfo, MonitorDescriptor};
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;

#[cfg(target_os = "windows")]
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
#[cfg(target_os = "windows")]
use windows_capture::dxgi_duplication_api::DxgiDuplicationApi;
#[cfg(target_os = "windows")]
use windows_capture::graphics_capture_api::InternalCaptureControl;
#[cfg(target_os = "windows")]
use windows_capture::monitor::Monitor;
#[cfg(target_os = "windows")]
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("No monitor detected: {0}")]
    MonitorNotFound(String),
    #[error("Capture pipeline error: {0}")]
    PipelineError(String),
    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

#[derive(Clone)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct CaptureShared {
    pub(crate) latest_frame: Arc<RwLock<Option<Arc<RawFrame>>>>,
    pub(crate) frame_counter: Arc<AtomicU64>,
}

#[cfg(target_os = "windows")]
struct FrameReceiverHandler {
    shared: CaptureShared,
}

#[cfg(target_os = "windows")]
impl GraphicsCaptureApiHandler for FrameReceiverHandler {
    type Flags = CaptureShared;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { shared: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let w = frame.width();
        let h = frame.height();

        if let Ok(raw_buffer) = frame.buffer() {
            let mut temp_buf = Vec::new();
            let nopad = raw_buffer.as_nopadding_buffer(&mut temp_buf);
            let frame_arc = Arc::new(RawFrame {
                width: w,
                height: h,
                data: nopad.to_vec(),
            });

            *self.shared.latest_frame.write() = Some(Arc::clone(&frame_arc));
            self.shared.frame_counter.fetch_add(1, Ordering::Release);
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
enum BackendControl {
    Wgc(CaptureControl<FrameReceiverHandler, Box<dyn std::error::Error + Send + Sync>>),
    Dxgi {
        stop_flag: Arc<AtomicBool>,
        thread_handle: Option<std::thread::JoinHandle<()>>,
    },
    Gdi {
        stop_flag: Arc<AtomicBool>,
        thread_handle: Option<std::thread::JoinHandle<()>>,
    },
}

#[cfg(target_os = "windows")]
impl BackendControl {
    pub fn stop(self) {
        match self {
            Self::Wgc(ctrl) => {
                let _ = ctrl.stop();
            }
            Self::Dxgi {
                stop_flag,
                mut thread_handle,
            } => {
                stop_flag.store(true, Ordering::SeqCst);
                if let Some(handle) = thread_handle.take() {
                    let _ = handle.join();
                }
            }
            Self::Gdi {
                stop_flag,
                mut thread_handle,
            } => {
                stop_flag.store(true, Ordering::SeqCst);
                if let Some(handle) = thread_handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn try_start_wgc(
    monitor: Monitor,
    shared: CaptureShared,
) -> Result<CaptureControl<FrameReceiverHandler, Box<dyn std::error::Error + Send + Sync>>, String>
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Attempt 1: Strict borderless (IsBorderRequired = false) with hardware cursor
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            shared.clone(),
        );

        match FrameReceiverHandler::start_free_threaded(settings) {
            Ok(ctrl) => Ok(ctrl),
            Err(e) => {
                // Attempt 2: Strict borderless (IsBorderRequired = false) with default cursor
                let settings_fallback = Settings::new(
                    monitor,
                    CursorCaptureSettings::Default,
                    DrawBorderSettings::WithoutBorder,
                    SecondaryWindowSettings::Default,
                    MinimumUpdateIntervalSettings::Default,
                    DirtyRegionSettings::Default,
                    ColorFormat::Bgra8,
                    shared,
                );
                FrameReceiverHandler::start_free_threaded(settings_fallback)
                    .map_err(|e2| {
                        format!(
                            "WGC WithoutBorder unsupported or failed: {:?} (attempt 2: {:?})",
                            e, e2
                        )
                    })
            }
        }
    }))
    .map_err(|p| format!("WGC panicked during initialization: {:?}", p))
    .and_then(|r| r)
}

#[cfg(target_os = "windows")]
fn try_start_dxgi(monitor: Monitor, shared: CaptureShared) -> Result<BackendControl, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut dup = DxgiDuplicationApi::new(monitor)
            .map_err(|e| format!("DXGI initialization failed: {:?}", e))?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        let thread_handle = std::thread::Builder::new()
            .name("openremote-dxgi-capture".to_string())
            .spawn(move || {
                let mut temp_buf = Vec::new();
                while !stop_clone.load(Ordering::Relaxed) {
                    match dup.acquire_next_frame(33) {
                        Ok(mut frame) => {
                            let w = frame.width();
                            let h = frame.height();
                            if let Ok(frame_buffer) = frame.buffer() {
                                let nopad = frame_buffer.as_nopadding_buffer(&mut temp_buf);
                                let frame_arc = Arc::new(RawFrame {
                                    width: w,
                                    height: h,
                                    data: nopad.to_vec(),
                                });
                                *shared.latest_frame.write() = Some(frame_arc);
                                shared.frame_counter.fetch_add(1, Ordering::Release);
                            }
                        }
                        Err(windows_capture::dxgi_duplication_api::Error::Timeout) => {}
                        Err(windows_capture::dxgi_duplication_api::Error::AccessLost) => {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            if let Ok(new_dup) = DxgiDuplicationApi::new(monitor) {
                                dup = new_dup;
                            }
                        }
                        Err(_) => {
                            std::thread::sleep(std::time::Duration::from_millis(16));
                        }
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn DXGI capture thread: {:?}", e))?;

        Ok(BackendControl::Dxgi {
            stop_flag,
            thread_handle: Some(thread_handle),
        })
    }))
    .map_err(|p| format!("DXGI panicked during initialization: {:?}", p))
    .and_then(|r| r)
}

#[cfg(target_os = "windows")]
fn start_gdi(x: i32, y: i32, width: u32, height: u32, shared: CaptureShared) -> BackendControl {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop_flag);

    let thread_handle = std::thread::Builder::new()
        .name("openremote-gdi-capture".to_string())
        .spawn(move || {
            use windows_sys::Win32::Graphics::Gdi::{
                BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
                ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
                HGDIOBJ, SRCCOPY, CAPTUREBLT,
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                DrawIconEx, GetCursorInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL,
            };

            let hdc_screen = unsafe { GetDC(std::ptr::null_mut()) };
            if hdc_screen.is_null() {
                return;
            }
            let hdc_mem = unsafe { CreateCompatibleDC(hdc_screen) };
            if hdc_mem.is_null() {
                unsafe { ReleaseDC(std::ptr::null_mut(), hdc_screen) };
                return;
            }

            let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width as i32;
            bmi.bmiHeader.biHeight = -(height as i32);
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let hbitmap =
                unsafe { CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits_ptr, std::ptr::null_mut(), 0) };

            if hbitmap.is_null() || bits_ptr.is_null() {
                unsafe {
                    DeleteDC(hdc_mem);
                    ReleaseDC(std::ptr::null_mut(), hdc_screen);
                }
                return;
            }

            let old_obj = unsafe { SelectObject(hdc_mem, hbitmap as HGDIOBJ) };
            let frame_byte_len = (width * height * 4) as usize;

            while !stop_clone.load(Ordering::Relaxed) {
                let start_time = std::time::Instant::now();

                let ok = unsafe {
                    BitBlt(
                        hdc_mem,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        hdc_screen,
                        x,
                        y,
                        SRCCOPY | CAPTUREBLT,
                    )
                };

                if ok != 0 {
                    let mut ci: CURSORINFO = unsafe { std::mem::zeroed() };
                    ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
                    if unsafe { GetCursorInfo(&mut ci) } != 0 && (ci.flags & CURSOR_SHOWING) != 0 {
                        let cur_x = ci.ptScreenPos.x - x;
                        let cur_y = ci.ptScreenPos.y - y;
                        if cur_x >= 0
                            && cur_x < width as i32
                            && cur_y >= 0
                            && cur_y < height as i32
                        {
                            unsafe {
                                DrawIconEx(
                                    hdc_mem,
                                    cur_x,
                                    cur_y,
                                    ci.hCursor,
                                    0,
                                    0,
                                    0,
                                    std::ptr::null_mut(),
                                    DI_NORMAL,
                                );
                            }
                        }
                    }

                    let pixel_slice = unsafe {
                        std::slice::from_raw_parts(bits_ptr as *const u8, frame_byte_len)
                    };
                    let frame_arc = Arc::new(RawFrame {
                        width,
                        height,
                        data: pixel_slice.to_vec(),
                    });
                    *shared.latest_frame.write() = Some(frame_arc);
                    shared.frame_counter.fetch_add(1, Ordering::Release);
                }

                let elapsed = start_time.elapsed();
                let target_interval = std::time::Duration::from_millis(30);
                if let Some(remaining) = target_interval.checked_sub(elapsed) {
                    std::thread::sleep(remaining);
                } else {
                    std::thread::yield_now();
                }
            }

            unsafe {
                SelectObject(hdc_mem, old_obj);
                DeleteObject(hbitmap as HGDIOBJ);
                DeleteDC(hdc_mem);
                ReleaseDC(std::ptr::null_mut(), hdc_screen);
            }
        })
        .expect("spawn gdi capture thread");

    BackendControl::Gdi {
        stop_flag,
        thread_handle: Some(thread_handle),
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
pub struct Win32MonitorEntry {
    pub hmonitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
    pub descriptor: MonitorDescriptor,
}

#[cfg(target_os = "windows")]
pub fn enumerate_win32_monitors_full() -> Vec<Win32MonitorEntry> {
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };

    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam as *mut Vec<Win32MonitorEntry>);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;

        if GetMonitorInfoW(hmon, &mut mi) != 0 {
            let index = list.len();
            let width = (mi.rcMonitor.right - mi.rcMonitor.left).max(1) as u32;
            let height = (mi.rcMonitor.bottom - mi.rcMonitor.top).max(1) as u32;
            let is_primary = (mi.dwFlags & 1) != 0;

            let friendly_name = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mon = Monitor::from_raw_hmonitor(hmon as *mut std::ffi::c_void);
                mon.name().ok()
            }))
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("Display {}", index + 1));

            list.push(Win32MonitorEntry {
                hmonitor: hmon,
                descriptor: MonitorDescriptor {
                    index,
                    name: friendly_name,
                    width,
                    height,
                    is_primary,
                    x: mi.rcMonitor.left,
                    y: mi.rcMonitor.top,
                },
            });
        }
        TRUE
    }

    let mut list: Vec<Win32MonitorEntry> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_proc),
            &mut list as *mut _ as LPARAM,
        );
    }

    // Sort: Primary monitor first, then left-to-right (x coordinate), then top-to-bottom (y coordinate)
    list.sort_by(|a: &Win32MonitorEntry, b: &Win32MonitorEntry| {
        b.descriptor.is_primary.cmp(&a.descriptor.is_primary)
            .then_with(|| a.descriptor.x.cmp(&b.descriptor.x))
            .then_with(|| a.descriptor.y.cmp(&b.descriptor.y))
    });

    for (idx, entry) in list.iter_mut().enumerate() {
        entry.descriptor.index = idx;
        if entry.descriptor.name.is_empty() || entry.descriptor.name.starts_with("Display ") {
            if entry.descriptor.is_primary {
                entry.descriptor.name = format!("Display {} (Primary)", idx + 1);
            } else {
                entry.descriptor.name = format!("Display {}", idx + 1);
            }
        } else if entry.descriptor.is_primary && !entry.descriptor.name.contains("Primary") {
            entry.descriptor.name = format!("{} (Primary)", entry.descriptor.name);
        }
    }

    if list.is_empty() {
        list.push(Win32MonitorEntry {
            hmonitor: std::ptr::null_mut(),
            descriptor: MonitorDescriptor {
                index: 0,
                name: "Display 1 (Primary)".to_string(),
                width: 1920,
                height: 1080,
                is_primary: true,
                x: 0,
                y: 0,
            },
        });
    }

    list
}

#[cfg(target_os = "macos")]
pub mod macos_capture {
    use super::*;

    pub type CGDirectDisplayID = u32;
    pub type CGError = i32;
    pub type CGFloat = f64;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGPoint {
        pub x: CGFloat,
        pub y: CGFloat,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGSize {
        pub width: CGFloat,
        pub height: CGFloat,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    pub enum CGImage {}
    pub type CGImageRef = *mut CGImage;

    pub enum CGDataProvider {}
    pub type CGDataProviderRef = *mut CGDataProvider;

    pub enum CFData {}
    pub type CFDataRef = *mut CFData;

    pub type CFTypeRef = *const std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    #[link(name = "ScreenCaptureKit", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut CGDirectDisplayID,
            display_count: *mut u32,
        ) -> CGError;

        pub fn CGMainDisplayID() -> CGDirectDisplayID;
        pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        pub fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
        pub fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;
        pub fn CGDisplayCreateImage(display: CGDirectDisplayID) -> CGImageRef;
        pub fn CGGetOnlineDisplayList(
            max_displays: u32,
            online_displays: *mut CGDirectDisplayID,
            display_count: *mut u32,
        ) -> CGError;
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
        pub fn CGImageRelease(image: CGImageRef);
        pub fn CGImageGetWidth(image: CGImageRef) -> usize;
        pub fn CGImageGetHeight(image: CGImageRef) -> usize;
        pub fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
        pub fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
        pub fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;
    }

    extern "C" {
        pub fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
        pub fn CFDataGetLength(data: CFDataRef) -> isize;
        pub fn CFRelease(cf: CFTypeRef);
    }

    pub fn ensure_screen_capture_permission() -> bool {
        unsafe {
            if CGPreflightScreenCaptureAccess() {
                true
            } else {
                CGRequestScreenCaptureAccess()
            }
        }
    }

    pub fn enumerate_monitors_macos() -> Vec<MonitorDescriptor> {
        let _ = ensure_screen_capture_permission();

        let mut displays = [0u32; 16];
        let mut display_count = 0u32;
        let mut err = unsafe {
            CGGetActiveDisplayList(16, displays.as_mut_ptr(), &mut display_count)
        };

        if err != 0 || display_count == 0 {
            err = unsafe {
                CGGetOnlineDisplayList(16, displays.as_mut_ptr(), &mut display_count)
            };
        }

        let main_id = unsafe { CGMainDisplayID() };
        let mut list = Vec::new();

        if (err == 0 || display_count > 0) && display_count > 0 {
            for i in 0..(display_count as usize) {
                let d_id = displays[i];
                let bounds = unsafe { CGDisplayBounds(d_id) };

                // On Retina displays, CGDisplayPixelsWide/High returns physical pixels
                let pixel_w = unsafe { CGDisplayPixelsWide(d_id) as u32 };
                let pixel_h = unsafe { CGDisplayPixelsHigh(d_id) as u32 };
                let bounds_w = if bounds.size.width > 0.0 { bounds.size.width as u32 } else { 0 };
                let bounds_h = if bounds.size.height > 0.0 { bounds.size.height as u32 } else { 0 };

                let width = pixel_w.max(bounds_w).max(640);
                let height = pixel_h.max(bounds_h).max(480);

                let is_primary = d_id == main_id;
                let index = i;
                let name = if is_primary {
                    format!("Display {} (Built-in / Primary)", index + 1)
                } else {
                    format!("Display {} (External)", index + 1)
                };

                list.push(MonitorDescriptor {
                    index,
                    name,
                    width,
                    height,
                    is_primary,
                    x: bounds.origin.x as i32,
                    y: bounds.origin.y as i32,
                });
            }
        }

        // If display list was empty, attempt querying main display directly
        if list.is_empty() && main_id != 0 {
            let bounds = unsafe { CGDisplayBounds(main_id) };
            let pixel_w = unsafe { CGDisplayPixelsWide(main_id) as u32 };
            let pixel_h = unsafe { CGDisplayPixelsHigh(main_id) as u32 };
            let bounds_w = if bounds.size.width > 0.0 { bounds.size.width as u32 } else { 0 };
            let bounds_h = if bounds.size.height > 0.0 { bounds.size.height as u32 } else { 0 };

            let width = pixel_w.max(bounds_w).max(1280);
            let height = pixel_h.max(bounds_h).max(800);

            list.push(MonitorDescriptor {
                index: 0,
                name: "Display 1 (Primary)".to_string(),
                width,
                height,
                is_primary: true,
                x: bounds.origin.x as i32,
                y: bounds.origin.y as i32,
            });
        }

        if list.is_empty() {
            list.push(MonitorDescriptor {
                index: 0,
                name: "Primary Display".to_string(),
                width: 1920,
                height: 1080,
                is_primary: true,
                x: 0,
                y: 0,
            });
        }

        list
    }

    pub fn get_display_id_by_index(index: usize) -> CGDirectDisplayID {
        let mut displays = [0u32; 16];
        let mut display_count = 0u32;
        let err = unsafe { CGGetActiveDisplayList(16, displays.as_mut_ptr(), &mut display_count) };
        if err == 0 && display_count > 0 {
            let idx = if index < display_count as usize {
                index
            } else if index > 0 && (index - 1) < display_count as usize {
                index - 1
            } else {
                0
            };
            displays[idx]
        } else {
            unsafe { CGMainDisplayID() }
        }
    }

    pub struct MacOsControl {
        pub stop_flag: Arc<AtomicBool>,
        pub thread_handle: Option<std::thread::JoinHandle<()>>,
    }

    impl MacOsControl {
        pub fn stop(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }

    pub(crate) fn start_macos_capture(display_id: CGDirectDisplayID, shared: CaptureShared) -> MacOsControl {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        let thread_handle = std::thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                let start_time = std::time::Instant::now();

                unsafe {
                    let image = CGDisplayCreateImage(display_id);
                    if !image.is_null() {
                        let w = CGImageGetWidth(image) as u32;
                        let h = CGImageGetHeight(image) as u32;
                        let bpr = CGImageGetBytesPerRow(image);
                        let provider = CGImageGetDataProvider(image);

                        if !provider.is_null() {
                            let data_ref = CGDataProviderCopyData(provider);
                            if !data_ref.is_null() {
                                let ptr = CFDataGetBytePtr(data_ref);
                                let len = CFDataGetLength(data_ref) as usize;
                                let target_len = (w * h * 4) as usize;

                                if !ptr.is_null() && len >= (h as usize) * bpr {
                                    let mut buffer = Vec::with_capacity(target_len);
                                    if bpr == (w * 4) as usize {
                                        let slice = std::slice::from_raw_parts(ptr, target_len);
                                        buffer.extend_from_slice(slice);
                                    } else {
                                        for row in 0..h {
                                            let row_offset = (row as usize) * bpr;
                                            let row_slice = std::slice::from_raw_parts(
                                                ptr.add(row_offset),
                                                (w * 4) as usize,
                                            );
                                            buffer.extend_from_slice(row_slice);
                                        }
                                    }

                                    let raw_frame = Arc::new(RawFrame {
                                        width: w,
                                        height: h,
                                        data: buffer,
                                    });

                                    *shared.latest_frame.write() = Some(raw_frame);
                                    shared.frame_counter.fetch_add(1, Ordering::Release);
                                }
                                CFRelease(data_ref as CFTypeRef);
                            }
                        }
                        CGImageRelease(image);
                    }
                }

                // Smooth 60 FPS pacing (~16ms)
                let elapsed = start_time.elapsed();
                let frame_budget = std::time::Duration::from_millis(16);
                if elapsed < frame_budget {
                    std::thread::sleep(frame_budget - elapsed);
                }
            }
        });

        MacOsControl {
            stop_flag,
            thread_handle: Some(thread_handle),
        }
    }
}

pub struct ScreenCapturer {
    shared: CaptureShared,
    #[cfg(target_os = "windows")]
    control: Arc<Mutex<Option<BackendControl>>>,
    #[cfg(target_os = "macos")]
    control: Arc<Mutex<Option<macos_capture::MacOsControl>>>,
    active_monitor_index: Arc<AtomicUsize>,
    pub width: u32,
    pub height: u32,
}

impl ScreenCapturer {
    pub fn enumerate_monitors() -> Vec<MonitorDescriptor> {
        #[cfg(target_os = "windows")]
        {
            enumerate_win32_monitors_full()
                .into_iter()
                .map(|e| e.descriptor)
                .collect()
        }
        #[cfg(target_os = "macos")]
        {
            macos_capture::enumerate_monitors_macos()
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            vec![MonitorDescriptor {
                index: 0,
                name: "Primary Display".to_string(),
                width: 1920,
                height: 1080,
                is_primary: true,
                x: 0,
                y: 0,
            }]
        }
    }

    pub fn enumerate_displays() -> Vec<DisplayInfo> {
        Self::enumerate_monitors()
            .into_iter()
            .map(DisplayInfo::from)
            .collect()
    }

    pub fn switch_capture_display(&self, display_id: usize) -> Result<(), CaptureError> {
        self.switch_monitor(display_id)
    }

    pub fn request_screen_capture_permission() -> bool {
        #[cfg(target_os = "macos")]
        {
            macos_capture::ensure_screen_capture_permission()
        }
        #[cfg(not(target_os = "macos"))]
        {
            true
        }
    }

    pub fn new() -> Result<Self, CaptureError> {
        Self::new_with_monitor_index(0)
    }

    pub fn new_with_monitor_index(index: usize) -> Result<Self, CaptureError> {
        #[cfg(target_os = "windows")]
        {
            let all_monitors = enumerate_win32_monitors_full();
            let target_idx = if index < all_monitors.len() {
                index
            } else if index > 0 && (index - 1) < all_monitors.len() {
                index - 1
            } else {
                0
            };

            let target_entry = &all_monitors[target_idx];
            let mon_desc = &target_entry.descriptor;
            let width = mon_desc.width;
            let height = mon_desc.height;

            let shared = CaptureShared {
                latest_frame: Arc::new(RwLock::new(None)),
                frame_counter: Arc::new(AtomicU64::new(0)),
            };

            let backend = if !target_entry.hmonitor.is_null() {
                let mon = Monitor::from_raw_hmonitor(target_entry.hmonitor as *mut std::ffi::c_void);
                match try_start_wgc(mon, shared.clone()) {
                    Ok(ctrl) => {
                        eprintln!("[core-capture] Active backend on {}: Windows Graphics Capture (WGC)", mon_desc.name);
                        BackendControl::Wgc(ctrl)
                    }
                    Err(wgc_err) => {
                        eprintln!(
                            "[core-capture] WGC unavailable for {} ({:?}), trying DXGI Duplication...",
                            mon_desc.name, wgc_err
                        );
                        match try_start_dxgi(mon, shared.clone()) {
                            Ok(ctrl) => {
                                eprintln!("[core-capture] Active backend on {}: DXGI Desktop Duplication", mon_desc.name);
                                ctrl
                            }
                            Err(dxgi_err) => {
                                eprintln!(
                                    "[core-capture] DXGI unavailable for {} ({:?}), falling back to Win32 GDI at ({}, {})...",
                                    mon_desc.name, dxgi_err, mon_desc.x, mon_desc.y
                                );
                                start_gdi(mon_desc.x, mon_desc.y, width, height, shared.clone())
                            }
                        }
                    }
                }
            } else {
                start_gdi(mon_desc.x, mon_desc.y, width, height, shared.clone())
            };

            Ok(Self {
                shared,
                control: Arc::new(Mutex::new(Some(backend))),
                active_monitor_index: Arc::new(AtomicUsize::new(target_idx)),
                width,
                height,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let monitors = Self::enumerate_monitors();
            let target_idx = if index < monitors.len() {
                index
            } else if index > 0 && (index - 1) < monitors.len() {
                index - 1
            } else {
                0
            };

            let mon_desc = monitors.get(target_idx).cloned().unwrap_or(MonitorDescriptor {
                index: 0,
                name: "Primary Display".to_string(),
                width: 1920,
                height: 1080,
                is_primary: true,
                x: 0,
                y: 0,
            });

            let width = mon_desc.width;
            let height = mon_desc.height;

            let shared = CaptureShared {
                latest_frame: Arc::new(RwLock::new(None)),
                frame_counter: Arc::new(AtomicU64::new(0)),
            };

            let display_id = macos_capture::get_display_id_by_index(target_idx);
            let ctrl = macos_capture::start_macos_capture(display_id, shared.clone());

            Ok(Self {
                shared,
                control: Arc::new(Mutex::new(Some(ctrl))),
                active_monitor_index: Arc::new(AtomicUsize::new(target_idx)),
                width,
                height,
            })
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let shared = CaptureShared {
                latest_frame: Arc::new(RwLock::new(None)),
                frame_counter: Arc::new(AtomicU64::new(0)),
            };
            Ok(Self {
                shared,
                active_monitor_index: Arc::new(AtomicUsize::new(index)),
                width: 1920,
                height: 1080,
            })
        }
    }

    pub fn switch_monitor(&self, index: usize) -> Result<(), CaptureError> {
        #[cfg(target_os = "windows")]
        {
            let all_monitors = enumerate_win32_monitors_full();
            let target_idx = if index < all_monitors.len() {
                index
            } else if index > 0 && (index - 1) < all_monitors.len() {
                index - 1
            } else {
                0
            };

            let target_entry = &all_monitors[target_idx];
            let mon_desc = &target_entry.descriptor;
            let width = mon_desc.width;
            let height = mon_desc.height;

            let new_backend = if !target_entry.hmonitor.is_null() {
                let mon = Monitor::from_raw_hmonitor(target_entry.hmonitor as *mut std::ffi::c_void);
                match try_start_wgc(mon, self.shared.clone()) {
                    Ok(ctrl) => {
                        eprintln!("[core-capture] Switched to {}: Windows Graphics Capture (WGC)", mon_desc.name);
                        BackendControl::Wgc(ctrl)
                    }
                    Err(wgc_err) => {
                        eprintln!(
                            "[core-capture] WGC switch unavailable for {} ({:?}), trying DXGI Duplication...",
                            mon_desc.name, wgc_err
                        );
                        match try_start_dxgi(mon, self.shared.clone()) {
                            Ok(ctrl) => {
                                eprintln!("[core-capture] Switched to {}: DXGI Desktop Duplication", mon_desc.name);
                                ctrl
                            }
                            Err(dxgi_err) => {
                                eprintln!(
                                    "[core-capture] DXGI switch unavailable for {} ({:?}), falling back to Win32 GDI at ({}, {})...",
                                    mon_desc.name, dxgi_err, mon_desc.x, mon_desc.y
                                );
                                start_gdi(mon_desc.x, mon_desc.y, width, height, self.shared.clone())
                            }
                        }
                    }
                }
            } else {
                start_gdi(mon_desc.x, mon_desc.y, width, height, self.shared.clone())
            };

            let mut guard = self.control.lock();
            if let Some(old_backend) = guard.take() {
                old_backend.stop();
            }
            *guard = Some(new_backend);
            self.active_monitor_index.store(target_idx, Ordering::SeqCst);
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let monitors = Self::enumerate_monitors();
            let target_idx = if index < monitors.len() {
                index
            } else if index > 0 && (index - 1) < monitors.len() {
                index - 1
            } else {
                0
            };

            let display_id = macos_capture::get_display_id_by_index(target_idx);
            let new_ctrl = macos_capture::start_macos_capture(display_id, self.shared.clone());

            let mut guard = self.control.lock();
            if let Some(mut old_ctrl) = guard.take() {
                old_ctrl.stop();
            }
            *guard = Some(new_ctrl);
            self.active_monitor_index.store(target_idx, Ordering::SeqCst);
            Ok(())
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            self.active_monitor_index.store(index, Ordering::SeqCst);
            Ok(())
        }
    }

    pub fn active_monitor_index(&self) -> usize {
        self.active_monitor_index.load(Ordering::Relaxed)
    }

    pub fn get_latest_frame(&self) -> Option<Arc<RawFrame>> {
        self.shared.latest_frame.read().clone()
    }

    pub fn frame_counter(&self) -> u64 {
        self.shared.frame_counter.load(Ordering::Acquire)
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if let Some(backend) = self.control.lock().take() {
                backend.stop();
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(mut ctrl) = self.control.lock().take() {
                ctrl.stop();
            }
        }
    }
}

