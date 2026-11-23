//! Keep PDF rasterization in a short-lived, resource-bounded Windows process.
use std::io::{self, Read, Write};
use std::ops::Range;
use std::sync::Arc;

use crate::content::MAX_FILE_BYTES;
use crate::pdf_preview::{self, HighlightBounds, RenderedPage};

pub(super) const WORKER_ARGUMENT: &str = "--pdf-render-worker";
const REQUEST_MAGIC: [u8; 4] = *b"OFP1";
const REPLY_MAGIC: [u8; 4] = *b"OFR1";
const MAX_PAGE: usize = 4096;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 512;
const MAX_PAGE_SIDE: u32 = 2048;

type RenderResult = Result<RenderedPage, String>;

struct Request {
    bytes: Arc<Vec<u8>>,
    page: usize,
    text: String,
    range: Range<usize>,
}

pub(super) fn render(
    bytes: Arc<Vec<u8>>,
    page: usize,
    text: &str,
    range: Range<usize>,
) -> RenderResult {
    if !valid_request(bytes.len(), page, text, &range) {
        return Err("PDF 渲染参数无效或超出大小限制".into());
    }
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|err| format!("无法定位 PDF 渲染程序：{err}"))?;
        windows::render_through(&exe, bytes, page, text, range)
    }
    #[cfg(not(windows))]
    pdf_preview::render(bytes, page, text, range)
}

#[cfg(windows)]
pub(super) fn worker_main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let Ok(request) = read_request(io::stdin().lock()) else {
        return ExitCode::FAILURE;
    };
    let result = pdf_preview::render(request.bytes, request.page, &request.text, request.range);
    let mut stdout = io::stdout().lock();
    if write_reply(&mut stdout, result)
        .and_then(|()| stdout.flush())
        .is_ok()
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn valid_request_size(file_len: usize, page: usize, text_len: usize, range: &Range<usize>) -> bool {
    (1..=MAX_FILE_BYTES).contains(&file_len)
        && (1..=MAX_PAGE).contains(&page)
        && text_len <= MAX_TEXT_BYTES
        && range.start < range.end
        && range.end <= text_len
}

fn valid_request(file_len: usize, page: usize, text: &str, range: &Range<usize>) -> bool {
    valid_request_size(file_len, page, text.len(), range)
        && text.is_char_boundary(range.start)
        && text.is_char_boundary(range.end)
}

fn write_request(
    mut output: impl Write,
    bytes: &[u8],
    page: usize,
    text: &str,
    range: Range<usize>,
) -> io::Result<()> {
    if !valid_request(bytes.len(), page, text, &range) {
        return Err(protocol_error());
    }
    output.write_all(&REQUEST_MAGIC)?;
    for value in [bytes.len(), page, text.len(), range.start, range.end] {
        write_u32(&mut output, value as u32)?;
    }
    output.write_all(text.as_bytes())?;
    output.write_all(bytes)
}

fn read_request(mut input: impl Read) -> io::Result<Request> {
    read_magic(&mut input, REQUEST_MAGIC)?;
    let file_len = read_u32(&mut input)? as usize;
    let page = read_u32(&mut input)? as usize;
    let text_len = read_u32(&mut input)? as usize;
    let range = read_u32(&mut input)? as usize..read_u32(&mut input)? as usize;
    if !valid_request_size(file_len, page, text_len, &range) {
        return Err(protocol_error());
    }
    let text = read_text(&mut input, text_len)?;
    if !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
        return Err(protocol_error());
    }
    let mut bytes = vec![0; file_len];
    input.read_exact(&mut bytes)?;
    require_end(&mut input)?;
    Ok(Request {
        bytes: Arc::new(bytes),
        page,
        text,
        range,
    })
}

fn pixel_bytes(width: u32, height: u32) -> io::Result<usize> {
    if !(1..=MAX_PAGE_SIDE).contains(&width) || !(1..=MAX_PAGE_SIDE).contains(&height) {
        return Err(protocol_error());
    }
    Ok(width as usize * height as usize * 4)
}

fn valid_highlight(bounds: HighlightBounds, width: u32, height: u32) -> bool {
    bounds.left < bounds.right
        && bounds.top < bounds.bottom
        && bounds.right <= width
        && bounds.bottom <= height
}

fn write_reply(mut output: impl Write, reply: RenderResult) -> io::Result<()> {
    output.write_all(&REPLY_MAGIC)?;
    match reply {
        Ok(page) => {
            if page.bgra.len() != pixel_bytes(page.width, page.height)?
                || page
                    .highlight
                    .is_some_and(|bounds| !valid_highlight(bounds, page.width, page.height))
            {
                return Err(protocol_error());
            }
            output.write_all(&[0])?;
            write_u32(&mut output, page.width)?;
            write_u32(&mut output, page.height)?;
            if let Some(bounds) = page.highlight {
                output.write_all(&[1])?;
                for edge in [bounds.left, bounds.top, bounds.right, bounds.bottom] {
                    write_u32(&mut output, edge)?;
                }
            } else {
                output.write_all(&[0])?;
            }
            output.write_all(&page.bgra)
        }
        Err(message) => {
            let message = if message.is_empty() {
                "PDF 原页渲染失败"
            } else {
                &message
            };
            let stop = message.floor_char_boundary(message.len().min(MAX_ERROR_BYTES));
            output.write_all(&[1])?;
            write_u32(&mut output, stop as u32)?;
            output.write_all(&message.as_bytes()[..stop])
        }
    }
}

