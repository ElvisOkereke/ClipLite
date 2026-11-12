//! Win32 window + D3D11 renderer for the fake game.
use super::{iterations_for, Args, FrametimeWriter, Mode, WINDOW_TITLE};
use anyhow::{anyhow, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{s, w, Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, TRUE, FALSE, WPARAM};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D_DRIVER_TYPE_HARDWARE, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

static QUIT: AtomicBool = AtomicBool::new(false);

const SHADER: &str = r#"
cbuffer Params : register(b0) { float time; float iters; float height; float pad; };
float4 vs_main(uint id : SV_VertexID) : SV_POSITION {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 ps_main(float4 pos : SV_POSITION) : SV_TARGET {
    float3 hue = 0.5 + 0.5 * cos(time + float3(0, 2.094, 4.188));
    float barY = frac(time) * height;
    float bar = abs(pos.y - barY) < height * 0.03 ? 1.0 : 0.0;
    float acc = 0;
    for (uint i = 0; i < (uint)iters; i++) { acc = sin(acc + pos.x * 0.001 + i * 0.0001); }
    return float4(lerp(hue, float3(1, 1, 1), bar) + acc * 1e-6, 1);
}
"#;

#[repr(C)]
struct Params {
    time: f32,
    iters: f32,
    height: f32,
    pad: f32,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            QUIT.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn on_ctrl(_ctrl_type: u32) -> BOOL {
    QUIT.store(true, Ordering::SeqCst);
    TRUE
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    unsafe {
        let mut code: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        let r = D3DCompile(
            SHADER.as_ptr() as *const _,
            SHADER.len(),
            s!("fakegame.hlsl"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        );
        if let Err(e) = r {
            let msg = errors
                .map(|b| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned())
                .unwrap_or_default();
            return Err(anyhow!("shader compile failed: {e} {msg}"));
        }
        let code = code.ok_or_else(|| anyhow!("no shader bytecode"))?;
        Ok(std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()).to_vec())
    }
}

struct Gfx {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain,
    rtv: Option<ID3D11RenderTargetView>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    width: u32,
    height: u32,
}

impl Gfx {
    fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let desc = DXGI_SWAP_CHAIN_DESC {
                BufferDesc: DXGI_MODE_DESC {
                    Width: width,
                    Height: height,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    ..Default::default()
                },
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 1,
                OutputWindow: hwnd,
                Windowed: TRUE,
                SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
                Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
            };
            let mut swap = None;
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDeviceAndSwapChain(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut swap),
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .context("D3D11CreateDeviceAndSwapChain")?;
            let (device, ctx, swap): (ID3D11Device, ID3D11DeviceContext, IDXGISwapChain) =
                (device.unwrap(), ctx.unwrap(), swap.unwrap());

            let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut cb))?;

            let mut g = Gfx {
                device,
                ctx,
                swap,
                rtv: None,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                width,
                height,
            };
            g.create_rtv()?;
            Ok(g)
        }
    }

    fn create_rtv(&mut self) -> Result<()> {
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            self.width = desc.Width;
            self.height = desc.Height;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&back.cast::<ID3D11Resource>()?, None, Some(&mut rtv))?;
            self.rtv = rtv;
        }
        Ok(())
    }

    /// Resize the swap chain to match the window/output (required after a
    /// fullscreen transition).
    fn resize(&mut self) -> Result<()> {
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            self.rtv = None;
            self.swap.ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH)?;
        }
        self.create_rtv()
    }

    fn set_fullscreen(&mut self, on: bool) -> Result<()> {
        unsafe { self.swap.SetFullscreenState(if on { TRUE } else { FALSE }, None)? };
        self.resize()
    }

    fn is_fullscreen(&self) -> bool {
        let mut fs = BOOL(0);
        unsafe { self.swap.GetFullscreenState(Some(&mut fs), None).ok() };
        fs.as_bool()
    }

    fn draw(&self, time: f32, iters: u32, vsync: bool) {
        unsafe {
            let rtv = self.rtv.clone();
            self.ctx.OMSetRenderTargets(Some(&[rtv.clone()]), None);
            let vp = D3D11_VIEWPORT {
                Width: self.width as f32,
                Height: self.height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            };
            self.ctx.RSSetViewports(Some(&[vp]));
            let params = Params { time, iters: iters as f32, height: self.height as f32, pad: 0.0 };
            self.ctx.UpdateSubresource(&self.cb, 0, None, &params as *const _ as *const _, 0, 0);
            if let Some(rtv) = &rtv {
                self.ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            self.ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs, None);
            self.ctx.PSSetShader(&self.ps, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.ctx.Draw(3, 0);
            let _ = self.swap.Present(if vsync { 1 } else { 0 }, DXGI_PRESENT(0));
        }
    }
}

