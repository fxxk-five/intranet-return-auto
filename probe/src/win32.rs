//! Win32 薄封装：打印机、DPI、磁盘、注册表、提权、进程（§7.2、§7.8.3）。
//!
//! 窗口枚举与截图在 §7.8.2 的实现阶段追加（见文件末尾的预留注释）。

use std::path::PathBuf;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 从宽字符缓冲读字符串（到 NUL 或结尾为止）
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// §7.1：两个候选路径
pub fn edge_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
        PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
    ]
}

pub fn find_edge() -> Option<PathBuf> {
    edge_paths().into_iter().find(|p| p.exists())
}

pub fn dpi() -> u32 {
    unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }
}

pub fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = wide(&path.to_string_lossy());
    let mut free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(w.as_ptr()),
            Some(&mut free),
            None,
            None,
        )
    };
    ok.ok().map(|_| free)
}

pub fn reg_dword(subkey: &str, name: &str) -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};

    let sub = wide(subkey);
    let nm = wide(name);
    let mut data: u32 = 0;
    let mut cb: u32 = 4;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            PCWSTR(nm.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut cb),
        )
    };
    (rc == ERROR_SUCCESS).then_some(data)
}

pub fn reg_string(subkey: &str, name: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    let sub = wide(subkey);
    let nm = wide(name);
    let mut buf = vec![0u16; 1024];
    let mut cb: u32 = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            PCWSTR(nm.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut cb),
        )
    };
    (rc == ERROR_SUCCESS).then(|| from_wide(&buf))
}

pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TokenUser, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        let _ = TokenUser; // 保留，后续如需用户名可扩展
        ok.is_ok() && elev.TokenIsElevated != 0
    }
}

/// 枚举本地 + 网络打印机名（§7.8.3）
pub fn printers() -> Vec<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Printing::{EnumPrintersW, PRINTER_INFO_4W};

    // PRINTER_ENUM_LOCAL(0x2) | PRINTER_ENUM_CONNECTIONS(0x4)
    let flags: u32 = 0x2 | 0x4;
    let mut needed: u32 = 0;
    let mut returned: u32 = 0;
    unsafe {
        let _ = EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut returned);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; needed as usize + 4096];
        if EnumPrintersW(
            flags,
            PCWSTR::null(),
            4,
            Some(&mut buf),
            &mut needed,
            &mut returned,
        )
        .is_err()
        {
            return Vec::new();
        }
        let arr = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, returned as usize);
        arr.iter()
            .map(|pi| {
                if pi.pPrinterName.is_null() {
                    String::new()
                } else {
                    pi.pPrinterName.to_string().unwrap_or_default()
                }
            })
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// 默认打印机全名（`GetDefaultPrinterW`）
pub fn default_printer() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Graphics::Printing::GetDefaultPrinterW;

    let mut buf = vec![0u16; 1024];
    let mut size: u32 = buf.len() as u32;
    unsafe {
        if GetDefaultPrinterW(PWSTR(buf.as_mut_ptr()), &mut size).as_bool() {
            Some(from_wide(&buf))
        } else {
            None
        }
    }
}

/// 虚拟打印机判定：现场如果默认打印机是 PDF 类，Q7 直接结案（§7.8.3）
pub fn is_virtual_printer(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "pdf",
        "xps",
        "onenote",
        "fax",
        "virtual",
        "adobe",
        "foxit",
        "wps",
        "microsoft print to",
    ]
    .iter()
    .any(|k| n.contains(k))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct JobInfo {
    pub job_id: u32,
    pub document: String,
    pub total_pages: u32,
    pub pages_printed: u32,
    pub status: u32,
}

/// 读默认打印机的作业队列（被动观测，绝不 AddJob / StartDoc，§7.8.3）
pub fn jobs(printer: &str) -> Vec<JobInfo> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Printing::{ClosePrinter, EnumJobsW, OpenPrinterW, JOB_INFO_1W};

    let w = wide(printer);
    let mut h = HANDLE::default();
    unsafe {
        if OpenPrinterW(PCWSTR(w.as_ptr()), &mut h, None).is_err() {
            return Vec::new();
        }
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;
        let _ = EnumJobsW(h, 0, 0xFFFF_FFFF, 1, None, &mut needed, &mut returned);
        if needed == 0 {
            let _ = ClosePrinter(h);
            return Vec::new();
        }
        let mut buf = vec![0u8; needed as usize + 8192];
        let ok = EnumJobsW(
            h,
            0,
            0xFFFF_FFFF,
            1,
            Some(&mut buf),
            &mut needed,
            &mut returned,
        );
        let mut out = Vec::new();
        if ok.is_ok() {
            let arr = std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, returned as usize);
            for j in arr {
                out.push(JobInfo {
                    job_id: j.JobId,
                    document: if j.pDocument.is_null() {
                        String::new()
                    } else {
                        j.pDocument.to_string().unwrap_or_default()
                    },
                    total_pages: j.TotalPages,
                    pages_printed: j.PagesPrinted,
                    status: j.Status,
                });
            }
        }
        let _ = ClosePrinter(h);
        out
    }
}