fn read_reply(mut input: impl Read) -> io::Result<RenderResult> {
    read_magic(&mut input, REPLY_MAGIC)?;
    let reply = match read_byte(&mut input)? {
        0 => {
            let width = read_u32(&mut input)?;
            let height = read_u32(&mut input)?;
            let len = pixel_bytes(width, height)?;
            let highlight = match read_byte(&mut input)? {
                0 => None,
                1 => {
                    let bounds = HighlightBounds {
                        left: read_u32(&mut input)?,
                        top: read_u32(&mut input)?,
                        right: read_u32(&mut input)?,
                        bottom: read_u32(&mut input)?,
                    };
                    if !valid_highlight(bounds, width, height) {
                        return Err(protocol_error());
                    }
                    Some(bounds)
                }
                _ => return Err(protocol_error()),
            };
            let mut bgra = vec![0; len];
            input.read_exact(&mut bgra)?;
            Ok(RenderedPage {
                width,
                height,
                bgra,
                highlight,
            })
        }
        1 => {
            let len = read_u32(&mut input)? as usize;
            if !(1..=MAX_ERROR_BYTES).contains(&len) {
                return Err(protocol_error());
            }
            Err(read_text(&mut input, len)?)
        }
        _ => return Err(protocol_error()),
    };
    require_end(&mut input)?;
    Ok(reply)
}

fn read_text(input: &mut impl Read, len: usize) -> io::Result<String> {
    let mut text = vec![0; len];
    input.read_exact(&mut text)?;
    String::from_utf8(text).map_err(|_| protocol_error())
}

fn read_magic(input: &mut impl Read, expected: [u8; 4]) -> io::Result<()> {
    let mut actual = [0; 4];
    input.read_exact(&mut actual)?;
    if actual == expected {
        Ok(())
    } else {
        Err(protocol_error())
    }
}