fn create_window(mode: Mode, width: u32, height: u32) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("ClipLiteFakeGame");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let title: Vec<u16> = WINDOW_TITLE.encode_utf16().chain(std::iter::once(0)).collect();
        let (style, w, h) = match mode {
            Mode::Windowed => (WS_OVERLAPPEDWINDOW | WS_VISIBLE, width as i32, height as i32),
            _ => (WS_POPUP | WS_VISIBLE, width as i32, height as i32),
        };
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(title.as_ptr()),
            style,
            if mode == Mode::Windowed { CW_USEDEFAULT } else { 0 },
            if mode == Mode::Windowed { CW_USEDEFAULT } else { 0 },
            w,
            h,
            None,
            None,
            instance,
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

fn spawn_cpu_load(threads: usize) -> Vec<std::thread::JoinHandle<()>> {
    (0..threads)
        .map(|_| {
            std::thread::spawn(|| {
                let mut x: u64 = 1;
                while !QUIT.load(Ordering::Relaxed) {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })
        })
        .collect()
}

pub fn run(args: Args) -> Result<()> {
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), TRUE).ok() };
    let (screen_w, screen_h) = unsafe { (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32) };
    let (width, height) = match args.mode {
        Mode::Windowed => (args.width.unwrap_or(1280), args.height.unwrap_or(720)),
        _ => (args.width.unwrap_or(screen_w), args.height.unwrap_or(screen_h)),
    };

    let hwnd = create_window(args.mode, width, height)?;
    let mut gfx = Gfx::new(hwnd, width, height)?;
    if args.mode == Mode::Exclusive {
        gfx.set_fullscreen(true).context("SetFullscreenState(TRUE)")?;
    }

    let mut frametimes = match &args.frametimes {
        Some(p) => Some(FrametimeWriter::new(std::io::BufWriter::new(std::fs::File::create(p)?))?),
        None => None,
    };
    let cpu = spawn_cpu_load(args.cpu_threads);
    let iters = iterations_for(args.gpu_load);
    let start = Instant::now();
    let deadline = (args.duration_secs > 0).then(|| start + Duration::from_secs(args.duration_secs));
    let mut last = start;
    let mut frame: u64 = 0;
    let ready_printed = Arc::new(AtomicBool::new(false));

    while !QUIT.load(Ordering::SeqCst) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    QUIT.store(true, Ordering::SeqCst);
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let now = Instant::now();
        gfx.draw(now.duration_since(start).as_secs_f32(), iters, args.vsync);
        let after = Instant::now();
        if let Some(w) = frametimes.as_mut() {
            w.record(frame, after.duration_since(start).as_secs_f64() * 1e3, after.duration_since(last).as_secs_f64() * 1e3)?;
        }
        last = after;
        frame += 1;
        if !ready_printed.swap(true, Ordering::SeqCst) {
            use std::io::Write;
            println!("READY hwnd=0x{:x} fullscreen={}", hwnd.0 as usize, gfx.is_fullscreen());
            std::io::stdout().flush().ok();
        }
    }

    QUIT.store(true, Ordering::SeqCst);
    if gfx.is_fullscreen() {
        let _ = gfx.set_fullscreen(false);
    }
    if let Some(mut w) = frametimes {
        w.flush()?;
    }
    for t in cpu {
        let _ = t.join();
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