/// 指定名字的进程 PID 列表（`tasklist` 的等价实现）
pub fn process_pids(exe_name: &str) -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            let name = from_wide(&pe.szExeFile);
            if name.eq_ignore_ascii_case(exe_name) {
                out.push(pe.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// 父进程 PID（`PrintWindow` 归属判定要用父链，§7.8.2）
pub fn parent_pid(pid: u32) -> Option<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = None;
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            if pe.th32ProcessID == pid {
                result = Some(pe.th32ParentProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
        result
    }
}

/// 进程可执行文件名（沿父链找 msedge.exe 时用）
pub fn process_exe_name(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = None;
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            if pe.th32ProcessID == pid {
                result = Some(from_wide(&pe.szExeFile));
                break;
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
        result
    }
}

/// 控制台切 UTF-8（§15.3）
pub fn console_utf8() {
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleOutputCP(65001);
    }
}

// ---------------------------------------------------------------------------
// 窗口枚举与原生截图（§7.6 native_window 判定、§7.8.2）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct WinInfo {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub exe: Option<String>,
    /// [left, top, width, height]
    pub rect: [i32; 4],
    pub visible: bool,
}

/// 枚举全部顶层窗口。
pub fn list_windows() -> Vec<WinInfo> {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible, GWL_STYLE, WS_VISIBLE,
    };

    thread_local! {
        static OUT: RefCell<Vec<WinInfo>> = const { RefCell::new(Vec::new()) };
    }
    OUT.with(|o| o.borrow_mut().clear());

    unsafe extern "system" fn cb(hwnd: HWND, _l: LPARAM) -> BOOL {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut title = [0u16; 512];
            let n = GetWindowTextW(hwnd, &mut title);
            let title = from_wide(&title[..n.max(0) as usize]);
            let mut class = [0u16; 256];
            let cn = GetClassNameW(hwnd, &mut class);
            let class = from_wide(&class[..cn.max(0) as usize]);
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let visible = IsWindowVisible(hwnd).as_bool() || (style & WS_VISIBLE.0) != 0;
            OUT.with(|o| {
                o.borrow_mut().push(WinInfo {
                    hwnd: hwnd.0 as isize,
                    title,
                    class,
                    pid,
                    exe: None,
                    rect: [r.left, r.top, r.right - r.left, r.bottom - r.top],
                    visible,
                })
            });
            BOOL(1)
        }
    }

    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }

    let mut v = OUT.with(|o| o.borrow().clone());
    // 补 exe 名（窗口归属判定要用）
    let mut cache: std::collections::HashMap<u32, Option<String>> = std::collections::HashMap::new();
    for w in v.iter_mut() {
        let e = cache.entry(w.pid).or_insert_with(|| process_exe_name(w.pid));
        w.exe = e.clone();
    }
    v
}

/// 窗口是否属于 msedge.exe 进程树（§7.8.2 的归属判定）
pub fn window_belongs_to_edge(pid: u32) -> bool {
    let mut cur = Some(pid);
    let mut guard = 0;
    while let Some(p) = cur {
        if let Some(name) = process_exe_name(p) {
            if name.eq_ignore_ascii_case("msedge.exe") {
                return true;
            }
        }
        guard += 1;
        if guard > 12 {
            break;
        }
        cur = parent_pid(p);
    }
    false
}

/// `PrintWindow` + 全黑降级 `BitBlt`，返回 PNG 字节与尺寸、方法名（§7.8.2）。
pub fn capture_window(hwnd: isize) -> anyhow::Result<(Vec<u8>, u32, u32, &'static str)> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
        HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    // §7.8.2：PrintWindow 的 nFlags 必须传 PW_RENDERFULLCONTENT(=2)，
    // 传 0 在 DWM 合成下大概率返回全黑。
    const PW_RENDERFULLCONTENT: u32 = 2;

    let h = HWND(hwnd as *mut c_void);
    let mut r = RECT::default();
    unsafe {
        GetWindowRect(h, &mut r)?;
    }
    let w = (r.right - r.left).max(1);
    let ht = (r.bottom - r.top).max(1);
    if w <= 0 || ht <= 0 || w > 20000 || ht > 20000 {
        anyhow::bail!("窗口尺寸异常：{w}x{ht}");
    }

    unsafe {
        let screen_dc = GetDC(HWND(std::ptr::null_mut()));
        let mem_dc = CreateCompatibleDC(screen_dc);
        let bmp = CreateCompatibleBitmap(screen_dc, w, ht);
        if bmp.is_invalid() || mem_dc.is_invalid() {
            ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            anyhow::bail!("创建兼容位图失败");
        }
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.0));

        let mut method = "printwindow";
        let _ = PrintWindow(h, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));

        let mut buf = vec![0u8; (w * ht * 4) as usize];
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -ht, // 负值 = top-down，否则图像上下颠倒
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = GetDIBits(
            mem_dc,
            bmp,
            0,
            ht as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );

        // 全黑则降级：从桌面 DC 拷（DWM 合成下 PrintWindow 常返回全黑）
        let all_black = buf.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
        if got == 0 || all_black {
            method = "bitblt_fallback";
            let _ = BitBlt(mem_dc, 0, 0, w, ht, screen_dc, r.left, r.top, SRCCOPY);
            let got2 = GetDIBits(
                mem_dc,
                bmp,
                0,
                ht as u32,
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut bi,
                DIB_RGB_COLORS,
            );
            if got2 == 0 {
                SelectObject(mem_dc, old);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(mem_dc);
                ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
                anyhow::bail!("GetDIBits 失败，窗口截图不可用");
            }
        }

        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);

        // BGRA → RGBA
        let mut rgba = buf;
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let img = image::RgbaImage::from_raw(w as u32, ht as u32, rgba)
            .ok_or_else(|| anyhow::anyhow!("构造图像失败 {w}x{ht}"))?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)?;
        Ok((out.into_inner(), w as u32, ht as u32, method))
    }
}