fn read_byte(input: &mut impl Read) -> io::Result<u8> {
    let mut byte = [0];
    input.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_u32(output: &mut impl Write, value: u32) -> io::Result<()> {
    output.write_all(&value.to_le_bytes())
}

fn require_end(input: &mut impl Read) -> io::Result<()> {
    match read_byte(input) {
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
        Ok(_) => Err(protocol_error()),
    }
}

fn protocol_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "PDF 渲染管道数据无效")
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, CREATE_NO_WINDOW};

    use super::*;

    const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
    const TIME_LIMIT: Duration = Duration::from_secs(15);

    fn create_job() -> io::Result<OwnedHandle> {
        // An unnamed, non-inheritable job contains just this render process.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = MEMORY_LIMIT;
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn render_through(
        exe: &Path,
        bytes: Arc<Vec<u8>>,
        page: usize,
        text: &str,
        range: Range<usize>,
    ) -> RenderResult {
        let job = create_job().map_err(|err| format!("无法设定 PDF 渲染进程限制：{err}"))?;
        let started = Instant::now();
        let mut child = Command::new(exe)
            .arg(WORKER_ARGUMENT)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("无法启动 PDF 渲染进程：{err}"))?;
        // The child waits for a request. Attach it to the job before any PDF data is sent.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            let err = io::Error::last_os_error();
            let _ = child.kill();
            return Err(format!("无法约束 PDF 渲染进程：{err}"));
        }
        let mut input = child.stdin.take().ok_or("PDF 渲染输入管道不可用")?;
        let output = child.stdout.take().ok_or("PDF 渲染输出管道不可用")?;

        std::thread::scope(|scope| {
            let pipes = std::thread::Builder::new()
                .spawn_scoped(scope, move || {
                    write_request(&mut input, &bytes, page, text, range)?;
                    drop(input);
                    read_reply(output)
                })
                .map_err(|err| format!("无法启动 PDF 渲染管道：{err}"))?;
            let remaining = TIME_LIMIT.saturating_sub(started.elapsed()).as_millis() as u32;
            let wait = unsafe { WaitForSingleObject(child.as_raw_handle(), remaining) };
            let wait_error = (wait == WAIT_FAILED).then(io::Error::last_os_error);
            if wait != WAIT_OBJECT_0 {
                // Termination closes the worker's endpoints before joining the pipe thread.
                unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
                drop(job);
            }
            let reply = pipes.join();
            match wait {
                WAIT_TIMEOUT => Err("PDF 原页渲染超时".into()),
                WAIT_OBJECT_0 => {
                    let status = child
                        .wait()
                        .map_err(|err| format!("无法取得 PDF 渲染结果：{err}"))?;
                    if !status.success() {
                        return Err("PDF 原页渲染进程提前退出".into());
                    }
                    reply
                        .map_err(|_| "PDF 渲染管道提前退出".to_string())?
                        .map_err(|err| format!("无法读取 PDF 原页渲染结果：{err}"))?
                }
                _ => Err(wait_error.map_or_else(
                    || "无法等待 PDF 渲染进程".into(),
                    |err| format!("无法等待 PDF 渲染进程：{err}"),
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_bytes_and_checks_utf8_boundaries() {
        let mut frame = Vec::new();
        write_request(&mut frame, b"%PDF synthetic", 3, "A 中文", 2..8).unwrap();
        let request = read_request(frame.as_slice()).unwrap();
        assert_eq!(request.bytes.as_slice(), b"%PDF synthetic");
        assert_eq!(request.page, 3);
        assert_eq!(&request.text[request.range], "中文");

        frame.push(0);
        assert!(read_request(frame.as_slice()).is_err());
        assert!(write_request(Vec::new(), b"bytes", 1, "中文", 1..3).is_err());
    }

    #[test]
    fn oversized_requests_and_replies_fail_before_allocating_payloads() {
        let mut request = REQUEST_MAGIC.to_vec();
        for value in [MAX_FILE_BYTES as u32 + 1, 1, 1, 0, 1] {
            write_u32(&mut request, value).unwrap();
        }
        assert_eq!(
            read_request(request.as_slice()).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );

        let mut reply = REPLY_MAGIC.to_vec();
        reply.push(0);
        write_u32(&mut reply, MAX_PAGE_SIDE + 1).unwrap();
        write_u32(&mut reply, 2).unwrap();
        assert_eq!(
            read_reply(reply.as_slice()).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn replies_validate_exact_pixel_count_and_highlight_bounds() {
        let highlight = HighlightBounds {
            left: 0,
            top: 1,
            right: 1,
            bottom: 2,
        };
        let mut frame = Vec::new();
        write_reply(
            &mut frame,
            Ok(RenderedPage {
                width: 1,
                height: 2,
                bgra: vec![255; 8],
                highlight: Some(highlight),
            }),
        )
        .unwrap();
        let page = read_reply(frame.as_slice()).unwrap().unwrap();
        assert_eq!((page.width, page.height, page.bgra), (1, 2, vec![255; 8]));
        assert_eq!(page.highlight, Some(highlight));
        frame.push(1);
        assert!(read_reply(frame.as_slice()).is_err());
        frame.truncate(frame.len() - 2);
        assert!(read_reply(frame.as_slice()).is_err());
        assert!(write_reply(
            Vec::new(),
            Ok(RenderedPage {
                width: 1,
                height: 1,
                bgra: vec![255; 4],
                highlight: Some(highlight),
            })
        )
        .is_err());
    }

    #[test]
    fn worker_error_replies_are_bounded_utf8() {
        let mut frame = Vec::new();
        write_reply(&mut frame, Err("图".repeat(MAX_ERROR_BYTES))).unwrap();
        let regression = match read_reply(frame.as_slice()).unwrap() {
            Err(message) => message,
            Ok(_) => panic!("expected a worker error"),
        };
        assert_eq!(regression, "图".repeat(MAX_ERROR_BYTES / "图".len()));
    }

    fn smoke_pdf() -> Arc<Vec<u8>> {
        use lopdf::{dictionary, Document, Stream};

        let mut pdf = Document::with_version("1.5");
        let pages = pdf.new_object_id();
        let font = pdf.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let contents = pdf.add_object(Stream::new(
            dictionary! {},
            b"0 0 1 rg 27 59 25 15 re f 0 g BT /F1 15 Tf 19 128 Td (needle) Tj ET".to_vec(),
        ));
        let page = pdf.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "Contents" => contents,
            "MediaBox" => vec![0.into(), 0.into(), 240.into(), 160.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        });
        pdf.set_object(
            pages,
            dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()] },
        );
        let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        Arc::new(bytes)
    }

    fn assert_smoke_highlight(rendered: RenderedPage) {
        let highlight = rendered
            .highlight
            .expect("indexed hit should have visible geometry");
        assert_eq!((rendered.width, rendered.height), (480, 320));
        assert!(highlight.left >= 36 && highlight.right <= 150);
        assert!(highlight.top > 0 && highlight.bottom <= 68);
        assert_eq!(rendered.bgra.len(), 480 * 320 * 4);
        let blue = (188 * 480 + 64) * 4;
        assert_eq!(&rendered.bgra[blue..blue + 4], &[255, 0, 0, 255]);
    }

    #[test]
    fn synthetic_worker_fixture_renders_a_highlight() {
        let rendered = pdf_preview::render(smoke_pdf(), 1, "needle\n", 0..6).unwrap();
        assert_smoke_highlight(rendered);
    }

    #[cfg(windows)]
    #[test]
    fn real_executable_renders_a_highlight_through_the_job() {
        let Some(exe) = std::env::var_os("OILFIND_PDF_WORKER_TEST_EXE") else {
            return;
        };
        let rendered =
            windows::render_through(std::path::Path::new(&exe), smoke_pdf(), 1, "needle\n", 0..6)
                .unwrap();
        assert_smoke_highlight(rendered);
    }
}
